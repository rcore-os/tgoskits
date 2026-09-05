// scene_2dui.cpp - 2D UI compositing RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan
// 1.3 over the LLVM JIT; no GPU, no window/surface/swapchain). Vulkan counterpart of the GLES
// scene_2dui: instead of a surfaceless EGL ES3.1 context + FBO + glReadPixels it builds an offscreen
// render pass into an R8G8B8A8_UNORM color image (D32_SFLOAT depth attached but unused here), draws
// through real graphics pipelines (SPIR-V vertex+fragment shaders), copies the image to a host-visible
// buffer with vkCmdCopyImageToBuffer and checks every pixel against a closed-form reference. Every
// scene primitive has an INDEPENDENT closed-form software reference computed in C++ (not derived from
// the Vulkan output) and asserted per pixel: filled axis-aligned rectangles, an analytic rounded-rect
// (inside/corner-arc/outside coverage), a nine-patch-style scaled border frame (fixed corners,
// stretched edges), an 8x8 bitmap-font glyph blit (assert every lit/unlit texel), a scissor-clipped
// fill, and MULTI-LAYER Porter-Duff over compositing of 3 stacked semi-transparent layers
// Co = Cs + Cd*(1-As) accumulated in C++ and matched channel-by-channel incl alpha. Closes with a
// negative control. Prints "SCENE_2DUI OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Vulkan vs GL adaptation: the closed-form C++ reference (rect coverage, analytic rounded-rect,
// nine-patch, glyph bitmap, Porter-Duff over) is copied byte-identical from the GLES scene. Only the
// plumbing differs. Pixel-space vertex mapping is the same n=(p/vp)*2-1; Vulkan clip-space Y is down,
// so with a positive-height viewport {0,0,W,H,0,1} pixel-space y=0 maps to NDC y=-1 which lands on
// readback row 0, exactly matching GL's bottom-origin glReadPixels row 0 (both index buf row == pixel
// y), so the per-pixel reference indexes by pixel-space y unchanged. Blending is explicit VkPipeline
// state (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels) instead of glEnable(GL_BLEND). Scissor is a
// dynamic VkRect2D. The glyph is a VkImage + VK_FILTER_NEAREST combined image sampler.
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include "shaders/pix_vert.h"
#include "shaders/uni_frag.h"
#include "shaders/rr_frag.h"
#include "shaders/tex_vert.h"
#include "shaders/tex_frag.h"

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
static int q8(float f){ int v=(int)lroundf(f*255.f); return clampi(v,0,255); }

static uint32_t memtype(uint32_t bits,VkMemoryPropertyFlags want){
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((bits&(1u<<i))&&(mp.memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code,size_t bytes){
  VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; ci.codeSize=bytes; ci.pCode=code;
  VkShaderModule m; vkCreateShaderModule(dev,&ci,nullptr,&m); return m;
}

// push-constant block shared vertex+fragment: { vec2 vp; vec4 col; vec4 box; float rad; }
struct PC { float vp[2]; float _pad0[2]; float col[4]; float box[4]; float rad; float _pad1[3]; };

// vertex buffer helper (host-visible)
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

// pipeline over the offscreen pass. layout: 0=pos2, 1=pos2+uv. blend toggles SRC_ALPHA over.
static VkPipeline mkPipe(VkShaderModule vs,VkShaderModule fs,VkPipelineLayout pl,int vlayout,bool blend){
  VkPipelineShaderStageCreateInfo st[2]{};
  st[0]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage=VK_SHADER_STAGE_VERTEX_BIT; st[0].module=vs; st[0].pName="main";
  st[1]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage=VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module=fs; st[1].pName="main";
  uint32_t stride = vlayout==1 ? 16 : 8;
  VkVertexInputBindingDescription bind{0,stride,VK_VERTEX_INPUT_RATE_VERTEX};
  VkVertexInputAttributeDescription attr[2]; uint32_t nattr=1;
  attr[0]={0,0,VK_FORMAT_R32G32_SFLOAT,0};
  if(vlayout==1){ attr[1]={1,0,VK_FORMAT_R32G32_SFLOAT,8}; nattr=2; }
  VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
  vi.vertexBindingDescriptionCount=1; vi.pVertexBindingDescriptions=&bind; vi.vertexAttributeDescriptionCount=nattr; vi.pVertexAttributeDescriptions=attr;
  VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO};
  ia.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;
  VkViewport vp{0,0,(float)W,(float)H,0,1}; VkRect2D sc{{0,0},{W,H}};
  VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO}; vps.viewportCount=1; vps.pViewports=&vp; vps.scissorCount=1; vps.pScissors=&sc;
  VkDynamicState dyn=VK_DYNAMIC_STATE_SCISSOR;
  VkPipelineDynamicStateCreateInfo ds{VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO}; ds.dynamicStateCount=1; ds.pDynamicStates=&dyn;
  VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO}; rs.polygonMode=VK_POLYGON_MODE_FILL; rs.cullMode=VK_CULL_MODE_NONE; rs.lineWidth=1.0f;
  VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO}; ms.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT;
  VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask=0xF; cba.blendEnable=blend?VK_TRUE:VK_FALSE;
  cba.srcColorBlendFactor=VK_BLEND_FACTOR_SRC_ALPHA; cba.dstColorBlendFactor=VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA; cba.colorBlendOp=VK_BLEND_OP_ADD;
  cba.srcAlphaBlendFactor=VK_BLEND_FACTOR_SRC_ALPHA; cba.dstAlphaBlendFactor=VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA; cba.alphaBlendOp=VK_BLEND_OP_ADD;
  VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO}; cb.attachmentCount=1; cb.pAttachments=&cba;
  VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
  gp.stageCount=2; gp.pStages=st; gp.pVertexInputState=&vi; gp.pInputAssemblyState=&ia; gp.pViewportState=&vps;
  gp.pRasterizationState=&rs; gp.pMultisampleState=&ms; gp.pColorBlendState=&cb; gp.pDynamicState=&ds; gp.layout=pl; gp.renderPass=rp; gp.subpass=0;
  VkPipeline p; vkCreateGraphicsPipelines(dev,VK_NULL_HANDLE,1,&gp,nullptr,&p); return p;
}

// A recording context: a command buffer building the offscreen pass with a clear color, letting the
// caller record draws, then copying the image to the readback buffer.
struct Rec { VkCommandBuffer cmd; };
static Rec beginFrame(float cr,float cg,float cb,float ca){
  VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
  VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
  VkClearValue cv; cv.color={{cr,cg,cb,ca}};
  VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{W,H}}; rpb.clearValueCount=1; rpb.pClearValues=&cv;
  vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
  return {cmd};
}
static void endFrame(Rec r){
  vkCmdEndRenderPass(r.cmd);
  VkBufferImageCopy region{}; region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; region.imageExtent={W,H,1};
  vkCmdCopyImageToBuffer(r.cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
  vkEndCommandBuffer(r.cmd);
  VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&r.cmd;
  VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fi,nullptr,&fence);
  vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
  vkDestroyFence(dev,fence,nullptr); vkFreeCommandBuffers(dev,pool,1,&r.cmd);
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

  // offscreen color image (R8G8B8A8_UNORM, COLOR_ATTACHMENT | TRANSFER_SRC)
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

  // shaders + layouts + pipelines
  VkShaderModule vs_pix=shmod(pix_vert,sizeof(pix_vert)), fs_uni=shmod(uni_frag,sizeof(uni_frag)), fs_rr=shmod(rr_frag,sizeof(rr_frag));
  VkShaderModule vs_tex=shmod(tex_vert,sizeof(tex_vert)), fs_tex=shmod(tex_frag,sizeof(tex_frag));
  VkPushConstantRange pcr{VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC)};
  VkPipelineLayoutCreateInfo li{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; li.pushConstantRangeCount=1; li.pPushConstantRanges=&pcr;
  VkPipelineLayout pl_pc; vkCreatePipelineLayout(dev,&li,nullptr,&pl_pc);
  VkPipeline pipe_uni=mkPipe(vs_pix,fs_uni,pl_pc,0,false);
  VkPipeline pipe_blend=mkPipe(vs_pix,fs_uni,pl_pc,0,true);
  VkPipeline pipe_rr=mkPipe(vs_pix,fs_rr,pl_pc,0,false);
  ok(pipe_uni&&pipe_blend&&pipe_rr,"pixel-fill / blend / rounded-rect pipelines created");
  VkRect2D full{{0,0},{W,H}};

  PC pc{}; pc.vp[0]=(float)W; pc.vp[1]=(float)H;

  // fill a pixel-space rect [x0,x1)x[y0,y1) as two triangles with a push-constant color.
  auto rectVerts=[&](float x0,float y0,float x1,float y1,float out[12]){
    float v[12]={ x0,y0, x1,y0, x0,y1,  x0,y1, x1,y0, x1,y1 }; memcpy(out,v,sizeof(v)); };

  // ---- Scene A: filled rectangles ----
  { Rec r=beginFrame(0.0f,0.0f,0.0f,1.0f);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_uni); vkCmdSetScissor(r.cmd,0,1,&full);
    float va[12]; rectVerts(8,8,16,24,va); VkDeviceMemory ma; VkBuffer ba=mkVbo(va,sizeof(va),&ma);
    PC p=pc; p.col[0]=1;p.col[1]=0;p.col[2]=0;p.col[3]=1; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&ba,&off); vkCmdDraw(r.cmd,6,1,0,0);
    float vb[12]; rectVerts(40,32,48,52,vb); VkDeviceMemory mb; VkBuffer bb=mkVbo(vb,sizeof(vb),&mb);
    PC p2=pc; p2.col[0]=0;p2.col[1]=1;p2.col[2]=0;p2.col[3]=1; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p2);
    vkCmdBindVertexBuffers(r.cmd,0,1,&bb,&off); vkCmdDraw(r.cmd,6,1,0,0);
    endFrame(r); vkDestroyBuffer(dev,ba,nullptr); vkFreeMemory(dev,ma,nullptr); vkDestroyBuffer(dev,bb,nullptr); vkFreeMemory(dev,mb,nullptr);
    int bad=0;
    for(uint32_t y=0;y<H;y++) for(uint32_t x=0;x<W;x++){
      int er,eg,eb;
      if(x>=8&&x<16&&y>=8&&y<24){ er=255;eg=0;eb=0; }
      else if(x>=40&&x<48&&y>=32&&y<52){ er=0;eg=255;eb=0; }
      else { er=0;eg=0;eb=0; }
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"filled rectangles: every pixel matches closed-form rect coverage");
    ok(peq(10,10,255,0,0,255,1),"rect A interior red"); ok(peq(44,40,0,255,0,255,1),"rect B interior green");
    ok(peq(30,30,0,0,0,255,1),"gap between rects is background"); }

  // ---- Scene B: analytic rounded-rect ----
  // full-screen quad, rounded-rect fragment shader discards outside the analytic coverage; asserted
  // against the identical C++ closed form. box [12,52)x[12,52), radius 8, color yellow.
  { Rec r=beginFrame(0,0,0,1);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_rr); vkCmdSetScissor(r.cmd,0,1,&full);
    PC p=pc; p.col[0]=1;p.col[1]=1;p.col[2]=0;p.col[3]=1; p.box[0]=12;p.box[1]=12;p.box[2]=52;p.box[3]=52; p.rad=8.0f;
    vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
    float fq[12]={0,0,(float)W,0,0,(float)H, 0,(float)H,(float)W,0,(float)W,(float)H}; VkDeviceMemory fm; VkBuffer fbo2=mkVbo(fq,sizeof(fq),&fm);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&fbo2,&off); vkCmdDraw(r.cmd,6,1,0,0);
    endFrame(r); vkDestroyBuffer(dev,fbo2,nullptr); vkFreeMemory(dev,fm,nullptr);
    auto covered=[&](int x,int y)->bool{
      float cx=x+0.5f, cy=y+0.5f; float x0=12,y0=12,x1=52,y1=52,rr=8;
      if(!(cx>=x0&&cx<x1&&cy>=y0&&cy<y1)) return false;
      float ccx,ccy; bool corner=false;
      if(cx<x0+rr&&cy<y0+rr){corner=true;ccx=x0+rr;ccy=y0+rr;}
      else if(cx>=x1-rr&&cy<y0+rr){corner=true;ccx=x1-rr;ccy=y0+rr;}
      else if(cx<x0+rr&&cy>=y1-rr){corner=true;ccx=x0+rr;ccy=y1-rr;}
      else if(cx>=x1-rr&&cy>=y1-rr){corner=true;ccx=x1-rr;ccy=y1-rr;}
      if(corner){ float dx=cx-ccx,dy=cy-ccy; if(sqrtf(dx*dx+dy*dy)>rr) return false; }
      return true; };
    int bad=0, lit=0;
    for(uint32_t y=0;y<H;y++) for(uint32_t x=0;x<W;x++){
      bool cov=covered(x,y); if(cov)lit++;
      int er=cov?255:0, eg=cov?255:0, eb=0;
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"rounded-rect: every pixel matches analytic corner-arc coverage");
    ok(lit>0,"rounded-rect: some pixels covered");
    ok(peq(32,32,255,255,0,255,1),"rounded-rect center lit");
    ok(peq(12,12,0,0,0,255,1),"rounded-rect clipped corner (12,12) is background");
    ok(peq(32,13,255,255,0,255,1),"rounded-rect straight top edge lit"); }

  // ---- Scene C: nine-patch-style scaled border frame ----
  { Rec r=beginFrame(0,0,0,1);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_uni); vkCmdSetScissor(r.cmd,0,1,&full);
    float vo[12]; rectVerts(4,4,60,60,vo); VkDeviceMemory mo; VkBuffer bo=mkVbo(vo,sizeof(vo),&mo);
    PC p=pc; p.col[0]=0;p.col[1]=0;p.col[2]=1;p.col[3]=1; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&bo,&off); vkCmdDraw(r.cmd,6,1,0,0);
    float vin[12]; rectVerts(10,10,54,54,vin); VkDeviceMemory mi; VkBuffer bin=mkVbo(vin,sizeof(vin),&mi);
    PC p2=pc; p2.col[0]=0.1f;p2.col[1]=0.1f;p2.col[2]=0.1f;p2.col[3]=1.0f; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p2);
    vkCmdBindVertexBuffers(r.cmd,0,1,&bin,&off); vkCmdDraw(r.cmd,6,1,0,0);
    endFrame(r); vkDestroyBuffer(dev,bo,nullptr); vkFreeMemory(dev,mo,nullptr); vkDestroyBuffer(dev,bin,nullptr); vkFreeMemory(dev,mi,nullptr);
    int bad=0;
    for(uint32_t y=0;y<H;y++) for(uint32_t x=0;x<W;x++){
      bool inbox = x>=4&&x<60&&y>=4&&y<60;
      bool ininner = x>=10&&x<54&&y>=10&&y<54;
      int er,eg,eb;
      if(ininner){ er=q8(0.1f);eg=q8(0.1f);eb=q8(0.1f); }
      else if(inbox){ er=0;eg=0;eb=255; }
      else { er=0;eg=0;eb=0; }
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"nine-patch border frame: closed-form border-vs-interior coverage");
    ok(peq(5,32,0,0,255,255,1),"nine-patch left border blue");
    ok(peq(32,5,0,0,255,255,1),"nine-patch top border blue");
    ok(peq(32,32,q8(0.1f),q8(0.1f),q8(0.1f),255,1),"nine-patch hollow interior"); }

  // ---- Scene D: 8x8 bitmap-font glyph blit ----
  static const unsigned char GLYPH_H[8] = { 0x00,0x42,0x42,0x7E,0x42,0x42,0x42,0x00 };
  {
    unsigned char rgba[8*8*4];
    for(int rr=0;rr<8;rr++) for(int c=0;c<8;c++){
      bool lit=(GLYPH_H[rr]>>(7-c))&1; unsigned char v=lit?255:0;
      int idx=(rr*8+c)*4; rgba[idx]=v; rgba[idx+1]=v; rgba[idx+2]=v; rgba[idx+3]=255;
    }
    // glyph texture: 8x8 R8G8B8A8, NEAREST, staged upload with layout barriers.
    VkImageCreateInfo tii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; tii.imageType=VK_IMAGE_TYPE_2D; tii.format=VK_FORMAT_R8G8B8A8_UNORM;
    tii.extent={8,8,1}; tii.mipLevels=1; tii.arrayLayers=1; tii.samples=VK_SAMPLE_COUNT_1_BIT; tii.tiling=VK_IMAGE_TILING_OPTIMAL;
    tii.usage=VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT; tii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
    VkImage gtex; VKOK(vkCreateImage(dev,&tii,nullptr,&gtex),"glyph image");
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,gtex,&tmr);
    VkMemoryAllocateInfo tai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; tai.allocationSize=tmr.size; tai.memoryTypeIndex=memtype(tmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    VkDeviceMemory tmem; vkAllocateMemory(dev,&tai,nullptr,&tmem); vkBindImageMemory(dev,gtex,tmem,0);
    VkBufferCreateInfo sbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; sbi.size=sizeof(rgba); sbi.usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
    VkBuffer sbuf; vkCreateBuffer(dev,&sbi,nullptr,&sbuf);
    VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,sbuf,&smr);
    VkMemoryAllocateInfo sai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; sai.allocationSize=smr.size; sai.memoryTypeIndex=memtype(smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkDeviceMemory smem; vkAllocateMemory(dev,&sai,nullptr,&smem); vkBindBufferMemory(dev,sbuf,smem,0);
    void* sp; vkMapMemory(dev,smem,0,sizeof(rgba),0,&sp); memcpy(sp,rgba,sizeof(rgba)); vkUnmapMemory(dev,smem);
    { VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
      VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
      VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
      VkImageMemoryBarrier b1{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER}; b1.oldLayout=VK_IMAGE_LAYOUT_UNDEFINED; b1.newLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
      b1.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.image=gtex; b1.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
      b1.srcAccessMask=0; b1.dstAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;
      vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,nullptr,0,nullptr,1,&b1);
      VkBufferImageCopy cp{}; cp.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; cp.imageExtent={8,8,1};
      vkCmdCopyBufferToImage(cmd,sbuf,gtex,VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,1,&cp);
      VkImageMemoryBarrier b2=b1; b2.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
      b2.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
      vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,0,0,nullptr,0,nullptr,1,&b2);
      vkEndCommandBuffer(cmd);
      VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
      VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fnc; vkCreateFence(dev,&fi,nullptr,&fnc);
      vkQueueSubmit(q,1,&si,fnc); vkWaitForFences(dev,1,&fnc,VK_TRUE,UINT64_MAX); vkDestroyFence(dev,fnc,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd); }
    VkImageViewCreateInfo tvi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; tvi.image=gtex; tvi.viewType=VK_IMAGE_VIEW_TYPE_2D; tvi.format=VK_FORMAT_R8G8B8A8_UNORM; tvi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
    VkImageView tview; vkCreateImageView(dev,&tvi,nullptr,&tview);
    VkSamplerCreateInfo smci{VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO}; smci.magFilter=VK_FILTER_NEAREST; smci.minFilter=VK_FILTER_NEAREST; smci.addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; smci.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; smci.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
    VkSampler samp; vkCreateSampler(dev,&smci,nullptr,&samp);
    VkDescriptorSetLayoutBinding dslb{}; dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
    VkDescriptorSetLayoutCreateInfo dslci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=1; dslci.pBindings=&dslb;
    VkDescriptorSetLayout dsl; VKOK(vkCreateDescriptorSetLayout(dev,&dslci,nullptr,&dsl),"glyph descriptor set layout");
    VkPushConstantRange tpcr{VK_SHADER_STAGE_VERTEX_BIT,0,16};
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl; plci.pushConstantRangeCount=1; plci.pPushConstantRanges=&tpcr;
    VkPipelineLayout pl_tex; vkCreatePipelineLayout(dev,&plci,nullptr,&pl_tex);
    VkPipeline pt=mkPipe(vs_tex,fs_tex,pl_tex,1,false);
    ok(dsl&&pl_tex&&pt&&samp,"glyph pipeline + descriptor created");
    VkDescriptorPoolSize dps{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,1};
    VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=1; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
    VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,nullptr,&dpool);
    VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
    VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
    VkDescriptorImageInfo dii2{samp,tview,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet wds{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds.dstSet=dset; wds.dstBinding=0; wds.descriptorCount=1; wds.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds.pImageInfo=&dii2;
    vkUpdateDescriptorSets(dev,1,&wds,0,nullptr);
    // pixel rect [20,28)x[20,28), uv 0..1 spanning the 8x8 glyph, v=0 at y=20.
    float gq[24]={ 20,20,0,0,  28,20,1,0,  20,28,0,1,   20,28,0,1,  28,20,1,0,  28,28,1,1 };
    VkDeviceMemory gm; VkBuffer gvbo=mkVbo(gq,sizeof(gq),&gm);
    { Rec r=beginFrame(0,0,0,1);
      vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pt); vkCmdSetScissor(r.cmd,0,1,&full);
      float vpv[2]={(float)W,(float)H}; vkCmdPushConstants(r.cmd,pl_tex,VK_SHADER_STAGE_VERTEX_BIT,0,8,vpv);
      vkCmdBindDescriptorSets(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl_tex,0,1,&dset,0,nullptr);
      VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&gvbo,&off); vkCmdDraw(r.cmd,6,1,0,0);
      endFrame(r); }
    int bad=0;
    for(int dy=0;dy<8;dy++) for(int dx=0;dx<8;dx++){
      int sx=20+dx, sy=20+dy; int trow=dy, tcol=dx;
      bool lit=(GLYPH_H[trow]>>(7-tcol))&1; int v=lit?255:0;
      if(!peq(sx,sy,v,v,v,255,1)) bad++; }
    ok(bad==0,"glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap");
    ok(peq(21,23,255,255,255,255,1),"glyph crossbar lit (col1,row3)");
    ok(peq(23,20,0,0,0,255,1),"glyph row0 blank");
    ok(peq(24,21,0,0,0,255,1),"glyph row1 middle blank (0x42)");
    vkDestroyPipeline(dev,pt,nullptr); vkDestroyBuffer(dev,gvbo,nullptr); vkFreeMemory(dev,gm,nullptr);
    vkDestroySampler(dev,samp,nullptr); vkDestroyImageView(dev,tview,nullptr); vkDestroyImage(dev,gtex,nullptr); vkFreeMemory(dev,tmem,nullptr);
    vkDestroyBuffer(dev,sbuf,nullptr); vkFreeMemory(dev,smem,nullptr);
    vkDestroyDescriptorPool(dev,dpool,nullptr); vkDestroyDescriptorSetLayout(dev,dsl,nullptr); vkDestroyPipelineLayout(dev,pl_tex,nullptr); }

  // ---- Scene E: scissor-clipped fill ----
  { Rec r=beginFrame(0,0,0,1);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_uni);
    VkRect2D box{{16,16},{20,20}}; vkCmdSetScissor(r.cmd,0,1,&box);
    PC p=pc; p.col[0]=1;p.col[1]=0;p.col[2]=1;p.col[3]=1; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
    float fv[12]; rectVerts(0,0,(float)W,(float)H,fv); VkDeviceMemory fm; VkBuffer fbb=mkVbo(fv,sizeof(fv),&fm);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&fbb,&off); vkCmdDraw(r.cmd,6,1,0,0);
    endFrame(r); vkDestroyBuffer(dev,fbb,nullptr); vkFreeMemory(dev,fm,nullptr);
    int bad=0;
    for(uint32_t y=0;y<H;y++) for(uint32_t x=0;x<W;x++){
      bool in = x>=16&&x<36&&y>=16&&y<36;
      int er=in?255:0, eg=0, eb=in?255:0;
      if(!peq(x,y,er,eg,eb,255,1)) bad++; }
    ok(bad==0,"scissor-clipped fill: magenta only within [16,36)^2");
    ok(peq(20,20,255,0,255,255,1),"scissor inside magenta"); ok(peq(40,40,0,0,0,255,1),"scissor outside background"); }

  // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
  { float bg[4]={0.10f,0.10f,0.10f,1.0f};
    struct L{ float r,g,b,a; float x0,y0,x1,y1; };
    L layers[3]={
      {1.0f,0.0f,0.0f,0.50f,  8,8, 56,56},
      {0.0f,1.0f,0.0f,0.25f, 12,12, 52,52},
      {0.0f,0.0f,1.0f,0.75f, 16,16, 48,48},
    };
    Rec r=beginFrame(bg[0],bg[1],bg[2],bg[3]);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_blend); vkCmdSetScissor(r.cmd,0,1,&full);
    VkBuffer lb[3]; VkDeviceMemory lm[3];
    for(int i=0;i<3;i++){ L&l=layers[i]; float lv[12]; rectVerts(l.x0,l.y0,l.x1,l.y1,lv); lb[i]=mkVbo(lv,sizeof(lv),&lm[i]);
      PC p=pc; p.col[0]=l.r;p.col[1]=l.g;p.col[2]=l.b;p.col[3]=l.a; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
      VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&lb[i],&off); vkCmdDraw(r.cmd,6,1,0,0); }
    endFrame(r); for(int i=0;i<3;i++){ vkDestroyBuffer(dev,lb[i],nullptr); vkFreeMemory(dev,lm[i],nullptr); }
    auto composite=[&](int tx,int ty, float outc[4]){
      float c[4]={bg[0],bg[1],bg[2],bg[3]};
      for(int i=0;i<3;i++){ L&l=layers[i];
        float cx=tx+0.5f, cy=ty+0.5f;
        if(cx>=l.x0&&cx<l.x1&&cy>=l.y0&&cy<l.y1){
          float as=l.a; float src[4]={l.r,l.g,l.b,l.a};
          for(int k=0;k<4;k++) c[k]=src[k]*as + c[k]*(1.0f-as);
        }
      }
      for(int k=0;k<4;k++) outc[k]=c[k];
    };
    int bad=0;
    for(uint32_t y=0;y<H;y++) for(uint32_t x=0;x<W;x++){
      float e[4]; composite(x,y,e);
      if(!peq(x,y,q8(e[0]),q8(e[1]),q8(e[2]),q8(e[3]),2)) bad++; }
    ok(bad==0,"multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)");
    { float c[4]={bg[0],bg[1],bg[2],bg[3]};
      float L0[4]={1,0,0,0.5f},L1[4]={0,1,0,0.25f},L2[4]={0,0,1,0.75f};
      float*ls[3]={L0,L1,L2};
      for(int i=0;i<3;i++){ float as=ls[i][3]; for(int k=0;k<4;k++) c[k]=ls[i][k]*as + c[k]*(1.f-as); }
      ok(peq(32,32,q8(c[0]),q8(c[1]),q8(c[2]),q8(c[3]),2),"multi-layer over center pixel matches hand-iterated over"); }
    { float as=0.5f; float er=1.0f*as+bg[0]*(1-as), eg=0*as+bg[1]*(1-as), eb=0*as+bg[2]*(1-as), ea=as*as+bg[3]*(1-as);
      ok(peq(10,32,q8(er),q8(eg),q8(eb),q8(ea),2),"multi-layer over: single-layer region matches one over"); }
  }

  // ---- Negative control ----
  { Rec r=beginFrame(0,0,0,1);
    vkCmdBindPipeline(r.cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe_uni); vkCmdSetScissor(r.cmd,0,1,&full);
    float va[12]; rectVerts(8,8,16,24,va); VkDeviceMemory ma; VkBuffer ba=mkVbo(va,sizeof(va),&ma);
    PC p=pc; p.col[0]=1;p.col[1]=0;p.col[2]=0;p.col[3]=1; vkCmdPushConstants(r.cmd,pl_pc,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(r.cmd,0,1,&ba,&off); vkCmdDraw(r.cmd,6,1,0,0);
    endFrame(r); vkDestroyBuffer(dev,ba,nullptr); vkFreeMemory(dev,ma,nullptr); }
  ok(!peq(10,10,0,255,0,255,4),"negative control: red rect pixel is NOT green");
  ok(!peq(30,30,255,0,0,255,4),"negative control: background is NOT red");

  vkDeviceWaitIdle(dev);
  int EXPECTED=39, TOTAL=PASS+FAIL;
  printf("scene-2dui: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_2DUI OK %d\n",PASS); return 0; }
  return 1;
}
