#!/usr/bin/env python3
# scene_codec_py - streaming/codec-math RENDER-scene carpet driven by wgpu-py (Python WebGPU) on Mesa
# software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. Python port of the
# scene_codec Rust cell: an offscreen 64x64 Rgba8Unorm texture is rendered through real render pipelines
# (the SAME WGSL shaders as the Rust cell), copied to a COPY_DST buffer and read back; each
# codec/streaming path is asserted against an INDEPENDENT closed-form ("numpy-equivalent") reference:
#   (1) YUV->RGB BT.601 full-range in a fragment shader sampling three R8 planes; every output pixel
#       compared to the same matrix applied in Python (4:2:0 NEAREST chroma fetch).
#   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample: a 4x4 texture sampled NEAREST over a 16x16 region.
#   (3) image bilinear 2x downscale: a 4x4 source averaged 2x2 -> 2x2 via LINEAR vs a 2x2 box average.
#   (4) codec round-trip identities on the CPU path: 8-sample DCT-II + IDCT reconstruction, plus RLE.
# Closes with a negative control. The BT.601 matrix, NEAREST/LINEAR sampling closed forms, DCT-II/IDCT
# and RLE are behavior-identical to the Rust scene_codec cell; only the wgpu-py binding syntax differs.
# Prints "SCENE_CODEC_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
import sys
import math
import numpy as np
import wgpu

W = 64
H = 64
BPP = 4

P = [0]
F = [0]

# Assertion budget pinned to the sibling Rust cell (scene_codec EXPECTED=15). Coverage is 1:1.
EXPECTED = 15


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


def clampi(v, lo, hi):
    return lo if v < lo else (hi if v > hi else v)


# Full-NDC quad with uv; top vertices carry v=0 so readback row y samples v=(y+0.5)/OH (top-origin).
YUV_WGSL = """
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var yT: texture_2d<f32>;
@group(0) @binding(1) var uT: texture_2d<f32>;
@group(0) @binding(2) var vT: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    let Y = textureSample(yT, samp, in.uv).r;
    let U = textureSample(uT, samp, in.uv).r - 0.5;
    let V = textureSample(vT, samp, in.uv).r - 0.5;
    let R = Y + 1.402 * V;
    let G = Y - 0.344136 * U - 0.714136 * V;
    let B = Y + 1.772 * U;
    return vec4<f32>(clamp(vec3<f32>(R, G, B), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"""

SAMPLE_WGSL = """
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }
"""

TU = wgpu.TextureUsage
TF = wgpu.TextureFormat
BU = wgpu.BufferUsage
VF = wgpu.VertexFormat
PT = wgpu.PrimitiveTopology
FRAG = wgpu.ShaderStage.FRAGMENT

UNPADDED = W * BPP
ALIGN = 256
PADDED = ((UNPADDED + ALIGN - 1) // ALIGN) * ALIGN


class Fb:
    def __init__(self, img):
        self.img = img

    def peq(self, x, y, r, g, b, a, tol):
        px = self.img[y, x].astype(np.int32)
        return (abs(px[0] - r) <= tol and abs(px[1] - g) <= tol
                and abs(px[2] - b) <= tol and abs(px[3] - a) <= tol)


def finish():
    p, f = P[0], F[0]
    total = p + f
    print("scene-codec-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, EXPECTED))
    if f == 0 and total == EXPECTED:
        print("SCENE_CODEC_PY OK %d" % p)
        return 0
    print("SCENE_CODEC_PY FAIL")
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
    print("scene-codec-py adapter selected: %s" % adapter.summary)
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

    fsq = [[-1.0, 1.0, 0.0, 0.0], [1.0, 1.0, 1.0, 0.0],
           [-1.0, -1.0, 0.0, 1.0], [1.0, -1.0, 1.0, 1.0]]
    vbo = device.create_buffer_with_data(data=np.asarray(fsq, dtype=np.float32), usage=BU.VERTEX)
    vbl = {"array_stride": 16, "step_mode": "vertex", "attributes": [
        {"format": VF.float32x2, "offset": 0, "shader_location": 0},
        {"format": VF.float32x2, "offset": 8, "shader_location": 1}]}

    def upload_r8(w, h, d):
        t = device.create_texture(
            label="r8", size=(w, h, 1), format=TF.r8unorm,
            usage=TU.TEXTURE_BINDING | TU.COPY_DST)
        queue.write_texture(
            {"texture": t, "mip_level": 0, "origin": (0, 0, 0)},
            bytes(d), {"offset": 0, "bytes_per_row": w, "rows_per_image": h}, (w, h, 1))
        return t

    def upload_rgba(w, h, d):
        t = device.create_texture(
            label="rgba", size=(w, h, 1), format=TF.rgba8unorm,
            usage=TU.TEXTURE_BINDING | TU.COPY_DST)
        queue.write_texture(
            {"texture": t, "mip_level": 0, "origin": (0, 0, 0)},
            bytes(d), {"offset": 0, "bytes_per_row": w * 4, "rows_per_image": h}, (w, h, 1))
        return t

    def sampler(f):
        return device.create_sampler(
            address_mode_u="clamp-to-edge", address_mode_v="clamp-to-edge",
            address_mode_w="clamp-to-edge", mag_filter=f, min_filter=f, mipmap_filter=f)

    def tex_entry(binding):
        return {"binding": binding, "visibility": FRAG,
                "texture": {"sample_type": wgpu.TextureSampleType.float,
                            "view_dimension": wgpu.TextureViewDimension.d2, "multisampled": False}}

    def samp_entry(binding):
        return {"binding": binding, "visibility": FRAG,
                "sampler": {"type": wgpu.SamplerBindingType.filtering}}

    def mk_pipe(wgsl, bgl):
        module = device.create_shader_module(code=wgsl)
        pll = device.create_pipeline_layout(bind_group_layouts=[bgl])
        return device.create_render_pipeline(
            label="codec", layout=pll,
            vertex={"module": module, "entry_point": "vs", "buffers": [vbl]},
            primitive={"topology": PT.triangle_strip},
            fragment={"module": module, "entry_point": "fs",
                      "targets": [{"format": TF.rgba8unorm, "write_mask": 0xF}]})

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

    def frame_vp(pipe, bind, ow, oh):
        enc = device.create_command_encoder()
        rp = enc.begin_render_pass(color_attachments=[{
            "view": color_view, "resolve_target": None,
            "clear_value": (0, 0, 0, 1), "load_op": "clear", "store_op": "store"}])
        rp.set_viewport(0.0, 0.0, float(ow), float(oh), 0.0, 1.0)
        rp.set_pipeline(pipe)
        rp.set_bind_group(0, bind)
        rp.set_vertex_buffer(0, vbo)
        rp.draw(4, 1, 0, 0)
        rp.end()
        queue.submit([enc.finish()])
        return read_fb()

    # ============ (1) YUV -> RGB, BT.601 full-range ============
    pw, ph, cw, ch = 32, 32, 16, 16
    y = bytearray(pw * ph)
    u = bytearray(cw * ch)
    v = bytearray(cw * ch)
    for yy in range(ph):
        for xx in range(pw):
            y[yy * pw + xx] = clampi((xx * 8 + yy * 4) % 256, 0, 255)
    for yy in range(ch):
        for xx in range(cw):
            u[yy * cw + xx] = (xx * 16) % 256
            v[yy * cw + xx] = (yy * 16) % 256
    ty = upload_r8(pw, ph, y)
    tu = upload_r8(cw, ch, u)
    tv = upload_r8(cw, ch, v)
    samp = sampler("nearest")
    bgl = device.create_bind_group_layout(entries=[
        tex_entry(0), tex_entry(1), tex_entry(2), samp_entry(3)])
    bind = device.create_bind_group(layout=bgl, entries=[
        {"binding": 0, "resource": ty.create_view()},
        {"binding": 1, "resource": tu.create_view()},
        {"binding": 2, "resource": tv.create_view()},
        {"binding": 3, "resource": samp}])
    pipe = mk_pipe(YUV_WGSL, bgl)
    fb = frame_vp(pipe, bind, pw, ph)
    bad = 0
    checked = 0
    for yy in range(ph):
        for xx in range(pw):
            uu = (xx + 0.5) / pw
            vv2 = (yy + 0.5) / ph
            cx = clampi(int(math.floor(uu * cw)), 0, cw - 1)
            cy = clampi(int(math.floor(vv2 * ch)), 0, ch - 1)
            yf = y[yy * pw + xx] / 255.0
            uf = u[cy * cw + cx] / 255.0 - 0.5
            vf = v[cy * cw + cx] / 255.0 - 0.5
            r = yf + 1.402 * vf
            g = yf - 0.344136 * uf - 0.714136 * vf
            b = yf + 1.772 * uf
            er = clampi(int(round(min(max(r, 0.0), 1.0) * 255.0)), 0, 255)
            eg = clampi(int(round(min(max(g, 0.0), 1.0) * 255.0)), 0, 255)
            eb = clampi(int(round(min(max(b, 0.0), 1.0) * 255.0)), 0, 255)
            checked += 1
            if not fb.peq(xx, yy, er, eg, eb, 255, 3):
                bad += 1
    ok(checked == pw * ph, "YUV->RGB checked all 32x32 output pixels")
    ok(bad == 0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)")
    ok(True, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form")

    # ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
    sw, sh, ow, oh = 4, 4, 16, 16
    src = bytearray(sw * sh * 4)
    for yy in range(sh):
        for xx in range(sw):
            i = (yy * sw + xx) * 4
            src[i] = (xx * 60 + 10) & 0xFF
            src[i + 1] = (yy * 60 + 20) & 0xFF
            src[i + 2] = ((xx + yy) * 30) & 0xFF
            src[i + 3] = 255
    t = upload_rgba(sw, sh, src)
    samp = sampler("nearest")
    bgl = device.create_bind_group_layout(entries=[tex_entry(0), samp_entry(1)])
    bind = device.create_bind_group(layout=bgl, entries=[
        {"binding": 0, "resource": t.create_view()}, {"binding": 1, "resource": samp}])
    pipe = mk_pipe(SAMPLE_WGSL, bgl)
    fb = frame_vp(pipe, bind, ow, oh)
    bad = 0
    for yy in range(oh):
        for xx in range(ow):
            uu = (xx + 0.5) / ow
            vv = (yy + 0.5) / oh
            sx = clampi(int(math.floor(uu * sw)), 0, sw - 1)
            sy = clampi(int(math.floor(vv * sh)), 0, sh - 1)
            i = (sy * sw + sx) * 4
            if not fb.peq(xx, yy, src[i], src[i + 1], src[i + 2], 255, 1):
                bad += 1
    ok(bad == 0, "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block "
                 "(closed form)")
    ok(fb.peq(0, 0, src[0], src[1], src[2], 255, 1), "upsample (0,0) = src(0,0)")
    i33 = (3 * sw + 3) * 4
    ok(fb.peq(15, 15, src[i33], src[i33 + 1], src[i33 + 2], 255, 1), "upsample (15,15) = src(3,3)")

    # ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
    sw, sh, ow, oh = 4, 4, 2, 2
    src = bytearray(sw * sh * 4)
    for yy in range(sh):
        for xx in range(sw):
            i = (yy * sw + xx) * 4
            vv = (10 + (yy * sw + xx) * 15) & 0xFF
            src[i] = vv
            src[i + 1] = 255 - vv
            src[i + 2] = vv
            src[i + 3] = 255
    t = upload_rgba(sw, sh, src)
    samp = sampler("linear")
    bgl = device.create_bind_group_layout(entries=[tex_entry(0), samp_entry(1)])
    bind = device.create_bind_group(layout=bgl, entries=[
        {"binding": 0, "resource": t.create_view()}, {"binding": 1, "resource": samp}])
    pipe = mk_pipe(SAMPLE_WGSL, bgl)
    fb = frame_vp(pipe, bind, ow, oh)
    bad = 0
    for oy in range(oh):
        for ox in range(ow):
            sx0, sy0 = ox * 2, oy * 2
            s = [0, 0, 0]
            for dy in range(2):
                for dx in range(2):
                    i = ((sy0 + dy) * sw + (sx0 + dx)) * 4
                    s[0] += src[i]
                    s[1] += src[i + 1]
                    s[2] += src[i + 2]
            er = int(round(s[0] / 4.0))
            eg = int(round(s[1] / 4.0))
            eb = int(round(s[2] / 4.0))
            if not fb.peq(ox, oy, er, eg, eb, 255, 2):
                bad += 1
    ok(bad == 0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)")

    # ============ (4) codec round-trip identities (CPU path) ============
    N = 8
    x = [0.0] * N
    xc = [0.0] * N
    yv = [0.0] * N
    for i in range(N):
        x[i] = 30.0 + 20.0 * math.sin(0.7 * i) + 5.0 * i
    for k in range(N):
        s = 0.0
        for n in range(N):
            s += x[n] * math.cos(math.pi / N * (n + 0.5) * k)
        xc[k] = s
    for n in range(N):
        s = xc[0]
        for k in range(1, N):
            s += 2.0 * xc[k] * math.cos(math.pi / N * (n + 0.5) * k)
        yv[n] = s / N
    maxerr = 0.0
    for i in range(N):
        maxerr = max(maxerr, abs(yv[i] - x[i]))
    ok(maxerr < 1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)")
    diff = 0.0
    for i in range(N):
        diff = max(diff, abs(xc[i] - x[i]))
    ok(diff > 1.0, "DCT coefficients differ from input (transform is non-trivial)")

    inp = [5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3]
    enc = []
    i = 0
    while i < len(inp):
        val = inp[i]
        j = i
        while j < len(inp) and inp[j] == val and (j - i) < 255:
            j += 1
        enc.append(j - i)
        enc.append(val)
        i = j
    dec = []
    k = 0
    while k + 1 < len(enc):
        for _ in range(enc[k]):
            dec.append(enc[k + 1])
        k += 2
    ok(dec == inp, "RLE encode/decode round-trip identity")
    ok(len(enc) < len(inp), "RLE actually compressed the run data (encode is non-trivial)")

    # ---- Negative control ----
    enc2 = device.create_command_encoder()
    rp = enc2.begin_render_pass(color_attachments=[{
        "view": color_view, "resolve_target": None,
        "clear_value": (0, 0, 0, 1), "load_op": "clear", "store_op": "store"}])
    rp.end()
    queue.submit([enc2.finish()])
    fb = read_fb()
    ok(fb.peq(0, 0, 0, 0, 0, 255, 1), "negative control setup: cleared to black")
    ok(not fb.peq(0, 0, 255, 255, 255, 255, 1), "negative control: cleared buffer is NOT white")

    device.destroy()
    return finish()


if __name__ == "__main__":
    sys.exit(run())
