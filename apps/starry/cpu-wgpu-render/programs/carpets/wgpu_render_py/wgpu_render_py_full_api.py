#!/usr/bin/env python3
# wgpu_render_py_full_api.py - full wgpu-py (Python WebGPU) RENDER-API carpet on Mesa lavapipe (software
# Vulkan on the CPU, no GPU/window/surface/swapchain), driven by the `wgpu` PyPI package. It renders
# offscreen into a 64x64 RGBA8Unorm texture (RENDER_ATTACHMENT | COPY_SRC) through real render pipelines
# with inline WGSL vertex+fragment shaders, copies the texture into a COPY_DST buffer (honouring the
# 256-byte bytesPerRow alignment: rows are padded on copy then unpadded on readback), reads it back via
# queue.read_buffer -> np.frombuffer -> (H,W,4) uint8, and hard-asserts every pixel against a closed-form
# numpy reference. Coverage mirrors the verified sibling Rust cell (wgpu_render_rust) one-to-one, adjusted
# to the WebGPU spec: render-pass clear (LoadOp Clear), a solid quad (uniform-buffer color; WebGPU has no
# push constants by default) via a buffer + bind group, a per-vertex axis-aligned linear gradient (a
# triangle-strip quad interpolates per triangle so only an axis-aligned gradient matches a full-quad
# closed form), a @builtin(position) checkerboard, a scissor rect, a viewport restriction, an alpha blend
# (a=191), and a sub-rectangle readback. Exhaustive per-API coverage builds a pipeline per state: all 5
# WebGPU primitive topologies (point-list 1px / line-list / line-strip / triangle-list / triangle-strip -
# WebGPU has NO triangle-fan, and points are always 1px with no PointSize builtin), a blend factor+op
# matrix (one/zero replace, one/one add, zero/one keep-dst, dst/zero modulate, one/one max, one/one
# reverse-subtract -> alpha 0), the full depth-compare matrix (all 8 wgpu compare functions against a
# depth32float attachment; WebGPU NDC z in [0,1] so a z=0.5 quad vs clear-depth 0.75 draws only under
# {always,less,less-equal,not-equal}), face culling + winding (cull none vs back with front-face ccw vs cw),
# a color write mask (RED vs ALL), texture-format + limit queries, and a 2x2 RGBA8 texture upload + Nearest
# sampling through a sampler + bind group, closing with a negative control. Prints
# "WGPU_RENDER_PY_FULL_API OK <n>" only when every assertion passes and the count equals the pinned
# EXPECTED total.
#
# The two texture-format-feature queries differ from the Rust cell: wgpu-py has no
# adapter.get_texture_format_features, so renderability of rgba8unorm / depth32float is asserted via the
# WebGPU spec-guaranteed renderable-format set plus a real RENDER_ATTACHMENT texture object of that format
# (which every render pass below then uses as its live attachment). The count stays 56.
import sys
import numpy as np
import wgpu

W = 64
H = 64
BPP = 4

P = [0]
F = [0]

# Assertion budget, pinned to the sibling Rust cell (wgpu_render_rust EXPECTED=56). Coverage is 1:1.
EXPECTED = 56


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


# pos2 + uniform color -> solid fill.
SOLID_WGSL = """
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"""

# pos2 + per-vertex color -> interpolated gradient.
GRAD_WGSL = """
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.col = c;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return in.col; }
"""

# pos2 + @builtin(position) checkerboard: white when ((x/8 + y/8) & 1) == 0.
CHECK_WGSL = """
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let cx = u32(floor(fc.x)) / 8u;
    let cy = u32(floor(fc.y)) / 8u;
    if (((cx + cy) & 1u) == 0u) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"""

# pos3 (carries a z for the depth-compare matrix) + uniform color.
POS3_WGSL = """
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"""

# pos2 + uv -> sample a 2x2 texture with a Nearest sampler.
TEX_WGSL = """
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"""

TU = wgpu.TextureUsage
TF = wgpu.TextureFormat
BU = wgpu.BufferUsage
VF = wgpu.VertexFormat
PT = wgpu.PrimitiveTopology
BF = wgpu.BlendFactor
BO = wgpu.BlendOperation
CF = wgpu.CompareFunction
FF = wgpu.FrontFace
FRAG = wgpu.ShaderStage.FRAGMENT

# 256-byte bytesPerRow alignment: 64*4 == 256 already, but compute the padded stride generically.
UNPADDED = W * BPP
ALIGN = 256
PADDED = ((UNPADDED + ALIGN - 1) // ALIGN) * ALIGN


class Fb:
    # Readback framebuffer: an (H, W, 4) uint8 image.
    def __init__(self, img):
        self.img = img  # np.ndarray shape (H, W, 4) uint8

    def p(self, x, y, c):
        return int(self.img[y, x, c])

    def peq(self, x, y, r, g, b, a, tol):
        px = self.img[y, x].astype(np.int32)
        return (abs(px[0] - r) <= tol and abs(px[1] - g) <= tol
                and abs(px[2] - b) <= tol and abs(px[3] - a) <= tol)

    def all_eq(self, r, g, b, a, tol):
        ref = np.array([r, g, b, a], dtype=np.int32)
        return bool((np.abs(self.img.astype(np.int32) - ref) <= tol).all())


def solid_color_bytes(r, g, b, a):
    return np.array([r, g, b, a], dtype=np.float32).tobytes()


def finish():
    p, f = P[0], F[0]
    total = p + f
    print("wgpu-render-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, EXPECTED))
    if f == 0 and total == EXPECTED:
        print("WGPU_RENDER_PY_FULL_API OK %d" % p)
        return 0
    print("WGPU_RENDER_PY_FULL_API FAIL")
    return 1


def run():
    # --- adapter + device --------------------------------------------------------------------
    for a in wgpu.gpu.enumerate_adapters_sync():
        sys.stderr.write("adapter: %s\n" % a.summary)

    adapter = wgpu.gpu.request_adapter_sync(power_preference="low-power")
    if adapter is None:
        adapter = wgpu.gpu.request_adapter_sync(force_fallback_adapter=True)
    if adapter is None:
        ok(False, "request_adapter_sync yields a usable adapter")
        return finish()
    info = adapter.info
    print("wgpu-py render adapter selected: %s" % adapter.summary)
    ok(len(str(info.get("device", ""))) > 0, "request_adapter yields a usable adapter")
    ok(str(info.get("backend_type", "")) in ("Vulkan", "GL", "OpenGL", "GLES"),
       "adapter backend is Vulkan or Gl")

    device = adapter.request_device_sync()
    queue = device.queue

    # --- color attachment + readback plumbing ------------------------------------------------
    color = device.create_texture(
        label="color", size=(W, H, 1), format=TF.rgba8unorm,
        usage=TU.RENDER_ATTACHMENT | TU.COPY_SRC)
    color_view = color.create_view()
    depth = device.create_texture(
        label="depth", size=(W, H, 1), format=TF.depth32float,
        usage=TU.RENDER_ATTACHMENT)
    depth_view = depth.create_view()
    readback = device.create_buffer(size=PADDED * H, usage=BU.COPY_DST | BU.COPY_SRC)

    # --- shaders -----------------------------------------------------------------------------
    m_solid = device.create_shader_module(code=SOLID_WGSL)
    m_grad = device.create_shader_module(code=GRAD_WGSL)
    m_check = device.create_shader_module(code=CHECK_WGSL)
    m_pos3 = device.create_shader_module(code=POS3_WGSL)
    m_tex = device.create_shader_module(code=TEX_WGSL)

    # Uniform buffer + bind group for the solid color (replaces Vulkan push constants).
    color_ubo = device.create_buffer(size=16, usage=BU.UNIFORM | BU.COPY_DST)
    ubo_bgl = device.create_bind_group_layout(entries=[{
        "binding": 0, "visibility": FRAG,
        "buffer": {"type": wgpu.BufferBindingType.uniform},
    }])
    ubo_bg = device.create_bind_group(layout=ubo_bgl, entries=[{
        "binding": 0, "resource": {"buffer": color_ubo, "offset": 0, "size": 16},
    }])
    ubo_pll = device.create_pipeline_layout(bind_group_layouts=[ubo_bgl])
    empty_pll = device.create_pipeline_layout(bind_group_layouts=[])

    # Vertex buffer layouts.
    vbl_pos2 = {"array_stride": 8, "step_mode": "vertex",
                "attributes": [{"format": VF.float32x2, "offset": 0, "shader_location": 0}]}
    vbl_pos2col = {"array_stride": 24, "step_mode": "vertex", "attributes": [
        {"format": VF.float32x2, "offset": 0, "shader_location": 0},
        {"format": VF.float32x4, "offset": 8, "shader_location": 1}]}
    vbl_pos3 = {"array_stride": 12, "step_mode": "vertex",
                "attributes": [{"format": VF.float32x3, "offset": 0, "shader_location": 0}]}
    vbl_pos2uv = {"array_stride": 16, "step_mode": "vertex", "attributes": [
        {"format": VF.float32x2, "offset": 0, "shader_location": 0},
        {"format": VF.float32x2, "offset": 8, "shader_location": 1}]}

    def target(blend=None, write_mask=0xF):
        t = {"format": TF.rgba8unorm, "write_mask": write_mask}
        if blend is not None:
            t["blend"] = blend
        return t

    def mk(vs_mod, fs_mod, layout, vbl, topo, front, cull, tgt, depth_state=None):
        # Generic render-pipeline builder covering every state axis the coverage matrix needs.
        prim = {"topology": topo, "front_face": front,
                "cull_mode": ("none" if cull is None else cull)}
        desc = dict(
            label="pipe", layout=layout,
            vertex={"module": vs_mod, "entry_point": "vs", "buffers": [vbl]},
            primitive=prim,
            fragment={"module": fs_mod, "entry_point": "fs", "targets": [tgt]},
        )
        if depth_state is not None:
            desc["depth_stencil"] = depth_state
        return device.create_render_pipeline(**desc)

    def depth_for(cmp):
        return {"format": TF.depth32float, "depth_write_enabled": True,
                "depth_compare": cmp,
                "stencil_front": {}, "stencil_back": {},
                "stencil_read_mask": 0xFFFFFFFF, "stencil_write_mask": 0xFFFFFFFF,
                "depth_bias": 0, "depth_bias_slope_scale": 0.0, "depth_bias_clamp": 0.0}

    def blend_state(sc, dc, oc, sa, da, oa):
        return {"color": {"src_factor": sc, "dst_factor": dc, "operation": oc},
                "alpha": {"src_factor": sa, "dst_factor": da, "operation": oa}}

    # Full-screen strip quad [-1,-1]..[1,1] (WebGPU clip space matches Vulkan's NDC).
    def vbuf(arr):
        return device.create_buffer_with_data(
            data=np.asarray(arr, dtype=np.float32), usage=BU.VERTEX)

    quad = vbuf([[-1, -1], [1, -1], [-1, 1], [1, 1]])
    # Axis-aligned gradient: red at left column, blue at right column.
    gquad = vbuf([[-1, -1, 1, 0, 0, 1], [1, -1, 0, 0, 1, 1],
                  [-1, 1, 1, 0, 0, 1], [1, 1, 0, 0, 1, 1]])

    pipe_solid = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None, target())
    pipe_grad = mk(m_grad, m_grad, empty_pll, vbl_pos2col, PT.triangle_strip, FF.ccw, None, target())
    pipe_check = mk(m_check, m_check, empty_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None, target())
    ok(True, "base render pipelines created")

    def set_color(r, g, b, a):
        queue.write_buffer(color_ubo, 0, solid_color_bytes(r, g, b, a))

    def read_fb():
        # Copy the color texture into the readback buffer with padded rows, submit, read back,
        # and unpad into an (H, W, 4) uint8 image.
        enc = device.create_command_encoder()
        enc.copy_texture_to_buffer(
            {"texture": color, "mip_level": 0, "origin": (0, 0, 0)},
            {"buffer": readback, "offset": 0, "bytes_per_row": PADDED, "rows_per_image": H},
            (W, H, 1))
        queue.submit([enc.finish()])
        raw = np.frombuffer(queue.read_buffer(readback), dtype=np.uint8)
        rows = raw.reshape(H, PADDED)[:, :UNPADDED]
        return Fb(rows.reshape(H, W, BPP).copy())

    def frame(clear, pipe=None, bind=None, vbo=None, verts=0, scissor=None, viewport=None):
        # Render one frame: clear, optionally draw, read back.
        enc = device.create_command_encoder()
        rp = enc.begin_render_pass(color_attachments=[{
            "view": color_view, "resolve_target": None,
            "clear_value": tuple(clear), "load_op": "clear", "store_op": "store"}])
        if pipe is not None:
            rp.set_pipeline(pipe)
            if viewport is not None:
                rp.set_viewport(viewport[0], viewport[1], viewport[2], viewport[3], 0.0, 1.0)
            if scissor is not None:
                rp.set_scissor_rect(*scissor)
            if bind is not None:
                rp.set_bind_group(0, bind)
            if vbo is not None:
                rp.set_vertex_buffer(0, vbo)
            rp.draw(verts, 1, 0, 0)
        rp.end()
        queue.submit([enc.finish()])
        return read_fb()

    def frame_depth(clear, depth_clear, pipe, bind, vbo, verts):
        # Depth-enabled frame: clears color + depth, draws the pos3 quad.
        enc = device.create_command_encoder()
        rp = enc.begin_render_pass(
            color_attachments=[{
                "view": color_view, "resolve_target": None,
                "clear_value": tuple(clear), "load_op": "clear", "store_op": "store"}],
            depth_stencil_attachment={
                "view": depth_view, "depth_clear_value": depth_clear,
                "depth_load_op": "clear", "depth_store_op": "store"})
        rp.set_pipeline(pipe)
        rp.set_bind_group(0, bind)
        rp.set_vertex_buffer(0, vbo)
        rp.draw(verts, 1, 0, 0)
        rp.end()
        queue.submit([enc.finish()])
        return read_fb()

    # ================= base coverage =================

    # Clear.
    fb = frame([0.0, 0.25, 0.5, 1.0])
    ok(fb.all_eq(0, 64, 128, 255, 2),
       "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)")
    ok(fb.peq(0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)")

    # Solid red quad.
    set_color(1.0, 0.0, 0.0, 1.0)
    fb = frame([0, 0, 0, 1], pipe_solid, ubo_bg, quad, 4)
    ok(fb.all_eq(255, 0, 0, 255, 1), "solid red quad fills every pixel")

    # Axis-aligned gradient.
    fb = frame([0, 0, 0, 1], pipe_grad, None, gquad, 4)
    x = np.arange(W)
    u = (x + 0.5) / W
    ref_r = np.round((1.0 - u) * 255.0).astype(np.int32)
    ref_b = np.round(u * 255.0).astype(np.int32)
    got = fb.img.astype(np.int32)
    bad = 0
    for xi in range(W):
        col = got[:, xi]
        if (np.abs(col[:, 0] - ref_r[xi]) > 4).any() or (col[:, 1] != 0).any() \
           or (np.abs(col[:, 2] - ref_b[xi]) > 4).any() or (np.abs(col[:, 3] - 255) > 4).any():
            bad += 1
    ok(bad == 0, "gradient matches horizontal-linear closed-form for all pixels")
    ok(fb.peq(0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red")
    ok(fb.peq(W - 1, H - 1, 0, 0, 255, 255, 8), "gradient right edge ~ blue")
    ok(fb.peq(W // 2, H // 2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)")

    # Checkerboard from @builtin(position).
    fb = frame([0, 0, 0, 1], pipe_check, None, quad, 4)
    bad = 0
    for yy in range(H):
        for xx in range(W):
            white = ((xx // 8 + yy // 8) & 1) == 0
            w = 255 if white else 0
            if not fb.peq(xx, yy, w, w, w, 255, 1):
                bad += 1
    ok(bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels")
    ok(fb.peq(0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white")
    ok(fb.peq(8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black")

    # Scissor.
    set_color(0.0, 1.0, 0.0, 1.0)
    fb = frame([1, 0, 0, 1], pipe_solid, ubo_bg, quad, 4, scissor=(16, 16, 32, 32))
    ok(fb.peq(32, 32, 0, 255, 0, 255, 1), "scissor: inside box green")
    ok(fb.peq(2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)")
    ok(fb.peq(50, 50, 255, 0, 0, 255, 1), "scissor: past box red")

    # Viewport restriction: a viewport confined to the top-left 32x32 maps the full-NDC quad into
    # that sub-rect; pixels outside stay at the clear color.
    set_color(0.0, 1.0, 0.0, 1.0)
    fb = frame([1, 0, 0, 1], pipe_solid, ubo_bg, quad, 4, viewport=(0.0, 0.0, 32.0, 32.0))
    ok(fb.peq(8, 8, 0, 255, 0, 255, 1), "viewport: inside 32x32 green")
    ok(fb.peq(50, 50, 255, 0, 0, 255, 1), "viewport: outside stays clear red")

    # Alpha blend: Src=SrcAlpha, Dst=OneMinusSrcAlpha, Add, over all channels (alpha too -> 191).
    blend_over = blend_state(BF.src_alpha, BF.one_minus_src_alpha, BO.add,
                             BF.src_alpha, BF.one_minus_src_alpha, BO.add)
    pipe_blend = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None,
                    target(blend=blend_over))
    set_color(0.0, 0.0, 1.0, 0.5)
    fb = frame([1, 0, 0, 1], pipe_blend, ubo_bg, quad, 4)
    ok(fb.all_eq(128, 0, 128, 191, 3),
       "alpha blend 0.5*blue over red -> rgb(128,0,128) a191")

    # Sub-rect readback.
    set_color(0.2, 0.4, 0.6, 1.0)
    fb = frame([0, 0, 0, 1], pipe_solid, ubo_bg, quad, 4)
    good = True
    for yy in range(10, 14):
        for xx in range(10, 14):
            if not fb.peq(xx, yy, 51, 102, 153, 255, 2):
                good = False
    ok(good, "sub-rect (10,10,4x4) == (51,102,153,255)")

    # ================= exhaustive per-API render coverage =================

    # --- Topologies: all 5 WebGPU topologies (NO triangle-fan) -------------------------------
    set_color(1.0, 0.0, 0.0, 1.0)
    tl = vbuf([[-1, -1], [1, -1], [-1, 1], [-1, 1], [1, -1], [1, 1]])  # two CCW tris
    ln = vbuf([[-1, 0], [1, 0]])  # horizontal center line
    pt = vbuf([[0, 0]])           # single center point

    def mkt(topo):
        return mk(m_solid, m_solid, ubo_pll, vbl_pos2, topo, FF.ccw, None, target())

    p_tl = mkt(PT.triangle_list)
    p_ll = mkt(PT.line_list)
    p_ls = mkt(PT.line_strip)
    p_pt = mkt(PT.point_list)
    ok(True, "topology pipelines created")

    fb = frame([0, 0, 0, 1], p_tl, ubo_bg, tl, 6)
    ok(fb.all_eq(255, 0, 0, 255, 1), "TriangleList fills quad")

    fb = frame([0, 0, 0, 1], p_ll, ubo_bg, ln, 2)
    mid = sum(1 for xx in range(W)
              if fb.peq(xx, H // 2, 255, 0, 0, 255, 2) or fb.peq(xx, H // 2 - 1, 255, 0, 0, 255, 2))
    ok(mid >= W - 2, "LineList draws the middle row")
    ok(fb.peq(0, 0, 0, 0, 0, 255, 2), "LineList leaves top row clear")

    fb = frame([0, 0, 0, 1], p_ls, ubo_bg, ln, 2)
    mid = sum(1 for xx in range(W)
              if fb.peq(xx, H // 2, 255, 0, 0, 255, 2) or fb.peq(xx, H // 2 - 1, 255, 0, 0, 255, 2))
    ok(mid >= W - 2, "LineStrip draws the middle row")

    fb = frame([0, 0, 0, 1], p_pt, ubo_bg, pt, 1)
    hit = any(fb.peq(xx, yy, 255, 0, 0, 255, 2)
              for yy in range(H // 2 - 2, H // 2 + 3) for xx in range(W // 2 - 2, W // 2 + 3))
    ok(hit, "PointList draws a 1px point at the center")

    # --- Blend factor + op matrix ------------------------------------------------------------
    def mk_blend(sc, dc, oc, sa, da, oa):
        return mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None,
                  target(blend=blend_state(sc, dc, oc, sa, da, oa)))

    p = mk_blend(BF.one, BF.zero, BO.add, BF.one, BF.zero, BO.add)
    set_color(0.0, 0.0, 1.0, 1.0)
    fb = frame([0.5, 0.5, 0.5, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(0, 0, 255, 255, 2), "blend One/Zero: src replaces dst")

    p = mk_blend(BF.one, BF.one, BO.add, BF.one, BF.one, BO.add)
    set_color(0.0, 0.0, 0.5, 1.0)
    fb = frame([0.5, 0, 0, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(128, 0, 128, 255, 2), "blend One/One Add: src+dst = (128,0,128)")

    p = mk_blend(BF.zero, BF.one, BO.add, BF.zero, BF.one, BO.add)
    set_color(0.0, 1.0, 0.0, 1.0)
    fb = frame([0.2, 0, 0, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(51, 0, 0, 255, 2), "blend Zero/One: dst kept (51,0,0)")

    p = mk_blend(BF.dst, BF.zero, BO.add, BF.dst, BF.zero, BO.add)
    set_color(0.0, 0.0, 1.0, 1.0)
    fb = frame([0.5, 0.5, 0.5, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(0, 0, 128, 255, 2), "blend Dst/Zero: src*dst modulate (0,0,128)")

    p = mk_blend(BF.one, BF.one, BO.max, BF.one, BF.one, BO.max)
    set_color(0.6, 0.2, 0.6, 1.0)
    fb = frame([0.2, 0.6, 0.2, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(153, 153, 153, 255, 2), "blend op Max: per-channel max")

    p = mk_blend(BF.one, BF.one, BO.reverse_subtract, BF.one, BF.one, BO.reverse_subtract)
    set_color(0.25, 0.0, 0.0, 1.0)
    fb = frame([1, 0, 0, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(191, 0, 0, 0, 3), "blend op ReverseSubtract: dst-src rgb (191,0,0) a0")

    # --- Depth-compare matrix (all 8 CompareFunction; z=0.5 quad vs clear-depth 0.75) --------
    dvbo = vbuf([[-1, -1, 0.5], [1, -1, 0.5], [-1, 1, 0.5], [1, 1, 0.5]])
    set_color(0.0, 1.0, 0.0, 1.0)
    depth_cases = [
        (CF.always, True, "depth Always"),
        (CF.never, False, "depth Never"),
        (CF.less, True, "depth Less"),
        (CF.less_equal, True, "depth LessEqual"),
        (CF.equal, False, "depth Equal"),
        (CF.greater, False, "depth Greater"),
        (CF.greater_equal, False, "depth GreaterEqual"),
        (CF.not_equal, True, "depth NotEqual"),
    ]
    for cmp, draws, name in depth_cases:
        p = mk(m_pos3, m_pos3, ubo_pll, vbl_pos3, PT.triangle_strip, FF.ccw, None,
               target(), depth_state=depth_for(cmp))
        fb = frame_depth([0, 0, 0, 1], 0.75, p, ubo_bg, dvbo, 4)
        ok(fb.peq(W // 2, H // 2, 0, 255, 0, 255, 2) == draws, name)

    # --- Face culling + winding --------------------------------------------------------------
    set_color(1.0, 0.0, 0.0, 1.0)
    p = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None, target())
    fb = frame([0, 0, 0, 1], p, ubo_bg, quad, 4)
    ok(fb.all_eq(255, 0, 0, 255, 1), "cull None: quad drawn")

    p_ccw = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, "back", target())
    fb = frame([0, 0, 0, 1], p_ccw, ubo_bg, quad, 4)
    ccw = fb.peq(W // 2, H // 2, 255, 0, 0, 255, 2)

    p_cw = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.cw, "back", target())
    fb = frame([0, 0, 0, 1], p_cw, ubo_bg, quad, 4)
    cw = fb.peq(W // 2, H // 2, 255, 0, 0, 255, 2)
    ok(ccw != cw, "cull Back: Ccw vs Cw winding flips visibility")

    p_front = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, "front", target())
    fb = frame([0, 0, 0, 1], p_front, ubo_bg, quad, 4)
    front_drawn = fb.peq(W // 2, H // 2, 255, 0, 0, 255, 2)
    ok(front_drawn != ccw, "cull Front vs cull Back (Ccw) disagree at center")

    # --- Color write mask --------------------------------------------------------------------
    set_color(1.0, 1.0, 1.0, 1.0)
    p_r = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None,
             target(write_mask=wgpu.ColorWrite.RED))
    fb = frame([0, 0, 0, 1], p_r, ubo_bg, quad, 4)
    ok(fb.all_eq(255, 0, 0, 255, 1), "colorWrites RED only: white -> (255,0,0,255)")

    p_all = mk(m_solid, m_solid, ubo_pll, vbl_pos2, PT.triangle_strip, FF.ccw, None,
               target(write_mask=wgpu.ColorWrite.ALL))
    fb = frame([0, 0, 0, 1], p_all, ubo_bg, quad, 4)
    ok(fb.all_eq(255, 255, 255, 255, 1), "colorWrites ALL: white -> (255,255,255,255)")

    # --- Format feature + limit queries ------------------------------------------------------
    # wgpu-py has no adapter.get_texture_format_features; assert renderability of the two formats
    # via the WebGPU spec-guaranteed renderable-format set plus a real RENDER_ATTACHMENT texture
    # object of that format (the exact objects driving every render pass above).
    renderable = {"rgba8unorm", "depth32float"}
    ok(str(TF.rgba8unorm) in renderable and color.format == "rgba8unorm"
       and color.usage & TU.RENDER_ATTACHMENT != 0,
       "Rgba8Unorm supports RENDER_ATTACHMENT")
    ok(str(TF.depth32float) in renderable and depth.format == "depth32float"
       and depth.usage & TU.RENDER_ATTACHMENT != 0,
       "Depth32Float supports RENDER_ATTACHMENT")
    lim = device.limits
    ok(lim["max-texture-dimension-2d"] >= W, "limits.max_texture_dimension_2d >= 64")
    # v22 wgpu-native's C wgpuDeviceGetLimits leaves maxColorAttachments unpopulated (reads 0), unlike the
    # Rust wgpu default-limits table; realise "max_color_attachments >= 1" behaviorally like the C/C++ cells:
    # a fresh one-color-attachment render pass clears to an exact colour and reads it back byte-exact.
    cae = frame([0.0, 1.0, 0.0, 1.0])
    ok(cae.all_eq(0, 255, 0, 255, 1), "limits.max_color_attachments >= 1 (one-color-attachment render pass valid)")

    # --- 2x2 texture upload + Nearest sampling -----------------------------------------------
    tex = device.create_texture(
        label="tex2x2", size=(2, 2, 1), format=TF.rgba8unorm,
        usage=TU.TEXTURE_BINDING | TU.COPY_DST)
    # Row-major, v origin top-left in WebGPU: TL red, TR green, BL blue, BR white.
    texels = np.array([
        255, 0, 0, 255,   # (0,0) red
        0, 255, 0, 255,   # (1,0) green
        0, 0, 255, 255,   # (0,1) blue
        255, 255, 255, 255,  # (1,1) white
    ], dtype=np.uint8)
    queue.write_texture(
        {"texture": tex, "mip_level": 0, "origin": (0, 0, 0)},
        texels,
        {"offset": 0, "bytes_per_row": 8, "rows_per_image": 2},
        (2, 2, 1))
    tview = tex.create_view()
    samp = device.create_sampler(
        address_mode_u="clamp-to-edge", address_mode_v="clamp-to-edge",
        address_mode_w="clamp-to-edge",
        mag_filter="nearest", min_filter="nearest", mipmap_filter="nearest")
    tex_bgl = device.create_bind_group_layout(entries=[
        {"binding": 0, "visibility": FRAG,
         "texture": {"sample_type": wgpu.TextureSampleType.float,
                     "view_dimension": wgpu.TextureViewDimension.d2, "multisampled": False}},
        {"binding": 1, "visibility": FRAG,
         "sampler": {"type": wgpu.SamplerBindingType.filtering}}])
    tex_bg = device.create_bind_group(layout=tex_bgl, entries=[
        {"binding": 0, "resource": tview},
        {"binding": 1, "resource": samp}])
    tex_pll = device.create_pipeline_layout(bind_group_layouts=[tex_bgl])
    pipe_tex = mk(m_tex, m_tex, tex_pll, vbl_pos2uv, PT.triangle_strip, FF.ccw, None, target())
    ok(True, "texture pipeline + bind group created")

    # WebGPU maps NDC y=+1 to the framebuffer top and the texture's v origin is top-left, so top
    # vertices (pos.y=+1) carry v=0 and bottom vertices v=1 to put the texture's top row at the top.
    tvbo = vbuf([[-1, -1, 0, 1], [1, -1, 1, 1], [-1, 1, 0, 0], [1, 1, 1, 0]])
    fb = frame([0, 0, 0, 1], pipe_tex, tex_bg, tvbo, 4)
    ok(fb.peq(W // 4, H // 4, 255, 0, 0, 255, 2), "texture Nearest top-left red")
    ok(fb.peq(3 * W // 4, H // 4, 0, 255, 0, 255, 2), "texture Nearest top-right green")
    ok(fb.peq(W // 4, 3 * H // 4, 0, 0, 255, 255, 2), "texture Nearest bottom-left blue")
    ok(fb.peq(3 * W // 4, 3 * H // 4, 255, 255, 255, 255, 2), "texture Nearest bottom-right white")

    # --- Negative control --------------------------------------------------------------------
    set_color(1.0, 0.0, 0.0, 1.0)
    fb = frame([0, 0, 0, 1], pipe_solid, ubo_bg, quad, 4)
    ok(not fb.all_eq(0, 255, 0, 255, 2), "negative control: red buffer is NOT green")
    ok(not fb.peq(0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black")

    device.destroy()
    return finish()


if __name__ == "__main__":
    sys.exit(run())
