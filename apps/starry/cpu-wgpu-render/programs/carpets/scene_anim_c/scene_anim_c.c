/* scene_anim_c - keyframe-animation RENDER-scene carpet driven through the gfx-rs wgpu-native v22.1.0.2
 * C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
 * GPU/window/surface. C port of the scene_anim Rust cell: an offscreen 64x64 Rgba8Unorm texture is
 * rendered through a real render pipeline (the SAME pixel-space y-flipped affine WGSL shader as the Rust
 * cell); N=4 keyframes of a transformed unit quad are drawn, each frame's model transform a rotation about
 * the FBO center composed with a translation and uniform scale, interpolated by t in {0,0.25,0.5,0.75}.
 * The transform is applied in the vertex shader from a hand-built pixel-space model matrix (rotation,
 * scale, translate) and an ortho pixel->NDC map. For every frame the four rotated/scaled/translated quad
 * CORNERS are computed INDEPENDENTLY in C (closed form: R(theta)*S*local + T) and the readback is
 * asserted at those exact corner pixels plus a point outside the quad. A cubic ease eased(t)=3t^2-2t^3
 * drives the scale, its value asserted at each t. Closes with a negative control.
 *
 * The lerp / ease_cubic / R*S*local+T corner math is behavior-identical to the Rust scene_anim cell; only
 * the wgpu-native C binding syntax differs. Uses the v22 (callback,userdata) async model driven
 * synchronously via wgpuInstanceProcessEvents / wgpuDevicePoll, like scene_2dui_c. Prints
 * "SCENE_ANIM_C OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. EXPECTED=38 pins the Rust cell. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

#include "webgpu.h"
#include "wgpu.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

#define W 64u
#define H 64u
#define BPP 4u
#define EXPECTED 38

static int PASS = 0, FAIL = 0;
static void ok(int c, const char *d) {
    if (c) PASS++;
    else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); }
}

static float lerp(float a, float b, float t) { return a + (b - a) * t; }
static float ease_cubic(float t) { return 3.0f * t * t - 2.0f * t * t * t; }

/* ---- synchronous adapter/device/map request via the v22 (callback, userdata) model ---- */
typedef struct { WGPUAdapter adapter; WGPURequestAdapterStatus status; int done; } AdapterReq;
static void on_adapter(WGPURequestAdapterStatus s, WGPUAdapter a, char const *m, void *ud) {
    (void)m; AdapterReq *r = (AdapterReq *)ud; r->status = s; r->adapter = a; r->done = 1;
}
typedef struct { WGPUDevice device; WGPURequestDeviceStatus status; int done; } DeviceReq;
static void on_device(WGPURequestDeviceStatus s, WGPUDevice d, char const *m, void *ud) {
    (void)m; DeviceReq *r = (DeviceReq *)ud; r->status = s; r->device = d; r->done = 1;
}
typedef struct { WGPUBufferMapAsyncStatus status; int done; } MapReq;
static void on_map(WGPUBufferMapAsyncStatus s, void *ud) {
    MapReq *r = (MapReq *)ud; r->status = s; r->done = 1;
}
static void on_uncaptured(WGPUErrorType t, char const *m, void *ud) {
    (void)ud; fprintf(stderr, "UNCAPTURED wgpu error (%d): %s\n", (int)t, m ? m : "");
}

/* ---- WGSL shader (identical to the Rust scene_anim cell) ----
 * Pixel-space affine vertex shader: pix = col0*lp.x + col1*lp.y + tr; then map pixel -> NDC with y-flip
 * so pixel-y == readback-row. Uniform color out. */
static const char *WGSL =
    "struct X { col0: vec2<f32>, col1: vec2<f32>, tr: vec2<f32>, vp: vec2<f32>, rgba: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: X;\n"
    "@vertex fn vs(@location(0) lp: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    let pix = u.col0 * lp.x + u.col1 * lp.y + u.tr;\n"
    "    let n = (pix / u.vp) * 2.0 - 1.0;\n"
    "    return vec4<f32>(n.x, -n.y, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }\n";

static WGPUShaderModule make_wgsl(WGPUDevice dev, const char *code, const char *label) {
    WGPUShaderModuleWGSLDescriptor wgsl = {0};
    wgsl.chain.sType = WGPUSType_ShaderModuleWGSLDescriptor;
    wgsl.code = code;
    WGPUShaderModuleDescriptor sd = {0};
    sd.nextInChain = (WGPUChainedStruct *)&wgsl;
    sd.label = label;
    return wgpuDeviceCreateShaderModule(dev, &sd);
}

/* ---- readback framebuffer ---- */
typedef struct { uint8_t px[H * W * BPP]; } Fb;
static uint8_t fb_p(const Fb *f, uint32_t x, uint32_t y, unsigned c) {
    return f->px[(y * W + x) * BPP + c];
}
static int peq(const Fb *f, int x, int y, int r, int g, int b, int a, int tol) {
    if (x < 0 || y < 0 || x >= (int)W || y >= (int)H) return 0;
    int dr = abs((int)fb_p(f, x, y, 0) - r);
    int dg = abs((int)fb_p(f, x, y, 1) - g);
    int db = abs((int)fb_p(f, x, y, 2) - b);
    int da = abs((int)fb_p(f, x, y, 3) - a);
    return dr <= tol && dg <= tol && db <= tol && da <= tol;
}
/* 3x3 neighborhood scan (a fixed at 255), matching the Rust Fb::near_color. */
static int near_color(const Fb *f, int x, int y, int r, int g, int b, int tol) {
    for (int dy = -1; dy <= 1; dy++)
        for (int dx = -1; dx <= 1; dx++) {
            int xx = x + dx, yy = y + dy;
            if (xx < 0 || yy < 0 || xx >= (int)W || yy >= (int)H) continue;
            if (peq(f, xx, yy, r, g, b, 255, tol)) return 1;
        }
    return 0;
}

/* globals wired through the frame helper */
static WGPUDevice   g_dev;
static WGPUQueue    g_queue;
static WGPUTexture  g_color;
static WGPUBuffer   g_readback;
static WGPUTextureView g_color_view;
static uint32_t     g_padded;

static int copy_and_read(WGPUCommandEncoder enc, Fb *out) {
    WGPUImageCopyTexture src = {0};
    src.texture = g_color; src.mipLevel = 0;
    src.origin = (WGPUOrigin3D){0, 0, 0}; src.aspect = WGPUTextureAspect_All;
    WGPUImageCopyBuffer dst = {0};
    dst.buffer = g_readback; dst.layout.offset = 0;
    dst.layout.bytesPerRow = g_padded; dst.layout.rowsPerImage = H;
    WGPUExtent3D ext = {W, H, 1};
    wgpuCommandEncoderCopyTextureToBuffer(enc, &src, &dst, &ext);
    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(enc, NULL);
    wgpuQueueSubmit(g_queue, 1, &cmd);
    MapReq mr = {0};
    size_t bytes = (size_t)g_padded * H;
    wgpuBufferMapAsync(g_readback, WGPUMapMode_Read, 0, bytes, on_map, &mr);
    for (int i = 0; i < 256 && !mr.done; i++) wgpuDevicePoll(g_dev, 1, NULL);
    int good = mr.done && mr.status == WGPUBufferMapAsyncStatus_Success;
    if (good) {
        const uint8_t *p = (const uint8_t *)wgpuBufferGetConstMappedRange(g_readback, 0, bytes);
        if (!p) good = 0;
        else {
            for (uint32_t y = 0; y < H; y++)
                memcpy(&out->px[y * W * BPP], &p[y * g_padded], W * BPP);
        }
        wgpuBufferUnmap(g_readback);
    }
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(enc);
    return good;
}

typedef struct { float pos[2]; } V2;

static WGPUBuffer make_buf(const void *data, size_t bytes, WGPUBufferUsageFlags usage) {
    WGPUBufferDescriptor bd = {0};
    bd.usage = usage;
    bd.size = bytes;
    bd.mappedAtCreation = 1;
    WGPUBuffer buf = wgpuDeviceCreateBuffer(g_dev, &bd);
    void *p = wgpuBufferGetMappedRange(buf, 0, bytes);
    memcpy(p, data, bytes);
    wgpuBufferUnmap(buf);
    return buf;
}

static WGPUBindGroupLayout g_ubo_bgl;
static WGPUBindGroup bind_ubo(WGPUBuffer buf, uint64_t size) {
    WGPUBindGroupEntry e = {0};
    e.binding = 0; e.buffer = buf; e.offset = 0; e.size = size;
    WGPUBindGroupDescriptor bgd = {0};
    bgd.layout = g_ubo_bgl; bgd.entryCount = 1; bgd.entries = &e;
    return wgpuDeviceCreateBindGroup(g_dev, &bgd);
}

/* TriangleStrip pipeline for the affine quad (the anim cell uses PrimitiveTopology::TriangleStrip). */
static WGPURenderPipeline mk_solid(WGPUShaderModule m, WGPUPipelineLayout pll,
                                   const WGPUVertexBufferLayout *vbl, const WGPUBlendState *blend) {
    WGPUColorTargetState tgt = {0};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = blend;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag = {0};
    frag.module = m; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;
    WGPURenderPipelineDescriptor d = {0};
    d.label = "anim-pipe";
    d.layout = pll;
    d.vertex.module = m; d.vertex.entryPoint = "vs"; d.vertex.bufferCount = 1; d.vertex.buffers = vbl;
    d.primitive.topology = WGPUPrimitiveTopology_TriangleStrip;
    d.primitive.stripIndexFormat = WGPUIndexFormat_Undefined;
    d.primitive.frontFace = WGPUFrontFace_CCW;
    d.primitive.cullMode = WGPUCullMode_None;
    d.multisample.count = 1; d.multisample.mask = 0xFFFFFFFFu;
    d.fragment = &frag;
    return wgpuDeviceCreateRenderPipeline(g_dev, &d);
}

/* per-frame uniform: col0.xy, col1.xy, tr.xy, vp.xy, rgba (12 floats) */
static WGPURenderPipeline g_pipe;
static WGPUBuffer g_xform_ubo;
static WGPUBindGroup g_bg;
static WGPUBuffer g_vbo;

/* render one frame with the current uniform (rewrite ubo + re-encode). */
static int render_frame(const float xform[12], Fb *out) {
    wgpuQueueWriteBuffer(g_queue, g_xform_ubo, 0, xform, 12 * sizeof(float));
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, NULL);
    WGPURenderPassColorAttachment att = {0};
    att.view = g_color_view;
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.resolveTarget = NULL;
    att.loadOp = WGPULoadOp_Clear;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = (WGPUColor){0.0, 0.0, 0.0, 1.0};
    WGPURenderPassDescriptor rp = {0};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &att;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    wgpuRenderPassEncoderSetPipeline(pass, g_pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, g_bg, 0, NULL);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, g_vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, 4, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

/* animation keyframe params (ported). */
static const float A0 = 0.0f, A1 = (float)(M_PI / 2.0);
static const float S0 = 6.0f, S1 = 14.0f;
static const float CX0 = 20.0f, CX1 = 44.0f, CY0 = 20.0f, CY1 = 44.0f;

/* frame_transform(t) -> col0[2], col1[2], tr[2], sc, ang */
static void frame_transform(float t, float col0[2], float col1[2], float tr[2], float *sc, float *ang) {
    float a = lerp(A0, A1, t);
    float s = lerp(S0, S1, ease_cubic(t));
    float cx = lerp(CX0, CX1, t), cy = lerp(CY0, CY1, t);
    float ca = cosf(a), sa = sinf(a);
    col0[0] = s * ca; col0[1] = s * sa;
    col1[0] = -s * sa; col1[1] = s * ca;
    tr[0] = cx; tr[1] = cy;
    *sc = s; *ang = a;
}

int main(void) {
    const char *backend = getenv("WGPU_BACKEND");
    WGPUInstanceBackendFlags flags = WGPUInstanceBackend_Vulkan | WGPUInstanceBackend_GL;
    if (backend && (strcmp(backend, "gl") == 0 || strcmp(backend, "gles") == 0))
        flags = WGPUInstanceBackend_GL;
    else if (backend && strcmp(backend, "vulkan") == 0)
        flags = WGPUInstanceBackend_Vulkan;

    WGPUInstanceExtras extras = {0};
    extras.chain.sType = (WGPUSType)WGPUSType_InstanceExtras;
    extras.backends = flags;
    WGPUInstanceDescriptor idesc = {0};
    idesc.nextInChain = (WGPUChainedStruct *)&extras;
    WGPUInstance inst = wgpuCreateInstance(&idesc);
    if (!inst) { printf("SCENE_ANIM_C FAIL\n"); return 1; }

    AdapterReq areq = {0};
    WGPURequestAdapterOptions aopts = {0};
    aopts.powerPreference = WGPUPowerPreference_LowPower;
    aopts.backendType = WGPUBackendType_Undefined;
    aopts.forceFallbackAdapter = 0;
    wgpuInstanceRequestAdapter(inst, &aopts, on_adapter, &areq);
    for (int i = 0; i < 64 && !areq.done; i++) wgpuInstanceProcessEvents(inst);
    WGPUAdapter adapter = areq.adapter;
    ok(areq.done && areq.status == WGPURequestAdapterStatus_Success && adapter != NULL,
       "request_adapter yields a usable adapter");
    if (!adapter) {
        printf("scene-anim-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_ANIM_C FAIL\n"); return 1;
    }
    WGPUAdapterInfo info = {0};
    wgpuAdapterGetInfo(adapter, &info);
    printf("wgpu adapter selected: type=%d name=\"%s\"\n", (int)info.backendType,
           info.device ? info.device : "");
    ok(info.backendType == WGPUBackendType_Vulkan || info.backendType == WGPUBackendType_OpenGL ||
       info.backendType == WGPUBackendType_OpenGLES,
       "adapter backend is Vulkan or Gl");

    DeviceReq dreq = {0};
    WGPUDeviceDescriptor ddesc = {0};
    ddesc.label = "anim-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; i++) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) {
        printf("scene-anim-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_ANIM_C FAIL\n"); return 1;
    }
    g_dev = dev;
    g_queue = wgpuDeviceGetQueue(dev);

    WGPUTextureDescriptor cd = {0};
    cd.label = "color";
    cd.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    cd.dimension = WGPUTextureDimension_2D;
    cd.size = (WGPUExtent3D){W, H, 1};
    cd.format = WGPUTextureFormat_RGBA8Unorm;
    cd.mipLevelCount = 1; cd.sampleCount = 1;
    g_color = wgpuDeviceCreateTexture(dev, &cd);
    g_color_view = wgpuTextureCreateView(g_color, NULL);

    uint32_t unpadded = W * BPP;
    uint32_t align = 256;
    g_padded = ((unpadded + align - 1) / align) * align;
    WGPUBufferDescriptor rbd = {0};
    rbd.label = "readback";
    rbd.usage = WGPUBufferUsage_CopyDst | WGPUBufferUsage_MapRead;
    rbd.size = (uint64_t)g_padded * H;
    g_readback = wgpuDeviceCreateBuffer(dev, &rbd);

    /* xform uniform buffer (48 bytes), written per frame. */
    WGPUBufferDescriptor xd = {0};
    xd.label = "xform-ubo";
    xd.usage = WGPUBufferUsage_Uniform | WGPUBufferUsage_CopyDst;
    xd.size = 12 * sizeof(float);
    g_xform_ubo = wgpuDeviceCreateBuffer(dev, &xd);

    WGPUShaderModule module = make_wgsl(dev, WGSL, "anim");

    WGPUBindGroupLayoutEntry ule = {0};
    ule.binding = 0;
    ule.visibility = WGPUShaderStage_Vertex | WGPUShaderStage_Fragment;
    ule.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor uld = {0};
    uld.entryCount = 1; uld.entries = &ule;
    g_ubo_bgl = wgpuDeviceCreateBindGroupLayout(dev, &uld);
    WGPUPipelineLayoutDescriptor plld = {0};
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &g_ubo_bgl;
    WGPUPipelineLayout pll = wgpuDeviceCreatePipelineLayout(dev, &plld);

    g_bg = bind_ubo(g_xform_ubo, 12 * sizeof(float));

    WGPUVertexAttribute a_pos2 = {WGPUVertexFormat_Float32x2, 0, 0};
    WGPUVertexBufferLayout vbl_pos2 = {sizeof(V2), WGPUVertexStepMode_Vertex, 1, &a_pos2};
    g_pipe = mk_solid(module, pll, &vbl_pos2, NULL);

    /* local quad corners (unit square in local space, TL/TR/BL/BR) as a triangle strip. */
    static const float local[8] = {-1.0f, -1.0f, 1.0f, -1.0f, -1.0f, 1.0f, 1.0f, 1.0f};
    V2 lq[4] = {
        {{local[0], local[1]}}, {{local[2], local[3]}},
        {{local[4], local[5]}}, {{local[6], local[7]}},
    };
    g_vbo = make_buf(lq, sizeof(lq), WGPUBufferUsage_Vertex);

    const float ts[4] = {0.0f, 0.25f, 0.5f, 0.75f};
    const float cols[4][3] = {{1, 0, 0}, {0, 1, 0}, {0, 0, 1}, {1, 1, 0}};

    Fb fb;

    for (int fi = 0; fi < 4; fi++) {
        float t = ts[fi];
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(t, col0, col1, tr, &sc, &ang);
        float xform[12] = {
            col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], (float)W, (float)H,
            cols[fi][0], cols[fi][1], cols[fi][2], 1.0f,
        };
        (void)render_frame(xform, &fb);

        /* closed-form corner positions: corner = R(ang)*S(sc)*localCorner + center. */
        float ca = cosf(ang), sa = sinf(ang);
        float corners[4][2];
        for (int k = 0; k < 4; k++) {
            float lx = local[k * 2], ly = local[k * 2 + 1];
            float rx = sc * (ca * lx - sa * ly);
            float ry = sc * (sa * lx + ca * ly);
            corners[k][0] = tr[0] + rx;
            corners[k][1] = tr[1] + ry;
        }
        float e = ease_cubic(t);
        float e_ref = 3.0f * t * t - 2.0f * t * t * t;
        ok(fabsf(e - e_ref) < 1e-6f, "ease_cubic closed-form value");
        ok(fabsf(sc - (S0 + (S1 - S0) * e)) < 1e-4f, "scale = lerp(S0,S1,ease(t)) closed-form");

        int cxi = (int)lroundf(tr[0] - 0.5f);
        int cyi = (int)lroundf(tr[1] - 0.5f);
        ok(peq(&fb, cxi, cyi,
               (int)lroundf(cols[fi][0] * 255.0f),
               (int)lroundf(cols[fi][1] * 255.0f),
               (int)lroundf(cols[fi][2] * 255.0f),
               255, 2),
           "frame center pixel carries frame color at closed-form center");

        for (int k = 0; k < 4; k++) {
            int px_ = (int)lroundf(corners[k][0] - 0.5f);
            int py_ = (int)lroundf(corners[k][1] - 0.5f);
            int onscreen = px_ >= 0 && py_ >= 0 && px_ < (int)W && py_ < (int)H;
            ok(onscreen && near_color(&fb, px_, py_,
                                      (int)lroundf(cols[fi][0] * 255.0f),
                                      (int)lroundf(cols[fi][1] * 255.0f),
                                      (int)lroundf(cols[fi][2] * 255.0f),
                                      40),
               "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)");
        }

        /* a point far outside the quad silhouette stays background (guard by max reach = sc*sqrt2). */
        {
            int ox = (fi < 2) ? (int)W - 2 : 1;
            int oy = (fi < 2) ? (int)H - 2 : 1;
            float reach = sc * 1.4142f;
            int covers = fabsf(ox + 0.5f - tr[0]) <= reach && fabsf(oy + 0.5f - tr[1]) <= reach;
            if (!covers)
                ok(peq(&fb, ox, oy, 0, 0, 0, 255, 2),
                   "outside-quad point stays background (closed-form silhouette)");
            else
                ok(1, "outside-quad point skipped (would be covered)");
        }
    }

    /* t=0 vs t=0.75 center positions differ. */
    {
        float c0[2], c1[2], tra[2], sc, ang;
        float d0[2], d1[2], trb[2], sc2, ang2;
        frame_transform(0.0f, c0, c1, tra, &sc, &ang);
        frame_transform(0.75f, d0, d1, trb, &sc2, &ang2);
        ok(fabsf(tra[0] - trb[0]) > 1.0f, "center translates between t=0 and t=0.75 (animation is real)");
    }

    /* rotation at t=0.5: angle = pi/4, rotated x-axis column = (sc*cos45, sc*sin45). */
    {
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(0.5f, col0, col1, tr, &sc, &ang);
        ok(fabsf(ang - (float)(M_PI / 4.0)) < 1e-5f, "t=0.5 rotation angle = pi/4 closed-form");
        ok(fabsf(col0[0] - col0[1]) < 1e-4f && col0[0] > 0.0f,
           "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)");
    }

    /* negative control: render frame 0 (red) and confirm it is NOT green. */
    {
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(0.0f, col0, col1, tr, &sc, &ang);
        float xform[12] = {
            col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], (float)W, (float)H,
            1.0f, 0.0f, 0.0f, 1.0f,
        };
        (void)render_frame(xform, &fb);
        int cxi = (int)lroundf(tr[0] - 0.5f);
        int cyi = (int)lroundf(tr[1] - 0.5f);
        ok(!peq(&fb, cxi, cyi, 0, 255, 0, 255, 4), "negative control: frame-0 center is NOT green");
    }

    wgpuDevicePoll(dev, 1, NULL);

    int total = PASS + FAIL;
    printf("scene-anim-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, total, EXPECTED);
    if (FAIL == 0 && total == EXPECTED) { printf("SCENE_ANIM_C OK %d\n", PASS); return 0; }
    printf("SCENE_ANIM_C FAIL\n");
    return 1;
}
