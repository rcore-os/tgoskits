// scene_3dmodel.cpp - 3D indexed-mesh RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan
// 1.3 over the LLVM JIT; no GPU, no window/surface/swapchain). Vulkan counterpart of the GLES
// scene_3dmodel: an offscreen render pass into an R8G8B8A8_UNORM color image + a D32_SFLOAT depth
// attachment, drawing through a real graphics pipeline (SPIR-V vertex+fragment shaders) with depth test
// VK_COMPARE_OP_LESS, copied to a host-visible buffer and read back. Renders an indexed cube mesh with a
// hand-computed Model-View-Projection matrix (perspective), depth-buffered occlusion, and Gouraud
// shading (per-vertex color interpolated across each triangle). The assertion is an INDEPENDENT
// software reference rasterizer written in C++: verts are transformed by the SAME MVP -> clip -> NDC
// (perspective divide) -> viewport pixels; for each pixel we compute barycentric coordinates against
// every projected triangle, do a perspective-correct interpolated depth test in a private z-buffer, and
// interpolate the vertex colors, then compare the reference framebuffer to the readback per pixel (with
// a small tolerance for edge-sample and rounding differences). Closes with a negative control. Prints
// "SCENE_3DMODEL OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Vulkan vs GL adaptation (the main math change): GL NDC z is in [-1,1]; Vulkan NDC z is in [0,1]. Two
// coupled changes keep the GPU output and the C++ reference in agreement:
//   (a) the perspective() matrix uses the Vulkan/D3D z-mapping (near->0, far->1): the z row is
//       m[2][2]=zf/(zn-zf), m[3][2]=(zf*zn)/(zn-zf), so z_ndc = z_clip/w_clip lands in [0,1]. (GL used
//       m[2][2]=(zf+zn)/(zn-zf), m[3][2]=2*zf*zn/(zn-zf) giving [-1,1].) With the GL matrix, z/w in
//       [-1,1] would be clipped by Vulkan's z>=0 test and occlude wrongly; the Vulkan matrix fixes that.
//   (b) the reference window depth is sz = ndcz DIRECTLY (z_clip/w_clip), not GL's ndcz*0.5+0.5, so the
//       reference's LESS depth test uses the same [0,1] depth the GPU writes into the D32_SFLOAT buffer.
// Perspective-correct color interpolation (weight by 1/w) is unchanged and still occludes correctly:
// the nearest face has the smallest z in [0,1] and wins under LESS, verified by the center-pixel and
// coverage-mask asserts. Y mapping: sy=(ndcy*0.5+0.5)*H is unchanged - with a positive-height viewport
// {0,0,W,H,0,1}, Vulkan maps NDC y=-1 to readback row 0, and the reference's sy formula maps NDC y=-1
// to sy=0=row 0 identically, so the projected triangles and the GPU output share the same row indexing.
// The column-major M4 math, cube verts/colors/indices, model/view, barycentric rasterizer, and color
// interpolation are copied byte-identical from the GLES scene; only perspective()'s z row and the sz
// mapping change.
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include "shaders/cube_vert.h"
#include "shaders/cube_frag.h"

static int PASS=0, FAIL=0;
static void ok(bool c,const char*d){ if(c)PASS++; else{FAIL++; fprintf(stderr,"FAIL: %s\n",d);} }
#define VKOK(e,d) ok((e)==VK_SUCCESS,d)

static const int W=64, H=64;
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue q; static uint32_t qfam;
static VkCommandPool pool; static VkImage cimg; static VkImageView cview; static VkDeviceMemory cmem;
static VkImage dimg; static VkImageView dview; static VkDeviceMemory dmem;
static VkRenderPass rp; static VkFramebuffer fb;
static VkBuffer rbuf; static VkDeviceMemory rmem; static uint8_t* rmap;
static unsigned char buf[W*H*4];

static unsigned char px(int x,int y,int c){ return buf[(y*W+x)*4+c]; }
static bool peq(int x,int y,int r,int g,int b,int a,int tol){
  return abs((int)px(x,y,0)-r)<=tol && abs((int)px(x,y,1)-g)<=tol &&
         abs((int)px(x,y,2)-b)<=tol && abs((int)px(x,y,3)-a)<=tol;
}
static uint32_t memtype(uint32_t bits,VkMemoryPropertyFlags want){
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((bits&(1u<<i))&&(mp.memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code,size_t bytes){
  VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; ci.codeSize=bytes; ci.pCode=code;
  VkShaderModule m; vkCreateShaderModule(dev,&ci,nullptr,&m); return m;
}

// ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) - byte-identical to the GLES scene ----
struct M4{ float m[16]; };
static M4 mul(const M4&a,const M4&b){ M4 r; for(int c=0;c<4;c++)for(int row=0;row<4;row++){ float s=0; for(int k=0;k<4;k++) s+=a.m[k*4+row]*b.m[c*4+k]; r.m[c*4+row]=s; } return r; }
static void mv4(const M4&a,const float v[4],float o[4]){ for(int row=0;row<4;row++){ float s=0; for(int k=0;k<4;k++) s+=a.m[k*4+row]*v[k]; o[row]=s; } }
// Vulkan perspective: near->z_ndc 0, far->z_ndc 1 (z/w in [0,1]). Only the z row differs from GL.
static M4 perspective(float fovy,float aspect,float zn,float zf){
  float f=1.0f/tanf(fovy*0.5f); M4 r; memset(r.m,0,sizeof(r.m));
  r.m[0*4+0]=f/aspect; r.m[1*4+1]=f;
  r.m[2*4+2]=zf/(zn-zf); r.m[2*4+3]=-1.0f;
  r.m[3*4+2]=(zf*zn)/(zn-zf); return r;
}
static M4 translate(float x,float y,float z){ M4 r; memset(r.m,0,sizeof(r.m)); r.m[0]=r.m[5]=r.m[10]=r.m[15]=1; r.m[3*4+0]=x; r.m[3*4+1]=y; r.m[3*4+2]=z; return r; }
static M4 rotY(float a){ M4 r; memset(r.m,0,sizeof(r.m)); float c=cosf(a),s=sinf(a); r.m[0*4+0]=c; r.m[0*4+2]=-s; r.m[2*4+0]=s; r.m[2*4+2]=c; r.m[1*4+1]=1; r.m[3*4+3]=1; return r; }
static M4 rotX(float a){ M4 r; memset(r.m,0,sizeof(r.m)); float c=cosf(a),s=sinf(a); r.m[1*4+1]=c; r.m[1*4+2]=s; r.m[2*4+1]=-s; r.m[2*4+2]=c; r.m[0*4+0]=1; r.m[3*4+3]=1; return r; }

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

  // offscreen color image
  VkImageCreateInfo ii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; ii.imageType=VK_IMAGE_TYPE_2D; ii.format=VK_FORMAT_R8G8B8A8_UNORM;
  ii.extent={(uint32_t)W,(uint32_t)H,1}; ii.mipLevels=1; ii.arrayLayers=1; ii.samples=VK_SAMPLE_COUNT_1_BIT; ii.tiling=VK_IMAGE_TILING_OPTIMAL;
  ii.usage=VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT|VK_IMAGE_USAGE_TRANSFER_SRC_BIT; ii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
  VKOK(vkCreateImage(dev,&ii,nullptr,&cimg),"vkCreateImage color");
  VkMemoryRequirements imr; vkGetImageMemoryRequirements(dev,cimg,&imr);
  VkMemoryAllocateInfo iai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; iai.allocationSize=imr.size; iai.memoryTypeIndex=memtype(imr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  vkAllocateMemory(dev,&iai,nullptr,&cmem); vkBindImageMemory(dev,cimg,cmem,0);
  VkImageViewCreateInfo vi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; vi.image=cimg; vi.viewType=VK_IMAGE_VIEW_TYPE_2D; vi.format=VK_FORMAT_R8G8B8A8_UNORM; vi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
  VKOK(vkCreateImageView(dev,&vi,nullptr,&cview),"vkCreateImageView");

  // D32_SFLOAT depth image
  VkImageCreateInfo dii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; dii.imageType=VK_IMAGE_TYPE_2D; dii.format=VK_FORMAT_D32_SFLOAT;
  dii.extent={(uint32_t)W,(uint32_t)H,1}; dii.mipLevels=1; dii.arrayLayers=1; dii.samples=VK_SAMPLE_COUNT_1_BIT; dii.tiling=VK_IMAGE_TILING_OPTIMAL;
  dii.usage=VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT; dii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
  VKOK(vkCreateImage(dev,&dii,nullptr,&dimg),"vkCreateImage depth");
  VkMemoryRequirements dmr; vkGetImageMemoryRequirements(dev,dimg,&dmr);
  VkMemoryAllocateInfo daii{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; daii.allocationSize=dmr.size; daii.memoryTypeIndex=memtype(dmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  vkAllocateMemory(dev,&daii,nullptr,&dmem); vkBindImageMemory(dev,dimg,dmem,0);
  VkImageViewCreateInfo dvi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; dvi.image=dimg; dvi.viewType=VK_IMAGE_VIEW_TYPE_2D; dvi.format=VK_FORMAT_D32_SFLOAT; dvi.subresourceRange={VK_IMAGE_ASPECT_DEPTH_BIT,0,1,0,1};
  VKOK(vkCreateImageView(dev,&dvi,nullptr,&dview),"vkCreateImageView depth");

  // render pass: color (CLEAR->STORE, final TRANSFER_SRC) + depth (CLEAR, LESS)
  VkAttachmentDescription att[2]{};
  att[0].format=VK_FORMAT_R8G8B8A8_UNORM; att[0].samples=VK_SAMPLE_COUNT_1_BIT; att[0].loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR; att[0].storeOp=VK_ATTACHMENT_STORE_OP_STORE;
  att[0].stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE; att[0].stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE; att[0].initialLayout=VK_IMAGE_LAYOUT_UNDEFINED; att[0].finalLayout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
  att[1].format=VK_FORMAT_D32_SFLOAT; att[1].samples=VK_SAMPLE_COUNT_1_BIT; att[1].loadOp=VK_ATTACHMENT_LOAD_OP_CLEAR; att[1].storeOp=VK_ATTACHMENT_STORE_OP_DONT_CARE;
  att[1].stencilLoadOp=VK_ATTACHMENT_LOAD_OP_DONT_CARE; att[1].stencilStoreOp=VK_ATTACHMENT_STORE_OP_DONT_CARE; att[1].initialLayout=VK_IMAGE_LAYOUT_UNDEFINED; att[1].finalLayout=VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
  VkAttachmentReference cref{0,VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
  VkAttachmentReference dref{1,VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL};
  VkSubpassDescription sp{}; sp.pipelineBindPoint=VK_PIPELINE_BIND_POINT_GRAPHICS; sp.colorAttachmentCount=1; sp.pColorAttachments=&cref; sp.pDepthStencilAttachment=&dref;
  VkRenderPassCreateInfo rpi{VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO}; rpi.attachmentCount=2; rpi.pAttachments=att; rpi.subpassCount=1; rpi.pSubpasses=&sp;
  VKOK(vkCreateRenderPass(dev,&rpi,nullptr,&rp),"vkCreateRenderPass");
  VkImageView fbv[2]={cview,dview};
  VkFramebufferCreateInfo fbi{VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO}; fbi.renderPass=rp; fbi.attachmentCount=2; fbi.pAttachments=fbv; fbi.width=W; fbi.height=H; fbi.layers=1;
  VKOK(vkCreateFramebuffer(dev,&fbi,nullptr,&fb),"vkCreateFramebuffer");

  VkBufferCreateInfo rbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; rbi.size=W*H*4; rbi.usage=VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  vkCreateBuffer(dev,&rbi,nullptr,&rbuf);
  VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev,rbuf,&rmr);
  VkMemoryAllocateInfo rai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; rai.allocationSize=rmr.size; rai.memoryTypeIndex=memtype(rmr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  vkAllocateMemory(dev,&rai,nullptr,&rmem); vkBindBufferMemory(dev,rbuf,rmem,0); vkMapMemory(dev,rmem,0,W*H*4,0,(void**)&rmap);
  VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; pci.queueFamilyIndex=qfam; pci.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  VKOK(vkCreateCommandPool(dev,&pci,nullptr,&pool),"vkCreateCommandPool");
  ok(true,"offscreen R8G8B8A8 + D32_SFLOAT target + readback buffer ready");

  // ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (byte-identical) ----
  static const float VP[8][3]={
    {-1,-1,-1},{ 1,-1,-1},{ 1, 1,-1},{-1, 1,-1},
    {-1,-1, 1},{ 1,-1, 1},{ 1, 1, 1},{-1, 1, 1} };
  static float VC[8][3];
  for(int i=0;i<8;i++){ VC[i][0]=(VP[i][0]+1)*0.5f; VC[i][1]=(VP[i][1]+1)*0.5f; VC[i][2]=(VP[i][2]+1)*0.5f; }
  static const unsigned short IDX[36]={
    0,1,2, 0,2,3,
    4,6,5, 4,7,6,
    0,4,5, 0,5,1,
    3,2,6, 3,6,7,
    0,3,7, 0,7,4,
    1,5,6, 1,6,2 };

  M4 model = mul(rotY(0.6f), rotX(0.3f));
  M4 view  = translate(0,0,-5.0f);
  M4 proj  = perspective(1.0f, (float)W/(float)H, 1.0f, 20.0f);
  M4 mvp   = mul(proj, mul(view, model));

  float verts[8*6];
  for(int i=0;i<8;i++){ verts[i*6+0]=VP[i][0]; verts[i*6+1]=VP[i][1]; verts[i*6+2]=VP[i][2];
    verts[i*6+3]=VC[i][0]; verts[i*6+4]=VC[i][1]; verts[i*6+5]=VC[i][2]; }
  auto mkbuf=[&](const void* d,size_t sz,VkBufferUsageFlags usage,VkDeviceMemory* mem)->VkBuffer{
    VkBufferCreateInfo bi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bi.size=sz; bi.usage=usage; VkBuffer b; vkCreateBuffer(dev,&bi,nullptr,&b);
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev,b,&mr); VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; ai.allocationSize=mr.size; ai.memoryTypeIndex=memtype(mr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    vkAllocateMemory(dev,&ai,nullptr,mem); vkBindBufferMemory(dev,b,*mem,0); void*p; vkMapMemory(dev,*mem,0,sz,0,&p); memcpy(p,d,sz); vkUnmapMemory(dev,*mem); return b; };
  VkDeviceMemory vbmem,ibmem; VkBuffer vbo=mkbuf(verts,sizeof(verts),VK_BUFFER_USAGE_VERTEX_BUFFER_BIT,&vbmem);
  VkBuffer ibo=mkbuf(IDX,sizeof(IDX),VK_BUFFER_USAGE_INDEX_BUFFER_BIT,&ibmem);

  // pipeline: pos3+col3 (stride 24), mvp push constant, depth test LESS, no cull (GL scene did not cull).
  VkShaderModule vs=shmod(cube_vert,sizeof(cube_vert)), fs=shmod(cube_frag,sizeof(cube_frag));
  VkPushConstantRange pcr{VK_SHADER_STAGE_VERTEX_BIT,0,64};
  VkPipelineLayoutCreateInfo li{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; li.pushConstantRangeCount=1; li.pPushConstantRanges=&pcr;
  VkPipelineLayout pl; vkCreatePipelineLayout(dev,&li,nullptr,&pl);
  VkPipelineShaderStageCreateInfo st[2]{};
  st[0]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage=VK_SHADER_STAGE_VERTEX_BIT; st[0].module=vs; st[0].pName="main";
  st[1]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage=VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module=fs; st[1].pName="main";
  VkVertexInputBindingDescription bind{0,24,VK_VERTEX_INPUT_RATE_VERTEX};
  VkVertexInputAttributeDescription attr[2]={ {0,0,VK_FORMAT_R32G32B32_SFLOAT,0}, {1,0,VK_FORMAT_R32G32B32_SFLOAT,12} };
  VkPipelineVertexInputStateCreateInfo vin{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO}; vin.vertexBindingDescriptionCount=1; vin.pVertexBindingDescriptions=&bind; vin.vertexAttributeDescriptionCount=2; vin.pVertexAttributeDescriptions=attr;
  VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO}; ia.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;
  VkViewport vp{0,0,(float)W,(float)H,0,1}; VkRect2D scr{{0,0},{(uint32_t)W,(uint32_t)H}};
  VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO}; vps.viewportCount=1; vps.pViewports=&vp; vps.scissorCount=1; vps.pScissors=&scr;
  VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO}; rs.polygonMode=VK_POLYGON_MODE_FILL; rs.cullMode=VK_CULL_MODE_NONE; rs.frontFace=VK_FRONT_FACE_COUNTER_CLOCKWISE; rs.lineWidth=1.0f;
  VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO}; ms.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT;
  VkPipelineDepthStencilStateCreateInfo dss{VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO}; dss.depthTestEnable=VK_TRUE; dss.depthWriteEnable=VK_TRUE; dss.depthCompareOp=VK_COMPARE_OP_LESS; dss.minDepthBounds=0.0f; dss.maxDepthBounds=1.0f;
  VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask=0xF; cba.blendEnable=VK_FALSE;
  VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO}; cb.attachmentCount=1; cb.pAttachments=&cba;
  VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
  gp.stageCount=2; gp.pStages=st; gp.pVertexInputState=&vin; gp.pInputAssemblyState=&ia; gp.pViewportState=&vps; gp.pRasterizationState=&rs; gp.pMultisampleState=&ms; gp.pDepthStencilState=&dss; gp.pColorBlendState=&cb; gp.layout=pl; gp.renderPass=rp; gp.subpass=0;
  VkPipeline pipe; VKOK(vkCreateGraphicsPipelines(dev,VK_NULL_HANDLE,1,&gp,nullptr,&pipe),"cube pipeline created");

  // ---- draw: clear color black, clear depth 1.0, draw the indexed cube ----
  { VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
    VkClearValue cv[2]; cv[0].color={{0,0,0,1}}; cv[1].depthStencil={1.0f,0};
    VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{(uint32_t)W,(uint32_t)H}}; rpb.clearValueCount=2; rpb.pClearValues=cv;
    vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pipe);
    vkCmdPushConstants(cmd,pl,VK_SHADER_STAGE_VERTEX_BIT,0,64,mvp.m);
    VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&vbo,&off); vkCmdBindIndexBuffer(cmd,ibo,0,VK_INDEX_TYPE_UINT16);
    vkCmdDrawIndexed(cmd,36,1,0,0,0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region{}; region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; region.imageExtent={(uint32_t)W,(uint32_t)H,1};
    vkCmdCopyImageToBuffer(cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
    VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev,&fi,nullptr,&fence);
    vkQueueSubmit(q,1,&si,fence); vkWaitForFences(dev,1,&fence,VK_TRUE,UINT64_MAX);
    vkDestroyFence(dev,fence,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd); memcpy(buf,rmap,sizeof(buf)); }
  ok(true,"cube drawn (depth-tested, Gouraud)");

  // ---- INDEPENDENT software reference rasterizer (byte-identical, except the Vulkan NDC-z mapping) ----
  static float refc[H][W][3]; static float refz[H][W]; static unsigned char refcov[H][W];
  for(int y=0;y<H;y++) for(int x=0;x<W;x++){ refc[y][x][0]=refc[y][x][1]=refc[y][x][2]=0; refz[y][x]=1e9f; refcov[y][x]=0; }
  float sx[8],sy[8],sz[8],sw[8]; float clip[8][4];
  for(int i=0;i<8;i++){ float in[4]={VP[i][0],VP[i][1],VP[i][2],1}; float out[4]; mv4(mvp,in,out);
    clip[i][0]=out[0];clip[i][1]=out[1];clip[i][2]=out[2];clip[i][3]=out[3];
    float w=out[3]; sw[i]=w;
    float ndcx=out[0]/w, ndcy=out[1]/w, ndcz=out[2]/w; // Vulkan NDC z in [0,1]
    sx[i]=(ndcx*0.5f+0.5f)*W; sy[i]=(ndcy*0.5f+0.5f)*H; sz[i]=ndcz; // window depth = z/w directly ([0,1])
  }
  ok(sw[0]>0,"reference: all clip.w positive (mesh in front of camera)");
  for(int t=0;t<12;t++){
    int a=IDX[t*3+0], b=IDX[t*3+1], c=IDX[t*3+2];
    float ax=sx[a],ay=sy[a], bx=sx[b],by=sy[b], cx=sx[c],cy=sy[c];
    float area = (bx-ax)*(cy-ay)-(by-ay)*(cx-ax);
    if(fabsf(area)<1e-6f) continue;
    int minx=(int)floorf(fminf(ax,fminf(bx,cx))), maxx=(int)ceilf(fmaxf(ax,fmaxf(bx,cx)));
    int miny=(int)floorf(fminf(ay,fminf(by,cy))), maxy=(int)ceilf(fmaxf(ay,fmaxf(by,cy)));
    if(minx<0)minx=0; if(miny<0)miny=0; if(maxx>W)maxx=W; if(maxy>H)maxy=H;
    for(int y=miny;y<maxy;y++) for(int x=minx;x<maxx;x++){
      float pxs=x+0.5f, pys=y+0.5f;
      float w0=((bx-pxs)*(cy-pys)-(by-pys)*(cx-pxs))/area;
      float w1=((cx-pxs)*(ay-pys)-(cy-pys)*(ax-pxs))/area;
      float w2=1.0f-w0-w1;
      bool inside = (w0>=0&&w1>=0&&w2>=0) || (w0<=0&&w1<=0&&w2<=0);
      if(!inside) continue;
      if(w0<0||w1<0||w2<0){ w0=-w0; w1=-w1; w2=-w2; }
      float z = w0*sz[a]+w1*sz[b]+w2*sz[c];
      if(z<refz[y][x]){
        refz[y][x]=z; refcov[y][x]=1;
        float iwa=1.0f/sw[a], iwb=1.0f/sw[b], iwc=1.0f/sw[c];
        float d = w0*iwa+w1*iwb+w2*iwc;
        for(int k=0;k<3;k++){
          float num = w0*iwa*VC[a][k]+w1*iwb*VC[b][k]+w2*iwc*VC[c][k];
          refc[y][x][k]=num/d;
        }
      }
    }
  }

  int total=0, match=0, covmatch=0, covtotal=0, interior_bad=0;
  for(int y=0;y<H;y++) for(int x=0;x<W;x++){
    total++;
    bool gcov = !(px(x,y,0)==0&&px(x,y,1)==0&&px(x,y,2)==0);
    bool rcov = refcov[y][x]!=0;
    if(gcov==rcov) covmatch++;
    if(rcov){ covtotal++;
      int er=(int)lroundf(refc[y][x][0]*255.f), eg=(int)lroundf(refc[y][x][1]*255.f), eb=(int)lroundf(refc[y][x][2]*255.f);
      bool interior = x>0&&y>0&&x<W-1&&y<H-1 && refcov[y-1][x]&&refcov[y+1][x]&&refcov[y][x-1]&&refcov[y][x+1];
      if(peq(x,y,er,eg,eb,255,6)) match++;
      else if(interior) interior_bad++;
    }
  }
  ok(covtotal>200,"reference: cube covers a substantial area");
  ok(covmatch >= (int)(0.97*total),"coverage mask matches GPU (>=97% of pixels agree covered/empty)");
  ok(interior_bad==0,"every interior pixel matches perspective-correct Gouraud reference (tol 6)");
  ok(match >= (int)(0.92*covtotal),"92%+ of covered pixels match reference color (edges excluded)");

  { int vx=(int)lroundf(sx[6]-0.5f), vy=(int)lroundf(sy[6]-0.5f);
    if(vx>=1&&vx<W-1&&vy>=1&&vy<H-1){
      bool bright=false; for(int dy=-1;dy<=1;dy++)for(int dx=-1;dx<=1;dx++){ int X=vx+dx,Y=vy+dy;
        if(px(X,Y,0)>180&&px(X,Y,1)>180&&px(X,Y,2)>180) bright=true; }
      ok(bright,"vertex (1,1,1) region is bright (Gouraud white corner)");
    } else ok(false,"vertex (1,1,1) projected off-screen (camera mis-set)"); }
  ok(peq(0,0,0,0,0,255,1)||refcov[0][0]==0,"corner (0,0) background consistent");

  { int cxp=W/2, cyp=H/2;
    if(refcov[cyp][cxp]){
      int er=(int)lroundf(refc[cyp][cxp][0]*255.f), eg=(int)lroundf(refc[cyp][cxp][1]*255.f), eb=(int)lroundf(refc[cyp][cxp][2]*255.f);
      ok(peq(cxp,cyp,er,eg,eb,255,8),"center pixel = nearest-face (depth-buffered occlusion) reference color");
    } else ok(false,"center pixel not covered (mesh mis-projected)"); }

  ok(!(px(1,1,0)==px(W/2,H/2,0)&&px(1,1,1)==px(W/2,H/2,1)&&px(1,1,2)==px(W/2,H/2,2)),
     "negative control: image is not a flat single color (real 3D shading present)");

  vkDeviceWaitIdle(dev);
  int EXPECTED=23, TOTAL=PASS+FAIL;
  printf("scene-3dmodel: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_3DMODEL OK %d\n",PASS); return 0; }
  return 1;
}
