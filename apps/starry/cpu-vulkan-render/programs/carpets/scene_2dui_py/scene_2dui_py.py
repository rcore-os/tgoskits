#!/usr/bin/env python3
# scene_2dui_py.py - 2D UI compositing RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
# no GPU/window/surface/swapchain), Python cffi binding (`import vulkan as vk`) of the same offscreen
# render pipeline as the C++ cell scene_2dui.cpp. Builds an offscreen render pass into an R8G8B8A8_UNORM
# color image, draws through real graphics pipelines (SPIR-V vertex+fragment shaders from shaders/*.spv),
# copies the image to a host-visible buffer with vkCmdCopyImageToBuffer, maps it into a numpy (H,W,4)
# uint8 array, and checks every pixel against a closed-form numpy reference: filled rectangles, an
# analytic rounded-rect, a nine-patch border frame, an 8x8 bitmap-font glyph blit (VkImage + NEAREST
# combined image sampler, all 64 texels), a scissor-clipped fill, and MULTI-LAYER Porter-Duff over
# compositing Co = Cs*As + Cd*(1-As). The reference math is behaviour-identical to the C++ cell; only the
# cffi-vs-C++ Vulkan binding syntax differs. Prints "SCENE_2DUI_PY OK <n>" only when FAIL==0 &&
# TOTAL==EXPECTED==PASS. Honest-skips (exit 1, "unavailable") if the vulkan cffi binding is not provisioned.
import sys
import os

try:
    import vulkan as vk
except Exception as e:  # noqa: BLE001
    print("SCENE_2DUI_PY unavailable: import vulkan failed: %s" % e)
    sys.exit(1)
try:
    import numpy as np
except Exception as e:  # noqa: BLE001
    print("SCENE_2DUI_PY unavailable: import numpy failed: %s" % e)
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
    print("SCENE_2DUI_PY unavailable: %s" % msg)
    sys.exit(1)


def clampi(v, lo, hi):
    return lo if v < lo else (hi if v > hi else v)


def q8(f):
    # lroundf: round-half-away-from-zero. f >= 0 here.
    return clampi(int(np.floor(f * 255.0 + 0.5)), 0, 255)


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


# ---- color image + render pass + framebuffer ----
ii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D,
                          format=vk.VK_FORMAT_R8G8B8A8_UNORM, extent=vk.VkExtent3D(W, H, 1),
                          mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
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
rai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=rmr.size,
                              memoryTypeIndex=memtype(rmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
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
    ai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=mr.size,
                                 memoryTypeIndex=memtype(mr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
    m = vk.vkAllocateMemory(dev, ai, None)
    vk.vkBindBufferMemory(dev, b, m, 0)
    p = vk.vkMapMemory(dev, m, 0, sz, 0)
    ffi.memmove(p, arr.tobytes(), sz)
    vk.vkUnmapMemory(dev, m)
    return b, m


# pipeline. vlayout 0 = pos2 (stride 8), 1 = pos2+uv (stride 16). blend toggles SRC_ALPHA over.
def mkPipe(vs, fs, pl, vlayout, blend):
    st = [vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_VERTEX_BIT, module=vs, pName="main"),
          vk.VkPipelineShaderStageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, stage=vk.VK_SHADER_STAGE_FRAGMENT_BIT, module=fs, pName="main")]
    stride = 16 if vlayout == 1 else 8
    bind = vk.VkVertexInputBindingDescription(0, stride, vk.VK_VERTEX_INPUT_RATE_VERTEX)
    attr = [vk.VkVertexInputAttributeDescription(0, 0, vk.VK_FORMAT_R32G32_SFLOAT, 0)]
    if vlayout == 1:
        attr.append(vk.VkVertexInputAttributeDescription(1, 0, vk.VK_FORMAT_R32G32_SFLOAT, 8))
    vi = vk.VkPipelineVertexInputStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, pVertexBindingDescriptions=[bind], pVertexAttributeDescriptions=attr)
    ia = vk.VkPipelineInputAssemblyStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, topology=vk.VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST)
    vp = vk.VkViewport(0, 0, float(W), float(H), 0, 1)
    sc = vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))
    vps = vk.VkPipelineViewportStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, pViewports=[vp], pScissors=[sc])
    dyn = vk.VkPipelineDynamicStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, pDynamicStates=[vk.VK_DYNAMIC_STATE_SCISSOR])
    rs = vk.VkPipelineRasterizationStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, polygonMode=vk.VK_POLYGON_MODE_FILL, cullMode=vk.VK_CULL_MODE_NONE, lineWidth=1.0)
    ms = vk.VkPipelineMultisampleStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, rasterizationSamples=vk.VK_SAMPLE_COUNT_1_BIT)
    cba = vk.VkPipelineColorBlendAttachmentState(blendEnable=vk.VK_TRUE if blend else vk.VK_FALSE,
                                                 srcColorBlendFactor=vk.VK_BLEND_FACTOR_SRC_ALPHA, dstColorBlendFactor=vk.VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA, colorBlendOp=vk.VK_BLEND_OP_ADD,
                                                 srcAlphaBlendFactor=vk.VK_BLEND_FACTOR_SRC_ALPHA, dstAlphaBlendFactor=vk.VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA, alphaBlendOp=vk.VK_BLEND_OP_ADD, colorWriteMask=0xF)
    cb = vk.VkPipelineColorBlendStateCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, pAttachments=[cba])
    gp = vk.VkGraphicsPipelineCreateInfo(sType=vk.VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, pStages=st, pVertexInputState=vi, pInputAssemblyState=ia, pViewportState=vps,
                                         pRasterizationState=rs, pMultisampleState=ms, pColorBlendState=cb, pDynamicState=dyn, layout=pl, renderPass=rp, subpass=0)
    return vk.vkCreateGraphicsPipelines(dev, vk.VK_NULL_HANDLE, 1, [gp], None)[0]


def alloc_cmd():
    cai = vk.VkCommandBufferAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, commandPool=pool, level=vk.VK_COMMAND_BUFFER_LEVEL_PRIMARY, commandBufferCount=1)
    return vk.vkAllocateCommandBuffers(dev, cai)[0]


def submit_wait(cmd):
    si = vk.VkSubmitInfo(sType=vk.VK_STRUCTURE_TYPE_SUBMIT_INFO, pCommandBuffers=[cmd])
    fi = vk.VkFenceCreateInfo(sType=vk.VK_STRUCTURE_TYPE_FENCE_CREATE_INFO)
    fence = vk.vkCreateFence(dev, fi, None)
    vk.vkQueueSubmit(q, 1, [si], fence)
    vk.vkWaitForFences(dev, 1, [fence], vk.VK_TRUE, 0xFFFFFFFFFFFFFFFF)
    vk.vkDestroyFence(dev, fence, None)


def begin_frame(clear):
    cmd = alloc_cmd()
    bi = vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT)
    vk.vkBeginCommandBuffer(cmd, bi)
    cv = vk.VkClearValue(color=vk.VkClearColorValue(float32=clear))
    rpb = vk.VkRenderPassBeginInfo(sType=vk.VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, renderPass=rp, framebuffer=fb,
                                   renderArea=vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H)), pClearValues=[cv])
    vk.vkCmdBeginRenderPass(cmd, rpb, vk.VK_SUBPASS_CONTENTS_INLINE)
    return cmd


def end_frame(cmd):
    vk.vkCmdEndRenderPass(cmd)
    region = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(W, H, 1))
    vk.vkCmdCopyImageToBuffer(cmd, cimg, vk.VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, rbuf, 1, [region])
    vk.vkEndCommandBuffer(cmd)
    submit_wait(cmd)
    vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
    return readback()


def full():
    return vk.VkRect2D(vk.VkOffset2D(0, 0), vk.VkExtent2D(W, H))


def rect_verts(x0, y0, x1, y1):
    return [x0, y0, x1, y0, x0, y1, x0, y1, x1, y0, x1, y1]


# push-constant block: { vec2 vp; vec2 pad; vec4 col; vec4 box; float rad; vec3 pad } (std430, 16 floats)
def make_pc(col=(0, 0, 0, 0), boxv=(0, 0, 0, 0), rad=0.0):
    a = np.zeros(16, dtype=np.float32)
    a[0] = W; a[1] = H
    a[4] = col[0]; a[5] = col[1]; a[6] = col[2]; a[7] = col[3]
    a[8] = boxv[0]; a[9] = boxv[1]; a[10] = boxv[2]; a[11] = boxv[3]
    a[12] = rad
    return a.tobytes()


# ---- shaders + layouts + pipelines ----
vs_pix = shmod("pix_vert.spv"); fs_uni = shmod("uni_frag.spv"); fs_rr = shmod("rr_frag.spv")
vs_tex = shmod("tex_vert.spv"); fs_tex = shmod("tex_frag.spv")
pcr = vk.VkPushConstantRange(vk.VK_SHADER_STAGE_VERTEX_BIT | vk.VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16 * 4)
li = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pPushConstantRanges=[pcr])
pl_pc = vk.vkCreatePipelineLayout(dev, li, None)
pipe_uni = mkPipe(vs_pix, fs_uni, pl_pc, 0, 0)
pipe_blend = mkPipe(vs_pix, fs_uni, pl_pc, 0, 1)
pipe_rr = mkPipe(vs_pix, fs_rr, pl_pc, 0, 0)
ok(pipe_uni and pipe_blend and pipe_rr, "pixel-fill / blend / rounded-rect pipelines created")
PC_STAGES = vk.VK_SHADER_STAGE_VERTEX_BIT | vk.VK_SHADER_STAGE_FRAGMENT_BIT


def push(cmd, pc_bytes):
    vk.vkCmdPushConstants(cmd, pl_pc, PC_STAGES, 0, 16 * 4, ffi.from_buffer(pc_bytes))


# ---- Scene A: filled rectangles ----
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
va = rect_verts(8, 8, 16, 24); ba, ma = mkVbo(va)
push(cmd, make_pc(col=(1, 0, 0, 1)))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [ba], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
vb = rect_verts(40, 32, 48, 52); bb, mb = mkVbo(vb)
push(cmd, make_pc(col=(0, 1, 0, 1)))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [bb], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)
bad = 0
for y in range(H):
    for x in range(W):
        if 8 <= x < 16 and 8 <= y < 24:
            er, eg, eb = 255, 0, 0
        elif 40 <= x < 48 and 32 <= y < 52:
            er, eg, eb = 0, 255, 0
        else:
            er, eg, eb = 0, 0, 0
        if not peq(px, x, y, er, eg, eb, 255, 1):
            bad += 1
ok(bad == 0, "filled rectangles: every pixel matches closed-form rect coverage")
ok(peq(px, 10, 10, 255, 0, 0, 255, 1), "rect A interior red")
ok(peq(px, 44, 40, 0, 255, 0, 255, 1), "rect B interior green")
ok(peq(px, 30, 30, 0, 0, 0, 255, 1), "gap between rects is background")

# ---- Scene B: analytic rounded-rect ----
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_rr)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
push(cmd, make_pc(col=(1, 1, 0, 1), boxv=(12, 12, 52, 52), rad=8.0))
fq = [0, 0, W, 0, 0, H, 0, H, W, 0, W, H]; fbo2, fm = mkVbo(fq)
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [fbo2], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)


def covered(x, y):
    cx = x + 0.5; cy = y + 0.5
    x0, y0, x1, y1, rr = 12.0, 12.0, 52.0, 52.0, 8.0
    if not (x0 <= cx < x1 and y0 <= cy < y1):
        return False
    corner = False; ccx = ccy = 0.0
    if cx < x0 + rr and cy < y0 + rr:
        corner = True; ccx = x0 + rr; ccy = y0 + rr
    elif cx >= x1 - rr and cy < y0 + rr:
        corner = True; ccx = x1 - rr; ccy = y0 + rr
    elif cx < x0 + rr and cy >= y1 - rr:
        corner = True; ccx = x0 + rr; ccy = y1 - rr
    elif cx >= x1 - rr and cy >= y1 - rr:
        corner = True; ccx = x1 - rr; ccy = y1 - rr
    if corner:
        dx = cx - ccx; dy = cy - ccy
        if (dx * dx + dy * dy) ** 0.5 > rr:
            return False
    return True


bad = 0; lit = 0
for y in range(H):
    for x in range(W):
        cov = covered(x, y)
        if cov:
            lit += 1
        er, eg, eb = (255, 255, 0) if cov else (0, 0, 0)
        if not peq(px, x, y, er, eg, eb, 255, 1):
            bad += 1
ok(bad == 0, "rounded-rect: every pixel matches analytic corner-arc coverage")
ok(lit > 0, "rounded-rect: some pixels covered")
ok(peq(px, 32, 32, 255, 255, 0, 255, 1), "rounded-rect center lit")
ok(peq(px, 12, 12, 0, 0, 0, 255, 1), "rounded-rect clipped corner (12,12) is background")
ok(peq(px, 32, 13, 255, 255, 0, 255, 1), "rounded-rect straight top edge lit")

# ---- Scene C: nine-patch-style scaled border frame ----
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
vo = rect_verts(4, 4, 60, 60); bo, mo = mkVbo(vo)
push(cmd, make_pc(col=(0, 0, 1, 1)))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [bo], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
vin = rect_verts(10, 10, 54, 54); bin_, mi = mkVbo(vin)
push(cmd, make_pc(col=(0.1, 0.1, 0.1, 1.0)))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [bin_], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)
bad = 0
for y in range(H):
    for x in range(W):
        inbox = 4 <= x < 60 and 4 <= y < 60
        ininner = 10 <= x < 54 and 10 <= y < 54
        if ininner:
            er = eg = eb = q8(0.1)
        elif inbox:
            er, eg, eb = 0, 0, 255
        else:
            er, eg, eb = 0, 0, 0
        if not peq(px, x, y, er, eg, eb, 255, 1):
            bad += 1
ok(bad == 0, "nine-patch border frame: closed-form border-vs-interior coverage")
ok(peq(px, 5, 32, 0, 0, 255, 255, 1), "nine-patch left border blue")
ok(peq(px, 32, 5, 0, 0, 255, 255, 1), "nine-patch top border blue")
ok(peq(px, 32, 32, q8(0.1), q8(0.1), q8(0.1), 255, 1), "nine-patch hollow interior")

# ---- Scene D: 8x8 bitmap-font glyph blit ----
GLYPH_H = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00]
rgba = bytearray(8 * 8 * 4)
for rr in range(8):
    for cc in range(8):
        litp = (GLYPH_H[rr] >> (7 - cc)) & 1
        v = 255 if litp else 0
        idx = (rr * 8 + cc) * 4
        rgba[idx] = v; rgba[idx + 1] = v; rgba[idx + 2] = v; rgba[idx + 3] = 255
tii = vk.VkImageCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, imageType=vk.VK_IMAGE_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                           extent=vk.VkExtent3D(8, 8, 1), mipLevels=1, arrayLayers=1, samples=vk.VK_SAMPLE_COUNT_1_BIT, tiling=vk.VK_IMAGE_TILING_OPTIMAL,
                           usage=vk.VK_IMAGE_USAGE_SAMPLED_BIT | vk.VK_IMAGE_USAGE_TRANSFER_DST_BIT, initialLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED)
gtex = vk.vkCreateImage(dev, tii, None)
ok(gtex is not None, "glyph image")
tmr = vk.vkGetImageMemoryRequirements(dev, gtex)
tai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=tmr.size, memoryTypeIndex=memtype(tmr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT))
tmem = vk.vkAllocateMemory(dev, tai, None)
vk.vkBindImageMemory(dev, gtex, tmem, 0)
sbi = vk.VkBufferCreateInfo(sType=vk.VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, size=len(rgba), usage=vk.VK_BUFFER_USAGE_TRANSFER_SRC_BIT)
sbuf = vk.vkCreateBuffer(dev, sbi, None)
smr = vk.vkGetBufferMemoryRequirements(dev, sbuf)
sai = vk.VkMemoryAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, allocationSize=smr.size, memoryTypeIndex=memtype(smr.memoryTypeBits, vk.VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | vk.VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
smem = vk.vkAllocateMemory(dev, sai, None)
vk.vkBindBufferMemory(dev, sbuf, smem, 0)
sp2 = vk.vkMapMemory(dev, smem, 0, len(rgba), 0)
ffi.memmove(sp2, bytes(rgba), len(rgba))
vk.vkUnmapMemory(dev, smem)
cmd = alloc_cmd()
vk.vkBeginCommandBuffer(cmd, vk.VkCommandBufferBeginInfo(sType=vk.VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, flags=vk.VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT))
sub = vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1)
b1 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_UNDEFINED, newLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=gtex, subresourceRange=sub,
                             srcAccessMask=0, dstAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT)
vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, None, 0, None, 1, [b1])
cp = vk.VkBufferImageCopy(imageSubresource=vk.VkImageSubresourceLayers(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1), imageExtent=vk.VkExtent3D(8, 8, 1))
vk.vkCmdCopyBufferToImage(cmd, sbuf, gtex, vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, [cp])
b2 = vk.VkImageMemoryBarrier(sType=vk.VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, oldLayout=vk.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, newLayout=vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                             srcQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, dstQueueFamilyIndex=vk.VK_QUEUE_FAMILY_IGNORED, image=gtex, subresourceRange=sub,
                             srcAccessMask=vk.VK_ACCESS_TRANSFER_WRITE_BIT, dstAccessMask=vk.VK_ACCESS_SHADER_READ_BIT)
vk.vkCmdPipelineBarrier(cmd, vk.VK_PIPELINE_STAGE_TRANSFER_BIT, vk.VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, 0, 0, None, 0, None, 1, [b2])
vk.vkEndCommandBuffer(cmd)
submit_wait(cmd)
vk.vkFreeCommandBuffers(dev, pool, 1, [cmd])
tvi = vk.VkImageViewCreateInfo(sType=vk.VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, image=gtex, viewType=vk.VK_IMAGE_VIEW_TYPE_2D, format=vk.VK_FORMAT_R8G8B8A8_UNORM,
                               subresourceRange=vk.VkImageSubresourceRange(vk.VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1))
tview = vk.vkCreateImageView(dev, tvi, None)
smci = vk.VkSamplerCreateInfo(sType=vk.VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, magFilter=vk.VK_FILTER_NEAREST, minFilter=vk.VK_FILTER_NEAREST,
                              addressModeU=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, addressModeV=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, addressModeW=vk.VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE)
samp = vk.vkCreateSampler(dev, smci, None)
dslb = vk.VkDescriptorSetLayoutBinding(binding=0, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, descriptorCount=1, stageFlags=vk.VK_SHADER_STAGE_FRAGMENT_BIT)
dslci = vk.VkDescriptorSetLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, pBindings=[dslb])
dsl = vk.vkCreateDescriptorSetLayout(dev, dslci, None)
ok(dsl is not None, "glyph descriptor set layout")
tpcr = vk.VkPushConstantRange(vk.VK_SHADER_STAGE_VERTEX_BIT, 0, 16)
plci = vk.VkPipelineLayoutCreateInfo(sType=vk.VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, pSetLayouts=[dsl], pPushConstantRanges=[tpcr])
pl_tex = vk.vkCreatePipelineLayout(dev, plci, None)
pt = mkPipe(vs_tex, fs_tex, pl_tex, 1, 0)
ok(pt is not None and samp is not None, "glyph pipeline + descriptor created")
dps = vk.VkDescriptorPoolSize(vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1)
dpci = vk.VkDescriptorPoolCreateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, maxSets=1, pPoolSizes=[dps])
dpool = vk.vkCreateDescriptorPool(dev, dpci, None)
dsai = vk.VkDescriptorSetAllocateInfo(sType=vk.VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, descriptorPool=dpool, pSetLayouts=[dsl])
dset = vk.vkAllocateDescriptorSets(dev, dsai)[0]
dii = vk.VkDescriptorImageInfo(samp, tview, vk.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL)
wds = vk.VkWriteDescriptorSet(sType=vk.VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, dstSet=dset, dstBinding=0, descriptorCount=1, descriptorType=vk.VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, pImageInfo=[dii])
vk.vkUpdateDescriptorSets(dev, 1, [wds], 0, None)
gq = [20, 20, 0, 0, 28, 20, 1, 0, 20, 28, 0, 1, 20, 28, 0, 1, 28, 20, 1, 0, 28, 28, 1, 1]
gvbo, gm = mkVbo(gq)
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pt)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
vpv = np.asarray([float(W), float(H)], dtype=np.float32).tobytes()
vk.vkCmdPushConstants(cmd, pl_tex, vk.VK_SHADER_STAGE_VERTEX_BIT, 0, 8, ffi.from_buffer(vpv))
vk.vkCmdBindDescriptorSets(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pl_tex, 0, 1, [dset], 0, None)
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [gvbo], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)
bad = 0
for dy in range(8):
    for dx in range(8):
        sx = 20 + dx; sy = 20 + dy
        litp = (GLYPH_H[dy] >> (7 - dx)) & 1
        v = 255 if litp else 0
        if not peq(px, sx, sy, v, v, v, 255, 1):
            bad += 1
ok(bad == 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap")
ok(peq(px, 21, 23, 255, 255, 255, 255, 1), "glyph crossbar lit (col1,row3)")
ok(peq(px, 23, 20, 0, 0, 0, 255, 1), "glyph row0 blank")
ok(peq(px, 24, 21, 0, 0, 0, 255, 1), "glyph row1 middle blank (0x42)")

# ---- Scene E: scissor-clipped fill ----
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni)
boxr = vk.VkRect2D(vk.VkOffset2D(16, 16), vk.VkExtent2D(20, 20))
vk.vkCmdSetScissor(cmd, 0, 1, [boxr])
push(cmd, make_pc(col=(1, 0, 1, 1)))
fv = rect_verts(0, 0, W, H); fbb, fm = mkVbo(fv)
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [fbb], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)
bad = 0
for y in range(H):
    for x in range(W):
        inr = 16 <= x < 36 and 16 <= y < 36
        er, eg, eb = (255, 0, 255) if inr else (0, 0, 0)
        if not peq(px, x, y, er, eg, eb, 255, 1):
            bad += 1
ok(bad == 0, "scissor-clipped fill: magenta only within [16,36)^2")
ok(peq(px, 20, 20, 255, 0, 255, 255, 1), "scissor inside magenta")
ok(peq(px, 40, 40, 0, 0, 0, 255, 1), "scissor outside background")

# ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
bg = [0.10, 0.10, 0.10, 1.0]
layers = [
    [1.0, 0.0, 0.0, 0.50, 8, 8, 56, 56],
    [0.0, 1.0, 0.0, 0.25, 12, 12, 52, 52],
    [0.0, 0.0, 1.0, 0.75, 16, 16, 48, 48],
]
cmd = begin_frame(bg)
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_blend)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
lbm = []
for l in layers:
    lv = rect_verts(l[4], l[5], l[6], l[7]); lb, lm = mkVbo(lv)
    push(cmd, make_pc(col=(l[0], l[1], l[2], l[3])))
    vk.vkCmdBindVertexBuffers(cmd, 0, 1, [lb], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
    lbm.append((lb, lm))
px = end_frame(cmd)


def composite(tx, ty):
    c = list(bg)
    for l in layers:
        cx = tx + 0.5; cy = ty + 0.5
        if l[4] <= cx < l[6] and l[5] <= cy < l[7]:
            as_ = l[3]; src = [l[0], l[1], l[2], l[3]]
            for k in range(4):
                c[k] = src[k] * as_ + c[k] * (1.0 - as_)
    return c


bad = 0
for y in range(H):
    for x in range(W):
        e = composite(x, y)
        if not peq(px, x, y, q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3]), 2):
            bad += 1
ok(bad == 0, "multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)")
c = list(bg)
for li in [[1.0, 0.0, 0.0, 0.5], [0.0, 1.0, 0.0, 0.25], [0.0, 0.0, 1.0, 0.75]]:
    as_ = li[3]
    for k in range(4):
        c[k] = li[k] * as_ + c[k] * (1.0 - as_)
ok(peq(px, 32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2), "multi-layer over center pixel matches hand-iterated over")
as_ = 0.5
er = 1.0 * as_ + bg[0] * (1 - as_); eg = 0.0 * as_ + bg[1] * (1 - as_); eb = 0.0 * as_ + bg[2] * (1 - as_); ea = as_ * as_ + bg[3] * (1 - as_)
ok(peq(px, 10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2), "multi-layer over: single-layer region matches one over")

# ---- Negative control ----
cmd = begin_frame([0.0, 0.0, 0.0, 1.0])
vk.vkCmdBindPipeline(cmd, vk.VK_PIPELINE_BIND_POINT_GRAPHICS, pipe_uni)
vk.vkCmdSetScissor(cmd, 0, 1, [full()])
va = rect_verts(8, 8, 16, 24); ba, ma = mkVbo(va)
push(cmd, make_pc(col=(1, 0, 0, 1)))
vk.vkCmdBindVertexBuffers(cmd, 0, 1, [ba], [0]); vk.vkCmdDraw(cmd, 6, 1, 0, 0)
px = end_frame(cmd)
ok(not peq(px, 10, 10, 0, 255, 0, 255, 4), "negative control: red rect pixel is NOT green")
ok(not peq(px, 30, 30, 255, 0, 0, 255, 4), "negative control: background is NOT red")

vk.vkDeviceWaitIdle(dev)
EXPECTED = 39
TOTAL = PASS + FAIL
print("scene-2dui-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED))
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_2DUI_PY OK %d" % PASS)
    sys.exit(0)
sys.exit(1)
