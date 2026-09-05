// vulkan_render_cpp_full_api.cpp - Vulkan RENDER carpet on Mesa lavapipe (software Vulkan on the CPU,
// no GPU, no window/surface/swapchain). Builds an offscreen render pass into an R8G8B8A8_UNORM color
// image, draws through a real graphics pipeline (SPIR-V vertex+fragment shaders), copies the image to a
// host-visible buffer with vkCmdCopyImageToBuffer, maps it, and checks every pixel against a closed-form
// reference for: render-pass clear, a solid quad (push-constant color), a per-vertex axis-aligned linear
// gradient (a triangle-strip quad interpolates per-triangle, so only an axis-aligned gradient matches a
// full-quad closed form), a procedural checkerboard from gl_FragCoord, a dynamic scissor, alpha blending
// (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels incl alpha), a sub-rectangle readback. Exhaustive
// per-API coverage builds a pipeline per state: primitive topologies (VkPrimitiveTopology TRIANGLE_LIST/
// TRIANGLE_FAN/LINE_LIST/LINE_STRIP/POINT_LIST), a blend factor+op matrix (VkBlendFactor ONE/ZERO,
// ONE/ONE, ZERO/ONE, DST_COLOR; VkBlendOp ADD/MAX/REVERSE_SUBTRACT), the full depth-func matrix (all 8
// VkCompareOp against a D32_SFLOAT attachment; Vulkan NDC z in [0,1] so a z=0.5 quad vs clear-depth
// 0.75), face culling + winding (VkCullModeFlags NONE/FRONT_AND_BACK/BACK x VkFrontFace CCW-vs-CW), a
// color write mask (VkColorComponentFlags), format+device property queries (vkGetPhysicalDeviceFormat
// Properties / vkGetPhysicalDeviceProperties), and a 2x2 texture upload + NEAREST sampling through a
// combined image sampler + descriptor set, closing with a negative control. Prints
// "VULKAN_RENDER_CPP_FULL_API OK <n>" only when every assertion passes and count == EXPECTED. lavapipe
// is Vulkan 1.3-conformant with no surface required; single vCPU, deterministic.
#include <vulkan/vulkan.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include "shaders/solid_vert.h"
#include "shaders/solid_frag.h"
#include "shaders/grad_vert.h"
#include "shaders/grad_frag.h"
#include "shaders/check_frag.h"
#include "shaders/point_vert.h"
#include "shaders/pos3_vert.h"
#include "shaders/tex_vert.h"
#include "shaders/tex_frag.h"

static int PASS = 0, FAIL = 0;
static void ok(bool c, const char* d) { if (c) PASS++; else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); } }
#define VKOK(e, d) ok((e) == VK_SUCCESS, d)

static const uint32_t W = 64, H = 64;
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue q; static uint32_t qfam;
static VkCommandPool pool; static VkImage cimg; static VkImageView cview; static VkFramebuffer fb;
static VkRenderPass rp; static VkDeviceMemory cmem;
static VkBuffer rbuf; static VkDeviceMemory rmem; static uint8_t* rmap;
static VkRenderPass rp_d; static VkFramebuffer fb_d; static VkImage dimg; static VkImageView dview; static VkDeviceMemory dmem;
static unsigned char px_[W * H * 4];

static uint32_t memtype(uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want) return i;
    return UINT32_MAX;
}
static VkShaderModule shmod(const uint32_t* code, size_t bytes) {
    VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
    ci.codeSize = bytes; ci.pCode = code; VkShaderModule m; vkCreateShaderModule(dev, &ci, nullptr, &m); return m;
}
static unsigned char P(int x, int y, int c) { return px_[(y * W + x) * 4 + c]; }
static bool peq(int x, int y, int r, int g, int b, int a, int tol) {
    return abs((int)P(x,y,0)-r)<=tol && abs((int)P(x,y,1)-g)<=tol && abs((int)P(x,y,2)-b)<=tol && abs((int)P(x,y,3)-a)<=tol;
}
static bool all_eq(int r, int g, int b, int a, int tol) {
    for (uint32_t y=0;y<H;y++) for (uint32_t x=0;x<W;x++) if(!peq(x,y,r,g,b,a,tol)) return false; return true;
}

// A graphics pipeline over the offscreen render pass. blend toggles SRC_ALPHA/ONE_MINUS_SRC_ALPHA.
static VkPipelineLayout mkLayout(bool pushConst) {
    VkPushConstantRange pcr{VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16};
    VkPipelineLayoutCreateInfo li{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    if (pushConst) { li.pushConstantRangeCount = 1; li.pPushConstantRanges = &pcr; }
    VkPipelineLayout pl; vkCreatePipelineLayout(dev, &li, nullptr, &pl); return pl;
}
static VkPipeline mkPipe(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl, bool withColorAttr, bool blend) {
    VkPipelineShaderStageCreateInfo st[2]{};
    st[0] = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage = VK_SHADER_STAGE_VERTEX_BIT; st[0].module = vs; st[0].pName = "main";
    st[1] = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage = VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module = fs; st[1].pName = "main";
    VkVertexInputBindingDescription bind{0, (uint32_t)(withColorAttr ? 24 : 8), VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription attr[2];
    attr[0] = {0, 0, VK_FORMAT_R32G32_SFLOAT, 0};
    attr[1] = {1, 0, VK_FORMAT_R32G32B32A32_SFLOAT, 8};
    VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    vi.vertexBindingDescriptionCount = 1; vi.pVertexBindingDescriptions = &bind;
    vi.vertexAttributeDescriptionCount = withColorAttr ? 2 : 1; vi.pVertexAttributeDescriptions = attr;
    VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO};
    ia.topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP;
    VkViewport vp{0, 0, (float)W, (float)H, 0, 1}; VkRect2D sc{{0, 0}, {W, H}};
    VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO};
    vps.viewportCount = 1; vps.pViewports = &vp; vps.scissorCount = 1; vps.pScissors = &sc;
    VkDynamicState dyn = VK_DYNAMIC_STATE_SCISSOR;
    VkPipelineDynamicStateCreateInfo ds{VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO};
    ds.dynamicStateCount = 1; ds.pDynamicStates = &dyn;
    VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO};
    rs.polygonMode = VK_POLYGON_MODE_FILL; rs.cullMode = VK_CULL_MODE_NONE; rs.lineWidth = 1.0f;
    VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO};
    ms.rasterizationSamples = VK_SAMPLE_COUNT_1_BIT;
    VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask = 0xF; cba.blendEnable = blend ? VK_TRUE : VK_FALSE;
    cba.srcColorBlendFactor = VK_BLEND_FACTOR_SRC_ALPHA; cba.dstColorBlendFactor = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA; cba.colorBlendOp = VK_BLEND_OP_ADD;
    cba.srcAlphaBlendFactor = VK_BLEND_FACTOR_SRC_ALPHA; cba.dstAlphaBlendFactor = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA; cba.alphaBlendOp = VK_BLEND_OP_ADD;
    VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO};
    cb.attachmentCount = 1; cb.pAttachments = &cba;
    VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
    gp.stageCount = 2; gp.pStages = st; gp.pVertexInputState = &vi; gp.pInputAssemblyState = &ia;
    gp.pViewportState = &vps; gp.pRasterizationState = &rs; gp.pMultisampleState = &ms; gp.pColorBlendState = &cb;
    gp.pDynamicState = &ds; gp.layout = pl; gp.renderPass = rp; gp.subpass = 0;
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, nullptr, &p); return p;
}

// Rich pipeline builder for exhaustive per-API coverage: vertex layout (0=pos2, 1=pos2+color,
// 2=pos3, 3=pos2+uv), topology, full blend factor+op config, cull mode + winding, depth test +
// compare op, the render pass to use, and a color write mask.
static VkPipeline mkPipe2(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl, int vlayout,
                          VkPrimitiveTopology topo, bool blend,
                          VkBlendFactor sC, VkBlendFactor dC, VkBlendOp oC,
                          VkBlendFactor sA, VkBlendFactor dA, VkBlendOp oA,
                          VkCullModeFlags cull, VkFrontFace front,
                          bool depthTest, VkCompareOp depthOp, VkRenderPass rpUse, uint32_t cwmask) {
    VkPipelineShaderStageCreateInfo st[2]{};
    st[0] = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[0].stage = VK_SHADER_STAGE_VERTEX_BIT; st[0].module = vs; st[0].pName = "main";
    st[1] = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO}; st[1].stage = VK_SHADER_STAGE_FRAGMENT_BIT; st[1].module = fs; st[1].pName = "main";
    uint32_t stride = vlayout == 0 ? 8 : vlayout == 1 ? 24 : vlayout == 2 ? 12 : 16;
    VkVertexInputBindingDescription bind{0, stride, VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription attr[2]; uint32_t nattr = 1;
    if (vlayout == 2) attr[0] = {0, 0, VK_FORMAT_R32G32B32_SFLOAT, 0};
    else attr[0] = {0, 0, VK_FORMAT_R32G32_SFLOAT, 0};
    if (vlayout == 1) { attr[1] = {1, 0, VK_FORMAT_R32G32B32A32_SFLOAT, 8}; nattr = 2; }
    else if (vlayout == 3) { attr[1] = {1, 0, VK_FORMAT_R32G32_SFLOAT, 8}; nattr = 2; }
    VkPipelineVertexInputStateCreateInfo vi{VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    vi.vertexBindingDescriptionCount = 1; vi.pVertexBindingDescriptions = &bind;
    vi.vertexAttributeDescriptionCount = nattr; vi.pVertexAttributeDescriptions = attr;
    VkPipelineInputAssemblyStateCreateInfo ia{VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO};
    ia.topology = topo;
    VkViewport vp{0, 0, (float)W, (float)H, 0, 1}; VkRect2D sc{{0, 0}, {W, H}};
    VkPipelineViewportStateCreateInfo vps{VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO};
    vps.viewportCount = 1; vps.pViewports = &vp; vps.scissorCount = 1; vps.pScissors = &sc;
    VkDynamicState dyn = VK_DYNAMIC_STATE_SCISSOR;
    VkPipelineDynamicStateCreateInfo ds{VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO};
    ds.dynamicStateCount = 1; ds.pDynamicStates = &dyn;
    VkPipelineRasterizationStateCreateInfo rs{VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO};
    rs.polygonMode = VK_POLYGON_MODE_FILL; rs.cullMode = cull; rs.frontFace = front; rs.lineWidth = 1.0f;
    VkPipelineMultisampleStateCreateInfo ms{VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO};
    ms.rasterizationSamples = VK_SAMPLE_COUNT_1_BIT;
    VkPipelineDepthStencilStateCreateInfo dss{VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO};
    dss.depthTestEnable = depthTest ? VK_TRUE : VK_FALSE; dss.depthWriteEnable = depthTest ? VK_TRUE : VK_FALSE;
    dss.depthCompareOp = depthOp; dss.minDepthBounds = 0.0f; dss.maxDepthBounds = 1.0f;
    VkPipelineColorBlendAttachmentState cba{}; cba.colorWriteMask = cwmask; cba.blendEnable = blend ? VK_TRUE : VK_FALSE;
    cba.srcColorBlendFactor = sC; cba.dstColorBlendFactor = dC; cba.colorBlendOp = oC;
    cba.srcAlphaBlendFactor = sA; cba.dstAlphaBlendFactor = dA; cba.alphaBlendOp = oA;
    VkPipelineColorBlendStateCreateInfo cb{VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO};
    cb.attachmentCount = 1; cb.pAttachments = &cba;
    VkGraphicsPipelineCreateInfo gp{VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO};
    gp.stageCount = 2; gp.pStages = st; gp.pVertexInputState = &vi; gp.pInputAssemblyState = &ia;
    gp.pViewportState = &vps; gp.pRasterizationState = &rs; gp.pMultisampleState = &ms; gp.pColorBlendState = &cb;
    gp.pDepthStencilState = &dss; gp.pDynamicState = &ds; gp.layout = pl; gp.renderPass = rpUse; gp.subpass = 0;
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, nullptr, &p); return p;
}

// vertex buffer helper (host-visible)
static VkBuffer mkVbo(const void* data, size_t sz, VkDeviceMemory* mem) {
    VkBufferCreateInfo bi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; bi.size = sz; bi.usage = VK_BUFFER_USAGE_VERTEX_BUFFER_BIT;
    VkBuffer b; vkCreateBuffer(dev, &bi, nullptr, &b);
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, b, &mr);
    VkMemoryAllocateInfo ai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; ai.allocationSize = mr.size;
    ai.memoryTypeIndex = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    vkAllocateMemory(dev, &ai, nullptr, mem); vkBindBufferMemory(dev, b, *mem, 0);
    void* p; vkMapMemory(dev, *mem, 0, sz, 0, &p); memcpy(p, data, sz); vkUnmapMemory(dev, *mem);
    return b;
}

// draw one frame: clear to (cr,cg,cb,ca), bind pipe (with optional push-const color + vbo), scissor,
// draw `verts`, copy image to the readback buffer, map into px_.
static void frame(float cr, float cg, float cb, float ca, VkPipeline pipe, VkPipelineLayout pl,
                  const float* pushColor, VkBuffer vbo, uint32_t verts, VkRect2D scissor) {
    VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
    cai.commandPool = pool; cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount = 1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv; cv.color = {{cr, cg, cb, ca}};
    VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO};
    rpb.renderPass = rp; rpb.framebuffer = fb; rpb.renderArea = {{0, 0}, {W, H}}; rpb.clearValueCount = 1; rpb.pClearValues = &cv;
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    if (pipe) {
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
        vkCmdSetScissor(cmd, 0, 1, &scissor);
        if (pushColor) vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, pushColor);
        if (vbo) { VkDeviceSize off = 0; vkCmdBindVertexBuffers(cmd, 0, 1, &vbo, &off); }
        vkCmdDraw(cmd, verts, 1, 0, 0);
    }
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region{}; region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {W, H, 1};
    vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount = 1; si.pCommandBuffers = &cmd;
    VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev, &fi, nullptr, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, nullptr); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    memcpy(px_, rmap, sizeof(px_));
}

// depth-enabled frame: clears color to (cr,cg,cb,ca) and depth to `depthClear`, uses the depth
// render pass/framebuffer, draws the vec3 quad through `pipe`, copies the color image to px_.
static void frameD(float cr, float cg, float cb, float ca, float depthClear, VkPipeline pipe,
                   VkPipelineLayout pl, const float* pushColor, VkBuffer vbo, uint32_t verts) {
    VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
    cai.commandPool = pool; cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount = 1;
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv[2]; cv[0].color = {{cr, cg, cb, ca}}; cv[1].depthStencil = {depthClear, 0};
    VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO};
    rpb.renderPass = rp_d; rpb.framebuffer = fb_d; rpb.renderArea = {{0, 0}, {W, H}}; rpb.clearValueCount = 2; rpb.pClearValues = cv;
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    VkRect2D scissor{{0, 0}, {W, H}};
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
    vkCmdSetScissor(cmd, 0, 1, &scissor);
    if (pushColor) vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, pushColor);
    if (vbo) { VkDeviceSize off = 0; vkCmdBindVertexBuffers(cmd, 0, 1, &vbo, &off); }
    vkCmdDraw(cmd, verts, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region{}; region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {W, H, 1};
    vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount = 1; si.pCommandBuffers = &cmd;
    VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fence; vkCreateFence(dev, &fi, nullptr, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, nullptr); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    memcpy(px_, rmap, sizeof(px_));
}

int main() {
    VkApplicationInfo ai{VK_STRUCTURE_TYPE_APPLICATION_INFO}; ai.apiVersion = VK_API_VERSION_1_1;
    VkInstanceCreateInfo ici{VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO}; ici.pApplicationInfo = &ai;
    VKOK(vkCreateInstance(&ici, nullptr, &inst), "vkCreateInstance");
    uint32_t n = 0; vkEnumeratePhysicalDevices(inst, &n, nullptr); ok(n >= 1, ">=1 physical device");
    std::vector<VkPhysicalDevice> pds(n); vkEnumeratePhysicalDevices(inst, &n, pds.data()); pd = pds[0];
    uint32_t nqf = 0; vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, nullptr);
    std::vector<VkQueueFamilyProperties> qf(nqf); vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qf.data());
    qfam = UINT32_MAX; for (uint32_t i = 0; i < nqf; i++) if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { qfam = i; break; }
    ok(qfam != UINT32_MAX, "graphics queue family");
    float pri = 1.0f; VkDeviceQueueCreateInfo qci{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    qci.queueFamilyIndex = qfam; qci.queueCount = 1; qci.pQueuePriorities = &pri;
    VkDeviceCreateInfo dci{VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO}; dci.queueCreateInfoCount = 1; dci.pQueueCreateInfos = &qci;
    VKOK(vkCreateDevice(pd, &dci, nullptr, &dev), "vkCreateDevice"); vkGetDeviceQueue(dev, qfam, 0, &q);

    // offscreen color image (R8G8B8A8_UNORM, COLOR_ATTACHMENT | TRANSFER_SRC)
    VkImageCreateInfo ii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; ii.imageType = VK_IMAGE_TYPE_2D;
    ii.format = VK_FORMAT_R8G8B8A8_UNORM; ii.extent = {W, H, 1}; ii.mipLevels = 1; ii.arrayLayers = 1;
    ii.samples = VK_SAMPLE_COUNT_1_BIT; ii.tiling = VK_IMAGE_TILING_OPTIMAL;
    ii.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    ii.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    VKOK(vkCreateImage(dev, &ii, nullptr, &cimg), "vkCreateImage color");
    VkMemoryRequirements imr; vkGetImageMemoryRequirements(dev, cimg, &imr);
    VkMemoryAllocateInfo iai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; iai.allocationSize = imr.size;
    iai.memoryTypeIndex = memtype(imr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    vkAllocateMemory(dev, &iai, nullptr, &cmem); vkBindImageMemory(dev, cimg, cmem, 0);
    VkImageViewCreateInfo vi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; vi.image = cimg; vi.viewType = VK_IMAGE_VIEW_TYPE_2D;
    vi.format = VK_FORMAT_R8G8B8A8_UNORM; vi.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    VKOK(vkCreateImageView(dev, &vi, nullptr, &cview), "vkCreateImageView");

    // render pass: 1 color attachment, CLEAR -> STORE, final layout TRANSFER_SRC_OPTIMAL
    VkAttachmentDescription att{}; att.format = VK_FORMAT_R8G8B8A8_UNORM; att.samples = VK_SAMPLE_COUNT_1_BIT;
    att.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR; att.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    att.stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE; att.stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    att.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED; att.finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    VkAttachmentReference ref{0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sp{}; sp.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS; sp.colorAttachmentCount = 1; sp.pColorAttachments = &ref;
    VkRenderPassCreateInfo rpi{VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO}; rpi.attachmentCount = 1; rpi.pAttachments = &att; rpi.subpassCount = 1; rpi.pSubpasses = &sp;
    VKOK(vkCreateRenderPass(dev, &rpi, nullptr, &rp), "vkCreateRenderPass");
    VkFramebufferCreateInfo fbi{VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO}; fbi.renderPass = rp; fbi.attachmentCount = 1; fbi.pAttachments = &cview; fbi.width = W; fbi.height = H; fbi.layers = 1;
    VKOK(vkCreateFramebuffer(dev, &fbi, nullptr, &fb), "vkCreateFramebuffer");

    // depth resources for the depth-func matrix: D32_SFLOAT image + a color+depth render pass sharing cimg
    VkImageCreateInfo dii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; dii.imageType = VK_IMAGE_TYPE_2D;
    dii.format = VK_FORMAT_D32_SFLOAT; dii.extent = {W, H, 1}; dii.mipLevels = 1; dii.arrayLayers = 1;
    dii.samples = VK_SAMPLE_COUNT_1_BIT; dii.tiling = VK_IMAGE_TILING_OPTIMAL;
    dii.usage = VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT; dii.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    VKOK(vkCreateImage(dev, &dii, nullptr, &dimg), "vkCreateImage depth");
    VkMemoryRequirements dmr; vkGetImageMemoryRequirements(dev, dimg, &dmr);
    VkMemoryAllocateInfo daii{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; daii.allocationSize = dmr.size;
    daii.memoryTypeIndex = memtype(dmr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    vkAllocateMemory(dev, &daii, nullptr, &dmem); vkBindImageMemory(dev, dimg, dmem, 0);
    VkImageViewCreateInfo dvi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; dvi.image = dimg; dvi.viewType = VK_IMAGE_VIEW_TYPE_2D;
    dvi.format = VK_FORMAT_D32_SFLOAT; dvi.subresourceRange = {VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1};
    VKOK(vkCreateImageView(dev, &dvi, nullptr, &dview), "vkCreateImageView depth");
    VkAttachmentDescription datt[2]{};
    datt[0].format = VK_FORMAT_R8G8B8A8_UNORM; datt[0].samples = VK_SAMPLE_COUNT_1_BIT;
    datt[0].loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR; datt[0].storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    datt[0].stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE; datt[0].stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    datt[0].initialLayout = VK_IMAGE_LAYOUT_UNDEFINED; datt[0].finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    datt[1].format = VK_FORMAT_D32_SFLOAT; datt[1].samples = VK_SAMPLE_COUNT_1_BIT;
    datt[1].loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR; datt[1].storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    datt[1].stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE; datt[1].stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    datt[1].initialLayout = VK_IMAGE_LAYOUT_UNDEFINED; datt[1].finalLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
    VkAttachmentReference dcref{0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkAttachmentReference ddref{1, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL};
    VkSubpassDescription dsp{}; dsp.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS;
    dsp.colorAttachmentCount = 1; dsp.pColorAttachments = &dcref; dsp.pDepthStencilAttachment = &ddref;
    VkRenderPassCreateInfo drpi{VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO}; drpi.attachmentCount = 2; drpi.pAttachments = datt; drpi.subpassCount = 1; drpi.pSubpasses = &dsp;
    VKOK(vkCreateRenderPass(dev, &drpi, nullptr, &rp_d), "vkCreateRenderPass depth");
    VkImageView dfbv[2] = {cview, dview};
    VkFramebufferCreateInfo dfbi{VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO}; dfbi.renderPass = rp_d; dfbi.attachmentCount = 2; dfbi.pAttachments = dfbv; dfbi.width = W; dfbi.height = H; dfbi.layers = 1;
    VKOK(vkCreateFramebuffer(dev, &dfbi, nullptr, &fb_d), "vkCreateFramebuffer depth");

    // readback buffer (host-visible)
    VkBufferCreateInfo rbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; rbi.size = W * H * 4; rbi.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    vkCreateBuffer(dev, &rbi, nullptr, &rbuf);
    VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev, rbuf, &rmr);
    VkMemoryAllocateInfo rai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; rai.allocationSize = rmr.size;
    rai.memoryTypeIndex = memtype(rmr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    vkAllocateMemory(dev, &rai, nullptr, &rmem); vkBindBufferMemory(dev, rbuf, rmem, 0);
    vkMapMemory(dev, rmem, 0, W * H * 4, 0, (void**)&rmap);

    VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO}; pci.queueFamilyIndex = qfam; pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    VKOK(vkCreateCommandPool(dev, &pci, nullptr, &pool), "vkCreateCommandPool");

    // pipelines
    VkShaderModule vs_s = shmod(solid_vert, sizeof(solid_vert)), fs_s = shmod(solid_frag, sizeof(solid_frag));
    VkShaderModule vs_g = shmod(grad_vert, sizeof(grad_vert)), fs_g = shmod(grad_frag, sizeof(grad_frag));
    VkShaderModule fs_c = shmod(check_frag, sizeof(check_frag));
    VkShaderModule vs_pt = shmod(point_vert, sizeof(point_vert)), vs_p3 = shmod(pos3_vert, sizeof(pos3_vert));
    VkShaderModule vs_tx = shmod(tex_vert, sizeof(tex_vert)), fs_tx = shmod(tex_frag, sizeof(tex_frag));
    VkPipelineLayout pl_push = mkLayout(true), pl_none = mkLayout(false);
    VkPipeline pipe_solid = mkPipe(vs_s, fs_s, pl_push, false, false);
    VkPipeline pipe_blend = mkPipe(vs_s, fs_s, pl_push, false, true);
    VkPipeline pipe_grad = mkPipe(vs_g, fs_g, pl_none, true, false);
    VkPipeline pipe_check = mkPipe(vs_s, fs_c, pl_none, false, false);
    ok(pipe_solid && pipe_grad && pipe_check && pipe_blend, "graphics pipelines created");

    // vertex data: fullscreen quad (triangle strip BL,BR,TL,TR), pos-only (stride 8) and pos+color (stride 24)
    const float quad[] = { -1,-1, 1,-1, -1,1, 1,1 };
    const float gquad[] = { -1,-1, 1,0,0,1,  1,-1, 0,0,1,1,  -1,1, 1,0,0,1,  1,1, 0,0,1,1 }; // left red, right blue
    VkDeviceMemory qm, gm; VkBuffer vbo = mkVbo(quad, sizeof(quad), &qm); VkBuffer gvbo = mkVbo(gquad, sizeof(gquad), &gm);
    VkRect2D full{{0, 0}, {W, H}};

    // --- Test 1: render-pass clear ---
    frame(0.0f, 0.25f, 0.5f, 1.0f, VK_NULL_HANDLE, pl_none, nullptr, VK_NULL_HANDLE, 0, full);
    ok(all_eq(0, 64, 128, 255, 2), "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)");
    ok(peq(0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");

    // --- Test 2: solid quad (push-constant color) ---
    { float col[4] = {1, 0, 0, 1};
      frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      ok(all_eq(255, 0, 0, 255, 1), "solid red quad fills every pixel"); }

    // --- Test 3: axis-aligned linear gradient (horizontal red->blue) ---
    frame(0, 0, 0, 1, pipe_grad, pl_none, nullptr, gvbo, 4, full);
    { int bad = 0;
      for (uint32_t y=0;y<H;y++) for (uint32_t x=0;x<W;x++){ float u=(x+0.5f)/W;
        int r=(int)lroundf((1.f-u)*255.f), b=(int)lroundf(u*255.f); if(!peq(x,y,r,0,b,255,4)) bad++; }
      ok(bad == 0, "gradient matches horizontal-linear closed-form for all pixels");
      ok(peq(0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red");
      ok(peq(W-1, H-1, 0, 0, 255, 255, 8), "gradient right edge ~ blue");
      ok(peq(W/2, H/2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)"); }

    // --- Test 4: checkerboard from gl_FragCoord ---
    frame(0, 0, 0, 1, pipe_check, pl_none, nullptr, vbo, 4, full);
    { int bad = 0;
      for (uint32_t y=0;y<H;y++) for (uint32_t x=0;x<W;x++){ bool e=(((x>>3)+(y>>3))&1)==0; int w=e?255:0; if(!peq(x,y,w,w,w,255,1)) bad++; }
      ok(bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels");
      ok(peq(0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white");
      ok(peq(8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black"); }

    // --- Test 5: dynamic scissor (clear whole, draw green only in a box) ---
    { float col[4] = {0, 1, 0, 1}; VkRect2D box{{16, 16}, {32, 32}};
      frame(1, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, box);
      ok(peq(32, 32, 0, 255, 0, 255, 1), "scissor: inside box green");
      ok(peq(2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)");
      ok(peq(50, 50, 255, 0, 0, 255, 1), "scissor: past box red"); }

    // --- Test 6: alpha blending (0.5*blue over red clear) ---
    { float col[4] = {0, 0, 1, 0.5f};
      frame(1, 0, 0, 1, pipe_blend, pl_push, col, vbo, 4, full);
      ok(all_eq(128, 0, 128, 191, 3), "alpha blend 0.5*blue over red -> rgb(128,0,128) a191"); }

    // --- Test 7: sub-rectangle readback (solid 0.2,0.4,0.6) ---
    { float col[4] = {0.2f, 0.4f, 0.6f, 1.0f};
      frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      bool s = true; for (int y=10;y<14;y++) for (int x=10;x<14;x++) if(!peq(x,y,51,102,153,255,2)) s=false;
      ok(s, "sub-rect (10,10,4x4) == (51,102,153,255)"); }

    // ==================== exhaustive per-API render coverage ====================
#define NOBLEND false, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD
    float red[4] = {1, 0, 0, 1};

    // --- Test 8: primitive topologies (VkPrimitiveTopology: TRIANGLE_LIST/FAN, LINE_LIST/STRIP, POINT_LIST) ---
    { const float tl[] = { -1,-1, 1,-1, -1,1,  -1,1, 1,-1, 1,1 };
      const float fan[] = { 0,0, -1,-1, 1,-1, 1,1, -1,1, -1,-1 };
      const float hln[] = { -1,0, 1,0 }; const float pt[] = { 0,0 };
      VkDeviceMemory m1,m2,m3,m4; VkBuffer b_tl=mkVbo(tl,sizeof(tl),&m1), b_fan=mkVbo(fan,sizeof(fan),&m2), b_ln=mkVbo(hln,sizeof(hln),&m3), b_pt=mkVbo(pt,sizeof(pt),&m4);
      VkPipeline p_tl=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_fan=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_FAN,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_ll=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_LINE_LIST,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_ls=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_LINE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_pt=mkPipe2(vs_pt,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_POINT_LIST,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      ok(p_tl&&p_fan&&p_ll&&p_ls&&p_pt,"topology pipelines created");
      frame(0,0,0,1,p_tl,pl_push,red,b_tl,6,full); ok(all_eq(255,0,0,255,1),"TRIANGLE_LIST fills quad");
      frame(0,0,0,1,p_fan,pl_push,red,b_fan,6,full); ok(all_eq(255,0,0,255,1),"TRIANGLE_FAN fills quad");
      frame(0,0,0,1,p_ll,pl_push,red,b_ln,2,full);
      { int mid=0; for(uint32_t x=0;x<W;x++) if(peq(x,H/2,255,0,0,255,2)||peq(x,H/2-1,255,0,0,255,2)) mid++;
        ok(mid>=(int)W-2,"LINE_LIST draws the middle row"); ok(peq(0,0,0,0,0,255,2),"LINE_LIST leaves top row clear"); }
      frame(0,0,0,1,p_ls,pl_push,red,b_ln,2,full);
      { int mid=0; for(uint32_t x=0;x<W;x++) if(peq(x,H/2,255,0,0,255,2)||peq(x,H/2-1,255,0,0,255,2)) mid++; ok(mid>=(int)W-2,"LINE_STRIP draws the middle row"); }
      frame(0,0,0,1,p_pt,pl_push,red,b_pt,1,full);
      { bool hit=false; for(uint32_t y=H/2-2;y<=H/2+2;y++) for(uint32_t x=W/2-2;x<=W/2+2;x++) if(peq(x,y,255,0,0,255,2)) hit=true; ok(hit,"POINT_LIST draws a pixel at the center"); }
      vkDestroyPipeline(dev,p_tl,nullptr); vkDestroyPipeline(dev,p_fan,nullptr); vkDestroyPipeline(dev,p_ll,nullptr); vkDestroyPipeline(dev,p_ls,nullptr); vkDestroyPipeline(dev,p_pt,nullptr);
      vkDestroyBuffer(dev,b_tl,nullptr); vkFreeMemory(dev,m1,nullptr); vkDestroyBuffer(dev,b_fan,nullptr); vkFreeMemory(dev,m2,nullptr);
      vkDestroyBuffer(dev,b_ln,nullptr); vkFreeMemory(dev,m3,nullptr); vkDestroyBuffer(dev,b_pt,nullptr); vkFreeMemory(dev,m4,nullptr); }

    // --- Test 9: blend factor + op matrix (VkBlendFactor/VkBlendOp, closed-form) ---
    { VkPipeline pb1=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c1[4]={0,0,1,1}; frame(0.5f,0.5f,0.5f,1,pb1,pl_push,c1,vbo,4,full); ok(all_eq(0,0,255,255,2),"blend ONE/ZERO: src replaces dst"); vkDestroyPipeline(dev,pb1,nullptr);
      VkPipeline pb2=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c2[4]={0,0,0.5f,1}; frame(0.5f,0,0,1,pb2,pl_push,c2,vbo,4,full); ok(all_eq(128,0,128,255,2),"blend ONE/ONE ADD: src+dst = (128,0,128)"); vkDestroyPipeline(dev,pb2,nullptr);
      VkPipeline pb3=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_ZERO,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ZERO,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c3[4]={0,1,0,1}; frame(0.2f,0,0,1,pb3,pl_push,c3,vbo,4,full); ok(all_eq(51,0,0,255,2),"blend ZERO/ONE: dst kept (51,0,0)"); vkDestroyPipeline(dev,pb3,nullptr);
      VkPipeline pb4=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_DST_COLOR,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_DST_COLOR,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c4[4]={0,0,1,1}; frame(0.5f,0.5f,0.5f,1,pb4,pl_push,c4,vbo,4,full); ok(all_eq(0,0,128,255,2),"blend DST_COLOR/ZERO: src*dst modulate (0,0,128)"); vkDestroyPipeline(dev,pb4,nullptr);
      VkPipeline pb5=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_MAX,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_MAX,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c5[4]={0.6f,0.2f,0.6f,1}; frame(0.2f,0.6f,0.2f,1,pb5,pl_push,c5,vbo,4,full); ok(all_eq(153,153,153,255,2),"blend op MAX: per-channel max"); vkDestroyPipeline(dev,pb5,nullptr);
      VkPipeline pb6=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,true,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_REVERSE_SUBTRACT,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_REVERSE_SUBTRACT,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c6[4]={0.25f,0,0,1}; frame(1,0,0,1,pb6,pl_push,c6,vbo,4,full); ok(all_eq(191,0,0,0,3),"blend op REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0"); vkDestroyPipeline(dev,pb6,nullptr); }

    // --- Test 10: depth-func matrix (VkCompareOp; Vulkan NDC z in [0,1]: quad z=0.5, clear depth 0.75) ---
    { const float dq[] = { -1,-1,0.5f, 1,-1,0.5f, -1,1,0.5f, 1,1,0.5f };
      VkDeviceMemory dm3; VkBuffer vbo3=mkVbo(dq,sizeof(dq),&dm3); float grn[4]={0,1,0,1};
      struct { VkCompareOp op; bool draws; const char* n; } dt[] = {
        {VK_COMPARE_OP_ALWAYS,true,"ALWAYS"},{VK_COMPARE_OP_NEVER,false,"NEVER"},{VK_COMPARE_OP_LESS,true,"LESS"},
        {VK_COMPARE_OP_LESS_OR_EQUAL,true,"LEQUAL"},{VK_COMPARE_OP_EQUAL,false,"EQUAL"},{VK_COMPARE_OP_GREATER,false,"GREATER"},
        {VK_COMPARE_OP_GREATER_OR_EQUAL,false,"GEQUAL"},{VK_COMPARE_OP_NOT_EQUAL,true,"NOTEQUAL"} };
      for (auto& d : dt) {
        VkPipeline pdp=mkPipe2(vs_p3,fs_s,pl_push,2,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,true,d.op,rp_d,0xF);
        frameD(0,0,0,1,0.75f,pdp,pl_push,grn,vbo3,4); ok(peq(W/2,H/2,0,255,0,255,2)==d.draws,d.n); vkDestroyPipeline(dev,pdp,nullptr); }
      vkDestroyBuffer(dev,vbo3,nullptr); vkFreeMemory(dev,dm3,nullptr); }

    // --- Test 11: face culling + winding (VkCullModeFlags / VkFrontFace) ---
    { VkPipeline pcn=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pcn,pl_push,red,vbo,4,full); ok(all_eq(255,0,0,255,1),"cull NONE: quad drawn"); vkDestroyPipeline(dev,pcn,nullptr);
      VkPipeline pcb=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_FRONT_AND_BACK,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pcb,pl_push,red,vbo,4,full); ok(all_eq(0,0,0,255,1),"cull FRONT_AND_BACK: nothing drawn"); vkDestroyPipeline(dev,pcb,nullptr);
      VkPipeline pccw=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_BACK_BIT,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pccw,pl_push,red,vbo,4,full); bool ccw=peq(W/2,H/2,255,0,0,255,2); vkDestroyPipeline(dev,pccw,nullptr);
      VkPipeline pccw2=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_BACK_BIT,VK_FRONT_FACE_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pccw2,pl_push,red,vbo,4,full); bool cw=peq(W/2,H/2,255,0,0,255,2); vkDestroyPipeline(dev,pccw2,nullptr);
      ok(ccw!=cw,"cull BACK: CCW vs CW winding flips visibility"); }

    // --- Test 12: color write mask (VkColorComponentFlags) ---
    { float white[4]={1,1,1,1};
      VkPipeline pmr=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,VK_COLOR_COMPONENT_R_BIT);
      frame(0,0,0,1,pmr,pl_push,white,vbo,4,full); ok(all_eq(255,0,0,255,1),"colorWriteMask R only: white -> (255,0,0,255)"); vkDestroyPipeline(dev,pmr,nullptr);
      VkPipeline pma=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pma,pl_push,white,vbo,4,full); ok(all_eq(255,255,255,255,1),"colorWriteMask RGBA: white -> (255,255,255,255)"); vkDestroyPipeline(dev,pma,nullptr); }

    // --- Test 13: format + device property queries ---
    { VkFormatProperties fp; vkGetPhysicalDeviceFormatProperties(pd, VK_FORMAT_R8G8B8A8_UNORM, &fp);
      ok((fp.optimalTilingFeatures & VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT) != 0, "R8G8B8A8_UNORM optimal-tiling COLOR_ATTACHMENT");
      VkFormatProperties fpd; vkGetPhysicalDeviceFormatProperties(pd, VK_FORMAT_D32_SFLOAT, &fpd);
      ok((fpd.optimalTilingFeatures & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0, "D32_SFLOAT optimal-tiling DEPTH_STENCIL_ATTACHMENT");
      VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd, &props);
      ok(VK_API_VERSION_MAJOR(props.apiVersion) >= 1, "device apiVersion major >= 1");
      ok(props.limits.maxImageDimension2D >= W, "limits.maxImageDimension2D >= 64"); }

    // --- Test 14: 2x2 texture upload + NEAREST sampling (combined image sampler + descriptor set) ---
    { VkDescriptorSetLayoutBinding dslb{}; dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
      VkDescriptorSetLayoutCreateInfo dslci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO}; dslci.bindingCount=1; dslci.pBindings=&dslb;
      VkDescriptorSetLayout dsl; VKOK(vkCreateDescriptorSetLayout(dev,&dslci,nullptr,&dsl),"descriptor set layout");
      VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO}; plci.setLayoutCount=1; plci.pSetLayouts=&dsl;
      VkPipelineLayout pl_tex; vkCreatePipelineLayout(dev,&plci,nullptr,&pl_tex);
      VkImageCreateInfo tii{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO}; tii.imageType=VK_IMAGE_TYPE_2D; tii.format=VK_FORMAT_R8G8B8A8_UNORM;
      tii.extent={2,2,1}; tii.mipLevels=1; tii.arrayLayers=1; tii.samples=VK_SAMPLE_COUNT_1_BIT; tii.tiling=VK_IMAGE_TILING_OPTIMAL;
      tii.usage=VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT; tii.initialLayout=VK_IMAGE_LAYOUT_UNDEFINED;
      VkImage timg; VKOK(vkCreateImage(dev,&tii,nullptr,&timg),"texture image");
      VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,timg,&tmr);
      VkMemoryAllocateInfo tai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; tai.allocationSize=tmr.size; tai.memoryTypeIndex=memtype(tmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
      VkDeviceMemory tmem; vkAllocateMemory(dev,&tai,nullptr,&tmem); vkBindImageMemory(dev,timg,tmem,0);
      unsigned char texels[16]={255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,255,255};
      VkBufferCreateInfo sbi{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO}; sbi.size=16; sbi.usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
      VkBuffer sbuf; vkCreateBuffer(dev,&sbi,nullptr,&sbuf);
      VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,sbuf,&smr);
      VkMemoryAllocateInfo sai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO}; sai.allocationSize=smr.size; sai.memoryTypeIndex=memtype(smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
      VkDeviceMemory smem; vkAllocateMemory(dev,&sai,nullptr,&smem); vkBindBufferMemory(dev,sbuf,smem,0);
      void* sp; vkMapMemory(dev,smem,0,16,0,&sp); memcpy(sp,texels,16); vkUnmapMemory(dev,smem);
      { VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
        VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
        VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
        VkImageMemoryBarrier b1{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER}; b1.oldLayout=VK_IMAGE_LAYOUT_UNDEFINED; b1.newLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        b1.srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED; b1.image=timg; b1.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
        b1.srcAccessMask=0; b1.dstAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;
        vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,nullptr,0,nullptr,1,&b1);
        VkBufferImageCopy cp{}; cp.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; cp.imageExtent={2,2,1};
        vkCmdCopyBufferToImage(cmd,sbuf,timg,VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,1,&cp);
        VkImageMemoryBarrier b2=b1; b2.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        b2.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
        vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,0,0,nullptr,0,nullptr,1,&b2);
        vkEndCommandBuffer(cmd);
        VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
        VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fnc; vkCreateFence(dev,&fi,nullptr,&fnc);
        vkQueueSubmit(q,1,&si,fnc); vkWaitForFences(dev,1,&fnc,VK_TRUE,UINT64_MAX); vkDestroyFence(dev,fnc,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd); }
      VkImageViewCreateInfo tvi{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO}; tvi.image=timg; tvi.viewType=VK_IMAGE_VIEW_TYPE_2D; tvi.format=VK_FORMAT_R8G8B8A8_UNORM; tvi.subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1};
      VkImageView tview; vkCreateImageView(dev,&tvi,nullptr,&tview);
      VkSamplerCreateInfo smci{VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO}; smci.magFilter=VK_FILTER_NEAREST; smci.minFilter=VK_FILTER_NEAREST; smci.addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; smci.addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE; smci.addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
      VkSampler samp; vkCreateSampler(dev,&smci,nullptr,&samp);
      VkDescriptorPoolSize dps{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,1};
      VkDescriptorPoolCreateInfo dpci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO}; dpci.maxSets=1; dpci.poolSizeCount=1; dpci.pPoolSizes=&dps;
      VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,nullptr,&dpool);
      VkDescriptorSetAllocateInfo dsai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO}; dsai.descriptorPool=dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&dsl;
      VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
      VkDescriptorImageInfo dii2{samp,tview,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
      VkWriteDescriptorSet wds{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET}; wds.dstSet=dset; wds.dstBinding=0; wds.descriptorCount=1; wds.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds.pImageInfo=&dii2;
      vkUpdateDescriptorSets(dev,1,&wds,0,nullptr);
      VkPipeline pt=mkPipe2(vs_tx,fs_tx,pl_tex,3,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,VK_FRONT_FACE_COUNTER_CLOCKWISE,false,VK_COMPARE_OP_ALWAYS,rp,0xF);
      ok(dsl&&pl_tex&&pt&&samp,"texture pipeline + descriptor created");
      const float tq[]={ -1,-1,0,0,  1,-1,1,0,  -1,1,0,1,  1,1,1,1 };
      VkDeviceMemory tqm; VkBuffer tvbo=mkVbo(tq,sizeof(tq),&tqm);
      { VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO}; cai.commandPool=pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
        VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
        VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO}; bi.flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT; vkBeginCommandBuffer(cmd,&bi);
        VkClearValue cv; cv.color={{0,0,0,1}};
        VkRenderPassBeginInfo rpb{VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO}; rpb.renderPass=rp; rpb.framebuffer=fb; rpb.renderArea={{0,0},{W,H}}; rpb.clearValueCount=1; rpb.pClearValues=&cv;
        vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
        vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pt);
        VkRect2D scr{{0,0},{W,H}}; vkCmdSetScissor(cmd,0,1,&scr);
        vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl_tex,0,1,&dset,0,nullptr);
        VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&tvbo,&off);
        vkCmdDraw(cmd,4,1,0,0);
        vkCmdEndRenderPass(cmd);
        VkBufferImageCopy region{}; region.imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}; region.imageExtent={W,H,1};
        vkCmdCopyImageToBuffer(cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
        vkEndCommandBuffer(cmd);
        VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO}; si.commandBufferCount=1; si.pCommandBuffers=&cmd;
        VkFenceCreateInfo fi{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO}; VkFence fnc; vkCreateFence(dev,&fi,nullptr,&fnc);
        vkQueueSubmit(q,1,&si,fnc); vkWaitForFences(dev,1,&fnc,VK_TRUE,UINT64_MAX); vkDestroyFence(dev,fnc,nullptr); vkFreeCommandBuffers(dev,pool,1,&cmd);
        memcpy(px_,rmap,sizeof(px_)); }
      ok(peq(W/4,H/4,255,0,0,255,2),"texture NEAREST top-left red");
      ok(peq(3*W/4,H/4,0,255,0,255,2),"texture NEAREST top-right green");
      ok(peq(W/4,3*H/4,0,0,255,255,2),"texture NEAREST bottom-left blue");
      ok(peq(3*W/4,3*H/4,255,255,255,255,2),"texture NEAREST bottom-right white");
      vkDestroyPipeline(dev,pt,nullptr); vkDestroyBuffer(dev,tvbo,nullptr); vkFreeMemory(dev,tqm,nullptr);
      vkDestroySampler(dev,samp,nullptr); vkDestroyImageView(dev,tview,nullptr); vkDestroyImage(dev,timg,nullptr); vkFreeMemory(dev,tmem,nullptr);
      vkDestroyBuffer(dev,sbuf,nullptr); vkFreeMemory(dev,smem,nullptr);
      vkDestroyDescriptorPool(dev,dpool,nullptr); vkDestroyDescriptorSetLayout(dev,dsl,nullptr); vkDestroyPipelineLayout(dev,pl_tex,nullptr); }
#undef NOBLEND

    // --- Negative control ---
    { float col[4] = {1, 0, 0, 1};
      frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      ok(!all_eq(0, 255, 0, 255, 2), "negative control: red buffer is NOT green");
      ok(!peq(0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black"); }

    vkDeviceWaitIdle(dev);
    int EXPECTED = 68, TOTAL = PASS + FAIL;
    printf("vulkan-render-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("VULKAN_RENDER_CPP_FULL_API OK %d\n", PASS); return 0; }
    return 1;
}
