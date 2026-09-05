#!/usr/bin/env python3
# scene_2dui_py - 2D UI compositing RENDER-scene carpet driven by wgpu-py (Python WebGPU) on Mesa
# software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. Python port of the
# scene_2dui Rust cell: an offscreen 64x64 Rgba8Unorm texture is rendered through real render pipelines
# (the SAME pixel-space y-flipped WGSL shaders as the Rust cell), copied to a COPY_DST buffer (256-byte
# bytesPerRow padding) and read back; every scene primitive has an INDEPENDENT closed-form software
# reference computed in Python (not derived from the GPU output) and asserted per pixel: filled
# axis-aligned rectangles, an analytic rounded-rect, a nine-patch-style scaled border frame, an 8x8
# bitmap-font glyph blit, a scissor-clipped fill, and MULTI-LAYER Porter-Duff over compositing of 3
# stacked semi-transparent layers. Closes with a negative control.
#
# The closed-form math (Porter-Duff over, analytic rounded-rect corner arc, nine-patch coverage, 8x8
# glyph bitmap, scissor clipping, q8 quantization) is behavior-identical to the Rust scene_2dui cell;
# only the wgpu-py binding syntax differs. Prints "SCENE_2DUI_PY OK <n>" only when FAIL==0 &&
# TOTAL==EXPECTED==PASS.
import sys
import numpy as np
import wgpu

W = 64
H = 64
BPP = 4

P = [0]
F = [0]

# Assertion budget pinned to the sibling Rust cell (scene_2dui EXPECTED=28). Coverage is 1:1: two
# adapter/device asserts, the offscreen-ready assert, and every per-pixel closed-form SCENE assertion.
EXPECTED = 28


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


def clampi(v, lo, hi):
    return lo if v < lo else (hi if v > hi else v)


def q8(f):
    return clampi(int(round(f * 255.0)), 0, 255)


# Pixel-space vertex shader: input pixel coords in [0,W]x[0,H], map to NDC with a y-flip so pixel row 0
# lands at readback row 0. Solid uniform color out. (Identical to the Rust cell's SOLID_WGSL.)
SOLID_WGSL = """
struct Solid { rgba: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"""

# Analytic rounded-rect fragment shader over a full-screen pixel quad.
RR_WGSL = """
struct RR { box: vec4<f32>, col: vec4<f32>, rad: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: RR;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let p = fc.xy;
    let x0 = u.box.x; let y0 = u.box.y; let x1 = u.box.z; let y1 = u.box.w;
    let rad = u.rad.x;
    let inside = p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1;
    if (!inside) { discard; }
    var corner = false;
    var cc = vec2<f32>(0.0, 0.0);
    if (p.x < x0 + rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x0 + rad, y0 + rad); }
    else if (p.x >= x1 - rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x1 - rad, y0 + rad); }
    else if (p.x < x0 + rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x0 + rad, y1 - rad); }
    else if (p.x >= x1 - rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x1 - rad, y1 - rad); }
    if (corner && distance(p, cc) > rad) { discard; }
    return u.col;
}
"""

# Glyph blit: pos2 + uv, sample an 8x8 texture (Nearest). Pixel-space vertex with y-flip; uv carried.
TEX_WGSL = """
struct Vp { vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Vp;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    o.pos = vec4<f32>(n.x, -n.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }
"""

TU = wgpu.TextureUsage
TF = wgpu.TextureFormat
BU = wgpu.BufferUsage
VF = wgpu.VertexFormat
PT = wgpu.PrimitiveTopology
BF = wgpu.BlendFactor
BO = wgpu.BlendOperation
FRAG = wgpu.ShaderStage.FRAGMENT
VERT = wgpu.ShaderStage.VERTEX

UNPADDED = W * BPP
ALIGN = 256
PADDED = ((UNPADDED + ALIGN - 1) // ALIGN) * ALIGN


class Fb:
    def __init__(self, img):
        self.img = img  # (H, W, 4) uint8

    def p(self, x, y, c):
        return int(self.img[y, x, c])

    def peq(self, x, y, r, g, b, a, tol):
        if x < 0 or y < 0 or x >= W or y >= H:
            return False
        px = self.img[y, x].astype(np.int32)
        return (abs(px[0] - r) <= tol and abs(px[1] - g) <= tol
                and abs(px[2] - b) <= tol and abs(px[3] - a) <= tol)


def finish():
    p, f = P[0], F[0]
    total = p + f
    print("scene-2dui-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, EXPECTED))
    if f == 0 and total == EXPECTED:
        print("SCENE_2DUI_PY OK %d" % p)
        return 0
    print("SCENE_2DUI_PY FAIL")
    return 1


def run():
    for a in wgpu.gpu.enumerate_adapters_sync():
        sys.stderr.write("adapter: %s\n" % a.summary)
    adapter = wgpu.gpu.request_adapter_sync(power_preference="low-power")
    if adapter is None:
        adapter = wgpu.gpu.request_adapter_sync(force_fallback_adapter=True)
    if adapter is None:
        ok(False, "request_adapter yields a usable adapter")
        return finish()
    info = adapter.info
    print("scene-2dui-py adapter selected: %s" % adapter.summary)
    ok(len(str(info.get("device", ""))) > 0, "request_adapter yields a usable adapter")
    ok(str(info.get("backend_type", "")) in ("Vulkan", "GL", "OpenGL", "GLES"),
       "adapter backend is Vulkan or Gl")

    device = adapter.request_device_sync()
    queue = device.queue

    color = device.create_texture(
        label="color", size=(W, H, 1), format=TF.rgba8unorm,
        usage=TU.RENDER_ATTACHMENT | TU.COPY_SRC)
    color_view = color.create_view()
    readback = device.create_buffer(size=PADDED * H, usage=BU.COPY_DST | BU.COPY_SRC)

    m_solid = device.create_shader_module(code=SOLID_WGSL)
    m_rr = device.create_shader_module(code=RR_WGSL)
    m_tex = device.create_shader_module(code=TEX_WGSL)

    def target(blend=None):
        t = {"format": TF.rgba8unorm, "write_mask": 0xF}
        if blend is not None:
            t["blend"] = blend
        return t

    vbl_pos2 = {"array_stride": 8, "step_mode": "vertex",
                "attributes": [{"format": VF.float32x2, "offset": 0, "shader_location": 0}]}
    vbl_pos2uv = {"array_stride": 16, "step_mode": "vertex", "attributes": [
        {"format": VF.float32x2, "offset": 0, "shader_location": 0},
        {"format": VF.float32x2, "offset": 8, "shader_location": 1}]}

    # bind-group layout for a single uniform buffer visible to vertex+fragment.
    ubo_bgl = device.create_bind_group_layout(entries=[{
        "binding": 0, "visibility": VERT | FRAG,
        "buffer": {"type": wgpu.BufferBindingType.uniform}}])
    solid_pll = device.create_pipeline_layout(bind_group_layouts=[ubo_bgl])

    def mk_solid(module, layout, blend=None):
        return device.create_render_pipeline(
            label="pipe", layout=layout,
            vertex={"module": module, "entry_point": "vs", "buffers": [vbl_pos2]},
            primitive={"topology": PT.triangle_list},
            fragment={"module": module, "entry_point": "fs", "targets": [target(blend)]})

    pipe_solid = mk_solid(m_solid, solid_pll)
    blend_over = {"color": {"src_factor": BF.src_alpha, "dst_factor": BF.one_minus_src_alpha,
                            "operation": BO.add},
                  "alpha": {"src_factor": BF.src_alpha, "dst_factor": BF.one_minus_src_alpha,
                            "operation": BO.add}}
    pipe_blend = mk_solid(m_solid, solid_pll, blend_over)

    def vbuf(arr):
        return device.create_buffer_with_data(
            data=np.asarray(arr, dtype=np.float32), usage=BU.VERTEX)

    def ubuf(arr):
        return device.create_buffer_with_data(
            data=np.asarray(arr, dtype=np.float32), usage=BU.UNIFORM)

    def solid_ubo(r, g, b, a):
        return ubuf([r, g, b, a, float(W), float(H), 0.0, 0.0])

    def bind_ubo(layout, buf, size):
        return device.create_bind_group(layout=layout, entries=[{
            "binding": 0, "resource": {"buffer": buf, "offset": 0, "size": size}}])

    # Two-triangle pixel rect [x0,x1) x [y0,y1).
    def rect_verts(x0, y0, x1, y1):
        return [[x0, y0], [x1, y0], [x0, y1], [x0, y1], [x1, y0], [x1, y1]]

    def read_fb():
        enc = device.create_command_encoder()
        enc.copy_texture_to_buffer(
            {"texture": color, "mip_level": 0, "origin": (0, 0, 0)},
            {"buffer": readback, "offset": 0, "bytes_per_row": PADDED, "rows_per_image": H},
            (W, H, 1))
        queue.submit([enc.finish()])
        raw = np.frombuffer(queue.read_buffer(readback), dtype=np.uint8)
        rows = raw.reshape(H, PADDED)[:, :UNPADDED]
        return Fb(rows.reshape(H, W, BPP).copy())

    # ops: list of (pipe, bind, vbo, verts, scissor-or-None).
    def frame(clear, ops):
        enc = device.create_command_encoder()
        rp = enc.begin_render_pass(color_attachments=[{
            "view": color_view, "resolve_target": None,
            "clear_value": tuple(clear), "load_op": "clear", "store_op": "store"}])
        for (pipe, bind, vbo, verts, scissor) in ops:
            rp.set_pipeline(pipe)
            if scissor is not None:
                rp.set_scissor_rect(*scissor)
            else:
                rp.set_scissor_rect(0, 0, W, H)
            rp.set_bind_group(0, bind)
            rp.set_vertex_buffer(0, vbo)
            rp.draw(verts, 1, 0, 0)
        rp.end()
        queue.submit([enc.finish()])
        return read_fb()

    ok(True, "offscreen Rgba8Unorm target + readback buffer ready")

    # ---- Scene A: filled rectangles ----
    ubo_a = solid_ubo(1.0, 0.0, 0.0, 1.0)
    ubo_b = solid_ubo(0.0, 1.0, 0.0, 1.0)
    bg_a = bind_ubo(ubo_bgl, ubo_a, 32)
    bg_b = bind_ubo(ubo_bgl, ubo_b, 32)
    vr1 = vbuf(rect_verts(8, 8, 16, 24))
    vr2 = vbuf(rect_verts(40, 32, 48, 52))
    fb = frame([0, 0, 0, 1], [(pipe_solid, bg_a, vr1, 6, None), (pipe_solid, bg_b, vr2, 6, None)])
    bad = 0
    for y in range(H):
        for x in range(W):
            if 8 <= x < 16 and 8 <= y < 24:
                er, eg, eb = 255, 0, 0
            elif 40 <= x < 48 and 32 <= y < 52:
                er, eg, eb = 0, 255, 0
            else:
                er, eg, eb = 0, 0, 0
            if not fb.peq(x, y, er, eg, eb, 255, 1):
                bad += 1
    ok(bad == 0, "filled rectangles: every pixel matches closed-form rect coverage")
    ok(fb.peq(10, 10, 255, 0, 0, 255, 1), "rect A interior red")
    ok(fb.peq(44, 40, 0, 255, 0, 255, 1), "rect B interior green")
    ok(fb.peq(30, 30, 0, 0, 0, 255, 1), "gap between rects is background")

    # ---- Scene B: analytic rounded-rect ----
    rr_ubo = ubuf([12, 12, 52, 52, 1, 1, 0, 1, 8, 0, 0, 0, float(W), float(H), 0, 0])
    rr_bgl = device.create_bind_group_layout(entries=[{
        "binding": 0, "visibility": VERT | FRAG,
        "buffer": {"type": wgpu.BufferBindingType.uniform}}])
    rr_bg = bind_ubo(rr_bgl, rr_ubo, 64)
    rr_pll = device.create_pipeline_layout(bind_group_layouts=[rr_bgl])
    pipe_rr = device.create_render_pipeline(
        label="rr", layout=rr_pll,
        vertex={"module": m_rr, "entry_point": "vs", "buffers": [vbl_pos2]},
        primitive={"topology": PT.triangle_list},
        fragment={"module": m_rr, "entry_point": "fs", "targets": [target()]})
    fq = vbuf(rect_verts(0, 0, W, H))
    fb = frame([0, 0, 0, 1], [(pipe_rr, rr_bg, fq, 6, None)])

    def covered(x, y):
        cx, cy = x + 0.5, y + 0.5
        x0, y0, x1, y1, r = 12.0, 12.0, 52.0, 52.0, 8.0
        if not (cx >= x0 and cx < x1 and cy >= y0 and cy < y1):
            return False
        corner = False
        ccx = ccy = 0.0
        if cx < x0 + r and cy < y0 + r:
            corner, ccx, ccy = True, x0 + r, y0 + r
        elif cx >= x1 - r and cy < y0 + r:
            corner, ccx, ccy = True, x1 - r, y0 + r
        elif cx < x0 + r and cy >= y1 - r:
            corner, ccx, ccy = True, x0 + r, y1 - r
        elif cx >= x1 - r and cy >= y1 - r:
            corner, ccx, ccy = True, x1 - r, y1 - r
        if corner:
            dx, dy = cx - ccx, cy - ccy
            if (dx * dx + dy * dy) ** 0.5 > r:
                return False
        return True

    bad = 0
    lit = 0
    for y in range(H):
        for x in range(W):
            cov = covered(x, y)
            if cov:
                lit += 1
            er = 255 if cov else 0
            eg = 255 if cov else 0
            if not fb.peq(x, y, er, eg, 0, 255, 1):
                bad += 1
    ok(bad == 0, "rounded-rect: every pixel matches analytic corner-arc coverage")
    ok(lit > 0, "rounded-rect: some pixels covered")
    ok(fb.peq(32, 32, 255, 255, 0, 255, 1), "rounded-rect center lit")
    ok(fb.peq(12, 12, 0, 0, 0, 255, 1), "rounded-rect clipped corner (12,12) is background")
    ok(fb.peq(32, 13, 255, 255, 0, 255, 1), "rounded-rect straight top edge lit")

    # ---- Scene C: nine-patch-style scaled border frame ----
    vbox = vbuf(rect_verts(4, 4, 60, 60))
    vinner = vbuf(rect_verts(10, 10, 54, 54))
    ubo_blue = solid_ubo(0.0, 0.0, 1.0, 1.0)
    ubo_dark = solid_ubo(0.1, 0.1, 0.1, 1.0)
    bg_blue = bind_ubo(ubo_bgl, ubo_blue, 32)
    bg_dark = bind_ubo(ubo_bgl, ubo_dark, 32)
    fb = frame([0, 0, 0, 1],
               [(pipe_solid, bg_blue, vbox, 6, None), (pipe_solid, bg_dark, vinner, 6, None)])
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
            if not fb.peq(x, y, er, eg, eb, 255, 1):
                bad += 1
    ok(bad == 0, "nine-patch border frame: closed-form border-vs-interior coverage")
    ok(fb.peq(5, 32, 0, 0, 255, 255, 1), "nine-patch left border blue")
    ok(fb.peq(32, 5, 0, 0, 255, 255, 1), "nine-patch top border blue")
    ok(fb.peq(32, 32, q8(0.1), q8(0.1), q8(0.1), 255, 1), "nine-patch hollow interior")

    # ---- Scene D: 8x8 bitmap-font glyph blit ----
    GLYPH_H = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00]
    rgba = np.zeros((8, 8, 4), dtype=np.uint8)
    for r in range(8):
        for c in range(8):
            lit_t = ((GLYPH_H[r] >> (7 - c)) & 1) == 1
            v = 255 if lit_t else 0
            rgba[r, c] = (v, v, v, 255)
    gtex = device.create_texture(
        label="glyph", size=(8, 8, 1), format=TF.rgba8unorm,
        usage=TU.TEXTURE_BINDING | TU.COPY_DST)
    queue.write_texture(
        {"texture": gtex, "mip_level": 0, "origin": (0, 0, 0)},
        rgba.tobytes(), {"offset": 0, "bytes_per_row": 32, "rows_per_image": 8}, (8, 8, 1))
    gview = gtex.create_view()
    samp = device.create_sampler(
        address_mode_u="clamp-to-edge", address_mode_v="clamp-to-edge",
        address_mode_w="clamp-to-edge",
        mag_filter="nearest", min_filter="nearest", mipmap_filter="nearest")
    vp_ubo = ubuf([float(W), float(H), 0.0, 0.0])
    tex_bgl = device.create_bind_group_layout(entries=[
        {"binding": 0, "visibility": VERT, "buffer": {"type": wgpu.BufferBindingType.uniform}},
        {"binding": 1, "visibility": FRAG,
         "texture": {"sample_type": wgpu.TextureSampleType.float,
                     "view_dimension": wgpu.TextureViewDimension.d2, "multisampled": False}},
        {"binding": 2, "visibility": FRAG,
         "sampler": {"type": wgpu.SamplerBindingType.filtering}}])
    tex_bg = device.create_bind_group(layout=tex_bgl, entries=[
        {"binding": 0, "resource": {"buffer": vp_ubo, "offset": 0, "size": 16}},
        {"binding": 1, "resource": gview},
        {"binding": 2, "resource": samp}])
    tex_pll = device.create_pipeline_layout(bind_group_layouts=[tex_bgl])
    pipe_tex = device.create_render_pipeline(
        label="glyph", layout=tex_pll,
        vertex={"module": m_tex, "entry_point": "vs", "buffers": [vbl_pos2uv]},
        primitive={"topology": PT.triangle_list},
        fragment={"module": m_tex, "entry_point": "fs", "targets": [target()]})
    gq = [[20, 20, 0, 0], [28, 20, 1, 0], [20, 28, 0, 1],
          [20, 28, 0, 1], [28, 20, 1, 0], [28, 28, 1, 1]]
    gvbo = vbuf(gq)
    fb = frame([0, 0, 0, 1], [(pipe_tex, tex_bg, gvbo, 6, None)])
    bad = 0
    for dy in range(8):
        for dx in range(8):
            sx, sy = 20 + dx, 20 + dy
            lit_t = ((GLYPH_H[dy] >> (7 - dx)) & 1) == 1
            v = 255 if lit_t else 0
            if not fb.peq(sx, sy, v, v, v, 255, 1):
                bad += 1
    ok(bad == 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap")
    ok(fb.peq(21, 23, 255, 255, 255, 255, 1), "glyph crossbar lit (col1,row3)")
    ok(fb.peq(23, 20, 0, 0, 0, 255, 1), "glyph row0 blank")
    ok(fb.peq(24, 21, 0, 0, 0, 255, 1), "glyph row1 middle blank (0x42)")

    # ---- Scene E: scissor-clipped fill ----
    ubo_mag = solid_ubo(1.0, 0.0, 1.0, 1.0)
    bg_mag = bind_ubo(ubo_bgl, ubo_mag, 32)
    sq = vbuf(rect_verts(0, 0, W, H))
    fb = frame([0, 0, 0, 1], [(pipe_solid, bg_mag, sq, 6, (16, 16, 20, 20))])
    bad = 0
    for y in range(H):
        for x in range(W):
            inb = 16 <= x < 36 and 16 <= y < 36
            er = 255 if inb else 0
            eb = 255 if inb else 0
            if not fb.peq(x, y, er, 0, eb, 255, 1):
                bad += 1
    ok(bad == 0, "scissor-clipped fill: magenta only within [16,36)^2")
    ok(fb.peq(20, 20, 255, 0, 255, 255, 1), "scissor inside magenta")
    ok(fb.peq(40, 40, 0, 0, 0, 255, 1), "scissor outside background")

    # ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
    bg = [0.10, 0.10, 0.10, 1.0]
    layers = [
        (1.0, 0.0, 0.0, 0.50, 8.0, 8.0, 56.0, 56.0),
        (0.0, 1.0, 0.0, 0.25, 12.0, 12.0, 52.0, 52.0),
        (0.0, 0.0, 1.0, 0.75, 16.0, 16.0, 48.0, 48.0),
    ]
    ops = []
    for (r, g, b, a, x0, y0, x1, y1) in layers:
        u = solid_ubo(r, g, b, a)
        bd = bind_ubo(ubo_bgl, u, 32)
        v = vbuf(rect_verts(x0, y0, x1, y1))
        ops.append((pipe_blend, bd, v, 6, None))
    fb = frame([bg[0], bg[1], bg[2], bg[3]], ops)

    def composite(tx, ty):
        c = list(bg)
        for (r, g, b, a, x0, y0, x1, y1) in layers:
            cx, cy = tx + 0.5, ty + 0.5
            if cx >= x0 and cx < x1 and cy >= y0 and cy < y1:
                src = [r, g, b, a]
                for k in range(4):
                    c[k] = src[k] * a + c[k] * (1.0 - a)
        return c

    bad = 0
    for y in range(H):
        for x in range(W):
            e = composite(x, y)
            if not fb.peq(x, y, q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3]), 2):
                bad += 1
    ok(bad == 0, "multi-layer over: every pixel matches Porter-Duff over accumulation "
                 "(incl partial-overlap regions)")
    c = list(bg)
    for li in [[1.0, 0.0, 0.0, 0.5], [0.0, 1.0, 0.0, 0.25], [0.0, 0.0, 1.0, 0.75]]:
        a = li[3]
        for k in range(4):
            c[k] = li[k] * a + c[k] * (1.0 - a)
    ok(fb.peq(32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2),
       "multi-layer over center pixel matches hand-iterated over")
    a = 0.5
    er = 1.0 * a + bg[0] * (1.0 - a)
    eg = 0.0 * a + bg[1] * (1.0 - a)
    eb = 0.0 * a + bg[2] * (1.0 - a)
    ea = a * a + bg[3] * (1.0 - a)
    ok(fb.peq(10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2),
       "multi-layer over: single-layer region matches one over")

    # ---- Negative control ----
    fb = frame([0, 0, 0, 1], [(pipe_solid, bg_a, vr1, 6, None)])
    ok(not fb.peq(10, 10, 0, 255, 0, 255, 4), "negative control: red rect pixel is NOT green")
    ok(not fb.peq(30, 30, 255, 0, 0, 255, 4), "negative control: background is NOT red")

    device.destroy()
    return finish()


if __name__ == "__main__":
    sys.exit(run())
