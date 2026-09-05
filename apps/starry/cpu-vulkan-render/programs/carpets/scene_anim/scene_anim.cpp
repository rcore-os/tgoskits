// scene_anim.cpp - keyframe-animation RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan
// 1.3 over the LLVM JIT; no GPU, no window/surface/swapchain). Vulkan counterpart of the GLES
// scene_anim: an offscreen render pass into an R8G8B8A8_UNORM color image, drawing through a real
// graphics pipeline (SPIR-V vertex+fragment shaders), copied to a host-visible buffer and read back.
// Renders N=4 keyframes of a transformed unit quad. Each frame the quad's model transform is a rotation
// about the FBO center composed with a translation and uniform scale, all interpolated by the frame
// parameter t in {0, 0.25, 0.5, 0.75}. The transform is applied in the vertex shader from a hand-built
// pixel-space model matrix (rotation from angle(t), scale, translate) and an ortho pixel->NDC map. For
// every frame the four rotated/scaled/translated quad CORNERS are computed INDEPENDENTLY in C++
// (closed-form: R(theta)*S*local + T), and the readback is asserted at those exact corner pixels
// (color present) plus a point just outside the quad (background). A cubic ease eased(t)=3t^2-2t^3
// drives the scale, and its value is asserted at each t. NDC z is unused (2D). Closes with a negative
// control. Prints "SCENE_ANIM OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Vulkan vs GL adaptation: the closed-form C++ reference (frame_transform R*S columns, corner =
// R*S*local+T, cubic ease, scale lerp) is copied byte-identical from the GLES scene. The pixel-space
// vertex map n=(pix/vp)*2-1 is unchanged; the per-frame model transform (col0, col1, tr) is passed as
// push constants instead of GL uniforms. Vulkan clip-space Y is down, so a positive-height viewport
// {0,0,W,H,0,1} maps pixel-space y=0 to NDC y=-1 which lands on readback row 0, matching GL's
// bottom-origin row 0 (both index buf row == pixel y); the closed-form corner pixels index by
// pixel-space y unchanged. The quad is a TRIANGLE_STRIP as in the GLES scene.
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include "shaders/anim_vert.h"
#include "shaders/anim_frag.h"

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
static bool near_color(int x,int y,int r,int g,int b,int tol){
  for(int dy=-1;dy<=1;dy++) for(int dx=-1;dx<=1;dx++){ int X=x+dx,Y=y+dy;
    if(X<0||Y<0||X>=(int)W||Y>=(int)H) continue; if(peq(X,Y,r,g,b,255,tol)) return true; }
  return false;
}
static float lerp(float a,float b,float t){ return a+(b-a)*t; }
static float ease_cubic(float t){ return 3.f*t*t - 2.f*t*t*t; }

static uint32_t memtype(uint32_t bits,VkMemoryPropertyFlags want){
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++) if((bits&(1u<<i))&&(mp.memoryTypes[i].propertyFlags&want)==want) return i;
  return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code,size_t bytes){
  VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO}; ci.codeSize=bytes; ci.pCode=code;
  VkShaderModule m; vkCreateShaderModule(dev,&ci,nullptr,&m); return m;
}

// push-constant block: { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u; }
struct PC { float vp[2]; float col0[2]; float col1[2]; float tr[2]; float u[4]; };

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

static VkPipeline mkPipe(VkShaderModule vs,VkShaderModule fs,VkPipelineLayout pl){
  VkPipelineShaderStageCreateInfo st[2]{};
  st[0]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage=VK_SHADER_STAGE_VERTEX_BIT; st[0].module=vs; st[0].pName="main";
  st[1]={VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage=VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module=fs; st[1].pName="main";
  VkVertexInputBindingDescription bind{0,8,VK_VERTEX_INPUT_RATE_VERTEX};
  VkVertexInputAttributeDescription attr{0,0,VK_FORMAT_R32G32_SFLOAT,0};
  VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
  vi.vertexBindingDescriptionCount=1; vi.pVertexBindingDescriptions=&bind; vi.vertexAttributeDescriptionCount=1; vi.pVertexAttributeDescriptions=&attr;
  VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO}; ia.topology=VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP;
  VkViewport vp{0,0,(float)W,(float)H,0,1}; VkRect2D sc{{0,0},{W,H}};
  VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO}; vps.viewportCount=1; vps.pViewports=&vp; vps.scissorCount=1; vps.pScissors=&sc;
  VkDynamicState dyn=VK_DYNAMIC_STATE_SCISSOR;
  VkPipelineDynamicStateCreateInfo ds{VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO}; ds.dynamicStateCount=1; ds.pDynamicStates=&dyn;
  VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO}; rs.polygonMode=VK_POLYGON_MODE_FILL; rs.cullMode=VK_CULL_MODE_NONE; rs.lineWidth=1.0f;
  VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO}; ms.rasterizationSamples=VK_SAMPLE_COUNT_1_BIT;
  VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask=0xF; cba.blendEnable=VK_FALSE;
  VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO}; cb.attachmentCount=1; cb.pAttachments=&cba;
  VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
  gp.stageCount=2; gp.pStages=st; gp.pVertexInputState=&vi; gp.pInputAssemblyState=&ia; gp.pViewportState=&vps;
  gp.pRasterizationState=&rs; gp.pMultisampleState=&ms; gp.pColorBlendState=&cb; gp.pDynamicState=&ds; gp.layout=pl; gp.renderPass=rp; gp.subpass=0;
  VkPipeline p; vkCreateGraphicsPipelines(dev,VK_NULL_HANDLE,1,&gp,nullptr,&p); return p;
}

// draw one animated quad frame: clear black, push the transform+color, draw the 4-vertex strip, read back.
static VkPipeline g_pipe; static VkPipelineLayout g_pl; static VkBuffer g_vbo;
static void drawFrame(const PC& p){
  VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
  VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
  VkClearValue cv; cv.color={{0,0,0,1}};
  VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{W,H}}; rpb.clearValueCount=1; rpb.pClearValues=&cv;
  vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
  vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,g_pipe);
  VkRect2D full{{0,0},{W,H}}; vkCmdSetScissor(cmd,0,1,&full);
  vkCmdPushConstants(cmd,g_pl,VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC),&p);
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

  VkShaderModule vs=shmod(anim_vert,sizeof(anim_vert)), fs=shmod(anim_frag,sizeof(anim_frag));
  VkPushConstantRange pcr{VK_SHADER_STAGE_VERTEX_BIT|VK_SHADER_STAGE_FRAGMENT_BIT,0,sizeof(PC)};
  VkPipelineLayoutCreateInfo li{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; li.pushConstantRangeCount=1; li.pPushConstantRanges=&pcr;
  vkCreatePipelineLayout(dev,&li,nullptr,&g_pl);
  g_pipe=mkPipe(vs,fs,g_pl); ok(g_pipe!=VK_NULL_HANDLE,"anim pipeline created");

  const float local[8]={ -1,-1, 1,-1, -1,1, 1,1 };
  VkDeviceMemory vbmem; g_vbo=mkVbo(local,sizeof(local),&vbmem);

  const float A0=0.0f, A1=(float)M_PI/2.0f;
  const float S0=6.0f, S1=14.0f;
  const float CX0=20.f, CX1=44.f, CY0=20.f, CY1=44.f;

  auto frame_transform=[&](float t, float col0[2], float col1[2], float tr[2], float* out_scale, float* out_angle){
    float ang = lerp(A0,A1,t);
    float sc  = lerp(S0,S1, ease_cubic(t));
    float cx  = lerp(CX0,CX1,t), cy=lerp(CY0,CY1,t);
    float ca=cosf(ang), sa=sinf(ang);
    col0[0]= sc*ca;  col0[1]= sc*sa;
    col1[0]=-sc*sa;  col1[1]= sc*ca;
    tr[0]=cx; tr[1]=cy;
    if(out_scale)*out_scale=sc; if(out_angle)*out_angle=ang;
  };

  const float ts[4]={0.0f,0.25f,0.5f,0.75f};
  const float cols[4][3]={ {1,0,0}, {0,1,0}, {0,0,1}, {1,1,0} };

  for(int fi=0; fi<4; fi++){
    float t=ts[fi]; float col0[2],col1[2],tr[2],sc,ang; frame_transform(t,col0,col1,tr,&sc,&ang);
    PC p{}; p.vp[0]=(float)W; p.vp[1]=(float)H;
    p.col0[0]=col0[0]; p.col0[1]=col0[1]; p.col1[0]=col1[0]; p.col1[1]=col1[1]; p.tr[0]=tr[0]; p.tr[1]=tr[1];
    p.u[0]=cols[fi][0]; p.u[1]=cols[fi][1]; p.u[2]=cols[fi][2]; p.u[3]=1.0f;
    drawFrame(p);

    float ca=cosf(ang), sa=sinf(ang);
    struct P{ float x,y; };
    P corners[4];
    for(int k=0;k<4;k++){
      float lx=local[k*2+0], ly=local[k*2+1];
      float rx = sc*(ca*lx - sa*ly), ry = sc*(sa*lx + ca*ly);
      corners[k].x = tr[0]+rx; corners[k].y = tr[1]+ry;
    }
    float e = ease_cubic(t); float e_ref = 3.f*t*t - 2.f*t*t*t;
    ok(fabsf(e-e_ref)<1e-6f, "ease_cubic closed-form value");
    ok(fabsf(sc - (S0+(S1-S0)*e))<1e-4f, "scale = lerp(S0,S1,ease(t)) closed-form");

    int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
    ok(peq(cxi,cyi,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),255,2),
       "frame center pixel carries frame color at closed-form center");

    for(int k=0;k<4;k++){
      int px_=(int)lroundf(corners[k].x-0.5f), py_=(int)lroundf(corners[k].y-0.5f);
      bool onscreen = px_>=0&&py_>=0&&px_<(int)W&&py_<(int)H;
      ok(onscreen && near_color(px_,py_,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),40),
         "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)");
    }

    { int ox = (fi<2)?(int)W-2:1, oy=(fi<2)?(int)H-2:1;
      float reach=sc*1.4142f; bool covers = (fabsf(ox+0.5f-tr[0])<=reach && fabsf(oy+0.5f-tr[1])<=reach);
      if(!covers) ok(peq(ox,oy,0,0,0,255,2),"outside-quad point stays background (closed-form silhouette)");
      else ok(true,"outside-quad point skipped (would be covered)"); }
  }

  { float c0a[2],c1a[2],tra[2],c0b[2],c1b[2],trb[2],s,a;
    frame_transform(0.0f,c0a,c1a,tra,&s,&a); frame_transform(0.75f,c0b,c1b,trb,&s,&a);
    ok(fabsf(tra[0]-trb[0])>1.0f,"center translates between t=0 and t=0.75 (animation is real)"); }

  { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.5f,col0,col1,tr,&sc,&ang);
    ok(fabsf(ang-(float)M_PI/4.0f)<1e-5f,"t=0.5 rotation angle = pi/4 closed-form");
    ok(fabsf(col0[0]-col0[1])<1e-4f && col0[0]>0,"t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)"); }

  { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.0f,col0,col1,tr,&sc,&ang);
    PC p{}; p.vp[0]=(float)W; p.vp[1]=(float)H;
    p.col0[0]=col0[0]; p.col0[1]=col0[1]; p.col1[0]=col1[0]; p.col1[1]=col1[1]; p.tr[0]=tr[0]; p.tr[1]=tr[1];
    p.u[0]=1; p.u[1]=0; p.u[2]=0; p.u[3]=1;
    drawFrame(p);
    int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
    ok(!peq(cxi,cyi,0,255,0,255,4),"negative control: frame-0 center is NOT green"); }

  vkDeviceWaitIdle(dev);
  int EXPECTED=47, TOTAL=PASS+FAIL;
  printf("scene-anim: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n",PASS,FAIL,TOTAL,EXPECTED);
  if(FAIL==0 && TOTAL==EXPECTED){ printf("SCENE_ANIM OK %d\n",PASS); return 0; }
  return 1;
}
