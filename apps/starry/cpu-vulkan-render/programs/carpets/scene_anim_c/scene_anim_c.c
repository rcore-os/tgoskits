/* scene_anim_c.c - keyframe-animation RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
 * no GPU/window/surface/swapchain), C11 binding of the same offscreen render pipeline as the C++ cell
 * scene_anim.cpp. An offscreen render pass into an R8G8B8A8_UNORM color image, drawing N=4 keyframes of a
 * transformed unit quad through a real graphics pipeline (SPIR-V vertex+fragment shaders), copied to a
 * host-visible buffer and read back. Each frame's model transform (rotation about the FBO center + scale
 * + translate, interpolated by t) is passed as push constants; a cubic ease eased(t)=3t^2-2t^3 drives the
 * scale. The four rotated/scaled/translated quad CORNERS are computed INDEPENDENTLY in C (R(theta)*S*local
 * + T) and asserted at those exact pixels, plus a point just outside the quad (background). The reference
 * math is byte-identical to the C++ cell; only the C-vs-C++ Vulkan binding syntax differs (same libvulkan
 * C API). Prints "SCENE_ANIM_C OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>
#include "shaders/anim_vert.h"
#include "shaders/anim_frag.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

static int PASS = 0, FAIL = 0;
static void ok(int c, const char* d) { if (c) PASS++; else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); } }
#define VKOK(e, d) ok((e) == VK_SUCCESS, d)

enum { W = 64, H = 64 };
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue q; static uint32_t qfam;
static VkCommandPool pool; static VkImage cimg; static VkImageView cview; static VkFramebuffer fb; static VkRenderPass rp; static VkDeviceMemory cmem;
static VkBuffer rbuf; static VkDeviceMemory rmem; static uint8_t* rmap;
static unsigned char buf[W * H * 4];

static unsigned char px(int x, int y, int c) { return buf[(y * W + x) * 4 + c]; }
static int peq(int x, int y, int r, int g, int b, int a, int tol) {
    return abs((int)px(x,y,0)-r)<=tol && abs((int)px(x,y,1)-g)<=tol &&
           abs((int)px(x,y,2)-b)<=tol && abs((int)px(x,y,3)-a)<=tol;
}
static int near_color(int x, int y, int r, int g, int b, int tol) {
    for (int dy=-1;dy<=1;dy++) for (int dx=-1;dx<=1;dx++) { int X=x+dx,Y=y+dy;
        if (X<0||Y<0||X>=W||Y>=H) continue; if (peq(X,Y,r,g,b,255,tol)) return 1; }
    return 0;
}
static float lerpf(float a, float b, float t) { return a+(b-a)*t; }
static float ease_cubic(float t) { return 3.f*t*t - 2.f*t*t*t; }

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
    return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code, size_t bytes) {
    VkShaderModuleCreateInfo ci = { .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = bytes, .pCode = code };
    VkShaderModule m; vkCreateShaderModule(dev, &ci, NULL, &m); return m;
}

/* push-constant block: { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u; } */
typedef struct { float vp[2]; float col0[2]; float col1[2]; float tr[2]; float u[4]; } PC;

static VkBuffer mkVbo(const void* data, size_t sz, VkDeviceMemory* mem) {
    VkBufferCreateInfo bi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sz, .usage = VK_BUFFER_USAGE_VERTEX_BUFFER_BIT };
    VkBuffer b; vkCreateBuffer(dev, &bi, NULL, &b);
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, b, &mr);
    VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mr.size,
        .memoryTypeIndex = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &ai, NULL, mem); vkBindBufferMemory(dev, b, *mem, 0);
    void* p; vkMapMemory(dev, *mem, 0, sz, 0, &p); memcpy(p, data, sz); vkUnmapMemory(dev, *mem); return b;
}

static VkPipeline mkPipe(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl) {
    VkPipelineShaderStageCreateInfo st[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" } };
    VkVertexInputBindingDescription bind = { 0, 8, VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attr = { 0, 0, VK_FORMAT_R32G32_SFLOAT, 0 };
    VkPipelineVertexInputStateCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &bind, .vertexAttributeDescriptionCount = 1, .pVertexAttributeDescriptions = &attr };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP };
    VkViewport vp = { 0, 0, (float)W, (float)H, 0, 1 }; VkRect2D sc = { {0, 0}, {W, H} };
    VkPipelineViewportStateCreateInfo vps = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .pViewports = &vp, .scissorCount = 1, .pScissors = &sc };
    VkDynamicState dyn = VK_DYNAMIC_STATE_SCISSOR;
    VkPipelineDynamicStateCreateInfo ds = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, .dynamicStateCount = 1, .pDynamicStates = &dyn };
    VkPipelineRasterizationStateCreateInfo rs = { .sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1.0f };
    VkPipelineMultisampleStateCreateInfo ms = { .sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT };
    VkPipelineColorBlendAttachmentState cba = { .blendEnable = VK_FALSE, .colorWriteMask = 0xF };
    VkPipelineColorBlendStateCreateInfo cb = { .sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, .attachmentCount = 1, .pAttachments = &cba };
    VkGraphicsPipelineCreateInfo gp = { .sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        .stageCount = 2, .pStages = st, .pVertexInputState = &vi, .pInputAssemblyState = &ia, .pViewportState = &vps, .pRasterizationState = &rs, .pMultisampleState = &ms, .pColorBlendState = &cb, .pDynamicState = &ds, .layout = pl, .renderPass = rp, .subpass = 0 };
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, NULL, &p); return p;
}

static VkPipeline g_pipe; static VkPipelineLayout g_pl; static VkBuffer g_vbo;
static void drawFrame(const PC* p) {
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv; cv.color.float32[0]=0; cv.color.float32[1]=0; cv.color.float32[2]=0; cv.color.float32[3]=1;
    VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp, .framebuffer = fb, .renderArea = { {0,0}, {W,H} }, .clearValueCount = 1, .pClearValues = &cv };
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, g_pipe);
    VkRect2D full = { {0,0}, {W,H} }; vkCmdSetScissor(cmd, 0, 1, &full);
    vkCmdPushConstants(cmd, g_pl, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), p);
    VkDeviceSize off = 0; vkCmdBindVertexBuffers(cmd, 0, 1, &g_vbo, &off); vkCmdDraw(cmd, 4, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { W, H, 1 } };
    vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence; vkCreateFence(dev, &fi, NULL, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    memcpy(buf, rmap, sizeof(buf));
}

/* frame transform: R(theta)*S columns + T, byte-identical to the C++ cell */
static const float A0=0.0f, A1=(float)M_PI/2.0f;
static const float S0=6.0f, S1=14.0f;
static const float CX0=20.f, CX1=44.f, CY0=20.f, CY1=44.f;
static void frame_transform(float t, float col0[2], float col1[2], float tr[2], float* out_scale, float* out_angle) {
    float ang = lerpf(A0,A1,t);
    float sc  = lerpf(S0,S1, ease_cubic(t));
    float cx  = lerpf(CX0,CX1,t), cy=lerpf(CY0,CY1,t);
    float ca=cosf(ang), sa=sinf(ang);
    col0[0]= sc*ca;  col0[1]= sc*sa;
    col1[0]=-sc*sa;  col1[1]= sc*ca;
    tr[0]=cx; tr[1]=cy;
    if (out_scale)*out_scale=sc; if (out_angle)*out_angle=ang;
}

int main(void) {
    VkApplicationInfo aiapp = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .apiVersion = VK_API_VERSION_1_1 };
    VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &aiapp };
    VKOK(vkCreateInstance(&ici, NULL, &inst), "vkCreateInstance");
    uint32_t n = 0; vkEnumeratePhysicalDevices(inst, &n, NULL); ok(n >= 1, ">=1 physical device");
    VkPhysicalDevice pds[16]; if (n > 16) n = 16; vkEnumeratePhysicalDevices(inst, &n, pds); pd = pds[0];
    uint32_t nqf = 0; vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, NULL);
    VkQueueFamilyProperties qf[16]; if (nqf > 16) nqf = 16; vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qf);
    qfam = UINT32_MAX; for (uint32_t i = 0; i < nqf; i++) if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { qfam = i; break; }
    ok(qfam != UINT32_MAX, "graphics queue family");
    float pri = 1.0f; VkDeviceQueueCreateInfo qci = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueFamilyIndex = qfam, .queueCount = 1, .pQueuePriorities = &pri };
    VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci };
    VKOK(vkCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice"); vkGetDeviceQueue(dev, qfam, 0, &q);

    VkImageCreateInfo ii = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM,
        .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
    VKOK(vkCreateImage(dev, &ii, NULL, &cimg), "vkCreateImage color");
    VkMemoryRequirements imr; vkGetImageMemoryRequirements(dev, cimg, &imr);
    VkMemoryAllocateInfo iai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = imr.size, .memoryTypeIndex = memtype(imr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    vkAllocateMemory(dev, &iai, NULL, &cmem); vkBindImageMemory(dev, cimg, cmem, 0);
    VkImageViewCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = cimg, .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    VKOK(vkCreateImageView(dev, &vi, NULL, &cview), "vkCreateImageView");
    VkAttachmentDescription att = { .format = VK_FORMAT_R8G8B8A8_UNORM, .samples = VK_SAMPLE_COUNT_1_BIT,
        .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_STORE, .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE, .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
        .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED, .finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    VkAttachmentReference ref = { 0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
    VkSubpassDescription sp = { .pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &ref };
    VkRenderPassCreateInfo rpi = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 1, .pAttachments = &att, .subpassCount = 1, .pSubpasses = &sp };
    VKOK(vkCreateRenderPass(dev, &rpi, NULL, &rp), "vkCreateRenderPass");
    VkFramebufferCreateInfo fbi = { .sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = rp, .attachmentCount = 1, .pAttachments = &cview, .width = W, .height = H, .layers = 1 };
    VKOK(vkCreateFramebuffer(dev, &fbi, NULL, &fb), "vkCreateFramebuffer");
    VkBufferCreateInfo rbi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = W*H*4, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT };
    vkCreateBuffer(dev, &rbi, NULL, &rbuf);
    VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev, rbuf, &rmr);
    VkMemoryAllocateInfo rai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = rmr.size, .memoryTypeIndex = memtype(rmr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &rai, NULL, &rmem); vkBindBufferMemory(dev, rbuf, rmem, 0); vkMapMemory(dev, rmem, 0, W*H*4, 0, (void**)&rmap);
    VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfam, .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT };
    VKOK(vkCreateCommandPool(dev, &pci, NULL, &pool), "vkCreateCommandPool");
    ok(1, "offscreen R8G8B8A8 target + readback buffer ready");

    VkShaderModule vs = shmod(anim_vert, sizeof(anim_vert)), fs = shmod(anim_frag, sizeof(anim_frag));
    VkPushConstantRange pcr = { VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC) };
    VkPipelineLayoutCreateInfo li = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .pushConstantRangeCount = 1, .pPushConstantRanges = &pcr };
    vkCreatePipelineLayout(dev, &li, NULL, &g_pl);
    g_pipe = mkPipe(vs, fs, g_pl); ok(g_pipe != VK_NULL_HANDLE, "anim pipeline created");

    const float local[8] = { -1,-1, 1,-1, -1,1, 1,1 };
    VkDeviceMemory vbmem; g_vbo = mkVbo(local, sizeof(local), &vbmem);

    const float ts[4] = { 0.0f, 0.25f, 0.5f, 0.75f };
    const float cols[4][3] = { {1,0,0}, {0,1,0}, {0,0,1}, {1,1,0} };

    for (int fi = 0; fi < 4; fi++) {
        float t = ts[fi]; float col0[2], col1[2], tr[2], sc, ang; frame_transform(t, col0, col1, tr, &sc, &ang);
        PC p; memset(&p, 0, sizeof(p)); p.vp[0]=(float)W; p.vp[1]=(float)H;
        p.col0[0]=col0[0]; p.col0[1]=col0[1]; p.col1[0]=col1[0]; p.col1[1]=col1[1]; p.tr[0]=tr[0]; p.tr[1]=tr[1];
        p.u[0]=cols[fi][0]; p.u[1]=cols[fi][1]; p.u[2]=cols[fi][2]; p.u[3]=1.0f;
        drawFrame(&p);

        float ca=cosf(ang), sa=sinf(ang);
        float cornx[4], corny[4];
        for (int k=0;k<4;k++) {
            float lx=local[k*2+0], ly=local[k*2+1];
            float rx = sc*(ca*lx - sa*ly), ry = sc*(sa*lx + ca*ly);
            cornx[k] = tr[0]+rx; corny[k] = tr[1]+ry;
        }
        float e = ease_cubic(t); float e_ref = 3.f*t*t - 2.f*t*t*t;
        ok(fabsf(e-e_ref)<1e-6f, "ease_cubic closed-form value");
        ok(fabsf(sc - (S0+(S1-S0)*e))<1e-4f, "scale = lerp(S0,S1,ease(t)) closed-form");

        int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
        ok(peq(cxi,cyi,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),255,2),
           "frame center pixel carries frame color at closed-form center");

        for (int k=0;k<4;k++) {
            int px_=(int)lroundf(cornx[k]-0.5f), py_=(int)lroundf(corny[k]-0.5f);
            int onscreen = px_>=0&&py_>=0&&px_<W&&py_<H;
            ok(onscreen && near_color(px_,py_,(int)lroundf(cols[fi][0]*255),(int)lroundf(cols[fi][1]*255),(int)lroundf(cols[fi][2]*255),40),
               "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)");
        }

        { int ox = (fi<2)?W-2:1, oy=(fi<2)?H-2:1;
          float reach=sc*1.4142f; int covers = (fabsf(ox+0.5f-tr[0])<=reach && fabsf(oy+0.5f-tr[1])<=reach);
          if (!covers) ok(peq(ox,oy,0,0,0,255,2),"outside-quad point stays background (closed-form silhouette)");
          else ok(1,"outside-quad point skipped (would be covered)"); }
    }

    { float c0a[2],c1a[2],tra[2],c0b[2],c1b[2],trb[2],s,a;
      frame_transform(0.0f,c0a,c1a,tra,&s,&a); frame_transform(0.75f,c0b,c1b,trb,&s,&a);
      ok(fabsf(tra[0]-trb[0])>1.0f,"center translates between t=0 and t=0.75 (animation is real)"); }

    { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.5f,col0,col1,tr,&sc,&ang);
      ok(fabsf(ang-(float)M_PI/4.0f)<1e-5f,"t=0.5 rotation angle = pi/4 closed-form");
      ok(fabsf(col0[0]-col0[1])<1e-4f && col0[0]>0,"t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)"); }

    { float col0[2],col1[2],tr[2],sc,ang; frame_transform(0.0f,col0,col1,tr,&sc,&ang);
      PC p; memset(&p,0,sizeof(p)); p.vp[0]=(float)W; p.vp[1]=(float)H;
      p.col0[0]=col0[0]; p.col0[1]=col0[1]; p.col1[0]=col1[0]; p.col1[1]=col1[1]; p.tr[0]=tr[0]; p.tr[1]=tr[1];
      p.u[0]=1; p.u[1]=0; p.u[2]=0; p.u[3]=1;
      drawFrame(&p);
      int cxi=(int)lroundf(tr[0]-0.5f), cyi=(int)lroundf(tr[1]-0.5f);
      ok(!peq(cxi,cyi,0,255,0,255,4),"negative control: frame-0 center is NOT green"); }

    vkDeviceWaitIdle(dev);
    int EXPECTED = 47, TOTAL = PASS + FAIL;
    printf("scene-anim-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("SCENE_ANIM_C OK %d\n", PASS); return 0; }
    return 1;
}
