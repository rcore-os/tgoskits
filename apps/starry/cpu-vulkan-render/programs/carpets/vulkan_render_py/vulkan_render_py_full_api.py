#!/usr/bin/env python3
# vulkan_render_py_full_api.py - Vulkan RENDER carpet on Mesa lavapipe (software Vulkan on the CPU, no
# GPU/window/surface/swapchain), Python cffi binding (`import vulkan as vk`) of the same offscreen render
# pipeline as the C and C++ cells. Builds an offscreen render pass into an R8G8B8A8_UNORM color image,
# draws through a real graphics pipeline (SPIR-V vertex+fragment shaders loaded from shaders/*.spv), copies
# the image to a host-visible buffer with vkCmdCopyImageToBuffer, maps it via vk.ffi into a numpy
# (H,W,4) uint8 array, and checks every pixel against a closed-form numpy reference for: render-pass
# clear, a solid quad (push-constant color), a per-vertex axis-aligned linear gradient (a triangle-strip
# quad interpolates per-triangle, so only an axis-aligned gradient matches a full-quad closed form), a
# gl_FragCoord checkerboard, a dynamic scissor, alpha blending (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all
# channels incl alpha), a sub-rectangle readback, and a negative control. Exhaustive per-API coverage
# builds a pipeline per state: primitive topologies (VkPrimitiveTopology TRIANGLE_LIST/TRIANGLE_FAN/
# LINE_LIST/LINE_STRIP/POINT_LIST; POINT uses point_vert which writes gl_PointSize), a blend factor+op
# matrix (VkBlendFactor ONE/ZERO, ONE/ONE, ZERO/ONE, DST_COLOR; VkBlendOp ADD/MAX/REVERSE_SUBTRACT), the
# full depth-func matrix (all 8 VkCompareOp against a D32_SFLOAT attachment; Vulkan NDC z in [0,1] so a
# z=0.5 quad vs clear-depth 0.75), face culling + winding (VkCullModeFlags NONE/FRONT_AND_BACK/BACK x
# VkFrontFace CCW-vs-CW), a color write mask (VkColorComponentFlags), format+device property queries, and
# a 2x2 texture upload + NEAREST sampling through a combined image sampler + descriptor set. Prints
# "VULKAN_RENDER_PY_FULL_API OK <n>" only when every assertion passes and count == EXPECTED. Mirrors the
# verified C cell (VULKAN_RENDER_C_FULL_API OK 68).
import sys, os

try:
    import vulkan as vk
except Exception as e:  # noqa: BLE001
    print("VULKAN_RENDER_PY_FULL_API unavailable: import vulkan failed: %s" % e)
    sys.exit(1)
try:
    import numpy as np
except Exception as e:  # noqa: BLE001
    print("VULKAN_RENDER_PY_FULL_API unavailable: import numpy failed: %s" % e)
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
    print("VULKAN_RENDER_PY_FULL_API unavailable: %s" % msg)
    sys.exit(1)


# ---- pixel helpers over an (H,W,4) uint8 numpy image ----
def peq(px, x, y, r, g, b, a, tol):
    p = px[y, x]
    return (abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol
            and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - a) <= tol)


def all_eq(px, r, g, b, a, tol):
    ref = np.array([r, g, b, a], dtype=np.int32)
    return bool(np.all(np.abs(px.astype(np.int32) - ref) <= tol))


# ---- Vulkan bring-up ----
try:
    aiapp = vk.VkApplicationInfo(sType=vk.VK_STRUCTURE_TYPE_APPLICATION_INFO,
                                 apiVersion=vk.VK_MAKE_VERSION(1, 1, 0))
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

qci = vk.VkDeviceQueueCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                 queueFamilyIndex=qfam, queueCount=1, pQueuePriorities=[1.0])
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
    ci = vk.VkShaderModuleCreateInfo(sType=vk.VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     codeSize=len(code), pCode=code)
    return vk.vkCreateShaderModule(dev, ci, None)


# ---- color image + render pass + framebuffer ----
ii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D,
                          format=vk.VK_FORMAT_R8G8B8A8_UNORM, extent=vk.VkExtent3D(W, H, 1),
                          mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT,
                          tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                          usage=vk.VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | vk.VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
                          initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
cimg = vk.vkCreateImage(dev, ii, None)
ok(cimg is not None, "vkCreateImage color")
imr = vk.vkGetImageMemoryRequirements(dev, cimg)
iai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=imr.size,
                              memoryTypeIndex=memtype(imr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
cmem = vk.vkAllocateMemory(dev, iai, None)
vk.vkBindImageMemory(dev, cimg, cmem, 0)
cvi = vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=cimg,
                               viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                               subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1))
cview = vk.vkCreateImageView(dev, cvi, None)
ok(cview is not None, "vkCreateImageView")

att = vk.VkAttachmentDescription(format=vk.VK_FORMAT_R8G8B8A8_UNORM, samples=vk.VK_SAMPLE_COUNT_1_BIT,
                                 loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_STORE,
                                 stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                                 stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                 initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED,
                                 finalLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL)
ref = vk.VkAttachmentReference(0, vk.VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL)
sp = vk.VkSubpassDescription(pipelineBindPoint=vk.VK_PIPELINE_BIND_POINT_GRAPHICS,
                             colorAttachmentCount=1, pColorAttachments=[ref])
rpi = vk.VkRenderPassCreateInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,
                                pAttachments=[att], pSubpasses=[sp])
rp = vk.vkCreateRenderPass(dev, rpi, None)
ok(rp is not None, "vkCreateRenderPass")
fbi = vk.VkFramebufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, renderPass=rp,
                                 pAttachments=[cview], width=W, height=H, layers=1)
fb = vk.vkCreateFramebuffer(dev, fbi, None)
ok(fb is not None, "vkCreateFramebuffer")

# ---- depth resources for the depth-func matrix ----
dii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D,
                           format=vk.VK_FORMAT_D32_SFLOAT, extent=vk.VkExtent3D(W, H, 1),
                           mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT,
                           tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                           usage=vk.VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT,
                           initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
dimg = vk.vkCreateImage(dev, dii, None)
ok(dimg is not None, "vkCreateImage depth")
dmr = vk.vkGetImageMemoryRequirements(dev, dimg)
daii = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=dmr.size,
                               memoryTypeIndex=memtype(dmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
dmem = vk.vkAllocateMemory(dev, daii, None)
vk.vkBindImageMemory(dev, dimg, dmem, 0)
dvi = vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=dimg,
                               viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_D32_SFLOAT,
                               subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1))
dview = vk.vkCreateImageView(dev, dvi, None)
ok(dview is not None, "vkCreateImageView depth")
datt0 = vk.VkAttachmentDescription(format=vk.VK_FORMAT_R8G8B8A8_UNORM, samples=vk.VK_SAMPLE_COUNT_1_BIT,
                                   loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_STORE,
                                   stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                                   stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED,
                                   finalLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL)
datt1 = vk.VkAttachmentDescription(format=vk.VK_FORMAT_D32_SFLOAT, samples=vk.VK_SAMPLE_COUNT_1_BIT,
                                   loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                                   stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED,
                                   finalLayout=vk.VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
dcref = vk.VkAttachmentReference(0, vk.VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL)
ddref = vk.VkAttachmentReference(1, vk.VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
dsp = vk.VkSubpassDescription(pipelineBindPoint=vk.VK_PIPELINE_BIND_POINT_GRAPHICS,
                              colorAttachmentCount=1, pColorAttachments=[dcref], pDepthStencilAttachment=[ddref])
drpi = vk.VkRenderPassCreateInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,
                                 pAttachments=[datt0, datt1], pSubpasses=[dsp])
rp_d = vk.vkCreateRenderPass(dev, drpi, None)
ok(rp_d is not None, "vkCreateRenderPass depth")
dfbi = vk.VkFramebufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, renderPass=rp_d,
                                  pAttachments=[cview, dview], width=W, height=H, layers=1)
fb_d = vk.vkCreateFramebuffer(dev, dfbi, None)
ok(fb_d is not None, "vkCreateFramebuffer depth")

# ---- readback buffer (host-visible), mapped once, viewed via numpy each frame ----
rbi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=W * H * 4,
                            usage=vk.VK_BUFFER_USAGE_TRANSFER_DST_BIT)
rbuf = vk.vkCreateBuffer(dev, rbi, None)
rmr = vk.vkGetBufferMemoryRequirements(dev, rbuf)
rai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=rmr.size,
                              memoryTypeIndex=memtype(rmr.memoryTypeBits,
                                                      vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
rmem = vk.vkAllocateMemory(dev, rai, None)
vk.vkBindBufferMemory(dev, rbuf, rmem, 0)
rmap = vk.vkMapMemory(dev, rmem, 0, W * H * 4, 0)


def readback():
    return np.frombuffer(rmap, dtype=np.uint8, count=W * H * 4).reshape(H, W, 4).copy()


pci = vk.VkCommandPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, queueFamilyIndex=qfam,
                                 flags=vk.VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT)
pool = vk.vkCreateCommandPool(dev, pci, None)
ok(pool is not None, "vkCreateCommandPool")


# ---- pipeline layouts ----
def mkLayout(push_const):
    if push_const:
        pcr = vk.VkPushConstantRange(vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16)
        li = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
                                           pPushConstantRanges=[pcr])
    else:
        li = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO)
    return vk.vkCreatePipelineLayout(dev, li, None)


# ---- vertex buffer helper ----
def mkVbo(data_f32):
    arr = np.asarray(data_f32, dtype=np.float32)
    sz = arr.nbytes
    bi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=sz,
                               usage=vk.VK_BUFFER_USAGE_VERTEX_BUFFER_BIT)
    b = vk.vkCreateBuffer(dev, bi, None)
    mr = vk.vkGetBufferMemoryRequirements(dev, b)
    ai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=mr.size,
                                 memoryTypeIndex=memtype(mr.memoryTypeBits,
                                                         vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
    m = vk.vkAllocateMemory(dev, ai, None)
    vk.vkBindBufferMemory(dev, b, m, 0)
    p = vk.vkMapMemory(dev, m, 0, sz, 0)
    ffi.memmove(p, arr.tobytes(), sz)
    vk.vkUnmapMemory(dev, m)
    return b, m


# ---- base pipeline builder (mirrors C mkPipe) ----
def mkPipe(vs, fs, pl, with_color_attr, blend):
    st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                                             stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
          vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                                             stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
    stride = 24 if with_color_attr else 8
    bind = vk.VkVertexInputBindingDescription(0, stride, vk.VK_VERTEX_INPUT_RATE_VERTEX)
    attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32_SFLOAT, 0)]
    if with_color_attr:
        attr.append(vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32B32A32_SFLOAT, 8))
    vi = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
                                                 pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=attr)
    ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
                                                   topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP)
    vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
    sc = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
    vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
                                               pViewports=[vp], pScissors=[sc])
    dyn = vk.VkPipelineDynamicStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
                                              pDynamicStates=[vk.VK_DYNAMIC_STATE_SCISSOR])
    rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
                                                   polygonMode=vk.VK_POLYGON_MODE_FILL,
                                                   cullMode=vk.VK_CULL_MODE_NONE, lineWidth=1.0)
    ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
                                                 rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
    cba = vk.VkPipelineColorBlendAttachmentState(
        blendEnable=vk.VK_TRUE if blend else vk.VK_FALSE,
        srcColorBlendFactor=vk.VK_BLEND_FACTOR_SRC_ALPHA, dstColorBlendFactor=vk.VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
        colorBlendOp=vk.VK_BLEND_OP_ADD,
        srcAlphaBlendFactor=vk.VK_BLEND_FACTOR_SRC_ALPHA, dstAlphaBlendFactor=vk.VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
        alphaBlendOp=vk.VK_BLEND_OP_ADD, colorWriteMask=0xF)
    cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
                                                pAttachments=[cba])
    gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st,
                                         pVertexInputState=vi, pInputAssemblyState=ia, pViewportState=vps,
                                         pRasterizationState=rs, pMultisampleState=ms, pColorBlendState=cb,
                                         pDynamicState=dyn, layout=pl, renderPass=rp, subpass=0)
    return vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]


# ---- rich pipeline builder (mirrors C mkPipe2) ----
def mkPipe2(vs, fs, pl, vlayout, topo, blend, sC, dC, oC, sA, dA, oA, cull, front, depth_test, depth_op, rp_use, cwmask):
    st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                                             stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
          vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                                             stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
    stride = {0: 8, 1: 24, 2: 12, 3: 16}[vlayout]
    bind = vk.VkVertexInputBindingDescription(0, stride, vk.VK_VERTEX_INPUT_RATE_VERTEX)
    if vlayout == 2:
        attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32B32_SFLOAT, 0)]
    else:
        attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32_SFLOAT, 0)]
    if vlayout == 1:
        attr.append(vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32B32A32_SFLOAT, 8))
    elif vlayout == 3:
        attr.append(vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32_SFLOAT, 8))
    vi = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
                                                 pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=attr)
    ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
                                                   topology=topo)
    vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
    sc = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
    vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
                                               pViewports=[vp], pScissors=[sc])
    dyn = vk.VkPipelineDynamicStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
                                              pDynamicStates=[vk.VK_DYNAMIC_STATE_SCISSOR])
    rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
                                                   polygonMode=vk.VK_POLYGON_MODE_FILL, cullMode=cull,
                                                   frontFace=front, lineWidth=1.0)
    ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
                                                 rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
    dss = vk.VkPipelineDepthStencilStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
                                                   depthTestEnable=vk.VK_TRUE if depth_test else vk.VK_FALSE,
                                                   depthWriteEnable=vk.VK_TRUE if depth_test else vk.VK_FALSE,
                                                   depthCompareOp=depth_op, minDepthBounds=0.0, maxDepthBounds=1.0)
    cba = vk.VkPipelineColorBlendAttachmentState(blendEnable=vk.VK_TRUE if blend else vk.VK_FALSE,
                                                 srcColorBlendFactor=sC, dstColorBlendFactor=dC, colorBlendOp=oC,
                                                 srcAlphaBlendFactor=sA, dstAlphaBlendFactor=dA, alphaBlendOp=oA,
                                                 colorWriteMask=cwmask)
    cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
                                                pAttachments=[cba])
    gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st,
                                         pVertexInputState=vi, pInputAssemblyState=ia, pViewportState=vps,
                                         pRasterizationState=rs, pMultisampleState=ms, pColorBlendState=cb,
                                         pDepthStencilState=dss, pDynamicState=dyn, layout=pl, renderPass=rp_use, subpass=0)
    return vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]


def submit_wait(cmd):
    si = vk.VkSubmitInfo(sType=vk.VK_STRUCTURE_TYPE_SUBMIT_INFO, pCommandBuffers=[cmd])
    fi = vk.VkFenceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FENCE_CREATE_INFO)
    fence = vk.vkCreateFence(dev, fi, None)
    vk.vkQueueSubmit(q, 1, [si], fence)
    vk.vkWaitForFences(dev, 1, [fence], vk.VK_TRUE, 0xFFFFFFFFFFFFFFFF)
    vk.vkDestroyFence(dev, fence, None)


def alloc_cmd():
    cai = vk.VkCommandBufferAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                         commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                         commandBufferCount=1)
    return vk.vkAllocateCommandBuffers(dev, cai)[0]


def frame(clear, pipe, pl, push_color, vbo, verts, scissor):
    cmd = alloc_cmd()
    bi = vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                                     flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT)
    vk.vkBeginCommandBuffer(cmd, bi)
    cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=clear))
    rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb,
                                   renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)),
                                   pClearValues=[cv])
    vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
    if pipe is not None:
        vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe)
        vk.vkCmdSetScissor(cmd, 0, 1, [scissor])
        if push_color is not None:
            data = np.asarray(push_color, dtype=np.float32).tobytes()
            vk.vkCmdPushConstants(cmd, pl, vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, ffi.from_buffer(data))
        if vbo is not None:
            vk.vkCmdBindVertexBuffers(cmd, 0, 1, [vbo], [0])
        vk.vkCmdDraw(cmd, verts, 1, 0, 0)
    vk.vkCmdEndRenderPass(cmd)
    region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1),
                                  imageExtent=vk.VkExtent3D(W, H, 1))
    vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
    vk.vkEndCommandBuffer(cmd)
    submit_wait(cmd)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    return readback()


def frameD(clear, depth_clear, pipe, pl, push_color, vbo, verts):
    cmd = alloc_cmd()
    bi = vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                                     flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT)
    vk.vkBeginCommandBuffer(cmd, bi)
    cv0 = vk.VkClearValue(color=vk.VkClearColorValue(float32=clear))
    cv1 = vk.VkClearValue(depthStencil=vk.VkClearDepthStencilValue(depth=depth_clear, stencil=0))
    rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp_d, framebuffer=fb_d,
                                   renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)),
                                   pClearValues=[cv0, cv1])
    vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
    vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe)
    vk.vkCmdSetScissor(cmd, 0, 1, [vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))])
    if push_color is not None:
        data = np.asarray(push_color, dtype=np.float32).tobytes()
        vk.vkCmdPushConstants(cmd, pl, vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, ffi.from_buffer(data))
    if vbo is not None:
        vk.vkCmdBindVertexBuffers(cmd, 0, 1, [vbo], [0])
    vk.vkCmdDraw(cmd, verts, 1, 0, 0)
    vk.vkCmdEndRenderPass(cmd)
    region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1),
                                  imageExtent=vk.VkExtent3D(W, H, 1))
    vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
    vk.vkEndCommandBuffer(cmd)
    submit_wait(cmd)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    return readback()


# ---- shader modules ----
vs_s = shmod("solid_vert.spv"); fs_s = shmod("solid_frag.spv")
vs_g = shmod("grad_vert.spv"); fs_g = shmod("grad_frag.spv")
fs_c = shmod("check_frag.spv")
vs_pt = shmod("point_vert.spv"); vs_p3 = shmod("pos3_vert.spv")
vs_tx = shmod("tex_vert.spv"); fs_tx = shmod("tex_frag.spv")

pl_push = mkLayout(1); pl_none = mkLayout(0)
pipe_solid = mkPipe(vs_s, fs_s, pl_push, 0, 0)
pipe_blend = mkPipe(vs_s, fs_s, pl_push, 0, 1)
pipe_grad = mkPipe(vs_g, fs_g, pl_none, 1, 0)
pipe_check = mkPipe(vs_s, fs_c, pl_none, 0, 0)
ok(pipe_solid and pipe_grad and pipe_check and pipe_blend, "graphics pipelines created")

quad = [-1, -1, 1, -1, -1, 1, 1, 1]
gquad = [-1, -1, 1, 0, 0, 1,  1, -1, 0, 0, 1, 1,  -1, 1, 1, 0, 0, 1,  1, 1, 0, 0, 1, 1]
vbo, qm = mkVbo(quad)
gvbo, gm = mkVbo(gquad)
full = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))

# ---- base tests ----
px = frame([0.0, 0.25, 0.5, 1.0], None, pl_none, None, None, 0, full)
ok(all_eq(px, 0, 64, 128, 255, 2), "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)")
ok(peq(px, 0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)")

px = frame([0, 0, 0, 1], pipe_solid, pl_push, [1, 0, 0, 1], vbo, 4, full)
ok(all_eq(px, 255, 0, 0, 255, 1), "solid red quad fills every pixel")

px = frame([0, 0, 0, 1], pipe_grad, pl_none, None, gvbo, 4, full)
xs = np.arange(W)
u = (xs + 0.5) / W
r_ref = np.rint((1.0 - u) * 255.0).astype(np.int32)
b_ref = np.rint(u * 255.0).astype(np.int32)
bad = 0
pxi = px.astype(np.int32)
for x in range(W):
    col_r = pxi[:, x, 0]; col_g = pxi[:, x, 1]; col_b = pxi[:, x, 2]; col_a = pxi[:, x, 3]
    if not (np.all(np.abs(col_r - r_ref[x]) <= 4) and np.all(np.abs(col_g - 0) <= 4)
            and np.all(np.abs(col_b - b_ref[x]) <= 4) and np.all(np.abs(col_a - 255) <= 4)):
        bad += 1
ok(bad == 0, "gradient matches horizontal-linear closed-form for all pixels")
ok(peq(px, 0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red")
ok(peq(px, W - 1, H - 1, 0, 0, 255, 255, 8), "gradient right edge ~ blue")
ok(peq(px, W // 2, H // 2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)")

px = frame([0, 0, 0, 1], pipe_check, pl_none, None, vbo, 4, full)
bad = 0
for y in range(H):
    for x in range(W):
        e = (((x >> 3) + (y >> 3)) & 1) == 0
        w = 255 if e else 0
        if not peq(px, x, y, w, w, w, 255, 1):
            bad += 1
ok(bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels")
ok(peq(px, 0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white")
ok(peq(px, 8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black")

box = vk.VkRect2D(vk.VkOffset2D(16, 16), vk.VkExtent2D(32, 32))
px = frame([1, 0, 0, 1], pipe_solid, pl_push, [0, 1, 0, 1], vbo, 4, box)
ok(peq(px, 32, 32, 0, 255, 0, 255, 1), "scissor: inside box green")
ok(peq(px, 2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)")
ok(peq(px, 50, 50, 255, 0, 0, 255, 1), "scissor: past box red")

px = frame([1, 0, 0, 1], pipe_blend, pl_push, [0, 0, 1, 0.5], vbo, 4, full)
ok(all_eq(px, 128, 0, 128, 191, 3), "alpha blend 0.5*blue over red -> rgb(128,0,128) a191")

px = frame([0, 0, 0, 1], pipe_solid, pl_push, [0.2, 0.4, 0.6, 1.0], vbo, 4, full)
s = all(peq(px, x, y, 51, 102, 153, 255, 2) for y in range(10, 14) for x in range(10, 14))
ok(s, "sub-rect (10,10,4x4) == (51,102,153,255)")

# ==================== exhaustive per-API render coverage ====================
NOBLEND = (0, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD,
           vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD)
CCWF = vk.VK_FRONT_FACE_COUNTER_CLOCKWISE
red = [1, 0, 0, 1]

# Test 8: primitive topologies
tl = [-1, -1, 1, -1, -1, 1,  -1, 1, 1, -1, 1, 1]
fan = [0, 0, -1, -1, 1, -1, 1, 1, -1, 1, -1, -1]
hln = [-1, 0, 1, 0]
pt = [0, 0]
b_tl, m1 = mkVbo(tl); b_fan, m2 = mkVbo(fan); b_ln, m3 = mkVbo(hln); b_pt, m4 = mkVbo(pt)
p_tl = mkPipe2(vs_s, fs_s, pl_push, 0, vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
p_fan = mkPipe2(vs_s, fs_s, pl_push, 0, vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_FAN, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
p_ll = mkPipe2(vs_s, fs_s, pl_push, 0, vk.VK_PRIMITIVE_TOPOLOGY_LINE_LIST, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
p_ls = mkPipe2(vs_s, fs_s, pl_push, 0, vk.VK_PRIMITIVE_TOPOLOGY_LINE_STRIP, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
p_pt = mkPipe2(vs_pt, fs_s, pl_push, 0, vk.VK_PRIMITIVE_TOPOLOGY_POINT_LIST, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
ok(p_tl and p_fan and p_ll and p_ls and p_pt, "topology pipelines created")
px = frame([0, 0, 0, 1], p_tl, pl_push, red, b_tl, 6, full); ok(all_eq(px, 255, 0, 0, 255, 1), "TRIANGLE_LIST fills quad")
px = frame([0, 0, 0, 1], p_fan, pl_push, red, b_fan, 6, full); ok(all_eq(px, 255, 0, 0, 255, 1), "TRIANGLE_FAN fills quad")
px = frame([0, 0, 0, 1], p_ll, pl_push, red, b_ln, 2, full)
mid = sum(1 for x in range(W) if peq(px, x, H // 2, 255, 0, 0, 255, 2) or peq(px, x, H // 2 - 1, 255, 0, 0, 255, 2))
ok(mid >= W - 2, "LINE_LIST draws the middle row")
ok(peq(px, 0, 0, 0, 0, 0, 255, 2), "LINE_LIST leaves top row clear")
px = frame([0, 0, 0, 1], p_ls, pl_push, red, b_ln, 2, full)
mid = sum(1 for x in range(W) if peq(px, x, H // 2, 255, 0, 0, 255, 2) or peq(px, x, H // 2 - 1, 255, 0, 0, 255, 2))
ok(mid >= W - 2, "LINE_STRIP draws the middle row")
px = frame([0, 0, 0, 1], p_pt, pl_push, red, b_pt, 1, full)
hit = any(peq(px, x, y, 255, 0, 0, 255, 2) for y in range(H // 2 - 2, H // 2 + 3) for x in range(W // 2 - 2, W // 2 + 3))
ok(hit, "POINT_LIST draws a pixel at the center")
for p in (p_tl, p_fan, p_ll, p_ls, p_pt):
    vk.vkDestroyPipeline(dev, p, None)
for b, m in ((b_tl, m1), (b_fan, m2), (b_ln, m3), (b_pt, m4)):
    vk.vkDestroyBuffer(dev, b, None); vk.vkFreeMemory(dev, m, None)

# Test 9: blend factor + op matrix
TS = vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP
pb1 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0.5, 0.5, 0.5, 1], pb1, pl_push, [0, 0, 1, 1], vbo, 4, full); ok(all_eq(px, 0, 0, 255, 255, 2), "blend ONE/ZERO: src replaces dst"); vk.vkDestroyPipeline(dev, pb1, None)
pb2 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_ADD, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_ADD, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0.5, 0, 0, 1], pb2, pl_push, [0, 0, 0.5, 1], vbo, 4, full); ok(all_eq(px, 128, 0, 128, 255, 2), "blend ONE/ONE ADD: src+dst = (128,0,128)"); vk.vkDestroyPipeline(dev, pb2, None)
pb3 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_ADD, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_ADD, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0.2, 0, 0, 1], pb3, pl_push, [0, 1, 0, 1], vbo, 4, full); ok(all_eq(px, 51, 0, 0, 255, 2), "blend ZERO/ONE: dst kept (51,0,0)"); vk.vkDestroyPipeline(dev, pb3, None)
pb4 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_DST_COLOR, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD, vk.VK_BLEND_FACTOR_DST_COLOR, vk.VK_BLEND_FACTOR_ZERO, vk.VK_BLEND_OP_ADD, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0.5, 0.5, 0.5, 1], pb4, pl_push, [0, 0, 1, 1], vbo, 4, full); ok(all_eq(px, 0, 0, 128, 255, 2), "blend DST_COLOR/ZERO: src*dst modulate (0,0,128)"); vk.vkDestroyPipeline(dev, pb4, None)
pb5 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_MAX, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_MAX, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0.2, 0.6, 0.2, 1], pb5, pl_push, [0.6, 0.2, 0.6, 1], vbo, 4, full); ok(all_eq(px, 153, 153, 153, 255, 2), "blend op MAX: per-channel max"); vk.vkDestroyPipeline(dev, pb5, None)
pb6 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, 1, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_REVERSE_SUBTRACT, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_FACTOR_ONE, vk.VK_BLEND_OP_REVERSE_SUBTRACT, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([1, 0, 0, 1], pb6, pl_push, [0.25, 0, 0, 1], vbo, 4, full); ok(all_eq(px, 191, 0, 0, 0, 3), "blend op REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0"); vk.vkDestroyPipeline(dev, pb6, None)

# Test 10: depth-func matrix (Vulkan NDC z in [0,1]: quad z=0.5, clear depth 0.75)
dq = [-1, -1, 0.5, 1, -1, 0.5, -1, 1, 0.5, 1, 1, 0.5]
vbo3, dm3 = mkVbo(dq)
grn = [0, 1, 0, 1]
dt = [(vk.VK_COMPARE_OP_ALWAYS, 1, "ALWAYS"), (vk.VK_COMPARE_OP_NEVER, 0, "NEVER"), (vk.VK_COMPARE_OP_LESS, 1, "LESS"),
      (vk.VK_COMPARE_OP_LESS_OR_EQUAL, 1, "LEQUAL"), (vk.VK_COMPARE_OP_EQUAL, 0, "EQUAL"), (vk.VK_COMPARE_OP_GREATER, 0, "GREATER"),
      (vk.VK_COMPARE_OP_GREATER_OR_EQUAL, 0, "GEQUAL"), (vk.VK_COMPARE_OP_NOT_EQUAL, 1, "NOTEQUAL")]
for op, draws, name in dt:
    pdp = mkPipe2(vs_p3, fs_s, pl_push, 2, TS, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 1, op, rp_d, 0xF)
    px = frameD([0, 0, 0, 1], 0.75, pdp, pl_push, grn, vbo3, 4)
    ok(peq(px, W // 2, H // 2, 0, 255, 0, 255, 2) == bool(draws), name)
    vk.vkDestroyPipeline(dev, pdp, None)
vk.vkDestroyBuffer(dev, vbo3, None); vk.vkFreeMemory(dev, dm3, None)

# Test 11: face culling + winding
pcn = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0, 0, 0, 1], pcn, pl_push, red, vbo, 4, full); ok(all_eq(px, 255, 0, 0, 255, 1), "cull NONE: quad drawn"); vk.vkDestroyPipeline(dev, pcn, None)
pcb = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_FRONT_AND_BACK, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0, 0, 0, 1], pcb, pl_push, red, vbo, 4, full); ok(all_eq(px, 0, 0, 0, 255, 1), "cull FRONT_AND_BACK: nothing drawn"); vk.vkDestroyPipeline(dev, pcb, None)
pc1 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_BACK_BIT, vk.VK_FRONT_FACE_COUNTER_CLOCKWISE, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0, 0, 0, 1], pc1, pl_push, red, vbo, 4, full); ccw = peq(px, W // 2, H // 2, 255, 0, 0, 255, 2); vk.vkDestroyPipeline(dev, pc1, None)
pc2 = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_BACK_BIT, vk.VK_FRONT_FACE_CLOCKWISE, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0, 0, 0, 1], pc2, pl_push, red, vbo, 4, full); cw = peq(px, W // 2, H // 2, 255, 0, 0, 255, 2); vk.vkDestroyPipeline(dev, pc2, None)
ok(ccw != cw, "cull BACK: CCW vs CW winding flips visibility")

# Test 12: color write mask
white = [1, 1, 1, 1]
pmr = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, vk.VK_COLOR_COMPONENT_R_BIT)
px = frame([0, 0, 0, 1], pmr, pl_push, white, vbo, 4, full); ok(all_eq(px, 255, 0, 0, 255, 1), "colorWriteMask R only: white -> (255,0,0,255)"); vk.vkDestroyPipeline(dev, pmr, None)
pma = mkPipe2(vs_s, fs_s, pl_push, 0, TS, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
px = frame([0, 0, 0, 1], pma, pl_push, white, vbo, 4, full); ok(all_eq(px, 255, 255, 255, 255, 1), "colorWriteMask RGBA: white -> (255,255,255,255)"); vk.vkDestroyPipeline(dev, pma, None)

# Test 13: format + device property queries
fp = vk.vkGetPhysicalDeviceFormatProperties(pd, vk.VK_FORMAT_R8G8B8A8_UNORM)
ok((fp.optimalTilingFeatures & vk.VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT) != 0, "R8G8B8A8_UNORM optimal-tiling COLOR_ATTACHMENT")
fpd = vk.vkGetPhysicalDeviceFormatProperties(pd, vk.VK_FORMAT_D32_SFLOAT)
ok((fpd.optimalTilingFeatures & vk.VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0, "D32_SFLOAT optimal-tiling DEPTH_STENCIL_ATTACHMENT")
props = vk.vkGetPhysicalDeviceProperties(pd)
ok(vk.VK_VERSION_MAJOR(props.apiVersion) >= 1, "device apiVersion major >= 1")
ok(props.limits.maxImageDimension2D >= W, "limits.maxImageDimension2D >= 64")

# Test 14: 2x2 texture upload + NEAREST sampling
dslb = vk.VkDescriptorSetLayoutBinding(binding=0, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                                       descriptorCount=1, stageFlags=vk.VK_SHADER_STAGE_FRAGMENT_BIT)
dslci = vk.VkDescriptorSetLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, pBindings=[dslb])
dsl = vk.vkCreateDescriptorSetLayout(dev, dslci, None)
ok(dsl is not None, "descriptor set layout")
plci = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pSetLayouts=[dsl])
pl_tex = vk.vkCreatePipelineLayout(dev, plci, None)
tii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D,
                           format=vk.VK_FORMAT_R8G8B8A8_UNORM, extent=vk.VkExtent3D(2, 2, 1), mipLevels=1, arrayLayers=1,
                           samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                           usage=vk.VK_IMAGE_USAGE_SAMPLED_BIT | vk.VK_IMAGE_USAGE_TRANSFER_DST_BIT,
                           initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
timg = vk.vkCreateImage(dev, tii, None)
ok(timg is not None, "texture image")
tmr = vk.vkGetImageMemoryRequirements(dev, timg)
tai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=tmr.size,
                              memoryTypeIndex=memtype(tmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
tmem = vk.vkAllocateMemory(dev, tai, None)
vk.vkBindImageMemory(dev, timg, tmem, 0)
texels = bytes([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255])
sbi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=16, usage=vk.VK_BUFFER_USAGE_TRANSFER_SRC_BIT)
sbuf = vk.vkCreateBuffer(dev, sbi, None)
smr = vk.vkGetBufferMemoryRequirements(dev, sbuf)
sai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=smr.size,
                              memoryTypeIndex=memtype(smr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
smem = vk.vkAllocateMemory(dev, sai, None)
vk.vkBindBufferMemory(dev, sbuf, smem, 0)
sp = vk.vkMapMemory(dev, smem, 0, 16, 0)
ffi.memmove(sp, texels, 16)
vk.vkUnmapMemory(dev, smem)
cmd = alloc_cmd()
vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
b1 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED,
                             newLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED,
                             dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=timg,
                             subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1),
                             srcAccessMask=0, dstAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT)
vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, None, 0, None, 1, [b1])
cp = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(2, 2, 1))
vk.vkCmdCopyBufferToImage(cmd, sbuf, timg, vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, [cp])
b2 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             newLayout=vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED,
                             dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=timg,
                             subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1),
                             srcAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT, dstAccessMask=vk.VK_ACCESS_SHADER_READ_BIT)
vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, vk.VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, None, 0, None, 1, [b2])
vk.vkEndCommandBuffer(cmd)
submit_wait(cmd)
vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
tvi = vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=timg, viewType=vk.VK_IMAGE_VIEW_TYPE_2D,
                               format=vk.VK_FORMAT_R8G8B8A8_UNORM, subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1))
tview = vk.vkCreateImageView(dev, tvi, None)
smci = vk.VkSamplerCreateInfo(sType=vk.VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, magFilter=vk.VK_FILTER_NEAREST, minFilter=vk.VK_FILTER_NEAREST,
                              addressModeU=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, addressModeV=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
                              addressModeW=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE)
samp = vk.vkCreateSampler(dev, smci, None)
dps = vk.VkDescriptorPoolSize(vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1)
dpci = vk.VkDescriptorPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, maxSets=1, pPoolSizes=[dps])
dpool = vk.vkCreateDescriptorPool(dev, dpci, None)
dsai = vk.VkDescriptorSetAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, descriptorPool=dpool, pSetLayouts=[dsl])
dset = vk.vkAllocateDescriptorSets(dev, dsai)[0]
dii2 = vk.VkDescriptorImageInfo(sampler=samp, imageView=tview, imageLayout=vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)
wds = vk.VkWriteDescriptorSet(sType=vk.VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, dstSet=dset, dstBinding=0, descriptorCount=1,
                              descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, pImageInfo=[dii2])
vk.vkUpdateDescriptorSets(dev, 1, [wds], 0, None)
ptp = mkPipe2(vs_tx, fs_tx, pl_tex, 3, TS, *NOBLEND, vk.VK_CULL_MODE_NONE, CCWF, 0, vk.VK_COMPARE_OP_ALWAYS, rp, 0xF)
ok(dsl and pl_tex and ptp and samp, "texture pipeline + descriptor created")
tq = [-1, -1, 0, 0, 1, -1, 1, 0, -1, 1, 0, 1, 1, 1, 1, 1]
tvbo, tqm = mkVbo(tq)
cmd = alloc_cmd()
vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0, 0, 0, 1]))
rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb,
                               renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv])
vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, ptp)
vk.vkCmdSetScissor(cmd, 0, 1, [vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))])
vk.vkCmdBindDescriptorSets(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pl_tex, 0, 1, [dset], 0, None)
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [tvbo], [0])
vk.vkCmdDraw(cmd, 4, 1, 0, 0)
vk.vkCmdEndRenderPass(cmd)
region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
vk.vkEndCommandBuffer(cmd)
submit_wait(cmd)
vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
px = readback()
ok(peq(px, W // 4, H // 4, 255, 0, 0, 255, 2), "texture NEAREST top-left red")
ok(peq(px, 3 * W // 4, H // 4, 0, 255, 0, 255, 2), "texture NEAREST top-right green")
ok(peq(px, W // 4, 3 * H // 4, 0, 0, 255, 255, 2), "texture NEAREST bottom-left blue")
ok(peq(px, 3 * W // 4, 3 * H // 4, 255, 255, 255, 255, 2), "texture NEAREST bottom-right white")
vk.vkDestroyPipeline(dev, ptp, None); vk.vkDestroyBuffer(dev, tvbo, None); vk.vkFreeMemory(dev, tqm, None)
vk.vkDestroySampler(dev, samp, None); vk.vkDestroyImageView(dev, tview, None); vk.vkDestroyImage(dev, timg, None); vk.vkFreeMemory(dev, tmem, None)
vk.vkDestroyBuffer(dev, sbuf, None); vk.vkFreeMemory(dev, smem, None)
vk.vkDestroyDescriptorPool(dev, dpool, None); vk.vkDestroyDescriptorSetLayout(dev, dsl, None); vk.vkDestroyPipelineLayout(dev, pl_tex, None)

# negative control
px = frame([0, 0, 0, 1], pipe_solid, pl_push, [1, 0, 0, 1], vbo, 4, full)
ok(not all_eq(px, 0, 255, 0, 255, 2), "negative control: red buffer is NOT green")
ok(not peq(px, 0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black")

vk.vkDeviceWaitIdle(dev)
EXPECTED = 68
TOTAL = PASS + FAIL
print("vulkan-render-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED))
if FAIL == 0 and TOTAL == EXPECTED:
    print("VULKAN_RENDER_PY_FULL_API OK %d" % PASS)
    sys.exit(0)
sys.exit(1)
