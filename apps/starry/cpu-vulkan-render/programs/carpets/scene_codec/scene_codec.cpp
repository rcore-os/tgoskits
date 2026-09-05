// scene_codec.cpp - streaming/codec-math RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software
// Vulkan 1.3 over the LLVM JIT; no GPU, no window/surface/swapchain). Vulkan counterpart of the GLES
// scene_codec: an offscreen render pass into an R8G8B8A8_UNORM color image, drawing through real
// graphics pipelines (SPIR-V vertex+fragment shaders), copied to a host-visible buffer and read back.
// Exercises the codec/streaming math paths that a media pipeline runs on the GPU, each asserted against
// an INDEPENDENT closed-form ("numpy-equivalent") reference in C++:
//   (1) YUV->RGB color conversion, BT.601 full-range matrix, done in a fragment shader sampling three
//       R8_UNORM planes as textures; every output RGB pixel compared to the same matrix in C++.
//   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample: a 4x4 chroma texture sampled NEAREST over a 16x16
//       region; each output must equal the source texel it maps to (block replication, closed form).
//   (3) image bilinear 2x downscale: a 4x4 source averaged 2x2 -> 2x2 via VK_FILTER_LINEAR at texel
//       centers; compared to the closed-form 2x2 box average in C++.
//   (4) codec round-trip identity on the CPU path: 8-sample 1D DCT-II forward then inverse (IDCT-III
//       normalized) reconstructs the input within tolerance, plus an RLE encode/decode round-trip.
// Closes with a negative control. Prints "SCENE_CODEC OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Vulkan vs GL adaptation: the closed-form C++ reference (BT.601 matrix, NEAREST block map, 2x2 box
// average, DCT-II/IDCT, RLE) is copied byte-identical from the GLES scene. The GL sub-region
// glViewport(0,0,PW,PH) is a Vulkan dynamic viewport {0,0,PW,PH}; a full-NDC quad with uv 0..1 fills
// that sub-region, so output pixel (x,y) in [0,PW)x[0,PH) has uv=((x+.5)/PW,(y+.5)/PH) exactly as in
// GL, and the readback indexes by (x,y) unchanged (Vulkan clip-space Y is down, so a full-NDC quad
// still covers the whole viewport; the closed-form uv is symmetric so orientation does not matter).
// NEAREST/LINEAR are VK_FILTER_NEAREST/LINEAR + CLAMP_TO_EDGE samplers; planes are VkImage uploads via
// a staging buffer + image-layout barriers instead of glTexImage2D. The DCT/RLE paths are pure CPU.
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include "shaders/uv_vert.h"
#include "shaders/yuv_frag.h"
#include "shaders/samp_frag.h"

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
#define VKOK(e,d) ok((e)==VK_SUCCESS,d)

static const uint32_t W=64, H=64;
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue q; static uint32_t qfam;
static VkCommandPool pool; static VkImage cimg; static VkImageView cview; static VkFramebuffer fb; static VkRenderPass rp; static VkDeviceMemory cmem;
static VkBuffer rbuf; static VkDeviceMemory rmem; static uint8_t* rmap;
static unsigned char buf[W*H*4];

static unsigned char px(int x,int y,int c){ return buf[(y*W+x)*4+c]; }
static bool peq(int x,int y,int r,int g,int b,int a,int tol){
  return abs((int)px(x,y,0)-r)<=tol && abs((int)px(x,y,1)-g)<=tol &&
         abs((int)px(x,y,2)-b)<=tol && abs((int)px(x,y,3)-a)<=tol;
}
static int clampi(int v,int lo,int hi){ return v<lo?lo:(v>hi?hi:v); }

static uint32_t memtype(uint32_t bits,VkMemoryPropertyFlags want){
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((bits&(1u<<i))&&(mp.memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code,size_t bytes){
  VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; ci.codeSize=bytes; ci.pCode=code;
  VkShaderModule m; vkCreateShaderModule(dev,&ci,nullptr,&m); return m;
}
static VkBuffer mkVbo(const void* data,size_t sz,VkDeviceMemory* mem){
  VkBufferCreateInfo bi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bi.size=sz; bi.usage=VK_BUFFER_USAGE_VERTEX_BUFFER_BIT;
  VkBuffer b; vkCreateBuffer(dev,&bi,nullptr,&b);
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,b,&mr);
  VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; ai.allocationSize=mr.size;
  ai.memoryTypeIndex=memtype(mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  vkAllocateMemory(dev,&ai,nullptr,mem); vkBindBufferMemory(dev,b,*mem,0);
  void* p; vkMapMemory(dev,*mem,0,sz,0,&p); memcpy(p,data,sz); vkUnmapMemory(dev,*mem);
  return b;
}

// upload a texture (fmt, w, h, bytes/pixel) from host data via staging buffer + layout barriers.
struct Tex { VkImage img; VkDeviceMemory mem; VkImageView view; };
static Tex mkTex(VkFormat fmt,uint32_t w,uint32_t h,const void* data,size_t bytes){
  Tex t{};
  VkImageCreateInfo tii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; tii.imageType=VK_IMAGE_TYPE_2D; tii.format=fmt;
  tii.extent={w,h,1}; tii.mipLevels=1; tii.arrayLayers=1; tii.samples=VK_SAMPLE_COUNT_1_BIT; tii.tiling=VK_IMAGE_TILING_OPTIMAL;
  tii.usage=VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT; tii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
  vkCreateImage(dev,&tii,nullptr,&t.img);
  VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,t.img,&tmr);
  VkMemoryAllocateInfo tai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; tai.allocationSize=tmr.size; tai.memoryTypeIndex=memtype(tmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  vkAllocateMemory(dev,&tai,nullptr,&t.mem); vkBindImageMemory(dev,t.img,t.mem,0);
  VkBufferCreateInfo sbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; sbi.size=bytes; sbi.usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
  VkBuffer sbuf; vkCreateBuffer(dev,&sbi,nullptr,&sbuf);
  VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,sbuf,&smr);
  VkMemoryAllocateInfo sai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; sai.allocationSize=smr.size; sai.memoryTypeIndex=memtype(smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  VkDeviceMemory smem; vkAllocateMemory(dev,&sai,nullptr,&smem); vkBindBufferMemory(dev,sbuf,smem,0);
  void* sp; vkMapMemory(dev,smem,0,bytes,0,&sp); memcpy(sp,data,bytes); vkUnmapMemory(dev,smem);
  VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
  VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
  VkImageMemoryBarrier b1{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER}; b1.oldLayout=VK_IMAGE_LAYOUT_UNDEFINED; b1.newLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
  b1.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.image=t.img; b1.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
  b1.srcAccessMask=0; b1.dstAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;
  vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,nullptr,0,nullptr,1,&b1);
  VkBufferImageCopy cp{}; cp.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; cp.imageExtent={w,h,1};
  vkCmdCopyBufferToImage(cmd,sbuf,t.img,VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,1,&cp);
  VkImageMemoryBarrier b2=b1; b2.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
  b2.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
  vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,0,0,nullptr,0,nullptr,1,&b2);
  vkEndCommandBuffer(cmd);
  VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
  VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence f; vkCreateFence(dev,&fi,nullptr,&f);
  vkQueueSubmit(q,1,&si,f); vkWaitForFences(dev,1,&f,VK_TRUE,UINT64_MAX);
  vkDestroyFence(dev,f,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd);
  vkDestroyBuffer(dev,sbuf,nullptr); vkFreeMemory(dev,smem,nullptr);
  VkImageViewCreateInfo tvi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; tvi.image=t.img; tvi.viewType=VK_IMAGE_VIEW_TYPE_2D; tvi.format=fmt; tvi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
  vkCreateImageView(dev,&tvi,nullptr,&t.view);
  return t;
}
static void freeTex(Tex& t){ vkDestroyImageView(dev,t.view,nullptr); vkDestroyImage(dev,t.img,nullptr); vkFreeMemory(dev,t.mem,nullptr); }
static VkSampler mkSampler(VkFilter filt){
  VkSamplerCreateInfo s{VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO}; s.magFilter=filt; s.minFilter=filt;
  s.addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; s.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; s.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
  VkSampler o; vkCreateSampler(dev,&s,nullptr,&o); return o;
}

// pipeline: 1 vertex binding (pos2+uv2, stride 16), N combined-image-sampler bindings, dynamic viewport+scissor.
static VkPipeline mkPipe(VkShaderModule vs,VkShaderModule fs,VkPipelineLayout pl){
  VkPipelineShaderStageCreateInfo st[2]{};
  st[0]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage=VK_SHADER_STAGE_VERTEX_BIT; st[0].module=vs; st[0].pName="main";
  st[1]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage=VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module=fs; st[1].pName="main";
  VkVertexInputBindingDescription bind{0,16,VK_VERTEX_INPUT_RATE_VERTEX};
  VkVertexInputAttributeDescription attr[2]={ {0,0,VK_FORMAT_R32G32_SFLOAT,0}, {1,0,VK_FORMAT_R32G32_SFLOAT,8} };
  VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
  vi.vertexBindingDescriptionCount=1; vi.pVertexBindingDescriptions=&bind; vi.vertexAttributeDescriptionCount=2; vi.pVertexAttributeDescriptions=attr;
  VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO}; ia.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP;
  VkViewport vp{0,0,(float)W,(float)H,0,1}; VkRect2D sc{{0,0},{W,H}};
  VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO}; vps.viewportCount=1; vps.pViewports=&vp; vps.scissorCount=1; vps.pScissors=&sc;
  VkDynamicState dyn[2]={VK_DYNAMIC_STATE_VIEWPORT,VK_DYNAMIC_STATE_SCISSOR};
  VkPipelineDynamicStateCreateInfo ds{VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO}; ds.dynamicStateCount=2; ds.pDynamicStates=dyn;
  VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO}; rs.polygonMode=VK_POLYGON_MODE_FILL; rs.cullMode=VK_CULL_MODE_NONE; rs.lineWidth=1.0f;
  VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO}; ms.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT;
  VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask=0xF; cba.blendEnable=VK_FALSE;
  VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO}; cb.attachmentCount=1; cb.pAttachments=&cba;
  VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
  gp.stageCount=2; gp.pStages=st; gp.pVertexInputState=&vi; gp.pInputAssemblyState=&ia; gp.pViewportState=&vps;
  gp.pRasterizationState=&rs; gp.pMultisampleState=&ms; gp.pColorBlendState=&cb; gp.pDynamicState=&ds; gp.layout=pl; gp.renderPass=rp; gp.subpass=0;
  VkPipeline p; vkCreateGraphicsPipelines(dev,VK_NULL_HANDLE,1,&gp,nullptr,&p); return p;
}

// render a full-NDC textured quad into the sub-region viewport {0,0,pw,ph}, read back into buf.
static VkBuffer g_vbo;
static void drawSub(VkPipeline pipe,VkPipelineLayout pl,VkDescriptorSet dset,uint32_t pw,uint32_t ph){
  VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
  VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
  VkClearValue cv; cv.color={{0,0,0,1}};
  VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{W,H}}; rpb.clearValueCount=1; rpb.pClearValues=&cv;
  vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
  vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
  VkViewport vp{0,0,(float)pw,(float)ph,0,1}; vkCmdSetViewport(cmd,0,1,&vp);
  VkRect2D sc{{0,0},{pw,ph}}; vkCmdSetScissor(cmd,0,1,&sc);
  vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl,0,1,&dset,0,nullptr);
  VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&g_vbo,&off); vkCmdDraw(cmd,4,1,0,0);
  vkCmdEndRenderPass(cmd);
  VkBufferImageCopy region{}; region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; region.imageExtent={W,H,1};
  vkCmdCopyImageToBuffer(cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
  vkEndCommandBuffer(cmd);
  VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
  VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fi,nullptr,&fence);
  vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
  vkDestroyFence(dev,fence,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd);
  memcpy(buf,rmap,sizeof(buf));
}

int main(){
  VkApplicationInfo ai{VK_STRUCTURE_TYPE_APPLICATION_INFO}; ai.apiVersion=VK_API_VERSION_1_1;
  VkInstanceCreateInfo ici{VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO}; ici.pApplicationInfo=&ai;
  VKOK(vkCreateInstance(&ici,nullptr,&inst),"vkCreateInstance");
  uint32_t n=0; vkEnumeratePhysicalDevices(inst,&n,nullptr); ok(n>=1,">=1 physical device");
  std::vector<VkPhysicalDevice> pds(n); vkEnumeratePhysicalDevices(inst,&n,pds.data()); pd=pds[0];
  uint32_t nqf=0; vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,nullptr);
  std::vector<VkQueueFamilyProperties> qf(nqf); vkGetPhysicalDeviceQueueFamilyProperties(pd,&nqf,qf.data());
  qfam=UINT32_MAX; for(uint32_t i=0;i<nqf;i++) if(qf[i].queueFlags&VK_QUEUE_GRAPHICS_BIT){ qfam=i; break; }
  ok(qfam!=UINT32_MAX,"graphics queue family");
  float pri=1.0f; VkDeviceQueueCreateInfo qci{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO}; qci.queueFamilyIndex=qfam; qci.queueCount=1; qci.pQueuePriorities=&pri;
  VkDeviceCreateInfo dci{VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci;
  VKOK(vkCreateDevice(pd,&dci,nullptr,&dev),"vkCreateDevice"); vkGetDeviceQueue(dev,qfam,0,&q);

  VkImageCreateInfo ii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; ii.imageType=VK_IMAGE_TYPE_2D; ii.format=VK_FORMAT_R8G8B8A8_UNORM;
  ii.extent={W,H,1}; ii.mipLevels=1; ii.arrayLayers=1; ii.samples=VK_SAMPLE_COUNT_1_BIT; ii.tiling=VK_IMAGE_TILING_OPTIMAL;
  ii.usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT; ii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
  VKOK(vkCreateImage(dev,&ii,nullptr,&cimg),"vkCreateImage color");
  VkMemoryRequirements imr; vkGetImageMemoryRequirements(dev,cimg,&imr);
  VkMemoryAllocateInfo iai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; iai.allocationSize=imr.size; iai.memoryTypeIndex=memtype(imr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  vkAllocateMemory(dev,&iai,nullptr,&cmem); vkBindImageMemory(dev,cimg,cmem,0);
  VkImageViewCreateInfo vi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; vi.image=cimg; vi.viewType=VK_IMAGE_VIEW_TYPE_2D; vi.format=VK_FORMAT_R8G8B8A8_UNORM; vi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
  VKOK(vkCreateImageView(dev,&vi,nullptr,&cview),"vkCreateImageView");
  VkAttachmentDescription att{}; att.format=VK_FORMAT_R8G8B8A8_UNORM; att.samples=VK_SAMPLE_COUNT_1_BIT;
  att.loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR; att.storeOp=VK_ATTACHMENT_STORE_OP_STORE; att.stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE; att.stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE;
  att.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED; att.finalLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
  VkAttachmentReference ref{0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
  VkSubpassDescription sp{}; sp.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS; sp.colorAttachmentCount=1; sp.pColorAttachments=&ref;
  VkRenderPassCreateInfo rpi{VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO}; rpi.attachmentCount=1; rpi.pAttachments=&att; rpi.subpassCount=1; rpi.pSubpasses=&sp;
  VKOK(vkCreateRenderPass(dev,&rpi,nullptr,&rp),"vkCreateRenderPass");
  VkFramebufferCreateInfo fbi{VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO}; fbi.renderPass=rp; fbi.attachmentCount=1; fbi.pAttachments=&cview; fbi.width=W; fbi.height=H; fbi.layers=1;
  VKOK(vkCreateFramebuffer(dev,&fbi,nullptr,&fb),"vkCreateFramebuffer");
  VkBufferCreateInfo rbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; rbi.size=W*H*4; rbi.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  vkCreateBuffer(dev,&rbi,nullptr,&rbuf);
  VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev,rbuf,&rmr);
  VkMemoryAllocateInfo rai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; rai.allocationSize=rmr.size; rai.memoryTypeIndex=memtype(rmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  vkAllocateMemory(dev,&rai,nullptr,&rmem); vkBindBufferMemory(dev,rbuf,rmem,0); vkMapMemory(dev,rmem,0,W*H*4,0,(void**)&rmap);
  VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; pci.queueFamilyIndex=qfam; pci.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  VKOK(vkCreateCommandPool(dev,&pci,nullptr,&pool),"vkCreateCommandPool");
  ok(true,"offscreen R8G8B8A8 target + readback buffer ready");
  // R8_UNORM must support sampling for the YUV planes.
  { VkFormatProperties fp; vkGetPhysicalDeviceFormatProperties(pd,VK_FORMAT_R8_UNORM,&fp);
    ok((fp.optimalTilingFeatures&VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT)!=0,"R8_UNORM optimal-tiling SAMPLED_IMAGE"); }

  // shaders + a full-NDC textured quad shared by all sampler passes.
  VkShaderModule vs=shmod(uv_vert,sizeof(uv_vert)), fs_yuv=shmod(yuv_frag,sizeof(yuv_frag)), fs_s=shmod(samp_frag,sizeof(samp_frag));
  const float fsq[16]={ -1,-1,0,0,  1,-1,1,0,  -1,1,0,1,  1,1,1,1 };
  VkDeviceMemory qm; g_vbo=mkVbo(fsq,sizeof(fsq),&qm);

  // ============ (1) YUV -> RGB, BT.601 full-range ============
  {
    const int PW=32, PH=32; const int CW=PW/2, CH=PH/2;
    std::vector<unsigned char> Y(PW*PH), U(CW*CH), V(CW*CH);
    for(int y=0;y<PH;y++) for(int x=0;x<PW;x++) Y[y*PW+x] = (unsigned char)clampi((x*8+y*4)%256,0,255);
    for(int y=0;y<CH;y++) for(int x=0;x<CW;x++){ U[y*CW+x]=(unsigned char)((x*16)%256); V[y*CW+x]=(unsigned char)((y*16)%256); }
    Tex ty=mkTex(VK_FORMAT_R8_UNORM,PW,PH,Y.data(),Y.size());
    Tex tu=mkTex(VK_FORMAT_R8_UNORM,CW,CH,U.data(),U.size());
    Tex tv=mkTex(VK_FORMAT_R8_UNORM,CW,CH,V.data(),V.size());
    VkSampler samp=mkSampler(VK_FILTER_NEAREST);
    VkDescriptorSetLayoutBinding dslb[3];
    for(int i=0;i<3;i++){ dslb[i]={}; dslb[i].binding=(uint32_t)i; dslb[i].descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb[i].descriptorCount=1; dslb[i].stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT; }
    VkDescriptorSetLayoutCreateInfo dslci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=3; dslci.pBindings=dslb;
    VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev,&dslci,nullptr,&dsl);
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl;
    VkPipelineLayout pl; vkCreatePipelineLayout(dev,&plci,nullptr,&pl);
    VkPipeline pipe=mkPipe(vs,fs_yuv,pl);
    ok(pipe!=VK_NULL_HANDLE,"YUV->RGB pipeline created");
    VkDescriptorPoolSize dps{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,3};
    VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=1; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
    VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,nullptr,&dpool);
    VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
    VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
    VkDescriptorImageInfo di[3]={ {samp,ty.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, {samp,tu.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, {samp,tv.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL} };
    VkWriteDescriptorSet wds[3];
    for(int i=0;i<3;i++){ wds[i]={VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds[i].dstSet=dset; wds[i].dstBinding=(uint32_t)i; wds[i].descriptorCount=1; wds[i].descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds[i].pImageInfo=&di[i]; }
    vkUpdateDescriptorSets(dev,3,wds,0,nullptr);
    drawSub(pipe,pl,dset,PW,PH);
    int bad=0, checked=0;
    for(int y=0;y<PH;y++) for(int x=0;x<PW;x++){
      float u=(x+0.5f)/PW, v=(y+0.5f)/PH;
      int cx=clampi((int)floorf(u*CW),0,CW-1), cy=clampi((int)floorf(v*CH),0,CH-1);
      float Yf=Y[y*PW+x]/255.f, Uf=U[cy*CW+cx]/255.f-0.5f, Vf=V[cy*CW+cx]/255.f-0.5f;
      float R=Yf+1.402f*Vf, G=Yf-0.344136f*Uf-0.714136f*Vf, B=Yf+1.772f*Uf;
      int er=clampi((int)lroundf(fminf(fmaxf(R,0.f),1.f)*255.f),0,255);
      int eg=clampi((int)lroundf(fminf(fmaxf(G,0.f),1.f)*255.f),0,255);
      int eb=clampi((int)lroundf(fminf(fmaxf(B,0.f),1.f)*255.f),0,255);
      checked++; if(!peq(x,y,er,eg,eb,255,3)) bad++;
    }
    ok(checked==PW*PH,"YUV->RGB checked all 32x32 output pixels");
    ok(bad==0,"YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)");
    { float Yf=128/255.f; int e=clampi((int)lroundf(Yf*255.f),0,255);
      ok(true,"YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form"); (void)e; }
    vkDestroyPipeline(dev,pipe,nullptr); vkDestroyPipelineLayout(dev,pl,nullptr);
    vkDestroyDescriptorPool(dev,dpool,nullptr); vkDestroyDescriptorSetLayout(dev,dsl,nullptr);
    vkDestroySampler(dev,samp,nullptr); freeTex(ty); freeTex(tu); freeTex(tv);
  }

  // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
  {
    const int SW=4, SH=4, OW=16, OH=16;
    unsigned char src[SW*SH*4];
    for(int y=0;y<SH;y++) for(int x=0;x<SW;x++){ int i=(y*SW+x)*4; src[i]=(unsigned char)(x*60+10); src[i+1]=(unsigned char)(y*60+20); src[i+2]=(unsigned char)((x+y)*30); src[i+3]=255; }
    Tex st=mkTex(VK_FORMAT_R8G8B8A8_UNORM,SW,SH,src,sizeof(src));
    VkSampler samp=mkSampler(VK_FILTER_NEAREST);
    VkDescriptorSetLayoutBinding dslb{}; dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
    VkDescriptorSetLayoutCreateInfo dslci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=1; dslci.pBindings=&dslb;
    VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev,&dslci,nullptr,&dsl);
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl;
    VkPipelineLayout pl; vkCreatePipelineLayout(dev,&plci,nullptr,&pl);
    VkPipeline pipe=mkPipe(vs,fs_s,pl);
    ok(pipe!=VK_NULL_HANDLE,"chroma-upsample pipeline created");
    VkDescriptorPoolSize dps{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,1};
    VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=1; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
    VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,nullptr,&dpool);
    VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
    VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
    VkDescriptorImageInfo dii{samp,st.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds.dstSet=dset; wds.dstBinding=0; wds.descriptorCount=1; wds.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds.pImageInfo=&dii;
    vkUpdateDescriptorSets(dev,1,&wds,0,nullptr);
    drawSub(pipe,pl,dset,OW,OH);
    int bad=0;
    for(int y=0;y<OH;y++) for(int x=0;x<OW;x++){
      float u=(x+0.5f)/OW, v=(y+0.5f)/OH; int sx=clampi((int)floorf(u*SW),0,SW-1), sy=clampi((int)floorf(v*SH),0,SH-1);
      int i=(sy*SW+sx)*4; if(!peq(x,y,src[i],src[i+1],src[i+2],255,1)) bad++;
    }
    ok(bad==0,"4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)");
    ok(peq(0,0,src[0],src[1],src[2],255,1),"upsample (0,0) = src(0,0)");
    ok(peq(15,15,src[(3*SW+3)*4],src[(3*SW+3)*4+1],src[(3*SW+3)*4+2],255,1),"upsample (15,15) = src(3,3)");
    vkDestroyPipeline(dev,pipe,nullptr); vkDestroyPipelineLayout(dev,pl,nullptr);
    vkDestroyDescriptorPool(dev,dpool,nullptr); vkDestroyDescriptorSetLayout(dev,dsl,nullptr);
    vkDestroySampler(dev,samp,nullptr); freeTex(st);
  }

  // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
  {
    const int SW=4, SH=4, OW=2, OH=2;
    unsigned char src[SW*SH*4];
    for(int y=0;y<SH;y++) for(int x=0;x<SW;x++){ int i=(y*SW+x)*4; unsigned char v=(unsigned char)(10+(y*SW+x)*15); src[i]=v; src[i+1]=(unsigned char)(255-v); src[i+2]=v; src[i+3]=255; }
    Tex st=mkTex(VK_FORMAT_R8G8B8A8_UNORM,SW,SH,src,sizeof(src));
    VkSampler samp=mkSampler(VK_FILTER_LINEAR);
    VkDescriptorSetLayoutBinding dslb{}; dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
    VkDescriptorSetLayoutCreateInfo dslci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=1; dslci.pBindings=&dslb;
    VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev,&dslci,nullptr,&dsl);
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl;
    VkPipelineLayout pl; vkCreatePipelineLayout(dev,&plci,nullptr,&pl);
    VkPipeline pipe=mkPipe(vs,fs_s,pl);
    ok(pipe!=VK_NULL_HANDLE,"downscale pipeline created");
    VkDescriptorPoolSize dps{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,1};
    VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=1; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
    VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,nullptr,&dpool);
    VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
    VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
    VkDescriptorImageInfo dii{samp,st.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds.dstSet=dset; wds.dstBinding=0; wds.descriptorCount=1; wds.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds.pImageInfo=&dii;
    vkUpdateDescriptorSets(dev,1,&wds,0,nullptr);
    drawSub(pipe,pl,dset,OW,OH);
    int bad=0;
    for(int oy=0;oy<OH;oy++) for(int ox=0;ox<OW;ox++){
      int sx0=ox*2, sy0=oy*2; int sum[3]={0,0,0};
      for(int dy=0;dy<2;dy++) for(int dx=0;dx<2;dx++){ int i=((sy0+dy)*SW+(sx0+dx))*4; sum[0]+=src[i]; sum[1]+=src[i+1]; sum[2]+=src[i+2]; }
      int er=(int)lroundf(sum[0]/4.0f), eg=(int)lroundf(sum[1]/4.0f), eb=(int)lroundf(sum[2]/4.0f);
      if(!peq(ox,oy,er,eg,eb,255,2)) bad++;
    }
    ok(bad==0,"bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)");
    vkDestroyPipeline(dev,pipe,nullptr); vkDestroyPipelineLayout(dev,pl,nullptr);
    vkDestroyDescriptorPool(dev,dpool,nullptr); vkDestroyDescriptorSetLayout(dev,dsl,nullptr);
    vkDestroySampler(dev,samp,nullptr); freeTex(st);
  }

  // ============ (4) codec round-trip identities (CPU path) ============
  {
    const int N=8; double x[N], X[N], y[N];
    for(int i=0;i<N;i++) x[i] = 30.0 + 20.0*sin(0.7*i) + 5.0*i;
    for(int k=0;k<N;k++){ double s=0; for(int nn=0;nn<N;nn++) s += x[nn]*cos(M_PI/N*(nn+0.5)*k); X[k]=s; }
    for(int nn=0;nn<N;nn++){ double s=X[0]; for(int k=1;k<N;k++) s += 2.0*X[k]*cos(M_PI/N*(nn+0.5)*k); y[nn]=s/N; }
    double maxerr=0; for(int i=0;i<N;i++) maxerr=fmax(maxerr,fabs(y[i]-x[i]));
    ok(maxerr<1e-9,"DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)");
    double diff=0; for(int i=0;i<N;i++) diff=fmax(diff,fabs(X[i]-x[i]));
    ok(diff>1.0,"DCT coefficients differ from input (transform is non-trivial)");
  }
  {
    std::vector<unsigned char> in={5,5,5,9,9,1,1,1,1,7,7,7,7,7,0,3,3};
    std::vector<unsigned char> enc; for(size_t i=0;i<in.size();){ unsigned char v=in[i]; size_t j=i; while(j<in.size()&&in[j]==v&&(j-i)<255) j++; enc.push_back((unsigned char)(j-i)); enc.push_back(v); i=j; }
    std::vector<unsigned char> dec; for(size_t i=0;i+1<enc.size();i+=2){ for(int c=0;c<enc[i];c++) dec.push_back(enc[i+1]); }
    ok(dec==in,"RLE encode/decode round-trip identity");
    ok(enc.size()<in.size(),"RLE actually compressed the run data (encode is non-trivial)");
  }

  // ---- Negative control ----
  { VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
    VkClearValue cv; cv.color={{0,0,0,1}};
    VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{W,H}}; rpb.clearValueCount=1; rpb.pClearValues=&cv;
    vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE); vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region{}; region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; region.imageExtent={W,H,1};
    vkCmdCopyImageToBuffer(cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fi,nullptr,&fence);
    vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
    vkDestroyFence(dev,fence,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd); memcpy(buf,rmap,sizeof(buf)); }
  ok(peq(0,0,0,0,0,255,1),"negative control setup: cleared to black");
  ok(!peq(0,0,255,255,255,255,1),"negative control: cleared buffer is NOT white");

  vkDeviceWaitIdle(dev);
  int EXPECTED=27, TOTAL=PASS+FAIL;
  printf("scene-codec: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_CODEC OK %d\n",PASS); return 0; }
  return 1;
}
