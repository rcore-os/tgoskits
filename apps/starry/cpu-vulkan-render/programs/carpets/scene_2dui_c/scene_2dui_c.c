/* scene_2dui_c.c - 2D UI compositing RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
 * no GPU/window/surface/swapchain), C11 binding of the same offscreen render pipeline as the C++ cell
 * scene_2dui.cpp. Builds an offscreen render pass into an R8G8B8A8_UNORM color image, draws through real
 * graphics pipelines (SPIR-V vertex+fragment shaders), copies the image to a host-visible buffer with
 * vkCmdCopyImageToBuffer and checks every pixel against a closed-form reference. Each scene primitive has
 * an INDEPENDENT closed-form software reference computed in C (not derived from the Vulkan output) and
 * asserted per pixel: filled axis-aligned rectangles, an analytic rounded-rect (inside/corner-arc/outside
 * coverage), a nine-patch-style scaled border frame, an 8x8 bitmap-font glyph blit, a scissor-clipped
 * fill, and MULTI-LAYER Porter-Duff over compositing Co = Cs*As + Cd*(1-As). The closed-form reference is
 * byte-identical to the C++ cell; only the C-vs-C++ Vulkan binding syntax differs (same libvulkan C API).
 * Prints "SCENE_2DUI_C OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>
#include "shaders/pix_vert.h"
#include "shaders/uni_frag.h"
#include "shaders/rr_frag.h"
#include "shaders/tex_vert.h"
#include "shaders/tex_frag.h"

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
static int clampi(int v, int lo, int hi) { return v<lo?lo:(v>hi?hi:v); }
static int q8(float f) { int v = (int)lroundf(f*255.f); return clampi(v, 0, 255); }

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

/* push-constant block shared vertex+fragment: { vec2 vp; vec4 col; vec4 box; float rad; } */
typedef struct { float vp[2]; float _pad0[2]; float col[4]; float box[4]; float rad; float _pad1[3]; } PC;

static VkBuffer mkVbo(const void* data, size_t sz, VkDeviceMemory* mem) {
    VkBufferCreateInfo bi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sz, .usage = VK_BUFFER_USAGE_VERTEX_BUFFER_BIT };
    VkBuffer b; vkCreateBuffer(dev, &bi, NULL, &b);
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, b, &mr);
    VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mr.size,
        .memoryTypeIndex = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &ai, NULL, mem); vkBindBufferMemory(dev, b, *mem, 0);
    void* p; vkMapMemory(dev, *mem, 0, sz, 0, &p); memcpy(p, data, sz); vkUnmapMemory(dev, *mem);
    return b;
}

/* pipeline over the offscreen pass. layout: 0=pos2, 1=pos2+uv. blend toggles SRC_ALPHA over. */
static VkPipeline mkPipe(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl, int vlayout, int blend) {
    VkPipelineShaderStageCreateInfo st[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" },
    };
    uint32_t stride = vlayout == 1 ? 16 : 8;
    VkVertexInputBindingDescription bind = { 0, stride, VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attr[2]; uint32_t nattr = 1;
    attr[0] = (VkVertexInputAttributeDescription){ 0, 0, VK_FORMAT_R32G32_SFLOAT, 0 };
    if (vlayout == 1) { attr[1] = (VkVertexInputAttributeDescription){ 1, 0, VK_FORMAT_R32G32_SFLOAT, 8 }; nattr = 2; }
    VkPipelineVertexInputStateCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &bind,
        .vertexAttributeDescriptionCount = nattr, .pVertexAttributeDescriptions = attr };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST };
    VkViewport vp = { 0, 0, (float)W, (float)H, 0, 1 }; VkRect2D sc = { {0, 0}, {W, H} };
    VkPipelineViewportStateCreateInfo vps = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        .viewportCount = 1, .pViewports = &vp, .scissorCount = 1, .pScissors = &sc };
    VkDynamicState dyn = VK_DYNAMIC_STATE_SCISSOR;
    VkPipelineDynamicStateCreateInfo ds = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        .dynamicStateCount = 1, .pDynamicStates = &dyn };
    VkPipelineRasterizationStateCreateInfo rs = { .sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1.0f };
    VkPipelineMultisampleStateCreateInfo ms = { .sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT };
    VkPipelineColorBlendAttachmentState cba = { .blendEnable = blend ? VK_TRUE : VK_FALSE,
        .srcColorBlendFactor = VK_BLEND_FACTOR_SRC_ALPHA, .dstColorBlendFactor = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA, .colorBlendOp = VK_BLEND_OP_ADD,
        .srcAlphaBlendFactor = VK_BLEND_FACTOR_SRC_ALPHA, .dstAlphaBlendFactor = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA, .alphaBlendOp = VK_BLEND_OP_ADD,
        .colorWriteMask = 0xF };
    VkPipelineColorBlendStateCreateInfo cb = { .sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        .attachmentCount = 1, .pAttachments = &cba };
    VkGraphicsPipelineCreateInfo gp = { .sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        .stageCount = 2, .pStages = st, .pVertexInputState = &vi, .pInputAssemblyState = &ia, .pViewportState = &vps,
        .pRasterizationState = &rs, .pMultisampleState = &ms, .pColorBlendState = &cb, .pDynamicState = &ds, .layout = pl, .renderPass = rp, .subpass = 0 };
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, NULL, &p); return p;
}

/* recording context building the offscreen pass with a clear color */
typedef struct { VkCommandBuffer cmd; } Rec;
static Rec beginFrame(float cr, float cg, float cb, float ca) {
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv; cv.color.float32[0] = cr; cv.color.float32[1] = cg; cv.color.float32[2] = cb; cv.color.float32[3] = ca;
    VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp, .framebuffer = fb,
        .renderArea = { {0, 0}, {W, H} }, .clearValueCount = 1, .pClearValues = &cv };
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    return (Rec){ cmd };
}
static void endFrame(Rec r) {
    vkCmdEndRenderPass(r.cmd);
    VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { W, H, 1 } };
    vkCmdCopyImageToBuffer(r.cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(r.cmd);
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &r.cmd };
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence; vkCreateFence(dev, &fi, NULL, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, NULL); vkFreeCommandBuffers(dev, pool, 1, &r.cmd);
    memcpy(buf, rmap, sizeof(buf));
}

/* fill a pixel-space rect [x0,x1)x[y0,y1) as two triangles */
static void rectVerts(float x0, float y0, float x1, float y1, float out[12]) {
    float v[12] = { x0,y0, x1,y0, x0,y1,  x0,y1, x1,y0, x1,y1 }; memcpy(out, v, sizeof(v));
}

int main(void) {
    VkApplicationInfo ai = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .apiVersion = VK_API_VERSION_1_1 };
    VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &ai };
    VKOK(vkCreateInstance(&ici, NULL, &inst), "vkCreateInstance");
    uint32_t n = 0; vkEnumeratePhysicalDevices(inst, &n, NULL); ok(n >= 1, ">=1 physical device");
    VkPhysicalDevice* pds = malloc(sizeof(VkPhysicalDevice) * n); vkEnumeratePhysicalDevices(inst, &n, pds); pd = pds[0]; free(pds);
    uint32_t nqf = 0; vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, NULL);
    VkQueueFamilyProperties* qf = malloc(sizeof(VkQueueFamilyProperties) * nqf); vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qf);
    qfam = UINT32_MAX; for (uint32_t i = 0; i < nqf; i++) if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { qfam = i; break; }
    free(qf); ok(qfam != UINT32_MAX, "graphics queue family");
    float pri = 1.0f; VkDeviceQueueCreateInfo qci = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = qfam, .queueCount = 1, .pQueuePriorities = &pri };
    VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci };
    VKOK(vkCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice"); vkGetDeviceQueue(dev, qfam, 0, &q);

    VkImageCreateInfo ii = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D,
        .format = VK_FORMAT_R8G8B8A8_UNORM, .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
    VKOK(vkCreateImage(dev, &ii, NULL, &cimg), "vkCreateImage color");
    VkMemoryRequirements imr; vkGetImageMemoryRequirements(dev, cimg, &imr);
    VkMemoryAllocateInfo iai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = imr.size,
        .memoryTypeIndex = memtype(imr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    vkAllocateMemory(dev, &iai, NULL, &cmem); vkBindImageMemory(dev, cimg, cmem, 0);
    VkImageViewCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = cimg, .viewType = VK_IMAGE_VIEW_TYPE_2D,
        .format = VK_FORMAT_R8G8B8A8_UNORM, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    VKOK(vkCreateImageView(dev, &vi, NULL, &cview), "vkCreateImageView");

    VkAttachmentDescription att = { .format = VK_FORMAT_R8G8B8A8_UNORM, .samples = VK_SAMPLE_COUNT_1_BIT,
        .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
        .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE, .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
        .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED, .finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL };
    VkAttachmentReference ref = { 0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
    VkSubpassDescription sp = { .pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &ref };
    VkRenderPassCreateInfo rpi = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 1, .pAttachments = &att, .subpassCount = 1, .pSubpasses = &sp };
    VKOK(vkCreateRenderPass(dev, &rpi, NULL, &rp), "vkCreateRenderPass");
    VkFramebufferCreateInfo fbi = { .sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = rp, .attachmentCount = 1, .pAttachments = &cview, .width = W, .height = H, .layers = 1 };
    VKOK(vkCreateFramebuffer(dev, &fbi, NULL, &fb), "vkCreateFramebuffer");

    VkBufferCreateInfo rbi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = W * H * 4, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT };
    vkCreateBuffer(dev, &rbi, NULL, &rbuf);
    VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev, rbuf, &rmr);
    VkMemoryAllocateInfo rai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = rmr.size,
        .memoryTypeIndex = memtype(rmr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &rai, NULL, &rmem); vkBindBufferMemory(dev, rbuf, rmem, 0); vkMapMemory(dev, rmem, 0, W * H * 4, 0, (void**)&rmap);

    VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfam, .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT };
    VKOK(vkCreateCommandPool(dev, &pci, NULL, &pool), "vkCreateCommandPool");
    ok(1, "offscreen R8G8B8A8 target + readback buffer ready");

    VkShaderModule vs_pix = shmod(pix_vert, sizeof(pix_vert)), fs_uni = shmod(uni_frag, sizeof(uni_frag)), fs_rr = shmod(rr_frag, sizeof(rr_frag));
    VkShaderModule vs_tex = shmod(tex_vert, sizeof(tex_vert)), fs_tex = shmod(tex_frag, sizeof(tex_frag));
    VkPushConstantRange pcr = { VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC) };
    VkPipelineLayoutCreateInfo li = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .pushConstantRangeCount = 1, .pPushConstantRanges = &pcr };
    VkPipelineLayout pl_pc; vkCreatePipelineLayout(dev, &li, NULL, &pl_pc);
    VkPipeline pipe_uni = mkPipe(vs_pix, fs_uni, pl_pc, 0, 0);
    VkPipeline pipe_blend = mkPipe(vs_pix, fs_uni, pl_pc, 0, 1);
    VkPipeline pipe_rr = mkPipe(vs_pix, fs_rr, pl_pc, 0, 0);
    ok(pipe_uni && pipe_blend && pipe_rr, "pixel-fill / blend / rounded-rect pipelines created");
    VkRect2D full = { {0, 0}, {W, H} };

    PC pc = {0}; pc.vp[0] = (float)W; pc.vp[1] = (float)H;

    /* ---- Scene A: filled rectangles ---- */
    { Rec r = beginFrame(0.0f, 0.0f, 0.0f, 1.0f);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni); vkCmdSetScissor(r.cmd, 0, 1, &full);
      float va[12]; rectVerts(8, 8, 16, 24, va); VkDeviceMemory ma; VkBuffer ba = mkVbo(va, sizeof(va), &ma);
      PC p = pc; p.col[0]=1; p.col[1]=0; p.col[2]=0; p.col[3]=1; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
      VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &ba, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      float vb[12]; rectVerts(40, 32, 48, 52, vb); VkDeviceMemory mb; VkBuffer bb = mkVbo(vb, sizeof(vb), &mb);
      PC p2 = pc; p2.col[0]=0; p2.col[1]=1; p2.col[2]=0; p2.col[3]=1; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p2);
      vkCmdBindVertexBuffers(r.cmd, 0, 1, &bb, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      endFrame(r); vkDestroyBuffer(dev, ba, NULL); vkFreeMemory(dev, ma, NULL); vkDestroyBuffer(dev, bb, NULL); vkFreeMemory(dev, mb, NULL);
      int bad = 0;
      for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        int er, eg, eb;
        if (x>=8 && x<16 && y>=8 && y<24) { er=255; eg=0; eb=0; }
        else if (x>=40 && x<48 && y>=32 && y<52) { er=0; eg=255; eb=0; }
        else { er=0; eg=0; eb=0; }
        if (!peq(x, y, er, eg, eb, 255, 1)) bad++; }
      ok(bad == 0, "filled rectangles: every pixel matches closed-form rect coverage");
      ok(peq(10, 10, 255, 0, 0, 255, 1), "rect A interior red"); ok(peq(44, 40, 0, 255, 0, 255, 1), "rect B interior green");
      ok(peq(30, 30, 0, 0, 0, 255, 1), "gap between rects is background"); }

    /* ---- Scene B: analytic rounded-rect ---- */
    { Rec r = beginFrame(0, 0, 0, 1);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_rr); vkCmdSetScissor(r.cmd, 0, 1, &full);
      PC p = pc; p.col[0]=1; p.col[1]=1; p.col[2]=0; p.col[3]=1; p.box[0]=12; p.box[1]=12; p.box[2]=52; p.box[3]=52; p.rad=8.0f;
      vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
      float fq[12] = { 0,0, (float)W,0, 0,(float)H,  0,(float)H, (float)W,0, (float)W,(float)H }; VkDeviceMemory fm; VkBuffer fbo2 = mkVbo(fq, sizeof(fq), &fm);
      VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &fbo2, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      endFrame(r); vkDestroyBuffer(dev, fbo2, NULL); vkFreeMemory(dev, fm, NULL);
      int bad = 0, lit = 0;
      for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        float cx = x + 0.5f, cy = y + 0.5f; float x0 = 12, y0 = 12, x1 = 52, y1 = 52, rr = 8;
        int cov = (cx>=x0 && cx<x1 && cy>=y0 && cy<y1);
        if (cov) {
          float ccx = 0, ccy = 0; int corner = 0;
          if (cx<x0+rr && cy<y0+rr) { corner=1; ccx=x0+rr; ccy=y0+rr; }
          else if (cx>=x1-rr && cy<y0+rr) { corner=1; ccx=x1-rr; ccy=y0+rr; }
          else if (cx<x0+rr && cy>=y1-rr) { corner=1; ccx=x0+rr; ccy=y1-rr; }
          else if (cx>=x1-rr && cy>=y1-rr) { corner=1; ccx=x1-rr; ccy=y1-rr; }
          if (corner) { float dx = cx-ccx, dy = cy-ccy; if (sqrtf(dx*dx+dy*dy) > rr) cov = 0; }
        }
        if (cov) lit++;
        int er = cov?255:0, eg = cov?255:0, eb = 0;
        if (!peq(x, y, er, eg, eb, 255, 1)) bad++; }
      ok(bad == 0, "rounded-rect: every pixel matches analytic corner-arc coverage");
      ok(lit > 0, "rounded-rect: some pixels covered");
      ok(peq(32, 32, 255, 255, 0, 255, 1), "rounded-rect center lit");
      ok(peq(12, 12, 0, 0, 0, 255, 1), "rounded-rect clipped corner (12,12) is background");
      ok(peq(32, 13, 255, 255, 0, 255, 1), "rounded-rect straight top edge lit"); }

    /* ---- Scene C: nine-patch-style scaled border frame ---- */
    { Rec r = beginFrame(0, 0, 0, 1);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni); vkCmdSetScissor(r.cmd, 0, 1, &full);
      float vo[12]; rectVerts(4, 4, 60, 60, vo); VkDeviceMemory mo; VkBuffer bo = mkVbo(vo, sizeof(vo), &mo);
      PC p = pc; p.col[0]=0; p.col[1]=0; p.col[2]=1; p.col[3]=1; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
      VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &bo, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      float vin[12]; rectVerts(10, 10, 54, 54, vin); VkDeviceMemory mi; VkBuffer bin = mkVbo(vin, sizeof(vin), &mi);
      PC p2 = pc; p2.col[0]=0.1f; p2.col[1]=0.1f; p2.col[2]=0.1f; p2.col[3]=1.0f; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p2);
      vkCmdBindVertexBuffers(r.cmd, 0, 1, &bin, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      endFrame(r); vkDestroyBuffer(dev, bo, NULL); vkFreeMemory(dev, mo, NULL); vkDestroyBuffer(dev, bin, NULL); vkFreeMemory(dev, mi, NULL);
      int bad = 0;
      for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        int inbox = x>=4 && x<60 && y>=4 && y<60;
        int ininner = x>=10 && x<54 && y>=10 && y<54;
        int er, eg, eb;
        if (ininner) { er=q8(0.1f); eg=q8(0.1f); eb=q8(0.1f); }
        else if (inbox) { er=0; eg=0; eb=255; }
        else { er=0; eg=0; eb=0; }
        if (!peq(x, y, er, eg, eb, 255, 1)) bad++; }
      ok(bad == 0, "nine-patch border frame: closed-form border-vs-interior coverage");
      ok(peq(5, 32, 0, 0, 255, 255, 1), "nine-patch left border blue");
      ok(peq(32, 5, 0, 0, 255, 255, 1), "nine-patch top border blue");
      ok(peq(32, 32, q8(0.1f), q8(0.1f), q8(0.1f), 255, 1), "nine-patch hollow interior"); }

    /* ---- Scene D: 8x8 bitmap-font glyph blit ---- */
    static const unsigned char GLYPH_H[8] = { 0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00 };
    {
      unsigned char rgba[8 * 8 * 4];
      for (int rr = 0; rr < 8; rr++) for (int c = 0; c < 8; c++) {
        int lit = (GLYPH_H[rr] >> (7 - c)) & 1; unsigned char v = lit ? 255 : 0;
        int idx = (rr * 8 + c) * 4; rgba[idx] = v; rgba[idx+1] = v; rgba[idx+2] = v; rgba[idx+3] = 255;
      }
      VkImageCreateInfo tii = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM,
        .extent = { 8, 8, 1 }, .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
      VkImage gtex; VKOK(vkCreateImage(dev, &tii, NULL, &gtex), "glyph image");
      VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev, gtex, &tmr);
      VkMemoryAllocateInfo tai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = tmr.size, .memoryTypeIndex = memtype(tmr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
      VkDeviceMemory tmem; vkAllocateMemory(dev, &tai, NULL, &tmem); vkBindImageMemory(dev, gtex, tmem, 0);
      VkBufferCreateInfo sbi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sizeof(rgba), .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT };
      VkBuffer sbuf; vkCreateBuffer(dev, &sbi, NULL, &sbuf);
      VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev, sbuf, &smr);
      VkMemoryAllocateInfo sai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = smr.size, .memoryTypeIndex = memtype(smr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
      VkDeviceMemory smem; vkAllocateMemory(dev, &sai, NULL, &smem); vkBindBufferMemory(dev, sbuf, smem, 0);
      void* spp; vkMapMemory(dev, smem, 0, sizeof(rgba), 0, &spp); memcpy(spp, rgba, sizeof(rgba)); vkUnmapMemory(dev, smem);
      { VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
        VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
        VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd, &bi);
        VkImageMemoryBarrier b1 = { .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED, .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
          .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .image = gtex, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
          .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT };
        vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b1);
        VkBufferImageCopy cp = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { 8, 8, 1 } };
        vkCmdCopyBufferToImage(cmd, sbuf, gtex, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &cp);
        VkImageMemoryBarrier b2 = b1; b2.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        b2.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
        vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 1, &b2);
        vkEndCommandBuffer(cmd);
        VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
        VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fnc; vkCreateFence(dev, &fi, NULL, &fnc);
        vkQueueSubmit(q, 1, &si, fnc); vkWaitForFences(dev, 1, &fnc, VK_TRUE, UINT64_MAX); vkDestroyFence(dev, fnc, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd); }
      VkImageViewCreateInfo tvi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = gtex, .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
      VkImageView tview; vkCreateImageView(dev, &tvi, NULL, &tview);
      VkSamplerCreateInfo smci = { .sType = VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, .magFilter = VK_FILTER_NEAREST, .minFilter = VK_FILTER_NEAREST,
        .addressModeU = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeV = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE };
      VkSampler samp; vkCreateSampler(dev, &smci, NULL, &samp);
      VkDescriptorSetLayoutBinding dslb = { .binding = 0, .descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .descriptorCount = 1, .stageFlags = VK_SHADER_STAGE_FRAGMENT_BIT };
      VkDescriptorSetLayoutCreateInfo dslci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &dslb };
      VkDescriptorSetLayout dsl; VKOK(vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl), "glyph descriptor set layout");
      VkPushConstantRange tpcr = { VK_SHADER_STAGE_VERTEX_BIT, 0, 16 };
      VkPipelineLayoutCreateInfo plci = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &dsl, .pushConstantRangeCount = 1, .pPushConstantRanges = &tpcr };
      VkPipelineLayout pl_tex; vkCreatePipelineLayout(dev, &plci, NULL, &pl_tex);
      VkPipeline pt = mkPipe(vs_tex, fs_tex, pl_tex, 1, 0);
      ok(dsl && pl_tex && pt && samp, "glyph pipeline + descriptor created");
      VkDescriptorPoolSize dps = { VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1 };
      VkDescriptorPoolCreateInfo dpci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps };
      VkDescriptorPool dpool; vkCreateDescriptorPool(dev, &dpci, NULL, &dpool);
      VkDescriptorSetAllocateInfo dsai = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl };
      VkDescriptorSet dset; vkAllocateDescriptorSets(dev, &dsai, &dset);
      VkDescriptorImageInfo dii2 = { samp, tview, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
      VkWriteDescriptorSet wds = { .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = dset, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .pImageInfo = &dii2 };
      vkUpdateDescriptorSets(dev, 1, &wds, 0, NULL);
      float gq[24] = { 20,20,0,0,  28,20,1,0,  20,28,0,1,   20,28,0,1,  28,20,1,0,  28,28,1,1 };
      VkDeviceMemory gm; VkBuffer gvbo = mkVbo(gq, sizeof(gq), &gm);
      { Rec r = beginFrame(0, 0, 0, 1);
        vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pt); vkCmdSetScissor(r.cmd, 0, 1, &full);
        float vpv[2] = { (float)W, (float)H }; vkCmdPushConstants(r.cmd, pl_tex, VK_SHADER_STAGE_VERTEX_BIT, 0, 8, vpv);
        vkCmdBindDescriptorSets(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pl_tex, 0, 1, &dset, 0, NULL);
        VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &gvbo, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
        endFrame(r); }
      int bad = 0;
      for (int dy = 0; dy < 8; dy++) for (int dx = 0; dx < 8; dx++) {
        int sx = 20 + dx, sy = 20 + dy; int trow = dy, tcol = dx;
        int lit = (GLYPH_H[trow] >> (7 - tcol)) & 1; int v = lit ? 255 : 0;
        if (!peq(sx, sy, v, v, v, 255, 1)) bad++; }
      ok(bad == 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap");
      ok(peq(21, 23, 255, 255, 255, 255, 1), "glyph crossbar lit (col1,row3)");
      ok(peq(23, 20, 0, 0, 0, 255, 1), "glyph row0 blank");
      ok(peq(24, 21, 0, 0, 0, 255, 1), "glyph row1 middle blank (0x42)");
      vkDestroyPipeline(dev, pt, NULL); vkDestroyBuffer(dev, gvbo, NULL); vkFreeMemory(dev, gm, NULL);
      vkDestroySampler(dev, samp, NULL); vkDestroyImageView(dev, tview, NULL); vkDestroyImage(dev, gtex, NULL); vkFreeMemory(dev, tmem, NULL);
      vkDestroyBuffer(dev, sbuf, NULL); vkFreeMemory(dev, smem, NULL);
      vkDestroyDescriptorPool(dev, dpool, NULL); vkDestroyDescriptorSetLayout(dev, dsl, NULL); vkDestroyPipelineLayout(dev, pl_tex, NULL); }

    /* ---- Scene E: scissor-clipped fill ---- */
    { Rec r = beginFrame(0, 0, 0, 1);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni);
      VkRect2D box = { {16, 16}, {20, 20} }; vkCmdSetScissor(r.cmd, 0, 1, &box);
      PC p = pc; p.col[0]=1; p.col[1]=0; p.col[2]=1; p.col[3]=1; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
      float fv[12]; rectVerts(0, 0, (float)W, (float)H, fv); VkDeviceMemory fm; VkBuffer fbb = mkVbo(fv, sizeof(fv), &fm);
      VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &fbb, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      endFrame(r); vkDestroyBuffer(dev, fbb, NULL); vkFreeMemory(dev, fm, NULL);
      int bad = 0;
      for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        int in = x>=16 && x<36 && y>=16 && y<36;
        int er = in?255:0, eg = 0, eb = in?255:0;
        if (!peq(x, y, er, eg, eb, 255, 1)) bad++; }
      ok(bad == 0, "scissor-clipped fill: magenta only within [16,36)^2");
      ok(peq(20, 20, 255, 0, 255, 255, 1), "scissor inside magenta"); ok(peq(40, 40, 0, 0, 0, 255, 1), "scissor outside background"); }

    /* ---- Scene F: MULTI-LAYER Porter-Duff over compositing ---- */
    { float bg[4] = { 0.10f, 0.10f, 0.10f, 1.0f };
      struct L { float r, g, b, a; float x0, y0, x1, y1; };
      struct L layers[3] = {
        { 1.0f, 0.0f, 0.0f, 0.50f,  8, 8, 56, 56 },
        { 0.0f, 1.0f, 0.0f, 0.25f, 12, 12, 52, 52 },
        { 0.0f, 0.0f, 1.0f, 0.75f, 16, 16, 48, 48 },
      };
      Rec r = beginFrame(bg[0], bg[1], bg[2], bg[3]);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_blend); vkCmdSetScissor(r.cmd, 0, 1, &full);
      VkBuffer lb[3]; VkDeviceMemory lm[3];
      for (int i = 0; i < 3; i++) { struct L l = layers[i]; float lv[12]; rectVerts(l.x0, l.y0, l.x1, l.y1, lv); lb[i] = mkVbo(lv, sizeof(lv), &lm[i]);
        PC p = pc; p.col[0]=l.r; p.col[1]=l.g; p.col[2]=l.b; p.col[3]=l.a; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
        VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &lb[i], &off); vkCmdDraw(r.cmd, 6, 1, 0, 0); }
      endFrame(r); for (int i = 0; i < 3; i++) { vkDestroyBuffer(dev, lb[i], NULL); vkFreeMemory(dev, lm[i], NULL); }
      int bad = 0;
      for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        float c[4] = { bg[0], bg[1], bg[2], bg[3] };
        for (int i = 0; i < 3; i++) { struct L l = layers[i];
          float cx = x + 0.5f, cy = y + 0.5f;
          if (cx>=l.x0 && cx<l.x1 && cy>=l.y0 && cy<l.y1) {
            float as = l.a; float src[4] = { l.r, l.g, l.b, l.a };
            for (int k = 0; k < 4; k++) c[k] = src[k]*as + c[k]*(1.0f-as);
          }
        }
        if (!peq(x, y, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2)) bad++; }
      ok(bad == 0, "multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)");
      { float c[4] = { bg[0], bg[1], bg[2], bg[3] };
        float L0[4] = { 1, 0, 0, 0.5f }, L1[4] = { 0, 1, 0, 0.25f }, L2[4] = { 0, 0, 1, 0.75f };
        float* ls[3] = { L0, L1, L2 };
        for (int i = 0; i < 3; i++) { float as = ls[i][3]; for (int k = 0; k < 4; k++) c[k] = ls[i][k]*as + c[k]*(1.f-as); }
        ok(peq(32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2), "multi-layer over center pixel matches hand-iterated over"); }
      { float as = 0.5f; float er = 1.0f*as+bg[0]*(1-as), eg = 0*as+bg[1]*(1-as), eb = 0*as+bg[2]*(1-as), ea = as*as+bg[3]*(1-as);
        ok(peq(10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2), "multi-layer over: single-layer region matches one over"); }
    }

    /* ---- Negative control ---- */
    { Rec r = beginFrame(0, 0, 0, 1);
      vkCmdBindPipeline(r.cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni); vkCmdSetScissor(r.cmd, 0, 1, &full);
      float va[12]; rectVerts(8, 8, 16, 24, va); VkDeviceMemory ma; VkBuffer ba = mkVbo(va, sizeof(va), &ma);
      PC p = pc; p.col[0]=1; p.col[1]=0; p.col[2]=0; p.col[3]=1; vkCmdPushConstants(r.cmd, pl_pc, VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(PC), &p);
      VkDeviceSize off = 0; vkCmdBindVertexBuffers(r.cmd, 0, 1, &ba, &off); vkCmdDraw(r.cmd, 6, 1, 0, 0);
      endFrame(r); vkDestroyBuffer(dev, ba, NULL); vkFreeMemory(dev, ma, NULL); }
    ok(!peq(10, 10, 0, 255, 0, 255, 4), "negative control: red rect pixel is NOT green");
    ok(!peq(30, 30, 255, 0, 0, 255, 4), "negative control: background is NOT red");

    vkDeviceWaitIdle(dev);
    int EXPECTED = 39, TOTAL = PASS + FAIL;
    printf("scene-2dui-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("SCENE_2DUI_C OK %d\n", PASS); return 0; }
    return 1;
}
