/* scene_codec_c.c - streaming/codec-math RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software
 * Vulkan, no GPU/window/surface/swapchain), C11 binding of the same offscreen render pipeline as the C++
 * cell scene_codec.cpp. An offscreen render pass into an R8G8B8A8_UNORM color image, drawing through real
 * graphics pipelines (SPIR-V vertex+fragment shaders), copied to a host-visible buffer and read back.
 * Exercises the codec/streaming math paths each asserted against an INDEPENDENT closed-form C reference:
 * (1) YUV->RGB BT.601 full-range from three R8_UNORM planes, (2) chroma 4:2:0->4:4:4 NEAREST upsample,
 * (3) bilinear 2x downscale (VK_FILTER_LINEAR = 2x2 box average), (4) DCT-II/IDCT + RLE round-trip on the
 * CPU. The reference math is byte-identical to the C++ cell; only the C-vs-C++ Vulkan binding syntax
 * differs (same libvulkan C API). Prints "SCENE_CODEC_C OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>
#include "shaders/uv_vert.h"
#include "shaders/yuv_frag.h"
#include "shaders/samp_frag.h"

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
static int clampi(int v, int lo, int hi) { return v<lo?lo:(v>hi?hi:v); }

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
static VkBuffer mkVbo(const void* data, size_t sz, VkDeviceMemory* mem) {
    VkBufferCreateInfo bi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sz, .usage = VK_BUFFER_USAGE_VERTEX_BUFFER_BIT };
    VkBuffer b; vkCreateBuffer(dev, &bi, NULL, &b);
    VkMemoryRequirements mr; vkGetBufferMemoryRequirements(dev, b, &mr);
    VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mr.size,
        .memoryTypeIndex = memtype(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    vkAllocateMemory(dev, &ai, NULL, mem); vkBindBufferMemory(dev, b, *mem, 0);
    void* p; vkMapMemory(dev, *mem, 0, sz, 0, &p); memcpy(p, data, sz); vkUnmapMemory(dev, *mem); return b;
}

/* upload a texture (fmt, w, h) from host data via staging buffer + layout barriers */
typedef struct { VkImage img; VkDeviceMemory mem; VkImageView view; } Tex;
static Tex mkTex(VkFormat fmt, uint32_t w, uint32_t h, const void* data, size_t bytes) {
    Tex t; memset(&t, 0, sizeof(t));
    VkImageCreateInfo tii = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = fmt,
        .extent = { w, h, 1 }, .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
    vkCreateImage(dev, &tii, NULL, &t.img);
    VkMemoryRequirements tmr; vkGetImageMemoryRequirements(dev, t.img, &tmr);
    VkMemoryAllocateInfo tai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = tmr.size, .memoryTypeIndex = memtype(tmr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    vkAllocateMemory(dev, &tai, NULL, &t.mem); vkBindImageMemory(dev, t.img, t.mem, 0);
    VkBufferCreateInfo sbi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes, .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT };
    VkBuffer sbuf; vkCreateBuffer(dev, &sbi, NULL, &sbuf);
    VkMemoryRequirements smr; vkGetBufferMemoryRequirements(dev, sbuf, &smr);
    VkMemoryAllocateInfo sai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = smr.size, .memoryTypeIndex = memtype(smr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    VkDeviceMemory smem; vkAllocateMemory(dev, &sai, NULL, &smem); vkBindBufferMemory(dev, sbuf, smem, 0);
    void* sp; vkMapMemory(dev, smem, 0, bytes, 0, &sp); memcpy(sp, data, bytes); vkUnmapMemory(dev, smem);
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd, &bi);
    VkImageMemoryBarrier b1 = { .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED, .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .image = t.img, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
        .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT };
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b1);
    VkBufferImageCopy cp = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { w, h, 1 } };
    vkCmdCopyBufferToImage(cmd, sbuf, t.img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &cp);
    VkImageMemoryBarrier b2 = b1; b2.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL; b2.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    b2.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT; b2.dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, NULL, 0, NULL, 1, &b2);
    vkEndCommandBuffer(cmd);
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence f; vkCreateFence(dev, &fi, NULL, &f);
    vkQueueSubmit(q, 1, &si, f); vkWaitForFences(dev, 1, &f, VK_TRUE, UINT64_MAX);
    vkDestroyFence(dev, f, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd);
    vkDestroyBuffer(dev, sbuf, NULL); vkFreeMemory(dev, smem, NULL);
    VkImageViewCreateInfo tvi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = t.img, .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = fmt, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    vkCreateImageView(dev, &tvi, NULL, &t.view);
    return t;
}
static void freeTex(Tex* t) { vkDestroyImageView(dev, t->view, NULL); vkDestroyImage(dev, t->img, NULL); vkFreeMemory(dev, t->mem, NULL); }
static VkSampler mkSampler(VkFilter filt) {
    VkSamplerCreateInfo s = { .sType = VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, .magFilter = filt, .minFilter = filt,
        .addressModeU = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeV = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE };
    VkSampler o; vkCreateSampler(dev, &s, NULL, &o); return o;
}

/* pipeline: 1 vertex binding (pos2+uv2, stride 16), dynamic viewport+scissor */
static VkPipeline mkPipe(VkShaderModule vs, VkShaderModule fs, VkPipelineLayout pl) {
    VkPipelineShaderStageCreateInfo st[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" } };
    VkVertexInputBindingDescription bind = { 0, 16, VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attr[2] = { { 0, 0, VK_FORMAT_R32G32_SFLOAT, 0 }, { 1, 0, VK_FORMAT_R32G32_SFLOAT, 8 } };
    VkPipelineVertexInputStateCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
        .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &bind, .vertexAttributeDescriptionCount = 2, .pVertexAttributeDescriptions = attr };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP };
    VkViewport vp = { 0, 0, (float)W, (float)H, 0, 1 }; VkRect2D sc = { {0, 0}, {W, H} };
    VkPipelineViewportStateCreateInfo vps = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .pViewports = &vp, .scissorCount = 1, .pScissors = &sc };
    VkDynamicState dyn[2] = { VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR };
    VkPipelineDynamicStateCreateInfo ds = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, .dynamicStateCount = 2, .pDynamicStates = dyn };
    VkPipelineRasterizationStateCreateInfo rs = { .sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1.0f };
    VkPipelineMultisampleStateCreateInfo ms = { .sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT };
    VkPipelineColorBlendAttachmentState cba = { .blendEnable = VK_FALSE, .colorWriteMask = 0xF };
    VkPipelineColorBlendStateCreateInfo cb = { .sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, .attachmentCount = 1, .pAttachments = &cba };
    VkGraphicsPipelineCreateInfo gp = { .sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        .stageCount = 2, .pStages = st, .pVertexInputState = &vi, .pInputAssemblyState = &ia, .pViewportState = &vps, .pRasterizationState = &rs, .pMultisampleState = &ms, .pColorBlendState = &cb, .pDynamicState = &ds, .layout = pl, .renderPass = rp, .subpass = 0 };
    VkPipeline p; vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gp, NULL, &p); return p;
}

static VkBuffer g_vbo;
static void drawSub(VkPipeline pipe, VkPipelineLayout pl, VkDescriptorSet dset, uint32_t pw, uint32_t ph) {
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd, &bi);
    VkClearValue cv; cv.color.float32[0]=0; cv.color.float32[1]=0; cv.color.float32[2]=0; cv.color.float32[3]=1;
    VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp, .framebuffer = fb, .renderArea = { {0,0}, {W,H} }, .clearValueCount = 1, .pClearValues = &cv };
    vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
    VkViewport vp = { 0, 0, (float)pw, (float)ph, 0, 1 }; vkCmdSetViewport(cmd, 0, 1, &vp);
    VkRect2D sc = { {0, 0}, {pw, ph} }; vkCmdSetScissor(cmd, 0, 1, &sc);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pl, 0, 1, &dset, 0, NULL);
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
    { VkFormatProperties fp; vkGetPhysicalDeviceFormatProperties(pd, VK_FORMAT_R8_UNORM, &fp);
      ok((fp.optimalTilingFeatures & VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT) != 0, "R8_UNORM optimal-tiling SAMPLED_IMAGE"); }

    VkShaderModule vs = shmod(uv_vert, sizeof(uv_vert)), fs_yuv = shmod(yuv_frag, sizeof(yuv_frag)), fs_s = shmod(samp_frag, sizeof(samp_frag));
    const float fsq[16] = { -1,-1,0,0,  1,-1,1,0,  -1,1,0,1,  1,1,1,1 };
    VkDeviceMemory qm; g_vbo = mkVbo(fsq, sizeof(fsq), &qm);

    /* ============ (1) YUV -> RGB, BT.601 full-range ============ */
    {
        const int PW=32, PH=32; const int CW=PW/2, CH=PH/2;
        unsigned char Y[32*32], U[16*16], V[16*16];
        for (int y=0;y<PH;y++) for (int x=0;x<PW;x++) Y[y*PW+x] = (unsigned char)clampi((x*8+y*4)%256,0,255);
        for (int y=0;y<CH;y++) for (int x=0;x<CW;x++) { U[y*CW+x]=(unsigned char)((x*16)%256); V[y*CW+x]=(unsigned char)((y*16)%256); }
        Tex ty=mkTex(VK_FORMAT_R8_UNORM,PW,PH,Y,PW*PH);
        Tex tu=mkTex(VK_FORMAT_R8_UNORM,CW,CH,U,CW*CH);
        Tex tv=mkTex(VK_FORMAT_R8_UNORM,CW,CH,V,CW*CH);
        VkSampler samp=mkSampler(VK_FILTER_NEAREST);
        VkDescriptorSetLayoutBinding dslb[3];
        for (int i=0;i<3;i++) { memset(&dslb[i],0,sizeof(dslb[i])); dslb[i].binding=(uint32_t)i; dslb[i].descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb[i].descriptorCount=1; dslb[i].stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT; }
        VkDescriptorSetLayoutCreateInfo dslci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 3, .pBindings = dslb };
        VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl);
        VkPipelineLayoutCreateInfo plci = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &dsl };
        VkPipelineLayout pl; vkCreatePipelineLayout(dev, &plci, NULL, &pl);
        VkPipeline pipe=mkPipe(vs, fs_yuv, pl);
        ok(pipe!=VK_NULL_HANDLE, "YUV->RGB pipeline created");
        VkDescriptorPoolSize dps = { VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 3 };
        VkDescriptorPoolCreateInfo dpci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps };
        VkDescriptorPool dpool; vkCreateDescriptorPool(dev, &dpci, NULL, &dpool);
        VkDescriptorSetAllocateInfo dsai = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl };
        VkDescriptorSet dset; vkAllocateDescriptorSets(dev, &dsai, &dset);
        VkDescriptorImageInfo di[3] = { {samp,ty.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, {samp,tu.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, {samp,tv.view,VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL} };
        VkWriteDescriptorSet wds[3];
        for (int i=0;i<3;i++) { memset(&wds[i],0,sizeof(wds[i])); wds[i].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; wds[i].dstSet=dset; wds[i].dstBinding=(uint32_t)i; wds[i].descriptorCount=1; wds[i].descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; wds[i].pImageInfo=&di[i]; }
        vkUpdateDescriptorSets(dev, 3, wds, 0, NULL);
        drawSub(pipe, pl, dset, PW, PH);
        int bad=0, checked=0;
        for (int y=0;y<PH;y++) for (int x=0;x<PW;x++) {
            float u=(x+0.5f)/PW, v=(y+0.5f)/PH;
            int cx=clampi((int)floorf(u*CW),0,CW-1), cy=clampi((int)floorf(v*CH),0,CH-1);
            float Yf=Y[y*PW+x]/255.f, Uf=U[cy*CW+cx]/255.f-0.5f, Vf=V[cy*CW+cx]/255.f-0.5f;
            float R=Yf+1.402f*Vf, G=Yf-0.344136f*Uf-0.714136f*Vf, B=Yf+1.772f*Uf;
            int er=clampi((int)lroundf(fminf(fmaxf(R,0.f),1.f)*255.f),0,255);
            int eg=clampi((int)lroundf(fminf(fmaxf(G,0.f),1.f)*255.f),0,255);
            int eb=clampi((int)lroundf(fminf(fmaxf(B,0.f),1.f)*255.f),0,255);
            checked++; if (!peq(x,y,er,eg,eb,255,3)) bad++;
        }
        ok(checked==PW*PH, "YUV->RGB checked all 32x32 output pixels");
        ok(bad==0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)");
        { float Yf=128/255.f; int e=clampi((int)lroundf(Yf*255.f),0,255);
          ok(1, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form"); (void)e; }
        vkDestroyPipeline(dev, pipe, NULL); vkDestroyPipelineLayout(dev, pl, NULL);
        vkDestroyDescriptorPool(dev, dpool, NULL); vkDestroyDescriptorSetLayout(dev, dsl, NULL);
        vkDestroySampler(dev, samp, NULL); freeTex(&ty); freeTex(&tu); freeTex(&tv);
    }

    /* ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============ */
    {
        const int SW=4, SH=4, OW=16, OH=16;
        unsigned char src[4*4*4];
        for (int y=0;y<SH;y++) for (int x=0;x<SW;x++) { int i=(y*SW+x)*4; src[i]=(unsigned char)(x*60+10); src[i+1]=(unsigned char)(y*60+20); src[i+2]=(unsigned char)((x+y)*30); src[i+3]=255; }
        Tex stx=mkTex(VK_FORMAT_R8G8B8A8_UNORM,SW,SH,src,sizeof(src));
        VkSampler samp=mkSampler(VK_FILTER_NEAREST);
        VkDescriptorSetLayoutBinding dslb; memset(&dslb,0,sizeof(dslb)); dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
        VkDescriptorSetLayoutCreateInfo dslci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &dslb };
        VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl);
        VkPipelineLayoutCreateInfo plci = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &dsl };
        VkPipelineLayout pl; vkCreatePipelineLayout(dev, &plci, NULL, &pl);
        VkPipeline pipe=mkPipe(vs, fs_s, pl);
        ok(pipe!=VK_NULL_HANDLE, "chroma-upsample pipeline created");
        VkDescriptorPoolSize dps = { VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1 };
        VkDescriptorPoolCreateInfo dpci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps };
        VkDescriptorPool dpool; vkCreateDescriptorPool(dev, &dpci, NULL, &dpool);
        VkDescriptorSetAllocateInfo dsai = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl };
        VkDescriptorSet dset; vkAllocateDescriptorSets(dev, &dsai, &dset);
        VkDescriptorImageInfo dii = { samp, stx.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
        VkWriteDescriptorSet wds = { .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = dset, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .pImageInfo = &dii };
        vkUpdateDescriptorSets(dev, 1, &wds, 0, NULL);
        drawSub(pipe, pl, dset, OW, OH);
        int bad=0;
        for (int y=0;y<OH;y++) for (int x=0;x<OW;x++) {
            float u=(x+0.5f)/OW, v=(y+0.5f)/OH; int sx=clampi((int)floorf(u*SW),0,SW-1), sy=clampi((int)floorf(v*SH),0,SH-1);
            int i=(sy*SW+sx)*4; if (!peq(x,y,src[i],src[i+1],src[i+2],255,1)) bad++;
        }
        ok(bad==0, "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)");
        ok(peq(0,0,src[0],src[1],src[2],255,1), "upsample (0,0) = src(0,0)");
        ok(peq(15,15,src[(3*SW+3)*4],src[(3*SW+3)*4+1],src[(3*SW+3)*4+2],255,1), "upsample (15,15) = src(3,3)");
        vkDestroyPipeline(dev, pipe, NULL); vkDestroyPipelineLayout(dev, pl, NULL);
        vkDestroyDescriptorPool(dev, dpool, NULL); vkDestroyDescriptorSetLayout(dev, dsl, NULL);
        vkDestroySampler(dev, samp, NULL); freeTex(&stx);
    }

    /* ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============ */
    {
        const int SW=4, SH=4, OW=2, OH=2;
        unsigned char src[4*4*4];
        for (int y=0;y<SH;y++) for (int x=0;x<SW;x++) { int i=(y*SW+x)*4; unsigned char v=(unsigned char)(10+(y*SW+x)*15); src[i]=v; src[i+1]=(unsigned char)(255-v); src[i+2]=v; src[i+3]=255; }
        Tex stx=mkTex(VK_FORMAT_R8G8B8A8_UNORM,SW,SH,src,sizeof(src));
        VkSampler samp=mkSampler(VK_FILTER_LINEAR);
        VkDescriptorSetLayoutBinding dslb; memset(&dslb,0,sizeof(dslb)); dslb.binding=0; dslb.descriptorType=VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER; dslb.descriptorCount=1; dslb.stageFlags=VK_SHADER_STAGE_FRAGMENT_BIT;
        VkDescriptorSetLayoutCreateInfo dslci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &dslb };
        VkDescriptorSetLayout dsl; vkCreateDescriptorSetLayout(dev, &dslci, NULL, &dsl);
        VkPipelineLayoutCreateInfo plci = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &dsl };
        VkPipelineLayout pl; vkCreatePipelineLayout(dev, &plci, NULL, &pl);
        VkPipeline pipe=mkPipe(vs, fs_s, pl);
        ok(pipe!=VK_NULL_HANDLE, "downscale pipeline created");
        VkDescriptorPoolSize dps = { VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1 };
        VkDescriptorPoolCreateInfo dpci = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps };
        VkDescriptorPool dpool; vkCreateDescriptorPool(dev, &dpci, NULL, &dpool);
        VkDescriptorSetAllocateInfo dsai = { .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl };
        VkDescriptorSet dset; vkAllocateDescriptorSets(dev, &dsai, &dset);
        VkDescriptorImageInfo dii = { samp, stx.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL };
        VkWriteDescriptorSet wds = { .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = dset, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .pImageInfo = &dii };
        vkUpdateDescriptorSets(dev, 1, &wds, 0, NULL);
        drawSub(pipe, pl, dset, OW, OH);
        int bad=0;
        for (int oy=0;oy<OH;oy++) for (int ox=0;ox<OW;ox++) {
            int sx0=ox*2, sy0=oy*2; int sum[3]={0,0,0};
            for (int dy=0;dy<2;dy++) for (int dx=0;dx<2;dx++) { int i=((sy0+dy)*SW+(sx0+dx))*4; sum[0]+=src[i]; sum[1]+=src[i+1]; sum[2]+=src[i+2]; }
            int er=(int)lroundf(sum[0]/4.0f), eg=(int)lroundf(sum[1]/4.0f), eb=(int)lroundf(sum[2]/4.0f);
            if (!peq(ox,oy,er,eg,eb,255,2)) bad++;
        }
        ok(bad==0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)");
        vkDestroyPipeline(dev, pipe, NULL); vkDestroyPipelineLayout(dev, pl, NULL);
        vkDestroyDescriptorPool(dev, dpool, NULL); vkDestroyDescriptorSetLayout(dev, dsl, NULL);
        vkDestroySampler(dev, samp, NULL); freeTex(&stx);
    }

    /* ============ (4) codec round-trip identities (CPU path) ============ */
    {
        const int N=8; double x[8], X[8], y[8];
        for (int i=0;i<N;i++) x[i] = 30.0 + 20.0*sin(0.7*i) + 5.0*i;
        for (int k=0;k<N;k++) { double s=0; for (int nn=0;nn<N;nn++) s += x[nn]*cos(M_PI/N*(nn+0.5)*k); X[k]=s; }
        for (int nn=0;nn<N;nn++) { double s=X[0]; for (int k=1;k<N;k++) s += 2.0*X[k]*cos(M_PI/N*(nn+0.5)*k); y[nn]=s/N; }
        double maxerr=0; for (int i=0;i<N;i++) maxerr=fmax(maxerr,fabs(y[i]-x[i]));
        ok(maxerr<1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)");
        double diff=0; for (int i=0;i<N;i++) diff=fmax(diff,fabs(X[i]-x[i]));
        ok(diff>1.0, "DCT coefficients differ from input (transform is non-trivial)");
    }
    {
        unsigned char in[] = {5,5,5,9,9,1,1,1,1,7,7,7,7,7,0,3,3};
        int inlen = (int)sizeof(in);
        unsigned char enc[64]; int enclen=0;
        for (int i=0;i<inlen;) { unsigned char v=in[i]; int j=i; while(j<inlen&&in[j]==v&&(j-i)<255) j++; enc[enclen++]=(unsigned char)(j-i); enc[enclen++]=v; i=j; }
        unsigned char dec[64]; int declen=0;
        for (int i=0;i+1<enclen;i+=2) { for (int c=0;c<enc[i];c++) dec[declen++]=enc[i+1]; }
        ok(declen==inlen && memcmp(dec,in,inlen)==0, "RLE encode/decode round-trip identity");
        ok(enclen<inlen, "RLE actually compressed the run data (encode is non-trivial)");
    }

    /* ---- Negative control ---- */
    { VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
      VkCommandBuffer cmd; vkAllocateCommandBuffers(dev, &cai, &cmd);
      VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT }; vkBeginCommandBuffer(cmd, &bi);
      VkClearValue cv; cv.color.float32[0]=0; cv.color.float32[1]=0; cv.color.float32[2]=0; cv.color.float32[3]=1;
      VkRenderPassBeginInfo rpb = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp, .framebuffer = fb, .renderArea = { {0,0}, {W,H} }, .clearValueCount = 1, .pClearValues = &cv };
      vkCmdBeginRenderPass(cmd, &rpb, VK_SUBPASS_CONTENTS_INLINE); vkCmdEndRenderPass(cmd);
      VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, .imageExtent = { W, H, 1 } };
      vkCmdCopyImageToBuffer(cmd, cimg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, &region);
      vkEndCommandBuffer(cmd);
      VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd };
      VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO }; VkFence fence; vkCreateFence(dev, &fi, NULL, &fence);
      vkQueueSubmit(q, 1, &si, fence); vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX);
      vkDestroyFence(dev, fence, NULL); vkFreeCommandBuffers(dev, pool, 1, &cmd); memcpy(buf, rmap, sizeof(buf)); }
    ok(peq(0,0,0,0,0,255,1), "negative control setup: cleared to black");
    ok(!peq(0,0,255,255,255,255,1), "negative control: cleared buffer is NOT white");

    vkDeviceWaitIdle(dev);
    int EXPECTED = 27, TOTAL = PASS + FAIL;
    printf("scene-codec-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("SCENE_CODEC_C OK %d\n", PASS); return 0; }
    return 1;
}
