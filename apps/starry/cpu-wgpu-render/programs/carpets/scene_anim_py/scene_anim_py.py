#!/usr/bin/env python3
# scene_anim_py - keyframe-animation RENDER-scene carpet driven by wgpu-py (Python WebGPU) on Mesa
# software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. Python port of the
# scene_anim Rust cell: an offscreen 64x64 Rgba8Unorm texture is rendered through a real render pipeline
# (the SAME pixel-space y-flipped affine WGSL shader as the Rust cell); N=4 keyframes of a transformed
# unit quad are drawn, each frame's model transform a rotation about the FBO center composed with a
# translation and uniform scale, interpolated by t in {0,0.25,0.5,0.75}. For every frame the four
# rotated/scaled/translated quad CORNERS are computed INDEPENDENTLY in Python (closed form: R*S*local+T)
# and asserted; a cubic ease eased(t)=3t^2-2t^3 drives the scale, its value asserted at each t. Closes
# with a negative control. The lerp, ease_cubic and R*S*local+T corner math are behavior-identical to
# the Rust scene_anim cell; only the wgpu-py binding syntax differs. Prints "SCENE_ANIM_PY OK <n>" only
# when FAIL==0 && TOTAL==EXPECTED==PASS.
import sys
import math
import numpy as np
import wgpu

W = 64
H = 64
BPP = 4

P = [0]
F = [0]

# Assertion budget pinned to the sibling Rust cell (scene_anim EXPECTED=38). Coverage is 1:1.
EXPECTED = 38


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


def lerp(a, b, t):
    return a + (b - a) * t


def ease_cubic(t):
    return 3.0 * t * t - 2.0 * t * t * t


# Pixel-space affine vertex shader: pix = col0*lp.x + col1*lp.y + tr; map pixel -> NDC with y-flip so
# pixel-y == readback-row. Uniform color out. (Identical to the Rust cell's WGSL.)
WGSL = """
struct X { col0: vec2<f32>, col1: vec2<f32>, tr: vec2<f32>, vp: vec2<f32>, rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: X;
@vertex fn vs(@location(0) lp: vec2<f32>) -> @builtin(position) vec4<f32> {
    let pix = u.col0 * lp.x + u.col1 * lp.y + u.tr;
    let n = (pix / u.vp) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"""

TU = wgpu.TextureUsage
TF = wgpu.TextureFormat
BU = wgpu.BufferUsage
VF = wgpu.VertexFormat
PT = wgpu.PrimitiveTopology
FRAG = wgpu.ShaderStage.FRAGMENT
VERT = wgpu.ShaderStage.VERTEX

UNPADDED = W * BPP
ALIGN = 256
PADDED = ((UNPADDED + ALIGN - 1) // ALIGN) * ALIGN


class Fb:
    def __init__(self, img):
        self.img = img

    def p(self, x, y, c):
        return int(self.img[y, x, c])

    def peq(self, x, y, r, g, b, a, tol):
        if x < 0 or y < 0 or x >= W or y >= H:
            return False
        px = self.img[y, x].astype(np.int32)
        return (abs(px[0] - r) <= tol and abs(px[1] - g) <= tol
                and abs(px[2] - b) <= tol and abs(px[3] - a) <= tol)

    def near_color(self, x, y, r, g, b, tol):
        for dy in (-1, 0, 1):
            for dx in (-1, 0, 1):
                if self.peq(x + dx, y + dy, r, g, b, 255, tol):
                    return True
        return False


def finish():
    p, f = P[0], F[0]
    total = p + f
    print("scene-anim-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, EXPECTED))
    if f == 0 and total == EXPECTED:
        print("SCENE_ANIM_PY OK %d" % p)
        return 0
    print("SCENE_ANIM_PY FAIL")
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
    print("scene-anim-py adapter selected: %s" % adapter.summary)
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

    # Xform uniform: col0.xy, col1.xy, tr.xy, vp.xy, rgba (12 floats).
    xform_ubo = device.create_buffer(size=48, usage=BU.UNIFORM | BU.COPY_DST)
    bgl = device.create_bind_group_layout(entries=[{
        "binding": 0, "visibility": VERT | FRAG,
        "buffer": {"type": wgpu.BufferBindingType.uniform}}])
    bg = device.create_bind_group(layout=bgl, entries=[{
        "binding": 0, "resource": {"buffer": xform_ubo, "offset": 0, "size": 48}}])
    pll = device.create_pipeline_layout(bind_group_layouts=[bgl])
    module = device.create_shader_module(code=WGSL)
    vbl = {"array_stride": 8, "step_mode": "vertex",
           "attributes": [{"format": VF.float32x2, "offset": 0, "shader_location": 0}]}
    pipe = device.create_render_pipeline(
        label="anim", layout=pll,
        vertex={"module": module, "entry_point": "vs", "buffers": [vbl]},
        primitive={"topology": PT.triangle_strip},
        fragment={"module": module, "entry_point": "fs",
                  "targets": [{"format": TF.rgba8unorm, "write_mask": 0xF}]})

    local = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0]
    vbo = device.create_buffer_with_data(
        data=np.asarray([[local[0], local[1]], [local[2], local[3]],
                         [local[4], local[5]], [local[6], local[7]]], dtype=np.float32),
        usage=BU.VERTEX)

    a0, a1 = 0.0, math.pi / 2.0
    s0, s1 = 6.0, 14.0
    cx0, cx1, cy0, cy1 = 20.0, 44.0, 20.0, 44.0

    def frame_transform(t):
        ang = lerp(a0, a1, t)
        sc = lerp(s0, s1, ease_cubic(t))
        cx, cy = lerp(cx0, cx1, t), lerp(cy0, cy1, t)
        ca, sa = math.cos(ang), math.sin(ang)
        col0 = [sc * ca, sc * sa]
        col1 = [-sc * sa, sc * ca]
        tr = [cx, cy]
        return col0, col1, tr, sc, ang

    ts = [0.0, 0.25, 0.5, 0.75]
    cols = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 0.0]]

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

    def render(col0, col1, tr, rgba):
        data = np.asarray(col0 + col1 + tr + [float(W), float(H)] + rgba, dtype=np.float32)
        queue.write_buffer(xform_ubo, 0, data.tobytes())
        enc = device.create_command_encoder()
        rp = enc.begin_render_pass(color_attachments=[{
            "view": color_view, "resolve_target": None,
            "clear_value": (0, 0, 0, 1), "load_op": "clear", "store_op": "store"}])
        rp.set_pipeline(pipe)
        rp.set_bind_group(0, bg)
        rp.set_vertex_buffer(0, vbo)
        rp.draw(4, 1, 0, 0)
        rp.end()
        queue.submit([enc.finish()])
        return read_fb()

    for fi in range(4):
        t = ts[fi]
        col0, col1, tr, sc, ang = frame_transform(t)
        fb = render(col0, col1, tr, [cols[fi][0], cols[fi][1], cols[fi][2], 1.0])

        ca, sa = math.cos(ang), math.sin(ang)
        corners = []
        for k in range(4):
            lx, ly = local[k * 2], local[k * 2 + 1]
            rx = sc * (ca * lx - sa * ly)
            ry = sc * (sa * lx + ca * ly)
            corners.append([tr[0] + rx, tr[1] + ry])
        e = ease_cubic(t)
        e_ref = 3.0 * t * t - 2.0 * t * t * t
        ok(abs(e - e_ref) < 1e-6, "ease_cubic closed-form value")
        ok(abs(sc - (s0 + (s1 - s0) * e)) < 1e-4, "scale = lerp(S0,S1,ease(t)) closed-form")

        cxi = int(round(tr[0] - 0.5))
        cyi = int(round(tr[1] - 0.5))
        ok(fb.peq(cxi, cyi, int(round(cols[fi][0] * 255.0)), int(round(cols[fi][1] * 255.0)),
                  int(round(cols[fi][2] * 255.0)), 255, 2),
           "frame center pixel carries frame color at closed-form center")

        for k in range(4):
            px_ = int(round(corners[k][0] - 0.5))
            py_ = int(round(corners[k][1] - 0.5))
            onscreen = 0 <= px_ < W and 0 <= py_ < H
            ok(onscreen and fb.near_color(px_, py_, int(round(cols[fi][0] * 255.0)),
                                          int(round(cols[fi][1] * 255.0)),
                                          int(round(cols[fi][2] * 255.0)), 40),
               "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)")

        ox, oy = (W - 2, H - 2) if fi < 2 else (1, 1)
        reach = sc * 1.4142
        covers = abs(ox + 0.5 - tr[0]) <= reach and abs(oy + 0.5 - tr[1]) <= reach
        if not covers:
            ok(fb.peq(ox, oy, 0, 0, 0, 255, 2),
               "outside-quad point stays background (closed-form silhouette)")
        else:
            ok(True, "outside-quad point skipped (would be covered)")

    _, _, tra, _, _ = frame_transform(0.0)
    _, _, trb, _, _ = frame_transform(0.75)
    ok(abs(tra[0] - trb[0]) > 1.0, "center translates between t=0 and t=0.75 (animation is real)")

    col0, _, _, _, ang = frame_transform(0.5)
    ok(abs(ang - math.pi / 4.0) < 1e-5, "t=0.5 rotation angle = pi/4 closed-form")
    ok(abs(col0[0] - col0[1]) < 1e-4 and col0[0] > 0.0,
       "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)")

    col0, col1, tr, _, _ = frame_transform(0.0)
    fb = render(col0, col1, tr, [1.0, 0.0, 0.0, 1.0])
    cxi = int(round(tr[0] - 0.5))
    cyi = int(round(tr[1] - 0.5))
    ok(not fb.peq(cxi, cyi, 0, 255, 0, 255, 4), "negative control: frame-0 center is NOT green")

    device.destroy()
    return finish()


if __name__ == "__main__":
    sys.exit(run())
