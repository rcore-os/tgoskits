/* vulkan_render_c_full_api.c - Vulkan RENDER carpet on Mesa lavapipe (software Vulkan on the CPU, no
 * GPU/window/surface/swapchain), C binding of the same offscreen render pipeline as the C++ cell.
 * Builds an offscreen render pass into an R8G8B8A8_UNORM color image, draws through a real graphics
 * pipeline (SPIR-V vertex+fragment shaders), copies the image to a host-visible buffer with
 * vkCmdCopyImageToBuffer, maps it, and checks every pixel against a closed-form reference for:
 * render-pass clear, a solid quad (push-constant color), a per-vertex axis-aligned linear gradient (a
 * triangle-strip quad interpolates per-triangle, so only an axis-aligned gradient matches a full-quad
 * closed form), a gl_FragCoord checkerboard, a dynamic scissor, alpha blending (SRC_ALPHA/
 * ONE_MINUS_SRC_ALPHA over all channels incl alpha), a sub-rectangle readback. Exhaustive per-API
 * coverage builds a pipeline per state: primitive topologies (VkPrimitiveTopology TRIANGLE_LIST/
 * TRIANGLE_FAN/LINE_LIST/LINE_STRIP/POINT_LIST), a blend factor+op matrix (VkBlendFactor ONE/ZERO,
 * ONE/ONE, ZERO/ONE, DST_COLOR; VkBlendOp ADD/MAX/REVERSE_SUBTRACT), the full depth-func matrix (all 8
 * VkCompareOp against a D32_SFLOAT attachment; Vulkan NDC z in [0,1] so a z=0.5 quad vs clear-depth
 * 0.75), face culling + winding (VkCullModeFlags NONE/FRONT_AND_BACK/BACK x VkFrontFace CCW-vs-CW), a
 * color write mask (VkColorComponentFlags), format+device property queries, and a 2x2 texture upload +
 * NEAREST sampling through a combined image sampler + descriptor set, closing with a negative control.
 * Prints "VULKAN_RENDER_C_FULL_API OK <n>" only when every assertion passes and count == EXPECTED. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>
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
static void ok(int c, const char* d) { if (c) PASS++; else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); } }
#define VKOK(e, d) ok((e) == VK_SUCCESS, d)

enum { W = 64, H = 64 };
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
    VkShaderModuleCreateInfo ci = { .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = bytes, .pCode = code };
    VkShaderModule m; vkCreateShaderModule(dev, &ci, NULL, &m); return m;
}
static unsigned char P(int x, int y, int c) { return px_[(y * W + x) * 4 + c]; }
static int peq(int x, int y, int r, int g, int b, int a, int tol) {
    return abs((int)P(x,y,0)-r)<=tol && abs((int)P(x,y,1)-g)<=tol && abs((int)P(x,y,2)-b)<=tol && abs((int)P(x,y,3)-a)<=tol;
}
static int all_eq(int r, int g, int b, int a, int tol) {
    for (int y=0;y<H;y++) for (int x=0;x<W;x++) if(!peq(x,y,r,g,b,a,tol)) return 0; return 1;
}
static VkPipelineLayout mkLayout(int pushConst) {
    VkPushConstantRange pcr = { VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16 };
    VkPipelineLayoutCreateInfo li = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO };
    if (pushConst) { li.pushConstantRangeCount = 1; li.pPushConstantRanges = &pcr; }
    VkPipelineLayout pl; vkCreatePipelineLayout(dev, &li, NULL, &pl); return pl;
}
static VkPipeline mkPipe(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl, int withColorAttr, int blend) {
    VkPipelineShaderStageCreateInfo st[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" },
    };
    VkVertexInputBindingDescription bind = { 0, (uint32_t)(withColorAttr ? 24 : 8), VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attr[2] = {
        { 0, 0, VK_FORMAT_R32G32_SFLOAT, 0 }, { 1, 0, VK_FORMAT_R32G32B32A32_SFLOAT, 8 } };
    VkPipelineVertexInputStateCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &bind,
        .vertexAttributeDescriptionCount = (uint32_t)(withColorAttr ? 2 : 1), .pVertexAttributeDescriptions = attr };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP };
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
        .stageCount = 2, .pStages = st, .pVertexInputState = &vi, .pInputAssemblyState = &ia,
        .pViewportState = &vps, .pRasterizationState = &rs, .pMultisampleState = &ms, .pColorBlendState = &cb,
        .pDynamicState = &ds, .layout = pl, .renderPass = rp, .subpass = 0 };
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, NULL, &p); return p;
}
/* Rich pipeline builder for exhaustive per-API coverage: vertex layout (0=pos2, 1=pos2+color,
 * 2=pos3, 3=pos2+uv), topology, full blend factor+op config, cull mode + winding, depth test +
 * compare op, the render pass to use, and a color write mask. */
static VkPipeline mkPipe2(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl, int vlayout,
                          VkPrimitiveTopology topo, int blend,
                          VkBlendFactor sC, VkBlendFactor dC, VkBlendOp oC,
                          VkBlendFactor sA, VkBlendFactor dA, VkBlendOp oA,
                          VkCullModeFlags cull, VkFrontFace front,
                          int depthTest, VkCompareOp depthOp, VkRenderPass rpUse, uint32_t cwmask) {
    VkPipelineShaderStageCreateInfo st[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" },
    };
    uint32_t stride = vlayout == 0 ? 8 : vlayout == 1 ? 24 : vlayout == 2 ? 12 : 16;
    VkVertexInputBindingDescription bind = { 0, stride, VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attr[2]; uint32_t nattr = 1;
    attr[0] = vlayout == 2 ? (VkVertexInputAttributeDescription){ 0, 0, VK_FORMAT_R32G32B32_SFLOAT, 0 }
                           : (VkVertexInputAttributeDescription){ 0, 0, VK_FORMAT_R32G32_SFLOAT, 0 };
    if (vlayout == 1) { attr[1] = (VkVertexInputAttributeDescription){ 1, 0, VK_FORMAT_R32G32B32A32_SFLOAT, 8 }; nattr = 2; }
    else if (vlayout == 3) { attr[1] = (VkVertexInputAttributeDescription){ 1, 0, VK_FORMAT_R32G32_SFLOAT, 8 }; nattr = 2; }
    VkPipelineVertexInputStateCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &bind,
        .vertexAttributeDescriptionCount = nattr, .pVertexAttributeDescriptions = attr };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = topo };
    VkViewport vp = { 0, 0, (float)W, (float)H, 0, 1 }; VkRect2D sc = { {0, 0}, {W, H} };
    VkPipelineViewportStateCreateInfo vps = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        .viewportCount = 1, .pViewports = &vp, .scissorCount = 1, .pScissors = &sc };
    VkDynamicState dyn = VK_DYNAMIC_STATE_SCISSOR;
    VkPipelineDynamicStateCreateInfo ds = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
        .dynamicStateCount = 1, .pDynamicStates = &dyn };
    VkPipelineRasterizationStateCreateInfo rs = { .sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = cull, .frontFace = front, .lineWidth = 1.0f };
    VkPipelineMultisampleStateCreateInfo ms = { .sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT };
    VkPipelineDepthStencilStateCreateInfo dss = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        .depthTestEnable = depthTest ? VK_TRUE : VK_FALSE, .depthWriteEnable = depthTest ? VK_TRUE : VK_FALSE,
        .depthCompareOp = depthOp, .minDepthBounds = 0.0f, .maxDepthBounds = 1.0f };
    VkPipelineColorBlendAttachmentState cba = { .blendEnable = blend ? VK_TRUE : VK_FALSE,
        .srcColorBlendFactor = sC, .dstColorBlendFactor = dC, .colorBlendOp = oC,
        .srcAlphaBlendFactor = sA, .dstAlphaBlendFactor = dA, .alphaBlendOp = oA, .colorWriteMask = cwmask };
    VkPipelineColorBlendStateCreateInfo cb = { .sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        .attachmentCount = 1, .pAttachments = &cba };
    VkGraphicsPipelineCreateInfo gp = { .sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        .stageCount = 2, .pStages = st, .pVertexInputState = &vi, .pInputAssemblyState = &ia,
        .pViewportState = &vps, .pRasterizationState = &rs, .pMultisampleState = &ms, .pColorBlendState = &cb,
        .pDepthStencilState = &dss, .pDynamicState = &ds, .layout = pl, .renderPass = rpUse, .subpass = 0 };
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, NULL, &p); return p;
}
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
static void frame(float cr, float cg, float cb, float ca, VkPipeline pipe, VkPipelineLayout pl,
                  const float* pushColor, VkBuffer vbo, uint32_t verts, VkRect2D scissor) {
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv; cv.color.float32[0] = cr; cv.color.float32[1] = cg; cv.color.float32[2] = cb; cv.color.float32[3] = ca;
    VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp, .framebuffer = fb,
        .renderArea = { {0, 0}, {W, H} }, .clearValueCount = 1, .pClearValues = &cv };
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    if (pipe) {
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
        vkCmdSetScissor(cmd, 0, 1, &scissor);
        if (pushColor) vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, pushColor);
        if (vbo) { VkDeviceSize off = 0; vkCmdBindVertexBuffers(cmd, 0, 1, &vbo, &off); }
        vkCmdDraw(cmd, verts, 1, 0, 0);
    }
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { W, H, 1 } };
    vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence; vkCreateFence(dev, &fi, NULL, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    memcpy(px_, rmap, sizeof(px_));
}
/* depth-enabled frame: clears color to (cr,cg,cb,ca) and depth to depthClear, uses the depth render
 * pass/framebuffer, draws the vec3 quad through pipe, copies the color image to px_. */
static void frameD(float cr, float cg, float cb, float ca, float depthClear, VkPipeline pipe,
                   VkPipelineLayout pl, const float* pushColor, VkBuffer vbo, uint32_t verts) {
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv[2]; cv[0].color.float32[0] = cr; cv[0].color.float32[1] = cg; cv[0].color.float32[2] = cb; cv[0].color.float32[3] = ca;
    cv[1].depthStencil.depth = depthClear; cv[1].depthStencil.stencil = 0;
    VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp_d, .framebuffer = fb_d,
        .renderArea = { {0, 0}, {W, H} }, .clearValueCount = 2, .pClearValues = cv };
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    VkRect2D scissor = { {0, 0}, {W, H} };
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
    vkCmdSetScissor(cmd, 0, 1, &scissor);
    if (pushColor) vkCmdPushConstants(cmd, pl, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, pushColor);
    if (vbo) { VkDeviceSize off = 0; vkCmdBindVertexBuffers(cmd, 0, 1, &vbo, &off); }
    vkCmdDraw(cmd, verts, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { W, H, 1 } };
    vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence; vkCreateFence(dev, &fi, NULL, &fence);
    vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, fence, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    memcpy(px_, rmap, sizeof(px_));
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

    /* depth resources for the depth-func matrix: D32_SFLOAT image + a color+depth render pass sharing cimg */
    VkImageCreateInfo dii = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D,
        .format = VK_FORMAT_D32_SFLOAT, .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
    VKOK(vkCreateImage(dev, &dii, NULL, &dimg), "vkCreateImage depth");
    VkMemoryRequirements dmr; vkGetImageMemoryRequirements(dev, dimg, &dmr);
    VkMemoryAllocateInfo daii = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = dmr.size,
        .memoryTypeIndex = memtype(dmr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    vkAllocateMemory(dev, &daii, NULL, &dmem); vkBindImageMemory(dev, dimg, dmem, 0);
    VkImageViewCreateInfo dvi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = dimg, .viewType = VK_IMAGE_VIEW_TYPE_2D,
        .format = VK_FORMAT_D32_SFLOAT, .subresourceRange = { VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1 } };
    VKOK(vkCreateImageView(dev, &dvi, NULL, &dview), "vkCreateImageView depth");
    VkAttachmentDescription datt[2] = {
        { .format = VK_FORMAT_R8G8B8A8_UNORM, .samples = VK_SAMPLE_COUNT_1_BIT,
          .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
          .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE, .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
          .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED, .finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL },
        { .format = VK_FORMAT_D32_SFLOAT, .samples = VK_SAMPLE_COUNT_1_BIT,
          .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
          .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE, .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
          .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED, .finalLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL } };
    VkAttachmentReference dcref = { 0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
    VkAttachmentReference ddref = { 1, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    VkSubpassDescription dsp = { .pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &dcref, .pDepthStencilAttachment = &ddref };
    VkRenderPassCreateInfo drpi = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = datt, .subpassCount = 1, .pSubpasses = &dsp };
    VKOK(vkCreateRenderPass(dev, &drpi, NULL, &rp_d), "vkCreateRenderPass depth");
    VkImageView dfbv[2] = { cview, dview };
    VkFramebufferCreateInfo dfbi = { .sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = rp_d, .attachmentCount = 2, .pAttachments = dfbv, .width = W, .height = H, .layers = 1 };
    VKOK(vkCreateFramebuffer(dev, &dfbi, NULL, &fb_d), "vkCreateFramebuffer depth");

    VkBufferCreateInfo rbi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = W * H * 4, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT };
    vkCreateBuffer(dev, &rbi, NULL, &rbuf);
    VkMemoryRequirements rmr; vkGetBufferMemoryRequirements(dev, rbuf, &rmr);
    VkMemoryAllocateInfo rai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = rmr.size,
        .memoryTypeIndex = memtype(rmr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &rai, NULL, &rmem); vkBindBufferMemory(dev, rbuf, rmem, 0);
    vkMapMemory(dev, rmem, 0, W * H * 4, 0, (void**)&rmap);

    VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfam, .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT };
    VKOK(vkCreateCommandPool(dev, &pci, NULL, &pool), "vkCreateCommandPool");

    VkShaderModule vs_s = shmod(solid_vert, sizeof(solid_vert)), fs_s = shmod(solid_frag, sizeof(solid_frag));
    VkShaderModule vs_g = shmod(grad_vert, sizeof(grad_vert)), fs_g = shmod(grad_frag, sizeof(grad_frag));
    VkShaderModule fs_c = shmod(check_frag, sizeof(check_frag));
    VkShaderModule vs_pt = shmod(point_vert, sizeof(point_vert)), vs_p3 = shmod(pos3_vert, sizeof(pos3_vert));
    VkShaderModule vs_tx = shmod(tex_vert, sizeof(tex_vert)), fs_tx = shmod(tex_frag, sizeof(tex_frag));
    VkPipelineLayout pl_push = mkLayout(1), pl_none = mkLayout(0);
    VkPipeline pipe_solid = mkPipe(vs_s, fs_s, pl_push, 0, 0);
    VkPipeline pipe_blend = mkPipe(vs_s, fs_s, pl_push, 0, 1);
    VkPipeline pipe_grad = mkPipe(vs_g, fs_g, pl_none, 1, 0);
    VkPipeline pipe_check = mkPipe(vs_s, fs_c, pl_none, 0, 0);
    ok(pipe_solid && pipe_grad && pipe_check && pipe_blend, "graphics pipelines created");

    const float quad[] = { -1,-1, 1,-1, -1,1, 1,1 };
    const float gquad[] = { -1,-1, 1,0,0,1,  1,-1, 0,0,1,1,  -1,1, 1,0,0,1,  1,1, 0,0,1,1 };
    VkDeviceMemory qm, gm; VkBuffer vbo = mkVbo(quad, sizeof(quad), &qm); VkBuffer gvbo = mkVbo(gquad, sizeof(gquad), &gm);
    VkRect2D full = { {0, 0}, {W, H} };

    frame(0.0f, 0.25f, 0.5f, 1.0f, VK_NULL_HANDLE, pl_none, NULL, VK_NULL_HANDLE, 0, full);
    ok(all_eq(0, 64, 128, 255, 2), "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)");
    ok(peq(0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");

    { float col[4] = {1, 0, 0, 1}; frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      ok(all_eq(255, 0, 0, 255, 1), "solid red quad fills every pixel"); }

    frame(0, 0, 0, 1, pipe_grad, pl_none, NULL, gvbo, 4, full);
    { int bad = 0;
      for (int y=0;y<H;y++) for (int x=0;x<W;x++){ float u=(x+0.5f)/W;
        int r=(int)lroundf((1.f-u)*255.f), b=(int)lroundf(u*255.f); if(!peq(x,y,r,0,b,255,4)) bad++; }
      ok(bad == 0, "gradient matches horizontal-linear closed-form for all pixels");
      ok(peq(0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red");
      ok(peq(W-1, H-1, 0, 0, 255, 255, 8), "gradient right edge ~ blue");
      ok(peq(W/2, H/2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)"); }

    frame(0, 0, 0, 1, pipe_check, pl_none, NULL, vbo, 4, full);
    { int bad = 0;
      for (int y=0;y<H;y++) for (int x=0;x<W;x++){ int e=(((x>>3)+(y>>3))&1)==0; int w=e?255:0; if(!peq(x,y,w,w,w,255,1)) bad++; }
      ok(bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels");
      ok(peq(0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white");
      ok(peq(8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black"); }

    { float col[4] = {0, 1, 0, 1}; VkRect2D box = { {16, 16}, {32, 32} };
      frame(1, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, box);
      ok(peq(32, 32, 0, 255, 0, 255, 1), "scissor: inside box green");
      ok(peq(2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)");
      ok(peq(50, 50, 255, 0, 0, 255, 1), "scissor: past box red"); }

    { float col[4] = {0, 0, 1, 0.5f}; frame(1, 0, 0, 1, pipe_blend, pl_push, col, vbo, 4, full);
      ok(all_eq(128, 0, 128, 191, 3), "alpha blend 0.5*blue over red -> rgb(128,0,128) a191"); }

    { float col[4] = {0.2f, 0.4f, 0.6f, 1.0f}; frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      int s = 1; for (int y=10;y<14;y++) for (int x=10;x<14;x++) if(!peq(x,y,51,102,153,255,2)) s=0;
      ok(s, "sub-rect (10,10,4x4) == (51,102,153,255)"); }

    /* ==================== exhaustive per-API render coverage ==================== */
#define NOBLEND 0, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD
#define CCWF VK_FRONT_FACE_COUNTER_CLOCKWISE
    float red[4] = {1, 0, 0, 1};

    /* Test 8: primitive topologies (VkPrimitiveTopology) */
    { const float tl[] = { -1,-1, 1,-1, -1,1,  -1,1, 1,-1, 1,1 };
      const float fan[] = { 0,0, -1,-1, 1,-1, 1,1, -1,1, -1,-1 };
      const float hln[] = { -1,0, 1,0 }; const float pt[] = { 0,0 };
      VkDeviceMemory m1,m2,m3,m4; VkBuffer b_tl=mkVbo(tl,sizeof(tl),&m1), b_fan=mkVbo(fan,sizeof(fan),&m2), b_ln=mkVbo(hln,sizeof(hln),&m3), b_pt=mkVbo(pt,sizeof(pt),&m4);
      VkPipeline p_tl=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_fan=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_FAN,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_ll=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_LINE_LIST,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_ls=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_LINE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      VkPipeline p_pt=mkPipe2(vs_pt,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_POINT_LIST,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      ok(p_tl&&p_fan&&p_ll&&p_ls&&p_pt,"topology pipelines created");
      frame(0,0,0,1,p_tl,pl_push,red,b_tl,6,full); ok(all_eq(255,0,0,255,1),"TRIANGLE_LIST fills quad");
      frame(0,0,0,1,p_fan,pl_push,red,b_fan,6,full); ok(all_eq(255,0,0,255,1),"TRIANGLE_FAN fills quad");
      frame(0,0,0,1,p_ll,pl_push,red,b_ln,2,full);
      { int mid=0; for(int x=0;x<W;x++) if(peq(x,H/2,255,0,0,255,2)||peq(x,H/2-1,255,0,0,255,2)) mid++;
        ok(mid>=W-2,"LINE_LIST draws the middle row"); ok(peq(0,0,0,0,0,255,2),"LINE_LIST leaves top row clear"); }
      frame(0,0,0,1,p_ls,pl_push,red,b_ln,2,full);
      { int mid=0; for(int x=0;x<W;x++) if(peq(x,H/2,255,0,0,255,2)||peq(x,H/2-1,255,0,0,255,2)) mid++; ok(mid>=W-2,"LINE_STRIP draws the middle row"); }
      frame(0,0,0,1,p_pt,pl_push,red,b_pt,1,full);
      { int hit=0; for(int y=H/2-2;y<=H/2+2;y++) for(int x=W/2-2;x<=W/2+2;x++) if(peq(x,y,255,0,0,255,2)) hit=1; ok(hit,"POINT_LIST draws a pixel at the center"); }
      vkDestroyPipeline(dev,p_tl,NULL); vkDestroyPipeline(dev,p_fan,NULL); vkDestroyPipeline(dev,p_ll,NULL); vkDestroyPipeline(dev,p_ls,NULL); vkDestroyPipeline(dev,p_pt,NULL);
      vkDestroyBuffer(dev,b_tl,NULL); vkFreeMemory(dev,m1,NULL); vkDestroyBuffer(dev,b_fan,NULL); vkFreeMemory(dev,m2,NULL);
      vkDestroyBuffer(dev,b_ln,NULL); vkFreeMemory(dev,m3,NULL); vkDestroyBuffer(dev,b_pt,NULL); vkFreeMemory(dev,m4,NULL); }

    /* Test 9: blend factor + op matrix (VkBlendFactor/VkBlendOp, closed-form) */
    { VkPipeline pb1=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c1[4]={0,0,1,1}; frame(0.5f,0.5f,0.5f,1,pb1,pl_push,c1,vbo,4,full); ok(all_eq(0,0,255,255,2),"blend ONE/ZERO: src replaces dst"); vkDestroyPipeline(dev,pb1,NULL);
      VkPipeline pb2=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c2[4]={0,0,0.5f,1}; frame(0.5f,0,0,1,pb2,pl_push,c2,vbo,4,full); ok(all_eq(128,0,128,255,2),"blend ONE/ONE ADD: src+dst = (128,0,128)"); vkDestroyPipeline(dev,pb2,NULL);
      VkPipeline pb3=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_ZERO,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_ZERO,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c3[4]={0,1,0,1}; frame(0.2f,0,0,1,pb3,pl_push,c3,vbo,4,full); ok(all_eq(51,0,0,255,2),"blend ZERO/ONE: dst kept (51,0,0)"); vkDestroyPipeline(dev,pb3,NULL);
      VkPipeline pb4=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_DST_COLOR,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_BLEND_FACTOR_DST_COLOR,VK_BLEND_FACTOR_ZERO,VK_BLEND_OP_ADD,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c4[4]={0,0,1,1}; frame(0.5f,0.5f,0.5f,1,pb4,pl_push,c4,vbo,4,full); ok(all_eq(0,0,128,255,2),"blend DST_COLOR/ZERO: src*dst modulate (0,0,128)"); vkDestroyPipeline(dev,pb4,NULL);
      VkPipeline pb5=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_MAX,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_MAX,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c5[4]={0.6f,0.2f,0.6f,1}; frame(0.2f,0.6f,0.2f,1,pb5,pl_push,c5,vbo,4,full); ok(all_eq(153,153,153,255,2),"blend op MAX: per-channel max"); vkDestroyPipeline(dev,pb5,NULL);
      VkPipeline pb6=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,1,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_REVERSE_SUBTRACT,VK_BLEND_FACTOR_ONE,VK_BLEND_FACTOR_ONE,VK_BLEND_OP_REVERSE_SUBTRACT,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      float c6[4]={0.25f,0,0,1}; frame(1,0,0,1,pb6,pl_push,c6,vbo,4,full); ok(all_eq(191,0,0,0,3),"blend op REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0"); vkDestroyPipeline(dev,pb6,NULL); }

    /* Test 10: depth-func matrix (VkCompareOp; Vulkan NDC z in [0,1]: quad z=0.5, clear depth 0.75) */
    { const float dq[] = { -1,-1,0.5f, 1,-1,0.5f, -1,1,0.5f, 1,1,0.5f };
      VkDeviceMemory dm3; VkBuffer vbo3=mkVbo(dq,sizeof(dq),&dm3); float grn[4]={0,1,0,1};
      struct { VkCompareOp op; int draws; const char* n; } dt[] = {
        {VK_COMPARE_OP_ALWAYS,1,"ALWAYS"},{VK_COMPARE_OP_NEVER,0,"NEVER"},{VK_COMPARE_OP_LESS,1,"LESS"},
        {VK_COMPARE_OP_LESS_OR_EQUAL,1,"LEQUAL"},{VK_COMPARE_OP_EQUAL,0,"EQUAL"},{VK_COMPARE_OP_GREATER,0,"GREATER"},
        {VK_COMPARE_OP_GREATER_OR_EQUAL,0,"GEQUAL"},{VK_COMPARE_OP_NOT_EQUAL,1,"NOTEQUAL"} };
      for (int i=0;i<8;i++) {
        VkPipeline pdp=mkPipe2(vs_p3,fs_s,pl_push,2,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,1,dt[i].op,rp_d,0xF);
        frameD(0,0,0,1,0.75f,pdp,pl_push,grn,vbo3,4); ok(peq(W/2,H/2,0,255,0,255,2)==dt[i].draws,dt[i].n); vkDestroyPipeline(dev,pdp,NULL); }
      vkDestroyBuffer(dev,vbo3,NULL); vkFreeMemory(dev,dm3,NULL); }

    /* Test 11: face culling + winding (VkCullModeFlags / VkFrontFace) */
    { VkPipeline pcn=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pcn,pl_push,red,vbo,4,full); ok(all_eq(255,0,0,255,1),"cull NONE: quad drawn"); vkDestroyPipeline(dev,pcn,NULL);
      VkPipeline pcb=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_FRONT_AND_BACK,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pcb,pl_push,red,vbo,4,full); ok(all_eq(0,0,0,255,1),"cull FRONT_AND_BACK: nothing drawn"); vkDestroyPipeline(dev,pcb,NULL);
      VkPipeline pc1=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_BACK_BIT,VK_FRONT_FACE_COUNTER_CLOCKWISE,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pc1,pl_push,red,vbo,4,full); int ccw=peq(W/2,H/2,255,0,0,255,2); vkDestroyPipeline(dev,pc1,NULL);
      VkPipeline pc2=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_BACK_BIT,VK_FRONT_FACE_CLOCKWISE,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pc2,pl_push,red,vbo,4,full); int cw=peq(W/2,H/2,255,0,0,255,2); vkDestroyPipeline(dev,pc2,NULL);
      ok(ccw!=cw,"cull BACK: CCW vs CW winding flips visibility"); }

    /* Test 12: color write mask (VkColorComponentFlags) */
    { float white[4]={1,1,1,1};
      VkPipeline pmr=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,VK_COLOR_COMPONENT_R_BIT);
      frame(0,0,0,1,pmr,pl_push,white,vbo,4,full); ok(all_eq(255,0,0,255,1),"colorWriteMask R only: white -> (255,0,0,255)"); vkDestroyPipeline(dev,pmr,NULL);
      VkPipeline pma=mkPipe2(vs_s,fs_s,pl_push,0,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      frame(0,0,0,1,pma,pl_push,white,vbo,4,full); ok(all_eq(255,255,255,255,1),"colorWriteMask RGBA: white -> (255,255,255,255)"); vkDestroyPipeline(dev,pma,NULL); }

    /* Test 13: format + device property queries */
    { VkFormatProperties fp; vkGetPhysicalDeviceFormatProperties(pd, VK_FORMAT_R8G8B8A8_UNORM, &fp);
      ok((fp.optimalTilingFeatures & VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT) != 0, "R8G8B8A8_UNORM optimal-tiling COLOR_ATTACHMENT");
      VkFormatProperties fpd; vkGetPhysicalDeviceFormatProperties(pd, VK_FORMAT_D32_SFLOAT, &fpd);
      ok((fpd.optimalTilingFeatures & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0, "D32_SFLOAT optimal-tiling DEPTH_STENCIL_ATTACHMENT");
      VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd, &props);
      ok(VK_API_VERSION_MAJOR(props.apiVersion) >= 1, "device apiVersion major >= 1");
      ok(props.limits.maxImageDimension2D >= W, "limits.maxImageDimension2D >= 64"); }

    /* Test 14: 2x2 texture upload + NEAREST sampling (combined image sampler + descriptor set) */
    { VkDescriptorSetLayoutBinding dslb = { .binding=0, .descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .descriptorCount=1, .stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT };
      VkDescriptorSetLayoutCreateInfo dslci = { .sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount=1, .pBindings=&dslb };
      VkDescriptorSetLayout dsl; VKOK(vkCreateDescriptorSetLayout(dev,&dslci,NULL,&dsl),"descriptor set layout");
      VkPipelineLayoutCreateInfo plci = { .sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount=1, .pSetLayouts=&dsl };
      VkPipelineLayout pl_tex; vkCreatePipelineLayout(dev,&plci,NULL,&pl_tex);
      VkImageCreateInfo tii = { .sType=VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType=VK_IMAGE_TYPE_2D, .format=VK_FORMAT_R8G8B8A8_UNORM,
        .extent={2,2,1}, .mipLevels=1, .arrayLayers=1, .samples=VK_SAMPLE_COUNT_1_BIT, .tiling=VK_IMAGE_TILING_OPTIMAL,
        .usage=VK_IMAGE_USAGE_SAMPLED_BIT|VK_IMAGE_USAGE_TRANSFER_DST_BIT, .initialLayout=VK_IMAGE_LAYOUT_UNDEFINED };
      VkImage timg; VKOK(vkCreateImage(dev,&tii,NULL,&timg),"texture image");
      VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev,timg,&tmr);
      VkMemoryAllocateInfo tai = { .sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize=tmr.size, .memoryTypeIndex=memtype(tmr.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
      VkDeviceMemory tmem; vkAllocateMemory(dev,&tai,NULL,&tmem); vkBindImageMemory(dev,timg,tmem,0);
      unsigned char texels[16]={255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,255,255};
      VkBufferCreateInfo sbi = { .sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size=16, .usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT };
      VkBuffer sbuf; vkCreateBuffer(dev,&sbi,NULL,&sbuf);
      VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev,sbuf,&smr);
      VkMemoryAllocateInfo sai = { .sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize=smr.size, .memoryTypeIndex=memtype(smr.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
      VkDeviceMemory smem; vkAllocateMemory(dev,&sai,NULL,&smem); vkBindBufferMemory(dev,sbuf,smem,0);
      void* sp; vkMapMemory(dev,smem,0,16,0,&sp); memcpy(sp,texels,16); vkUnmapMemory(dev,smem);
      { VkCommandBufferAllocateInfo cai = { .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool=pool, .level=VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount=1 };
        VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
        VkCommandBufferBeginInfo bi = { .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd,&bi);
        VkImageMemoryBarrier b1 = { .sType=VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, .oldLayout=VK_IMAGE_LAYOUT_UNDEFINED, .newLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
          .srcQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED, .dstQueueFamilyIndex=VK_QUEUE_FAMILY_IGNORED, .image=timg, .subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1},
          .srcAccessMask=0, .dstAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT };
        vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,0,NULL,0,NULL,1,&b1);
        VkBufferImageCopy cp = { .imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}, .imageExtent={2,2,1} };
        vkCmdCopyBufferToImage(cmd,sbuf,timg,VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,1,&cp);
        VkImageMemoryBarrier b2 = b1; b2.oldLayout=VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout=VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
        b2.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
        vkCmdPipelineBarrier(cmd,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,0,0,NULL,0,NULL,1,&b2);
        vkEndCommandBuffer(cmd);
        VkSubmitInfo si = { .sType=VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount=1, .pCommandBuffers=&cmd };
        VkFenceCreateInfo fi = { .sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fnc; vkCreateFence(dev,&fi,NULL,&fnc);
        vkQueueSubmit(q,1,&si,fnc); vkWaitForFences(dev,1,&fnc,VK_TRUE,UINT64_MAX); vkDestroyFence(dev,fnc,NULL); vkFreeCommandBuffers(dev,pool,1,&cmd); }
      VkImageViewCreateInfo tvi = { .sType=VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image=timg, .viewType=VK_IMAGE_VIEW_TYPE_2D, .format=VK_FORMAT_R8G8B8A8_UNORM, .subresourceRange={VK_IMAGE_ASPECT_COLOR_BIT,0,1,0,1} };
      VkImageView tview; vkCreateImageView(dev,&tvi,NULL,&tview);
      VkSamplerCreateInfo smci = { .sType=VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, .magFilter=VK_FILTER_NEAREST, .minFilter=VK_FILTER_NEAREST,
        .addressModeU=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeV=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeW=VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE };
      VkSampler samp; vkCreateSampler(dev,&smci,NULL,&samp);
      VkDescriptorPoolSize dps = { VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1 };
      VkDescriptorPoolCreateInfo dpci = { .sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets=1, .poolSizeCount=1, .pPoolSizes=&dps };
      VkDescriptorPool dpool; vkCreateDescriptorPool(dev,&dpci,NULL,&dpool);
      VkDescriptorSetAllocateInfo dsai = { .sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool=dpool, .descriptorSetCount=1, .pSetLayouts=&dsl };
      VkDescriptorSet dset; vkAllocateDescriptorSets(dev,&dsai,&dset);
      VkDescriptorImageInfo dii2 = { samp, tview, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
      VkWriteDescriptorSet wds = { .sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet=dset, .dstBinding=0, .descriptorCount=1, .descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .pImageInfo=&dii2 };
      vkUpdateDescriptorSets(dev,1,&wds,0,NULL);
      VkPipeline pt=mkPipe2(vs_tx,fs_tx,pl_tex,3,VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,NOBLEND,VK_CULL_MODE_NONE,CCWF,0,VK_COMPARE_OP_ALWAYS,rp,0xF);
      ok(dsl&&pl_tex&&pt&&samp,"texture pipeline + descriptor created");
      const float tq[]={ -1,-1,0,0,  1,-1,1,0,  -1,1,0,1,  1,1,1,1 };
      VkDeviceMemory tqm; VkBuffer tvbo=mkVbo(tq,sizeof(tq),&tqm);
      { VkCommandBufferAllocateInfo cai = { .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool=pool, .level=VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount=1 };
        VkCommandBuffer cmd; vkAllocateCommandBuffers(dev,&cai,&cmd);
        VkCommandBufferBeginInfo bi = { .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags=VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd,&bi);
        VkClearValue cv; cv.color.float32[0]=0; cv.color.float32[1]=0; cv.color.float32[2]=0; cv.color.float32[3]=1;
        VkRenderPassBeginInfo rpb = { .sType=VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass=rp, .framebuffer=fb, .renderArea={{0,0},{W,H}}, .clearValueCount=1, .pClearValues=&cv };
        vkCmdBeginRenderPass(cmd,&rpb,VK_SUBPASS_CONTENTS_INLINE);
        vkCmdBindPipeline(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pt);
        VkRect2D scr = { {0,0}, {W,H} }; vkCmdSetScissor(cmd,0,1,&scr);
        vkCmdBindDescriptorSets(cmd,VK_PIPELINE_BIND_POINT_GRAPHICS,pl_tex,0,1,&dset,0,NULL);
        VkDeviceSize off=0; vkCmdBindVertexBuffers(cmd,0,1,&tvbo,&off);
        vkCmdDraw(cmd,4,1,0,0);
        vkCmdEndRenderPass(cmd);
        VkBufferImageCopy region = { .imageSubresource={VK_IMAGE_ASPECT_COLOR_BIT,0,0,1}, .imageExtent={W,H,1} };
        vkCmdCopyImageToBuffer(cmd,cimg,VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,rbuf,1,&region);
        vkEndCommandBuffer(cmd);
        VkSubmitInfo si = { .sType=VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount=1, .pCommandBuffers=&cmd };
        VkFenceCreateInfo fi = { .sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fnc; vkCreateFence(dev,&fi,NULL,&fnc);
        vkQueueSubmit(q,1,&si,fnc); vkWaitForFences(dev,1,&fnc,VK_TRUE,UINT64_MAX); vkDestroyFence(dev,fnc,NULL); vkFreeCommandBuffers(dev,pool,1,&cmd);
        memcpy(px_,rmap,sizeof(px_)); }
      ok(peq(W/4,H/4,255,0,0,255,2),"texture NEAREST top-left red");
      ok(peq(3*W/4,H/4,0,255,0,255,2),"texture NEAREST top-right green");
      ok(peq(W/4,3*H/4,0,0,255,255,2),"texture NEAREST bottom-left blue");
      ok(peq(3*W/4,3*H/4,255,255,255,255,2),"texture NEAREST bottom-right white");
      vkDestroyPipeline(dev,pt,NULL); vkDestroyBuffer(dev,tvbo,NULL); vkFreeMemory(dev,tqm,NULL);
      vkDestroySampler(dev,samp,NULL); vkDestroyImageView(dev,tview,NULL); vkDestroyImage(dev,timg,NULL); vkFreeMemory(dev,tmem,NULL);
      vkDestroyBuffer(dev,sbuf,NULL); vkFreeMemory(dev,smem,NULL);
      vkDestroyDescriptorPool(dev,dpool,NULL); vkDestroyDescriptorSetLayout(dev,dsl,NULL); vkDestroyPipelineLayout(dev,pl_tex,NULL); }
#undef NOBLEND
#undef CCWF

    { float col[4] = {1, 0, 0, 1}; frame(0, 0, 0, 1, pipe_solid, pl_push, col, vbo, 4, full);
      ok(!all_eq(0, 255, 0, 255, 2), "negative control: red buffer is NOT green");
      ok(!peq(0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black"); }

    vkDeviceWaitIdle(dev);
    int EXPECTED = 68, TOTAL = PASS + FAIL;
    printf("vulkan-render-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("VULKAN_RENDER_C_FULL_API OK %d\n", PASS); return 0; }
    return 1;
}
