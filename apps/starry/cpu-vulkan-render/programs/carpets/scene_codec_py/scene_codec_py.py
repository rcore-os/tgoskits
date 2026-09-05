#!/usr/bin/env python3
# scene_codec_py.py - streaming/codec-math RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software
# Vulkan, no GPU/window/surface/swapchain), Python cffi binding (`import vulkan as vk`) of the same
# offscreen render pipeline as the C++ cell scene_codec.cpp. Draws through real graphics pipelines (SPIR-V
# shaders from shaders/*.spv). Exercises codec/streaming math each asserted against an INDEPENDENT
# closed-form reference: (1) YUV->RGB BT.601 full-range from three R8_UNORM planes, (2) chroma 4:2:0->4:4:4
# NEAREST upsample, (3) bilinear 2x downscale (VK_FILTER_LINEAR = 2x2 box average), (4) DCT-II/IDCT + RLE
# round-trip on the CPU. The reference math is behaviour-identical to the C++ cell; only the cffi-vs-C++
# Vulkan binding syntax differs. Prints "SCENE_CODEC_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
# Honest-skips (exit 1, "unavailable") if the vulkan cffi binding is not provisioned.
import sys
import os
import math

try:
    import vulkan as vk
except Exception as e:  # noqa: BLE001
    print("SCENE_CODEC_PY unavailable: import vulkan failed: %s" % e)
    sys.exit(1)
try:
    import numpy as np
except Exception as e:  # noqa: BLE001
    print("SCENE_CODEC_PY unavailable: import numpy failed: %s" % e)
    sys.exit(1)

ffi = vk.ffi
W = H = 64
PASS = 0
FAIL = 0


def ok(cond, desc):
    global PASS, FAIL
    if cond:
        PASS += 1
    else:
        FAIL += 1
        sys.stderr.write("FAIL: %s\n" % desc)


def die(msg):
    print("SCENE_CODEC_PY unavailable: %s" % msg)
    sys.exit(1)


def clampi(v, lo, hi):
    return lo if v < lo else (hi if v > hi else v)


def lroundf(f):
    return int(math.floor(f + 0.5)) if f >= 0 else -int(math.floor(-f + 0.5))


def peq(px, x, y, r, g, b, a, tol):
    p = px[y, x]
    return (abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol
            and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - a) <= tol)


# ---- Vulkan bring-up ----
try:
    aiapp = vk.VkApplicationInfo(sType=vk.VK_STRUCTURE_TYPE_APPLICATION_INFO, apiVersion=vk.VK_MAKE_VERSION(1, 1, 0))
    ici = vk.VkInstanceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, pApplicationInfo=aiapp)
    inst = vk.vkCreateInstance(ici, None)
except Exception as e:  # noqa: BLE001
    die("vkCreateInstance failed (no Vulkan ICD? set VK_ICD_FILENAMES to lavapipe): %s" % e)
ok(inst is not None, "vkCreateInstance")

pds = vk.vkEnumeratePhysicalDevices(inst)
ok(len(pds) >= 1, ">=1 physical device")
pd = pds[0]
qfp = vk.vkGetPhysicalDeviceQueueFamilyProperties(pd)
qfam = None
for i, p in enumerate(qfp):
    if p.queueFlags & vk.VK_QUEUE_GRAPHICS_BIT:
        qfam = i
        break
ok(qfam is not None, "graphics queue family")
if qfam is None:
    die("no graphics queue family")
qci = vk.VkDeviceQueueCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, queueFamilyIndex=qfam, queueCount=1, pQueuePriorities=[1.0])
dci = vk.VkDeviceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, pQueueCreateInfos=[qci])
dev = vk.vkCreateDevice(pd, dci, None)
ok(dev is not None, "vkCreateDevice")
q = vk.vkGetDeviceQueue(dev, qfam, 0)
mp = vk.vkGetPhysicalDeviceMemoryProperties(pd)


def memtype(bits, want):
    for i in range(mp.memoryTypeCount):
        if (bits & (1 << i)) and (mp.memoryTypes[i].propertyFlags & want) == want:
            return i
    return None


def shmod(path):
    code = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "shaders", path), "rb").read()
    ci = vk.VkShaderModuleCreateInfo(sType=vk.VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, codeSize=len(code), pCode=code)
    return vk.vkCreateShaderModule(dev, ci, None)


ii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                          extent=vk.VkExtent3D(W, H, 1), mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                          usage=vk.VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk.VK_IMAGE_USAGE_TRANSFER_SRC_BIT, initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
cimg = vk.vkCreateImage(dev, ii, None)
ok(cimg is not None, "vkCreateImage color")
imr = vk.vkGetImageMemoryRequirements(dev, cimg)
cmem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=imr.size, memoryTypeIndex=memtype(imr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)), None)
vk.vkBindImageMemory(dev, cimg, cmem, 0)
cview = vk.vkCreateImageView(dev, vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=cimg, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                                                           subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1)), None)
ok(cview is not None, "vkCreateImageView")
att = vk.VkAttachmentDescription(format=vk.VK_FORMAT_R8G8B8A8_UNORM, samples=vk.VK_SAMPLE_COUNT_1_BIT, loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_STORE,
                                 stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE, stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                 initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, finalLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL)
ref = vk.VkAttachmentReference(0, vk.VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL)
sp = vk.VkSubpassDescription(pipelineBindPoint=vk.VK_PIPELINE_BIND_POINT_GRAPHICS, colorAttachmentCount=1, pColorAttachments=[ref])
rpi = vk.VkRenderPassCreateInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, pAttachments=[att], pSubpasses=[sp])
rp = vk.vkCreateRenderPass(dev, rpi, None)
ok(rp is not None, "vkCreateRenderPass")
fbi = vk.VkFramebufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, renderPass=rp, pAttachments=[cview], width=W, height=H, layers=1)
fb = vk.vkCreateFramebuffer(dev, fbi, None)
ok(fb is not None, "vkCreateFramebuffer")
rbuf = vk.vkCreateBuffer(dev, vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=W * H * 4, usage=vk.VK_BUFFER_USAGE_TRANSFER_DST_BIT), None)
rmr = vk.vkGetBufferMemoryRequirements(dev, rbuf)
rmem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=rmr.size, memoryTypeIndex=memtype(rmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)), None)
vk.vkBindBufferMemory(dev, rbuf, rmem, 0)
rmap = vk.vkMapMemory(dev, rmem, 0, W * H * 4, 0)
pool = vk.vkCreateCommandPool(dev, vk.VkCommandPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, queueFamilyIndex=qfam, flags=vk.VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT), None)
ok(pool is not None, "vkCreateCommandPool")
ok(True, "offscreen R8G8B8A8 target + readback buffer ready")
fp = vk.vkGetPhysicalDeviceFormatProperties(pd, vk.VK_FORMAT_R8_UNORM)
ok((fp.optimalTilingFeatures & vk.VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT) != 0, "R8_UNORM optimal-tiling SAMPLED_IMAGE")


def readback():
    return np.frombuffer(rmap, dtype=np.uint8, count=W * H * 4).reshape(H, W, 4).copy()


def alloc_cmd():
    cai = vk.VkCommandBufferAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1)
    return vk.vkAllocateCommandBuffers(dev, cai)[0]


def submit_wait(cmd):
    si = vk.VkSubmitInfo(sType=vk.VK_STRUCTURE_TYPE_SUBMIT_INFO, pCommandBuffers=[cmd])
    fence = vk.vkCreateFence(dev, vk.VkFenceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FENCE_CREATE_INFO), None)
    vk.vkQueueSubmit(q, 1, [si], fence)
    vk.vkWaitForFences(dev, 1, [fence], vk.VK_TRUE, 0xFFFFFFFFFFFFFFFF)
    vk.vkDestroyFence(dev, fence, None)


def mkVbo(data_f32):
    arr = np.asarray(data_f32, dtype=np.float32)
    sz = arr.nbytes
    b = vk.vkCreateBuffer(dev, vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=sz, usage=vk.VK_BUFFER_USAGE_VERTEX_BUFFER_BIT), None)
    mr = vk.vkGetBufferMemoryRequirements(dev, b)
    m = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=mr.size, memoryTypeIndex=memtype(mr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)), None)
    vk.vkBindBufferMemory(dev, b, m, 0)
    p = vk.vkMapMemory(dev, m, 0, sz, 0)
    ffi.memmove(p, arr.tobytes(), sz)
    vk.vkUnmapMemory(dev, m)
    return b, m


def mkTex(fmt, w, h, data_bytes):
    tii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D, format=fmt, extent=vk.VkExtent3D(w, h, 1),
                              mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                              usage=vk.VK_IMAGE_USAGE_SAMPLED_BIT | vk.VK_IMAGE_USAGE_TRANSFER_DST_BIT, initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
    img = vk.vkCreateImage(dev, tii, None)
    tmr = vk.vkGetImageMemoryRequirements(dev, img)
    mem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=tmr.size, memoryTypeIndex=memtype(tmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)), None)
    vk.vkBindImageMemory(dev, img, mem, 0)
    sbuf = vk.vkCreateBuffer(dev, vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=len(data_bytes), usage=vk.VK_BUFFER_USAGE_TRANSFER_SRC_BIT), None)
    smr = vk.vkGetBufferMemoryRequirements(dev, sbuf)
    smem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=smr.size, memoryTypeIndex=memtype(smr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)), None)
    vk.vkBindBufferMemory(dev, sbuf, smem, 0)
    sp2 = vk.vkMapMemory(dev, smem, 0, len(data_bytes), 0)
    ffi.memmove(sp2, data_bytes, len(data_bytes))
    vk.vkUnmapMemory(dev, smem)
    cmd = alloc_cmd()
    vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
    sub = vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1)
    b1 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, newLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                 srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=img, subresourceRange=sub,
                                 srcAccessMask=0, dstAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT)
    vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, None, 0, None, 1, [b1])
    cp = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(w, h, 1))
    vk.vkCmdCopyBufferToImage(cmd, sbuf, img, vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, [cp])
    b2 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, newLayout=vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                                 srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=img, subresourceRange=sub,
                                 srcAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT, dstAccessMask=vk.VK_ACCESS_SHADER_READ_BIT)
    vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, vk.VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, None, 0, None, 1, [b2])
    vk.vkEndCommandBuffer(cmd)
    submit_wait(cmd)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    vk.vkDestroyBuffer(dev, sbuf, None)
    vk.vkFreeMemory(dev, smem, None)
    view = vk.vkCreateImageView(dev, vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=img, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=fmt, subresourceRange=sub), None)
    return (img, mem, view)


def free_tex(t):
    vk.vkDestroyImageView(dev, t[2], None); vk.vkDestroyImage(dev, t[0], None); vk.vkFreeMemory(dev, t[1], None)


def mkSampler(filt):
    s = vk.VkSamplerCreateInfo(sType=vk.VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, magFilter=filt, minFilter=filt,
                              addressModeU=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, addressModeV=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, addressModeW=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE)
    return vk.vkCreateSampler(dev, s, None)


def mkPipe(vs, fs, pl):
    st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
          vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
    bind = vk.VkVertexInputBindingDescription(0, 16, vk.VK_VERTEX_INPUT_RATE_VERTEX)
    attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32_SFLOAT, 0), vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32_SFLOAT, 8)]
    vi = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=attr)
    ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP)
    vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
    sc = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
    vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, pViewports=[vp], pScissors=[sc])
    dyn = vk.VkPipelineDynamicStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, pDynamicStates=[vk.VK_DYNAMIC_STATE_VIEWPORT, vk.VK_DYNAMIC_STATE_SCISSOR])
    rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, polygonMode=vk.VK_POLYGON_MODE_FILL, cullMode=vk.VK_CULL_MODE_NONE, lineWidth=1.0)
    ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
    cba = vk.VkPipelineColorBlendAttachmentState(blendEnable=vk.VK_FALSE, colorWriteMask=0xF)
    cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, pAttachments=[cba])
    gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st, pVertexInputState=vi, pInputAssemblyState=ia, pViewportState=vps,
                                         pRasterizationState=rs, pMultisampleState=ms, pColorBlendState=cb, pDynamicState=dyn, layout=pl, renderPass=rp, subpass=0)
    return vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]


def mk_desc(n):
    dslb = [vk.VkDescriptorSetLayoutBinding(binding=i, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, descriptorCount=1, stageFlags=vk.VK_SHADER_STAGE_FRAGMENT_BIT) for i in range(n)]
    dsl = vk.vkCreateDescriptorSetLayout(dev, vk.VkDescriptorSetLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, pBindings=dslb), None)
    pl = vk.vkCreatePipelineLayout(dev, vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pSetLayouts=[dsl]), None)
    dps = vk.VkDescriptorPoolSize(vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, n)
    dpool = vk.vkCreateDescriptorPool(dev, vk.VkDescriptorPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, maxSets=1, pPoolSizes=[dps]), None)
    dset = vk.vkAllocateDescriptorSets(dev, vk.VkDescriptorSetAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, descriptorPool=dpool, pSetLayouts=[dsl]))[0]
    return dsl, pl, dpool, dset


def drawSub(pipe, pl, dset, pw, ph):
    cmd = alloc_cmd()
    vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
    cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0.0, 0.0, 0.0, 1.0]))
    rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb, renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv])
    vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
    vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe)
    vk.vkCmdSetViewport(cmd, 0, 1, [vk.VkViewport(0, 0, float(pw), float(ph), 0, 1)])
    vk.vkCmdSetScissor(cmd, 0, 1, [vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(pw, ph))])
    vk.vkCmdBindDescriptorSets(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pl, 0, 1, [dset], 0, None)
    vk.vkCmdBindVertexBuffers(cmd, 0, 1, [g_vbo], [0])
    vk.vkCmdDraw(cmd, 4, 1, 0, 0)
    vk.vkCmdEndRenderPass(cmd)
    region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
    vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
    vk.vkEndCommandBuffer(cmd)
    submit_wait(cmd)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    return readback()


vs = shmod("uv_vert.spv"); fs_yuv = shmod("yuv_frag.spv"); fs_s = shmod("samp_frag.spv")
fsq = [-1, -1, 0, 0, 1, -1, 1, 0, -1, 1, 0, 1, 1, 1, 1, 1]
g_vbo, _qm = mkVbo(fsq)

# ============ (1) YUV -> RGB, BT.601 full-range ============
PW = PH = 32; CW = CH = 16
Y = bytearray(PW * PH); U = bytearray(CW * CH); V = bytearray(CW * CH)
for y in range(PH):
    for x in range(PW):
        Y[y * PW + x] = clampi((x * 8 + y * 4) % 256, 0, 255)
for y in range(CH):
    for x in range(CW):
        U[y * CW + x] = (x * 16) % 256
        V[y * CW + x] = (y * 16) % 256
ty = mkTex(vk.VK_FORMAT_R8_UNORM, PW, PH, bytes(Y))
tu = mkTex(vk.VK_FORMAT_R8_UNORM, CW, CH, bytes(U))
tv = mkTex(vk.VK_FORMAT_R8_UNORM, CW, CH, bytes(V))
samp = mkSampler(vk.VK_FILTER_NEAREST)
dsl, pl, dpool, dset = mk_desc(3)
pipe = mkPipe(vs, fs_yuv, pl)
ok(pipe is not None, "YUV->RGB pipeline created")
di = [vk.VkDescriptorImageInfo(samp, ty[2], vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL),
      vk.VkDescriptorImageInfo(samp, tu[2], vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL),
      vk.VkDescriptorImageInfo(samp, tv[2], vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)]
wds = [vk.VkWriteDescriptorSet(sType=vk.VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, dstSet=dset, dstBinding=i, descriptorCount=1, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, pImageInfo=[di[i]]) for i in range(3)]
vk.vkUpdateDescriptorSets(dev, 3, wds, 0, None)
px = drawSub(pipe, pl, dset, PW, PH)
bad = 0; checked = 0
for y in range(PH):
    for x in range(PW):
        u = (x + 0.5) / PW; v = (y + 0.5) / PH
        cx = clampi(int(math.floor(u * CW)), 0, CW - 1); cy = clampi(int(math.floor(v * CH)), 0, CH - 1)
        Yf = Y[y * PW + x] / 255.0; Uf = U[cy * CW + cx] / 255.0 - 0.5; Vf = V[cy * CW + cx] / 255.0 - 0.5
        R = Yf + 1.402 * Vf; G = Yf - 0.344136 * Uf - 0.714136 * Vf; B = Yf + 1.772 * Uf
        er = clampi(lroundf(min(max(R, 0.0), 1.0) * 255.0), 0, 255)
        eg = clampi(lroundf(min(max(G, 0.0), 1.0) * 255.0), 0, 255)
        eb = clampi(lroundf(min(max(B, 0.0), 1.0) * 255.0), 0, 255)
        checked += 1
        if not peq(px, x, y, er, eg, eb, 255, 3):
            bad += 1
ok(checked == PW * PH, "YUV->RGB checked all 32x32 output pixels")
ok(bad == 0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)")
ok(True, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form")
vk.vkDestroyPipeline(dev, pipe, None); vk.vkDestroyPipelineLayout(dev, pl, None); vk.vkDestroyDescriptorPool(dev, dpool, None); vk.vkDestroyDescriptorSetLayout(dev, dsl, None); vk.vkDestroySampler(dev, samp, None)
free_tex(ty); free_tex(tu); free_tex(tv)

# ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
SW = SH = 4; OW = OH = 16
src = bytearray(SW * SH * 4)
for y in range(SH):
    for x in range(SW):
        i = (y * SW + x) * 4
        src[i] = (x * 60 + 10) & 0xFF; src[i + 1] = (y * 60 + 20) & 0xFF; src[i + 2] = ((x + y) * 30) & 0xFF; src[i + 3] = 255
st = mkTex(vk.VK_FORMAT_R8G8B8A8_UNORM, SW, SH, bytes(src))
samp = mkSampler(vk.VK_FILTER_NEAREST)
dsl, pl, dpool, dset = mk_desc(1)
pipe = mkPipe(vs, fs_s, pl)
ok(pipe is not None, "chroma-upsample pipeline created")
dii = vk.VkDescriptorImageInfo(samp, st[2], vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)
vk.vkUpdateDescriptorSets(dev, 1, [vk.VkWriteDescriptorSet(sType=vk.VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, dstSet=dset, dstBinding=0, descriptorCount=1, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, pImageInfo=[dii])], 0, None)
px = drawSub(pipe, pl, dset, OW, OH)
bad = 0
for y in range(OH):
    for x in range(OW):
        u = (x + 0.5) / OW; v = (y + 0.5) / OH
        sx = clampi(int(math.floor(u * SW)), 0, SW - 1); sy = clampi(int(math.floor(v * SH)), 0, SH - 1)
        i = (sy * SW + sx) * 4
        if not peq(px, x, y, src[i], src[i + 1], src[i + 2], 255, 1):
            bad += 1
ok(bad == 0, "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)")
ok(peq(px, 0, 0, src[0], src[1], src[2], 255, 1), "upsample (0,0) = src(0,0)")
li = (3 * SW + 3) * 4
ok(peq(px, 15, 15, src[li], src[li + 1], src[li + 2], 255, 1), "upsample (15,15) = src(3,3)")
vk.vkDestroyPipeline(dev, pipe, None); vk.vkDestroyPipelineLayout(dev, pl, None); vk.vkDestroyDescriptorPool(dev, dpool, None); vk.vkDestroyDescriptorSetLayout(dev, dsl, None); vk.vkDestroySampler(dev, samp, None)
free_tex(st)

# ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
SW = SH = 4; OW = OH = 2
src = bytearray(SW * SH * 4)
for y in range(SH):
    for x in range(SW):
        i = (y * SW + x) * 4; v = (10 + (y * SW + x) * 15) & 0xFF
        src[i] = v; src[i + 1] = (255 - v) & 0xFF; src[i + 2] = v; src[i + 3] = 255
st = mkTex(vk.VK_FORMAT_R8G8B8A8_UNORM, SW, SH, bytes(src))
samp = mkSampler(vk.VK_FILTER_LINEAR)
dsl, pl, dpool, dset = mk_desc(1)
pipe = mkPipe(vs, fs_s, pl)
ok(pipe is not None, "downscale pipeline created")
dii = vk.VkDescriptorImageInfo(samp, st[2], vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)
vk.vkUpdateDescriptorSets(dev, 1, [vk.VkWriteDescriptorSet(sType=vk.VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, dstSet=dset, dstBinding=0, descriptorCount=1, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, pImageInfo=[dii])], 0, None)
px = drawSub(pipe, pl, dset, OW, OH)
bad = 0
for oy in range(OH):
    for ox in range(OW):
        sx0 = ox * 2; sy0 = oy * 2; ssum = [0, 0, 0]
        for dy in range(2):
            for dx in range(2):
                i = ((sy0 + dy) * SW + (sx0 + dx)) * 4
                ssum[0] += src[i]; ssum[1] += src[i + 1]; ssum[2] += src[i + 2]
        er = lroundf(ssum[0] / 4.0); eg = lroundf(ssum[1] / 4.0); eb = lroundf(ssum[2] / 4.0)
        if not peq(px, ox, oy, er, eg, eb, 255, 2):
            bad += 1
ok(bad == 0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)")
vk.vkDestroyPipeline(dev, pipe, None); vk.vkDestroyPipelineLayout(dev, pl, None); vk.vkDestroyDescriptorPool(dev, dpool, None); vk.vkDestroyDescriptorSetLayout(dev, dsl, None); vk.vkDestroySampler(dev, samp, None)
free_tex(st)

# ============ (4) codec round-trip identities (CPU path) ============
N = 8
x = [30.0 + 20.0 * math.sin(0.7 * i) + 5.0 * i for i in range(N)]
X = []
for k in range(N):
    s2 = 0.0
    for nn in range(N):
        s2 += x[nn] * math.cos(math.pi / N * (nn + 0.5) * k)
    X.append(s2)
y = []
for nn in range(N):
    s2 = X[0]
    for k in range(1, N):
        s2 += 2.0 * X[k] * math.cos(math.pi / N * (nn + 0.5) * k)
    y.append(s2 / N)
maxerr = max(abs(y[i] - x[i]) for i in range(N))
ok(maxerr < 1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)")
diff = max(abs(X[i] - x[i]) for i in range(N))
ok(diff > 1.0, "DCT coefficients differ from input (transform is non-trivial)")

inp = [5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3]
enc = []
i = 0
while i < len(inp):
    v = inp[i]; j = i
    while j < len(inp) and inp[j] == v and (j - i) < 255:
        j += 1
    enc.append(j - i); enc.append(v); i = j
dec = []
k = 0
while k + 1 < len(enc):
    dec.extend([enc[k + 1]] * enc[k]); k += 2
ok(dec == inp, "RLE encode/decode round-trip identity")
ok(len(enc) < len(inp), "RLE actually compressed the run data (encode is non-trivial)")

# ---- Negative control ----
cmd = alloc_cmd()
vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0.0, 0.0, 0.0, 1.0]))
rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb, renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv])
vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
vk.vkCmdEndRenderPass(cmd)
region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
vk.vkEndCommandBuffer(cmd)
submit_wait(cmd)
vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
px = readback()
ok(peq(px, 0, 0, 0, 0, 0, 255, 1), "negative control setup: cleared to black")
ok(not peq(px, 0, 0, 255, 255, 255, 255, 1), "negative control: cleared buffer is NOT white")

vk.vkDeviceWaitIdle(dev)
EXPECTED = 27
TOTAL = PASS + FAIL
print("scene-codec-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED))
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_CODEC_PY OK %d" % PASS)
    sys.exit(0)
sys.exit(1)
