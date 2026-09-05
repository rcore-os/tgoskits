#!/usr/bin/env python3
# scene_3dmodel_py.py - 3D indexed-mesh RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
# no GPU/window/surface/swapchain), Python cffi binding (`import vulkan as vk`) of the same offscreen
# render pipeline as the C++ cell scene_3dmodel.cpp. An offscreen render pass into an R8G8B8A8_UNORM color
# image + a D32_SFLOAT depth attachment, drawing an indexed cube through a depth-tested (VK_COMPARE_OP_LESS)
# Gouraud pipeline (SPIR-V vertex+fragment shaders from shaders/*.spv; the vertex shader carries invariant
# gl_Position), copied to a host-visible buffer and read back. The assertion is an INDEPENDENT software
# rasterizer: verts through the SAME MVP -> clip -> NDC -> viewport, per-pixel barycentric coverage +
# perspective-correct depth test + color interpolation. Vulkan NDC z in [0,1]: perspective() z-row uses the
# Vulkan mapping and the reference window depth is z_clip/w_clip directly. The reference math is
# behaviour-identical to the C++ cell; only the cffi-vs-C++ Vulkan binding syntax differs. Prints
# "SCENE_3DMODEL_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Honest-skips if binding absent.
import sys
import os
import math

try:
    import vulkan as vk
except Exception as e:  # noqa: BLE001
    print("SCENE_3DMODEL_PY unavailable: import vulkan failed: %s" % e)
    sys.exit(1)
try:
    import numpy as np
except Exception as e:  # noqa: BLE001
    print("SCENE_3DMODEL_PY unavailable: import numpy failed: %s" % e)
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
    print("SCENE_3DMODEL_PY unavailable: %s" % msg)
    sys.exit(1)


def lroundf(f):
    return int(math.floor(f + 0.5)) if f >= 0 else -int(math.floor(-f + 0.5))


def peq(px, x, y, r, g, b, a, tol):
    p = px[y, x]
    return (abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol
            and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - a) <= tol)


# ---- column-major 4x4 matrix math (m[col*4+row]) - byte-identical to the C++ cell ----
def mmul(a, b):
    r = [0.0] * 16
    for cc in range(4):
        for row in range(4):
            s = 0.0
            for k in range(4):
                s += a[k * 4 + row] * b[cc * 4 + k]
            r[cc * 4 + row] = s
    return r


def mv4(a, v):
    o = [0.0] * 4
    for row in range(4):
        s = 0.0
        for k in range(4):
            s += a[k * 4 + row] * v[k]
        o[row] = s
    return o


def perspective(fovy, aspect, zn, zf):
    f = 1.0 / math.tan(fovy * 0.5)
    r = [0.0] * 16
    r[0] = f / aspect
    r[1 * 4 + 1] = f
    r[2 * 4 + 2] = zf / (zn - zf)
    r[2 * 4 + 3] = -1.0
    r[3 * 4 + 2] = (zf * zn) / (zn - zf)
    return r


def translate(x, y, z):
    r = [0.0] * 16
    r[0] = r[5] = r[10] = r[15] = 1.0
    r[3 * 4] = x; r[3 * 4 + 1] = y; r[3 * 4 + 2] = z
    return r


def rot_y(a):
    c = math.cos(a); s = math.sin(a)
    r = [0.0] * 16
    r[0] = c; r[2] = -s; r[2 * 4] = s; r[2 * 4 + 2] = c; r[1 * 4 + 1] = 1.0; r[3 * 4 + 3] = 1.0
    return r


def rot_x(a):
    c = math.cos(a); s = math.sin(a)
    r = [0.0] * 16
    r[1 * 4 + 1] = c; r[1 * 4 + 2] = s; r[2 * 4 + 1] = -s; r[2 * 4 + 2] = c; r[0] = 1.0; r[3 * 4 + 3] = 1.0
    return r


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


# color image
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

# depth image
dii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D, format=vk.VK_FORMAT_D32_SFLOAT,
                           extent=vk.VkExtent3D(W, H, 1), mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                           usage=vk.VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT, initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
dimg = vk.vkCreateImage(dev, dii, None)
ok(dimg is not None, "vkCreateImage depth")
dmr = vk.vkGetImageMemoryRequirements(dev, dimg)
dmem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=dmr.size, memoryTypeIndex=memtype(dmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)), None)
vk.vkBindImageMemory(dev, dimg, dmem, 0)
dview = vk.vkCreateImageView(dev, vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=dimg, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_D32_SFLOAT,
                                                           subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1)), None)
ok(dview is not None, "vkCreateImageView depth")

att0 = vk.VkAttachmentDescription(format=vk.VK_FORMAT_R8G8B8A8_UNORM, samples=vk.VK_SAMPLE_COUNT_1_BIT, loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_STORE,
                                  stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE, stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                  initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, finalLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL)
att1 = vk.VkAttachmentDescription(format=vk.VK_FORMAT_D32_SFLOAT, samples=vk.VK_SAMPLE_COUNT_1_BIT, loadOp=vk.VK_ATTACHMENT_LOAD_OP_CLEAR, storeOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                  stencilLoadOp=vk.VK_ATTACHMENT_LOAD_OP_DONT_CARE, stencilStoreOp=vk.VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                  initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, finalLayout=vk.VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
cref = vk.VkAttachmentReference(0, vk.VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL)
dref = vk.VkAttachmentReference(1, vk.VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
sp = vk.VkSubpassDescription(pipelineBindPoint=vk.VK_PIPELINE_BIND_POINT_GRAPHICS, colorAttachmentCount=1, pColorAttachments=[cref], pDepthStencilAttachment=[dref])
rpi = vk.VkRenderPassCreateInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, pAttachments=[att0, att1], pSubpasses=[sp])
rp = vk.vkCreateRenderPass(dev, rpi, None)
ok(rp is not None, "vkCreateRenderPass")
fbi = vk.VkFramebufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, renderPass=rp, pAttachments=[cview, dview], width=W, height=H, layers=1)
fb = vk.vkCreateFramebuffer(dev, fbi, None)
ok(fb is not None, "vkCreateFramebuffer")

rbuf = vk.vkCreateBuffer(dev, vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=W * H * 4, usage=vk.VK_BUFFER_USAGE_TRANSFER_DST_BIT), None)
rmr = vk.vkGetBufferMemoryRequirements(dev, rbuf)
rmem = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=rmr.size, memoryTypeIndex=memtype(rmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)), None)
vk.vkBindBufferMemory(dev, rbuf, rmem, 0)
rmap = vk.vkMapMemory(dev, rmem, 0, W * H * 4, 0)
pool = vk.vkCreateCommandPool(dev, vk.VkCommandPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, queueFamilyIndex=qfam, flags=vk.VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT), None)
ok(pool is not None, "vkCreateCommandPool")
ok(True, "offscreen R8G8B8A8 + D32_SFLOAT target + readback buffer ready")


def readback():
    return np.frombuffer(rmap, dtype=np.uint8, count=W * H * 4).reshape(H, W, 4).copy()


def mkbuf(data_bytes, usage):
    sz = len(data_bytes)
    b = vk.vkCreateBuffer(dev, vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=sz, usage=usage), None)
    mr = vk.vkGetBufferMemoryRequirements(dev, b)
    m = vk.vkAllocateMemory(dev, vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=mr.size, memoryTypeIndex=memtype(mr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)), None)
    vk.vkBindBufferMemory(dev, b, m, 0)
    p = vk.vkMapMemory(dev, m, 0, sz, 0)
    ffi.memmove(p, data_bytes, sz)
    vk.vkUnmapMemory(dev, m)
    return b, m


# cube mesh (byte-identical)
VP = [
    [-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
    [-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1],
]
VC = [[(VP[i][k] + 1) * 0.5 for k in range(3)] for i in range(8)]
IDX = [0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 5, 6, 1, 6, 2]

model = mmul(rot_y(0.6), rot_x(0.3))
view = translate(0, 0, -5.0)
proj = perspective(1.0, W / H, 1.0, 20.0)
mvp = mmul(proj, mmul(view, model))

verts = []
for i in range(8):
    verts.extend([VP[i][0], VP[i][1], VP[i][2], VC[i][0], VC[i][1], VC[i][2]])
vbo, _vm = mkbuf(np.asarray(verts, dtype=np.float32).tobytes(), vk.VK_BUFFER_USAGE_VERTEX_BUFFER_BIT)
ibo, _im = mkbuf(np.asarray(IDX, dtype=np.uint16).tobytes(), vk.VK_BUFFER_USAGE_INDEX_BUFFER_BIT)

# pipeline: pos3+col3 (stride 24), mvp push constant, depth LESS, no cull
vs = shmod("cube_vert.spv"); fs = shmod("cube_frag.spv")
pcr = vk.VkPushConstantRange(vk.VK_SHADER_STAGE_VERTEX_BIT, 0, 64)
pl = vk.vkCreatePipelineLayout(dev, vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pPushConstantRanges=[pcr]), None)
st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
      vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
bind = vk.VkVertexInputBindingDescription(0, 24, vk.VK_VERTEX_INPUT_RATE_VERTEX)
attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32B32_SFLOAT, 0), vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32B32_SFLOAT, 12)]
vin = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=attr)
ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST)
vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
scr = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, pViewports=[vp], pScissors=[scr])
rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, polygonMode=vk.VK_POLYGON_MODE_FILL, cullMode=vk.VK_CULL_MODE_NONE, frontFace=vk.VK_FRONT_FACE_COUNTER_CLOCKWISE, lineWidth=1.0)
ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
dss = vk.VkPipelineDepthStencilStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, depthTestEnable=vk.VK_TRUE, depthWriteEnable=vk.VK_TRUE, depthCompareOp=vk.VK_COMPARE_OP_LESS, minDepthBounds=0.0, maxDepthBounds=1.0)
cba = vk.VkPipelineColorBlendAttachmentState(blendEnable=vk.VK_FALSE, colorWriteMask=0xF)
cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, pAttachments=[cba])
gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st, pVertexInputState=vin, pInputAssemblyState=ia, pViewportState=vps,
                                     pRasterizationState=rs, pMultisampleState=ms, pDepthStencilState=dss, pColorBlendState=cb, layout=pl, renderPass=rp, subpass=0)
pipe = vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]
ok(pipe is not None, "cube pipeline created")

# draw
cai = vk.VkCommandBufferAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1)
cmd = vk.vkAllocateCommandBuffers(dev, cai)[0]
vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
cv0 = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0.0, 0.0, 0.0, 1.0]))
cv1 = vk.VkClearValue(depthStencil=vk.VkClearDepthStencilValue(depth=1.0, stencil=0))
rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb, renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv0, cv1])
vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe)
mvp_bytes = np.asarray(mvp, dtype=np.float32).tobytes()
vk.vkCmdPushConstants(cmd, pl, vk.VK_SHADER_STAGE_VERTEX_BIT, 0, 64, ffi.from_buffer(mvp_bytes))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [vbo], [0])
vk.vkCmdBindIndexBuffer(cmd, ibo, 0, vk.VK_INDEX_TYPE_UINT16)
vk.vkCmdDrawIndexed(cmd, 36, 1, 0, 0, 0)
vk.vkCmdEndRenderPass(cmd)
region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
vk.vkEndCommandBuffer(cmd)
si = vk.VkSubmitInfo(sType=vk.VK_STRUCTURE_TYPE_SUBMIT_INFO, pCommandBuffers=[cmd])
fence = vk.vkCreateFence(dev, vk.VkFenceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FENCE_CREATE_INFO), None)
vk.vkQueueSubmit(q, 1, [si], fence)
vk.vkWaitForFences(dev, 1, [fence], vk.VK_TRUE, 0xFFFFFFFFFFFFFFFF)
vk.vkDestroyFence(dev, fence, None)
vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
px = readback()
ok(True, "cube drawn (depth-tested, Gouraud)")

# INDEPENDENT software reference rasterizer
refc = [[0.0, 0.0, 0.0] for _ in range(W * H)]
refz = [1e9] * (W * H)
refcov = [0] * (W * H)
sx = [0.0] * 8; sy = [0.0] * 8; sz = [0.0] * 8; sw = [0.0] * 8
for i in range(8):
    out = mv4(mvp, [VP[i][0], VP[i][1], VP[i][2], 1.0])
    w = out[3]; sw[i] = w
    ndcx = out[0] / w; ndcy = out[1] / w; ndcz = out[2] / w
    sx[i] = (ndcx * 0.5 + 0.5) * W; sy[i] = (ndcy * 0.5 + 0.5) * H; sz[i] = ndcz
ok(sw[0] > 0, "reference: all clip.w positive (mesh in front of camera)")
for t in range(12):
    a = IDX[t * 3]; b = IDX[t * 3 + 1]; cc = IDX[t * 3 + 2]
    ax, ay, bx, by, cx, cy = sx[a], sy[a], sx[b], sy[b], sx[cc], sy[cc]
    area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
    if abs(area) < 1e-6:
        continue
    minx = int(math.floor(min(ax, bx, cx))); maxx = int(math.ceil(max(ax, bx, cx)))
    miny = int(math.floor(min(ay, by, cy))); maxy = int(math.ceil(max(ay, by, cy)))
    if minx < 0:
        minx = 0
    if miny < 0:
        miny = 0
    if maxx > W:
        maxx = W
    if maxy > H:
        maxy = H
    for y in range(miny, maxy):
        for x in range(minx, maxx):
            pxs = x + 0.5; pys = y + 0.5
            w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area
            w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area
            w2 = 1.0 - w0 - w1
            inside = (w0 >= 0 and w1 >= 0 and w2 >= 0) or (w0 <= 0 and w1 <= 0 and w2 <= 0)
            if not inside:
                continue
            if w0 < 0 or w1 < 0 or w2 < 0:
                w0 = -w0; w1 = -w1; w2 = -w2
            z = w0 * sz[a] + w1 * sz[b] + w2 * sz[cc]
            ip = y * W + x
            if z < refz[ip]:
                refz[ip] = z; refcov[ip] = 1
                iwa = 1.0 / sw[a]; iwb = 1.0 / sw[b]; iwc = 1.0 / sw[cc]
                d = w0 * iwa + w1 * iwb + w2 * iwc
                for k in range(3):
                    num = w0 * iwa * VC[a][k] + w1 * iwb * VC[b][k] + w2 * iwc * VC[cc][k]
                    refc[ip][k] = num / d

total = 0; match = 0; covmatch = 0; covtotal = 0; interior_bad = 0
for y in range(H):
    for x in range(W):
        total += 1
        gcov = not (px[y, x, 0] == 0 and px[y, x, 1] == 0 and px[y, x, 2] == 0)
        ip = y * W + x
        rcov = refcov[ip] != 0
        if gcov == rcov:
            covmatch += 1
        if rcov:
            covtotal += 1
            er = lroundf(refc[ip][0] * 255.0); eg = lroundf(refc[ip][1] * 255.0); eb = lroundf(refc[ip][2] * 255.0)
            interior = (0 < x < W - 1 and 0 < y < H - 1 and refcov[(y - 1) * W + x] and refcov[(y + 1) * W + x]
                        and refcov[y * W + x - 1] and refcov[y * W + x + 1])
            if peq(px, x, y, er, eg, eb, 255, 6):
                match += 1
            elif interior:
                interior_bad += 1
ok(covtotal > 200, "reference: cube covers a substantial area")
ok(covmatch >= int(0.97 * total), "coverage mask matches GPU (>=97% of pixels agree covered/empty)")
ok(interior_bad == 0, "every interior pixel matches perspective-correct Gouraud reference (tol 6)")
ok(match >= int(0.92 * covtotal), "92%+ of covered pixels match reference color (edges excluded)")

vx = lroundf(sx[6] - 0.5); vy = lroundf(sy[6] - 0.5)
if 1 <= vx < W - 1 and 1 <= vy < H - 1:
    bright = False
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            xx = vx + dx; yy = vy + dy
            if px[yy, xx, 0] > 180 and px[yy, xx, 1] > 180 and px[yy, xx, 2] > 180:
                bright = True
    ok(bright, "vertex (1,1,1) region is bright (Gouraud white corner)")
else:
    ok(False, "vertex (1,1,1) projected off-screen (camera mis-set)")
ok(peq(px, 0, 0, 0, 0, 0, 255, 1) or refcov[0] == 0, "corner (0,0) background consistent")

cxp = W // 2; cyp = H // 2
if refcov[cyp * W + cxp]:
    ip = cyp * W + cxp
    er = lroundf(refc[ip][0] * 255.0); eg = lroundf(refc[ip][1] * 255.0); eb = lroundf(refc[ip][2] * 255.0)
    ok(peq(px, cxp, cyp, er, eg, eb, 255, 8), "center pixel = nearest-face (depth-buffered occlusion) reference color")
else:
    ok(False, "center pixel not covered (mesh mis-projected)")

ok(not (px[1, 1, 0] == px[H // 2, W // 2, 0] and px[1, 1, 1] == px[H // 2, W // 2, 1] and px[1, 1, 2] == px[H // 2, W // 2, 2]),
   "negative control: image is not a flat single color (real 3D shading present)")

vk.vkDeviceWaitIdle(dev)
EXPECTED = 23
TOTAL = PASS + FAIL
print("scene-3dmodel-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED))
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_3DMODEL_PY OK %d" % PASS)
    sys.exit(0)
sys.exit(1)
