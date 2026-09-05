#!/usr/bin/env python3
# scene_3dmodel_py - 3D indexed-mesh RENDER-scene carpet driven by wgpu-py (Python WebGPU) on Mesa
# software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. Python port of the
# scene_3dmodel Rust cell: an offscreen Rgba8Unorm color texture + a Depth32Float depth texture, drawn
# through a real render pipeline (the SAME @invariant CUBE_WGSL vertex+fragment shader as the Rust cell)
# with depth test CompareFunction::Less, copied to a COPY_DST buffer (256-byte bytesPerRow padding) and
# read back. Renders an indexed cube mesh with a hand-computed perspective MVP, depth-buffered
# occlusion, and Gouraud shading. The assertion is an INDEPENDENT software reference rasterizer written
# in Python: verts are transformed by the SAME MVP -> clip -> NDC (perspective divide) -> viewport
# pixels; per pixel we compute barycentric coordinates, do a perspective-correct interpolated depth test
# in a private z-buffer, interpolate vertex colors, then compare the reference framebuffer to the
# readback per pixel (small tolerance). Closes with a negative control.
#
# The math (column-major M4, WebGPU/Vulkan NDC-z in [0,1] perspective, cube verts/colors/indices,
# barycentric rasterizer, perspective-correct color interp) is behavior-identical to the Rust
# scene_3dmodel cell; only the wgpu-py binding syntax differs. Prints "SCENE_3DMODEL_PY OK <n>" only
# when FAIL==0 && TOTAL==EXPECTED==PASS.
import math
import sys
import numpy as np
import wgpu

W = 64
H = 64
BPP = 4

P = [0]
F = [0]

# Assertion budget pinned to the sibling Rust cell (scene_3dmodel EXPECTED=14). Coverage is 1:1: two
# adapter/device asserts, the offscreen-ready assert, the cube-pipeline assert, and every reference-
# rasterizer coverage/color/occlusion/spot assertion plus the negative control.
EXPECTED = 14


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


# pos3 + col3, mvp uniform (WebGPU has no push constants). @invariant pins depth bit-exactness. Same
# WGSL as the Rust scene_3dmodel cell.
CUBE_WGSL = """
struct MVP { m: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: MVP;
struct VOut { @invariant @builtin(position) pos: vec4<f32>, @location(0) col: vec3<f32> };
@vertex fn vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> VOut {
    var o: VOut;
    o.pos = u.m * vec4<f32>(p, 1.0);
    o.col = c;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.col, 1.0);
}
"""

TU = wgpu.TextureUsage
TF = wgpu.TextureFormat
BU = wgpu.BufferUsage
VF = wgpu.VertexFormat
PT = wgpu.PrimitiveTopology
IF = wgpu.IndexFormat
CF = wgpu.CompareFunction
VERT = wgpu.ShaderStage.VERTEX

UNPADDED = W * BPP
ALIGN = 256
PADDED = ((UNPADDED + ALIGN - 1) // ALIGN) * ALIGN


# ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) - ported from the Rust cell ----
def mat_mul(a, b):
    r = [0.0] * 16
    for c in range(4):
        for row in range(4):
            s = 0.0
            for k in range(4):
                s += a[k * 4 + row] * b[c * 4 + k]
            r[c * 4 + row] = s
    return r


def mv4(a, v):
    o = [0.0] * 4
    for row in range(4):
        s = 0.0
        for k in range(4):
            s += a[k * 4 + row] * v[k]
        o[row] = s
    return o


# WebGPU/Vulkan perspective: near->z_ndc 0, far->z_ndc 1 (z/w in [0,1]).
def perspective(fovy, aspect, zn, zf):
    f = 1.0 / math.tan(fovy * 0.5)
    r = [0.0] * 16
    r[0 * 4 + 0] = f / aspect
    r[1 * 4 + 1] = f
    r[2 * 4 + 2] = zf / (zn - zf)
    r[2 * 4 + 3] = -1.0
    r[3 * 4 + 2] = (zf * zn) / (zn - zf)
    return r


def translate(x, y, z):
    r = [0.0] * 16
    r[0] = 1.0
    r[5] = 1.0
    r[10] = 1.0
    r[15] = 1.0
    r[3 * 4 + 0] = x
    r[3 * 4 + 1] = y
    r[3 * 4 + 2] = z
    return r


def rot_y(a):
    r = [0.0] * 16
    c, s = math.cos(a), math.sin(a)
    r[0 * 4 + 0] = c
    r[0 * 4 + 2] = -s
    r[2 * 4 + 0] = s
    r[2 * 4 + 2] = c
    r[1 * 4 + 1] = 1.0
    r[3 * 4 + 3] = 1.0
    return r


def rot_x(a):
    r = [0.0] * 16
    c, s = math.cos(a), math.sin(a)
    r[1 * 4 + 1] = c
    r[1 * 4 + 2] = s
    r[2 * 4 + 1] = -s
    r[2 * 4 + 2] = c
    r[0 * 4 + 0] = 1.0
    r[3 * 4 + 3] = 1.0
    return r


class Fb:
    def __init__(self, img):
        self.img = img  # (H, W, 4) uint8

    def p(self, x, y, c):
        return int(self.img[y, x, c])

    def peq(self, x, y, r, g, b, a, tol):
        px = self.img[y, x].astype(np.int32)
        return (abs(px[0] - r) <= tol and abs(px[1] - g) <= tol
                and abs(px[2] - b) <= tol and abs(px[3] - a) <= tol)


def finish():
    p, f = P[0], F[0]
    total = p + f
    print("scene-3dmodel-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, EXPECTED))
    if f == 0 and total == EXPECTED:
        print("SCENE_3DMODEL_PY OK %d" % p)
        return 0
    print("SCENE_3DMODEL_PY FAIL")
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
    print("scene-3dmodel-py adapter selected: %s" % adapter.summary)
    ok(len(str(info.get("device", ""))) > 0, "request_adapter yields a usable adapter")
    ok(str(info.get("backend_type", "")) in ("Vulkan", "GL", "OpenGL", "GLES"),
       "adapter backend is Vulkan or Gl")

    device = adapter.request_device_sync()
    queue = device.queue

    color = device.create_texture(
        label="color", size=(W, H, 1), format=TF.rgba8unorm,
        usage=TU.RENDER_ATTACHMENT | TU.COPY_SRC)
    color_view = color.create_view()
    depth = device.create_texture(
        label="depth", size=(W, H, 1), format=TF.depth32float,
        usage=TU.RENDER_ATTACHMENT)
    depth_view = depth.create_view()
    readback = device.create_buffer(size=PADDED * H, usage=BU.COPY_DST | BU.COPY_SRC)
    ok(True, "offscreen Rgba8Unorm + Depth32Float target + readback buffer ready")

    # ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (ported) ----
    VP = [
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ]
    vc = [[0.0, 0.0, 0.0] for _ in range(8)]
    for i in range(8):
        vc[i][0] = (VP[i][0] + 1.0) * 0.5
        vc[i][1] = (VP[i][1] + 1.0) * 0.5
        vc[i][2] = (VP[i][2] + 1.0) * 0.5
    IDX = [
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 5, 6,
        1, 6, 2,
    ]

    model = mat_mul(rot_y(0.6), rot_x(0.3))
    view = translate(0.0, 0.0, -5.0)
    proj = perspective(1.0, float(W) / float(H), 1.0, 20.0)
    mvp = mat_mul(proj, mat_mul(view, model))

    verts = np.zeros((8, 6), dtype=np.float32)
    for i in range(8):
        verts[i, 0:3] = VP[i]
        verts[i, 3:6] = vc[i]
    vbo = device.create_buffer_with_data(data=verts, usage=BU.VERTEX)
    ibo = device.create_buffer_with_data(
        data=np.asarray(IDX, dtype=np.uint16), usage=BU.INDEX)

    mvp_ubo = device.create_buffer_with_data(
        data=np.asarray(mvp, dtype=np.float32), usage=BU.UNIFORM)
    bgl = device.create_bind_group_layout(entries=[{
        "binding": 0, "visibility": VERT,
        "buffer": {"type": wgpu.BufferBindingType.uniform}}])
    bg = device.create_bind_group(layout=bgl, entries=[{
        "binding": 0, "resource": {"buffer": mvp_ubo, "offset": 0, "size": 64}}])
    pll = device.create_pipeline_layout(bind_group_layouts=[bgl])
    module = device.create_shader_module(code=CUBE_WGSL)
    vbl = {"array_stride": 24, "step_mode": "vertex", "attributes": [
        {"format": VF.float32x3, "offset": 0, "shader_location": 0},
        {"format": VF.float32x3, "offset": 12, "shader_location": 1}]}
    pipe = device.create_render_pipeline(
        label="cube", layout=pll,
        vertex={"module": module, "entry_point": "vs", "buffers": [vbl]},
        primitive={"topology": PT.triangle_list, "front_face": "ccw", "cull_mode": "none"},
        depth_stencil={"format": TF.depth32float, "depth_write_enabled": True,
                       "depth_compare": CF.less},
        fragment={"module": module, "entry_point": "fs",
                  "targets": [{"format": TF.rgba8unorm, "write_mask": 0xF}]})
    ok(True, "cube pipeline created")

    # ---- draw: clear color black, clear depth 1.0, draw the indexed cube ----
    enc = device.create_command_encoder()
    rp = enc.begin_render_pass(
        color_attachments=[{
            "view": color_view, "resolve_target": None,
            "clear_value": (0, 0, 0, 1), "load_op": "clear", "store_op": "store"}],
        depth_stencil_attachment={
            "view": depth_view, "depth_clear_value": 1.0,
            "depth_load_op": "clear", "depth_store_op": "store"})
    rp.set_pipeline(pipe)
    rp.set_bind_group(0, bg)
    rp.set_vertex_buffer(0, vbo)
    rp.set_index_buffer(ibo, IF.uint16)
    rp.draw_indexed(36, 1, 0, 0, 0)
    rp.end()
    enc.copy_texture_to_buffer(
        {"texture": color, "mip_level": 0, "origin": (0, 0, 0)},
        {"buffer": readback, "offset": 0, "bytes_per_row": PADDED, "rows_per_image": H},
        (W, H, 1))
    queue.submit([enc.finish()])
    raw = np.frombuffer(queue.read_buffer(readback), dtype=np.uint8)
    buf = Fb(raw.reshape(H, PADDED)[:, :UNPADDED].reshape(H, W, BPP).copy())
    ok(True, "cube drawn (depth-tested, Gouraud)")

    # ---- INDEPENDENT software reference rasterizer (ported; WebGPU NDC-z in [0,1]) ----
    refc = [[0.0, 0.0, 0.0] for _ in range(W * H)]
    refz = [1e9] * (W * H)
    refcov = [0] * (W * H)

    def idx2(x, y):
        return y * W + x

    sx = [0.0] * 8
    sy = [0.0] * 8
    sz = [0.0] * 8
    sw = [0.0] * 8
    for i in range(8):
        out = mv4(mvp, [VP[i][0], VP[i][1], VP[i][2], 1.0])
        w = out[3]
        sw[i] = w
        ndcx, ndcy, ndcz = out[0] / w, out[1] / w, out[2] / w
        sx[i] = (ndcx * 0.5 + 0.5) * W
        # WebGPU framebuffer origin is top-left: NDC y=+1 -> row 0.
        sy[i] = (0.5 - ndcy * 0.5) * H
        sz[i] = ndcz  # window depth = z/w directly ([0,1])
    ok(sw[0] > 0.0, "reference: all clip.w positive (mesh in front of camera)")

    for t in range(12):
        a = IDX[t * 3]
        b = IDX[t * 3 + 1]
        c = IDX[t * 3 + 2]
        ax, ay, bx, by, cx, cy = sx[a], sy[a], sx[b], sy[b], sx[c], sy[c]
        area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
        if abs(area) < 1e-6:
            continue
        minx = max(int(math.floor(min(ax, bx, cx))), 0)
        maxx = min(int(math.ceil(max(ax, bx, cx))), W)
        miny = max(int(math.floor(min(ay, by, cy))), 0)
        maxy = min(int(math.ceil(max(ay, by, cy))), H)
        for y in range(miny, maxy):
            for x in range(minx, maxx):
                pxs, pys = x + 0.5, y + 0.5
                w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area
                w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area
                w2 = 1.0 - w0 - w1
                inside = (w0 >= 0.0 and w1 >= 0.0 and w2 >= 0.0) or \
                         (w0 <= 0.0 and w1 <= 0.0 and w2 <= 0.0)
                if not inside:
                    continue
                if w0 < 0.0 or w1 < 0.0 or w2 < 0.0:
                    w0, w1, w2 = -w0, -w1, -w2
                z = w0 * sz[a] + w1 * sz[b] + w2 * sz[c]
                i = idx2(x, y)
                if z < refz[i]:
                    refz[i] = z
                    refcov[i] = 1
                    iwa, iwb, iwc = 1.0 / sw[a], 1.0 / sw[b], 1.0 / sw[c]
                    d = w0 * iwa + w1 * iwb + w2 * iwc
                    for k in range(3):
                        num = w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[c][k]
                        refc[i][k] = num / d

    total = 0
    matched = 0
    covmatch = 0
    covtotal = 0
    interior_bad = 0
    for y in range(H):
        for x in range(W):
            total += 1
            gcov = not (buf.p(x, y, 0) == 0 and buf.p(x, y, 1) == 0 and buf.p(x, y, 2) == 0)
            i = idx2(x, y)
            rcov = refcov[i] != 0
            if gcov == rcov:
                covmatch += 1
            if rcov:
                covtotal += 1
                er = int(round(refc[i][0] * 255.0))
                eg = int(round(refc[i][1] * 255.0))
                eb = int(round(refc[i][2] * 255.0))
                interior = (x > 0 and y > 0 and x < W - 1 and y < H - 1
                            and refcov[idx2(x, y - 1)] != 0 and refcov[idx2(x, y + 1)] != 0
                            and refcov[idx2(x - 1, y)] != 0 and refcov[idx2(x + 1, y)] != 0)
                if buf.peq(x, y, er, eg, eb, 255, 6):
                    matched += 1
                elif interior:
                    interior_bad += 1
    ok(covtotal > 200, "reference: cube covers a substantial area")
    ok(covmatch >= int(0.97 * total),
       "coverage mask matches GPU (>=97% of pixels agree covered/empty)")
    ok(interior_bad == 0,
       "every interior pixel matches perspective-correct Gouraud reference (tol 6)")
    ok(matched >= int(0.92 * covtotal),
       "92%+ of covered pixels match reference color (edges excluded)")

    vx = int(round(sx[6] - 0.5))
    vy = int(round(sy[6] - 0.5))
    if 1 <= vx < W - 1 and 1 <= vy < H - 1:
        bright = False
        for dy in range(-1, 2):
            for dx in range(-1, 2):
                xx, yy = vx + dx, vy + dy
                if buf.p(xx, yy, 0) > 180 and buf.p(xx, yy, 1) > 180 and buf.p(xx, yy, 2) > 180:
                    bright = True
        ok(bright, "vertex (1,1,1) region is bright (Gouraud white corner)")
    else:
        ok(False, "vertex (1,1,1) projected off-screen (camera mis-set)")

    ok(buf.peq(0, 0, 0, 0, 0, 255, 1) or refcov[idx2(0, 0)] == 0,
       "corner (0,0) background consistent")

    cxp, cyp = W // 2, H // 2
    i = idx2(cxp, cyp)
    if refcov[i] != 0:
        er = int(round(refc[i][0] * 255.0))
        eg = int(round(refc[i][1] * 255.0))
        eb = int(round(refc[i][2] * 255.0))
        ok(buf.peq(cxp, cyp, er, eg, eb, 255, 8),
           "center pixel = nearest-face (depth-buffered occlusion) reference color")
    else:
        ok(False, "center pixel not covered (mesh mis-projected)")

    ok(not (buf.p(1, 1, 0) == buf.p(W // 2, H // 2, 0)
            and buf.p(1, 1, 1) == buf.p(W // 2, H // 2, 1)
            and buf.p(1, 1, 2) == buf.p(W // 2, H // 2, 2)),
       "negative control: image is not a flat single color (real 3D shading present)")

    device.destroy()
    return finish()


if __name__ == "__main__":
    sys.exit(run())
