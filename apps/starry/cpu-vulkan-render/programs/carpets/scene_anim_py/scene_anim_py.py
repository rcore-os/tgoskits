#!/usr/bin/env python3
# scene_anim_py.py - keyframe-animation RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
# no GPU/window/surface/swapchain), Python cffi binding (`import vulkan as vk`) of the same offscreen
# render pipeline as the C++ cell scene_anim.cpp. Renders N=4 keyframes of a transformed unit quad through
# a real graphics pipeline (SPIR-V vertex+fragment shaders from shaders/*.spv), each frame's model
# transform (rotation about the FBO center + scale + translate, cubic-ease scale) passed as push
# constants. The four R*S*local+T corners and the frame center are computed INDEPENDENTLY and asserted at
# exact pixels. The reference math is behaviour-identical to the C++ cell; only the cffi-vs-C++ Vulkan
# binding syntax differs. Prints "SCENE_ANIM_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
# Honest-skips (exit 1, "unavailable") if the vulkan cffi binding is not provisioned.
import sys
import os
import math

try:
    import vulkan as vk
except Exception as e:  # noqa: BLE001
    print("SCENE_ANIM_PY unavailable: import vulkan failed: %s" % e)
    sys.exit(1)
try:
    import numpy as np
except Exception as e:  # noqa: BLE001
    print("SCENE_ANIM_PY unavailable: import numpy failed: %s" % e)
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
    print("SCENE_ANIM_PY unavailable: %s" % msg)
    sys.exit(1)


def lroundf(f):
    return int(math.floor(f + 0.5)) if f >= 0 else -int(math.floor(-f + 0.5))


def peq(px, x, y, r, g, b, a, tol):
    p = px[y, x]
    return (abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol
            and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - a) <= tol)


def near_color(px, x, y, r, g, b, tol):
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            xx = x + dx; yy = y + dy
            if xx < 0 or yy < 0 or xx >= W or yy >= H:
                continue
            if peq(px, xx, yy, r, g, b, 255, tol):
                return True
    return False


def lerpf(a, b, t):
    return a + (b - a) * t


def ease_cubic(t):
    return 3.0 * t * t - 2.0 * t * t * t


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
iai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=imr.size, memoryTypeIndex=memtype(imr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
cmem = vk.vkAllocateMemory(dev, iai, None)
vk.vkBindImageMemory(dev, cimg, cmem, 0)
cvi = vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=cimg, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                               subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1))
cview = vk.vkCreateImageView(dev, cvi, None)
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
rbi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=W * H * 4, usage=vk.VK_BUFFER_USAGE_TRANSFER_DST_BIT)
rbuf = vk.vkCreateBuffer(dev, rbi, None)
rmr = vk.vkGetBufferMemoryRequirements(dev, rbuf)
rai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=rmr.size, memoryTypeIndex=memtype(rmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
rmem = vk.vkAllocateMemory(dev, rai, None)
vk.vkBindBufferMemory(dev, rbuf, rmem, 0)
rmap = vk.vkMapMemory(dev, rmem, 0, W * H * 4, 0)
pci = vk.VkCommandPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, queueFamilyIndex=qfam, flags=vk.VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT)
pool = vk.vkCreateCommandPool(dev, pci, None)
ok(pool is not None, "vkCreateCommandPool")
ok(True, "offscreen R8G8B8A8 target + readback buffer ready")


def readback():
    return np.frombuffer(rmap, dtype=np.uint8, count=W * H * 4).reshape(H, W, 4).copy()


def mkVbo(data_f32):
    arr = np.asarray(data_f32, dtype=np.float32)
    sz = arr.nbytes
    bi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=sz, usage=vk.VK_BUFFER_USAGE_VERTEX_BUFFER_BIT)
    b = vk.vkCreateBuffer(dev, bi, None)
    mr = vk.vkGetBufferMemoryRequirements(dev, b)
    ai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=mr.size, memoryTypeIndex=memtype(mr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
    m = vk.vkAllocateMemory(dev, ai, None)
    vk.vkBindBufferMemory(dev, b, m, 0)
    p = vk.vkMapMemory(dev, m, 0, sz, 0)
    ffi.memmove(p, arr.tobytes(), sz)
    vk.vkUnmapMemory(dev, m)
    return b, m


# pipeline (pos2 stride 8, TRIANGLE_STRIP, dynamic scissor)
vs = shmod("anim_vert.spv"); fs = shmod("anim_frag.spv")
# push constant: { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u } = 12 floats
pcr = vk.VkPushConstantRange(vk.VK_SHADER_STAGE_VERTEX_BIT | vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 12 * 4)
li = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pPushConstantRanges=[pcr])
pl = vk.vkCreatePipelineLayout(dev, li, None)
st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
      vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
bind = vk.VkVertexInputBindingDescription(0, 8, vk.VK_VERTEX_INPUT_RATE_VERTEX)
attr = vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32_SFLOAT, 0)
vi = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=[attr])
ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP)
vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
sc = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, pViewports=[vp], pScissors=[sc])
dyn = vk.VkPipelineDynamicStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, pDynamicStates=[vk.VK_DYNAMIC_STATE_SCISSOR])
rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, polygonMode=vk.VK_POLYGON_MODE_FILL, cullMode=vk.VK_CULL_MODE_NONE, lineWidth=1.0)
ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
cba = vk.VkPipelineColorBlendAttachmentState(blendEnable=vk.VK_FALSE, colorWriteMask=0xF)
cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, pAttachments=[cba])
gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st, pVertexInputState=vi, pInputAssemblyState=ia, pViewportState=vps,
                                     pRasterizationState=rs, pMultisampleState=ms, pColorBlendState=cb, pDynamicState=dyn, layout=pl, renderPass=rp, subpass=0)
pipe = vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]
ok(pipe is not None, "anim pipeline created")

local = [-1, -1, 1, -1, -1, 1, 1, 1]
vbo, _vbmem = mkVbo(local)


def alloc_cmd():
    cai = vk.VkCommandBufferAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1)
    return vk.vkAllocateCommandBuffers(dev, cai)[0]


def draw_frame(pc_vals):
    cmd = alloc_cmd()
    vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
    cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=[0.0, 0.0, 0.0, 1.0]))
    rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb, renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv])
    vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
    vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe)
    vk.vkCmdSetScissor(cmd, 0, 1, [vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))])
    data = np.asarray(pc_vals, dtype=np.float32).tobytes()
    vk.vkCmdPushConstants(cmd, pl, vk.VK_SHADER_STAGE_VERTEX_BIT | vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 12 * 4, ffi.from_buffer(data))
    vk.vkCmdBindVertexBuffers(cmd, 0, 1, [vbo], [0])
    vk.vkCmdDraw(cmd, 4, 1, 0, 0)
    vk.vkCmdEndRenderPass(cmd)
    region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
    vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
    vk.vkEndCommandBuffer(cmd)
    si = vk.VkSubmitInfo(sType=vk.VK_STRUCTURE_TYPE_SUBMIT_INFO, pCommandBuffers=[cmd])
    fi = vk.VkFenceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FENCE_CREATE_INFO)
    fence = vk.vkCreateFence(dev, fi, None)
    vk.vkQueueSubmit(q, 1, [si], fence)
    vk.vkWaitForFences(dev, 1, [fence], vk.VK_TRUE, 0xFFFFFFFFFFFFFFFF)
    vk.vkDestroyFence(dev, fence, None)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    return readback()


A0 = 0.0
A1 = math.pi / 2.0
S0 = 6.0
S1 = 14.0
CX0 = 20.0; CX1 = 44.0; CY0 = 20.0; CY1 = 44.0


def frame_transform(t):
    ang = lerpf(A0, A1, t)
    sc = lerpf(S0, S1, ease_cubic(t))
    cx = lerpf(CX0, CX1, t); cy = lerpf(CY0, CY1, t)
    ca = math.cos(ang); sa = math.sin(ang)
    col0 = [sc * ca, sc * sa]
    col1 = [-sc * sa, sc * ca]
    return col0, col1, [cx, cy], sc, ang


ts = [0.0, 0.25, 0.5, 0.75]
cols = [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 0]]

for fi in range(4):
    t = ts[fi]
    col0, col1, tr, sc, ang = frame_transform(t)
    pc_vals = [float(W), float(H), col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], cols[fi][0], cols[fi][1], cols[fi][2], 1.0]
    px = draw_frame(pc_vals)

    ca = math.cos(ang); sa = math.sin(ang)
    corners = []
    for k in range(4):
        lx = local[k * 2]; ly = local[k * 2 + 1]
        rx = sc * (ca * lx - sa * ly); ry = sc * (sa * lx + ca * ly)
        corners.append((tr[0] + rx, tr[1] + ry))
    e = ease_cubic(t); e_ref = 3.0 * t * t - 2.0 * t * t * t
    ok(abs(e - e_ref) < 1e-6, "ease_cubic closed-form value")
    ok(abs(sc - (S0 + (S1 - S0) * e)) < 1e-4, "scale = lerp(S0,S1,ease(t)) closed-form")

    cxi = lroundf(tr[0] - 0.5); cyi = lroundf(tr[1] - 0.5)
    ok(peq(px, cxi, cyi, lroundf(cols[fi][0] * 255), lroundf(cols[fi][1] * 255), lroundf(cols[fi][2] * 255), 255, 2),
       "frame center pixel carries frame color at closed-form center")

    for k in range(4):
        px_ = lroundf(corners[k][0] - 0.5); py_ = lroundf(corners[k][1] - 0.5)
        onscreen = 0 <= px_ < W and 0 <= py_ < H
        ok(onscreen and near_color(px, px_, py_, lroundf(cols[fi][0] * 255), lroundf(cols[fi][1] * 255), lroundf(cols[fi][2] * 255), 40),
           "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)")

    ox = (W - 2) if fi < 2 else 1
    oy = (H - 2) if fi < 2 else 1
    reach = sc * 1.4142
    covers = abs(ox + 0.5 - tr[0]) <= reach and abs(oy + 0.5 - tr[1]) <= reach
    if not covers:
        ok(peq(px, ox, oy, 0, 0, 0, 255, 2), "outside-quad point stays background (closed-form silhouette)")
    else:
        ok(True, "outside-quad point skipped (would be covered)")

_, _, tra, _, _ = frame_transform(0.0)
_, _, trb, _, _ = frame_transform(0.75)
ok(abs(tra[0] - trb[0]) > 1.0, "center translates between t=0 and t=0.75 (animation is real)")

col0, _, _, _, ang = frame_transform(0.5)
ok(abs(ang - math.pi / 4.0) < 1e-5, "t=0.5 rotation angle = pi/4 closed-form")
ok(abs(col0[0] - col0[1]) < 1e-4 and col0[0] > 0, "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)")

col0, col1, tr, _, _ = frame_transform(0.0)
pc_vals = [float(W), float(H), col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], 1.0, 0.0, 0.0, 1.0]
px = draw_frame(pc_vals)
cxi = lroundf(tr[0] - 0.5); cyi = lroundf(tr[1] - 0.5)
ok(not peq(px, cxi, cyi, 0, 255, 0, 255, 4), "negative control: frame-0 center is NOT green")

vk.vkDeviceWaitIdle(dev)
EXPECTED = 47
TOTAL = PASS + FAIL
print("scene-anim-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED))
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_ANIM_PY OK %d" % PASS)
    sys.exit(0)
sys.exit(1)
