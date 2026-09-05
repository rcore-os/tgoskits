/* wgpu_render_c_full_api - WebGPU/wgpu RENDER carpet on Mesa lavapipe (software Vulkan on the CPU,
 * no GPU/window/surface/swapchain), driven through the C webgpu.h / wgpu.h API of gfx-rs wgpu-native
 * v22.1.0.2 (matches the wgpu="22" crate the Rust reference cell links). It renders offscreen into a
 * 64x64 RGBA8Unorm texture (RENDER_ATTACHMENT | COPY_SRC) through real render pipelines with WGSL
 * vertex+fragment shaders, copies the texture into a MAP_READ buffer honouring the 256-byte
 * bytesPerRow alignment (rows are padded on copy then unpadded on readback), maps it, and
 * hard-asserts every pixel against a closed-form reference.
 *
 * Coverage mirrors the verified Rust reference cell 1:1 (56 assertions, same closed-form pixel
 * references): render-pass clear, a solid quad (uniform-buffer color; WebGPU has no push constants by
 * default), a per-vertex axis-aligned linear gradient, an @builtin(position) checkerboard, a scissor
 * rect, a viewport restriction, alpha blend a=191, a sub-rectangle readback; then exhaustive per-API
 * coverage: all 5 WebGPU primitive topologies (PointList 1px / LineList / LineStrip / TriangleList /
 * TriangleStrip - WebGPU has NO triangle-fan), a blend factor+op matrix (One/Zero, One/One Add,
 * Zero/One, Dst/Zero, One/One Max, One/One ReverseSubtract alpha=0), the full 8-way
 * WGPUCompareFunction depth matrix (Depth32Float attachment; a z=0.5 quad vs clear-depth 0.75 draws
 * only under {Always,Less,LessEqual,NotEqual}), face culling + winding (cull None vs Back with
 * FrontFace CCW vs CW, cull Front vs Back), a colour write mask (RED vs ALL), format+limit queries, a
 * 2x2 RGBA8 texture upload + Nearest sampling through a sampler + bind group (corners TL red / TR
 * green / BL blue / BR white), closing with a negative control.
 *
 * v22 API note vs the newer sibling compute cell: this header is the OLD callback model -
 * wgpuInstanceRequestAdapter/wgpuAdapterRequestDevice take a bare (callback, userdata) pair whose
 * callback receives a `char const *` message (not the WGPUFuture/WGPUStringView model), WGSL modules
 * chain a WGPUShaderModuleWGSLDescriptor, and wgpuDevicePopErrorScope uses a WGPUErrorCallback. The
 * async requests resolve synchronously under wgpu-native, driven with wgpuInstanceProcessEvents /
 * wgpuDevicePoll(device, wait=true, NULL). Backend is selected via WGPU_BACKEND
 * (vulkan=lavapipe / gl=llvmpipe) like the compute cell.
 *
 * Format-feature query calibration: wgpu-native v22's webgpu.h exposes NO
 * wgpuAdapterGetTextureFormatFeatures (the Rust wgpu::Adapter::get_texture_format_features has no
 * C-API counterpart in this header - confirmed absent from both the header and the .so exports). To
 * keep the assertion count at the pinned 56 with a genuinely equivalent check, the two format-feature
 * assertions are realised behaviorally: a texture of that format is created with RENDER_ATTACHMENT
 * usage under a validation error scope, and the popped error must be NoError - functionally proving
 * the format supports RENDER_ATTACHMENT on this adapter. Likewise, v22 wgpu-native's C
 * wgpuDeviceGetLimits leaves maxColorAttachments unpopulated (reads 0, unlike the Rust wgpu crate's
 * default-limits table), so the "max_color_attachments >= 1" assertion is realised behaviorally by
 * validating a one-color-attachment render pass under an error scope. Both keep the count at 56.
 *
 * Prints "WGPU_RENDER_C_FULL_API OK 56" with FAIL=0 only when every assertion passes and the count
 * equals the pinned EXPECTED total. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

#include "webgpu.h"
#include "wgpu.h"

#define W 64u
#define H 64u
#define BPP 4u
#define EXPECTED 56

static int PASS = 0, FAIL = 0;
static void ok(int c, const char *d) {
    if (c) PASS++;
    else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); }
}

/* ---- synchronous adapter/device request via the v22 (callback, userdata) model ---- */
typedef struct { WGPUAdapter adapter; WGPURequestAdapterStatus status; int done; } AdapterReq;
static void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter,
                       char const *message, void *ud) {
    (void)message;
    AdapterReq *r = (AdapterReq *)ud;
    r->status = status; r->adapter = adapter; r->done = 1;
}
typedef struct { WGPUDevice device; WGPURequestDeviceStatus status; int done; } DeviceReq;
static void on_device(WGPURequestDeviceStatus status, WGPUDevice device,
                      char const *message, void *ud) {
    (void)message;
    DeviceReq *r = (DeviceReq *)ud;
    r->status = status; r->device = device; r->done = 1;
}
typedef struct { WGPUBufferMapAsyncStatus status; int done; } MapReq;
static void on_map(WGPUBufferMapAsyncStatus status, void *ud) {
    MapReq *r = (MapReq *)ud;
    r->status = status; r->done = 1;
}
/* pop-error-scope callback (v22 WGPUErrorCallback): capture the caught error type */
typedef struct { WGPUErrorType type; int done; } ScopeReq;
static void on_scope(WGPUErrorType type, char const *message, void *ud) {
    (void)message;
    ScopeReq *r = (ScopeReq *)ud;
    r->type = type; r->done = 1;
}
/* uncaptured-error callback: count device-level validation errors */
typedef struct { int count; WGPUErrorType last; } UncapReq;
static void on_uncaptured(WGPUErrorType type, char const *message, void *ud) {
    (void)message;
    UncapReq *r = (UncapReq *)ud;
    r->count++; r->last = type;
}

/* push a validation scope, run `body`, pop it synchronously; returns the caught error type */
static WGPUErrorType pop_error_scope(WGPUDevice dev, WGPUInstance inst) {
    ScopeReq sr = {0};
    wgpuDevicePopErrorScope(dev, on_scope, &sr);
    for (int i = 0; i < 256 && !sr.done; i++) {
        wgpuDevicePoll(dev, 1, NULL);
        wgpuInstanceProcessEvents(inst);
    }
    if (!sr.done) return WGPUErrorType_Unknown;
    return sr.type;
}

/* ---- WGSL shaders (identical to the Rust reference cell) ---- */
static const char *SOLID_WGSL =
    "struct Solid { rgba: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: Solid;\n"
    "@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    return vec4<f32>(p, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }\n";

static const char *GRAD_WGSL =
    "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };\n"
    "@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    o.pos = vec4<f32>(p, 0.0, 1.0);\n"
    "    o.col = c;\n"
    "    return o;\n"
    "}\n"
    "@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return in.col; }\n";

static const char *CHECK_WGSL =
    "@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    return vec4<f32>(p, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {\n"
    "    let cx = u32(floor(fc.x)) / 8u;\n"
    "    let cy = u32(floor(fc.y)) / 8u;\n"
    "    if (((cx + cy) & 1u) == 0u) {\n"
    "        return vec4<f32>(1.0, 1.0, 1.0, 1.0);\n"
    "    }\n"
    "    return vec4<f32>(0.0, 0.0, 0.0, 1.0);\n"
    "}\n";

/* @invariant on the clip-space position makes @builtin(position).z bit-exact, so an exact-EQUAL /
 * NotEqual depth compare against the cleared depth is deterministic across pipelines/runs (this is
 * the informational invariance warning wgpu-native emits for depth-tested pipelines). */
static const char *POS3_WGSL =
    "struct Solid { rgba: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: Solid;\n"
    "@vertex fn vs(@location(0) p: vec3<f32>) -> @invariant @builtin(position) vec4<f32> {\n"
    "    return vec4<f32>(p, 1.0);\n"
    "}\n"
    "@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }\n";

static const char *TEX_WGSL =
    "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };\n"
    "@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    o.pos = vec4<f32>(p, 0.0, 1.0);\n"
    "    o.uv = uv;\n"
    "    return o;\n"
    "}\n"
    "@group(0) @binding(0) var t: texture_2d<f32>;\n"
    "@group(0) @binding(1) var s: sampler;\n"
    "@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {\n"
    "    return textureSample(t, s, in.uv);\n"
    "}\n";

static WGPUShaderModule make_wgsl(WGPUDevice dev, const char *code, const char *label) {
    WGPUShaderModuleWGSLDescriptor wgsl = {0};
    wgsl.chain.sType = WGPUSType_ShaderModuleWGSLDescriptor;
    wgsl.code = code;
    WGPUShaderModuleDescriptor sd = {0};
    sd.nextInChain = (WGPUChainedStruct *)&wgsl;
    sd.label = label;
    return wgpuDeviceCreateShaderModule(dev, &sd);
}

/* ---- readback framebuffer: an (H*W*4) unpadded RGBA8 image ---- */
typedef struct { uint8_t px[H * W * BPP]; } Fb;
static uint8_t fb_p(const Fb *f, uint32_t x, uint32_t y, unsigned c) {
    return f->px[(y * W + x) * BPP + c];
}
static int peq(const Fb *f, uint32_t x, uint32_t y, int r, int g, int b, int a, int tol) {
    int dr = abs((int)fb_p(f, x, y, 0) - r);
    int dg = abs((int)fb_p(f, x, y, 1) - g);
    int db = abs((int)fb_p(f, x, y, 2) - b);
    int da = abs((int)fb_p(f, x, y, 3) - a);
    return dr <= tol && dg <= tol && db <= tol && da <= tol;
}
static int all_eq(const Fb *f, int r, int g, int b, int a, int tol) {
    for (uint32_t y = 0; y < H; y++)
        for (uint32_t x = 0; x < W; x++)
            if (!peq(f, x, y, r, g, b, a, tol)) return 0;
    return 1;
}

/* globals wired through the frame helpers */
static WGPUDevice   g_dev;
static WGPUInstance g_inst;
static WGPUQueue    g_queue;
static WGPUTexture  g_color;
static WGPUBuffer   g_readback;
static WGPUTextureView g_color_view, g_depth_view;
static uint32_t     g_padded;

/* copy the color texture into the readback buffer with padded rows, submit, map, unpad into `out` */
static int copy_and_read(WGPUCommandEncoder enc, Fb *out) {
    WGPUImageCopyTexture src = {0};
    src.texture = g_color;
    src.mipLevel = 0;
    src.origin = (WGPUOrigin3D){0, 0, 0};
    src.aspect = WGPUTextureAspect_All;
    WGPUImageCopyBuffer dst = {0};
    dst.buffer = g_readback;
    dst.layout.offset = 0;
    dst.layout.bytesPerRow = g_padded;
    dst.layout.rowsPerImage = H;
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

/* one draw's parameters */
typedef struct {
    WGPURenderPipeline pipe;   /* NULL => clear only */
    WGPUBindGroup bind;        /* NULL => none */
    WGPUBuffer vbo;            /* NULL => none */
    uint32_t verts;
    int has_scissor; uint32_t sx, sy, sw, sh;
    int has_viewport; float vx, vy, vw, vh;
} Draw;

static int frame(double cr, double cg, double cb, double ca, Draw d, Fb *out) {
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, NULL);
    WGPURenderPassColorAttachment ca_att = {0};
    ca_att.view = g_color_view;
    ca_att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    ca_att.resolveTarget = NULL;
    ca_att.loadOp = WGPULoadOp_Clear;
    ca_att.storeOp = WGPUStoreOp_Store;
    ca_att.clearValue = (WGPUColor){cr, cg, cb, ca};
    WGPURenderPassDescriptor rp = {0};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &ca_att;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    if (d.pipe) {
        wgpuRenderPassEncoderSetPipeline(pass, d.pipe);
        if (d.has_viewport)
            wgpuRenderPassEncoderSetViewport(pass, d.vx, d.vy, d.vw, d.vh, 0.0f, 1.0f);
        if (d.has_scissor)
            wgpuRenderPassEncoderSetScissorRect(pass, d.sx, d.sy, d.sw, d.sh);
        if (d.bind) wgpuRenderPassEncoderSetBindGroup(pass, 0, d.bind, 0, NULL);
        if (d.vbo) wgpuRenderPassEncoderSetVertexBuffer(pass, 0, d.vbo, 0, WGPU_WHOLE_SIZE);
        wgpuRenderPassEncoderDraw(pass, d.verts, 1, 0, 0);
    }
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

/* depth-enabled frame: clears color + depth, draws the pos3 quad through `pipe` */
static int frame_depth(double cr, double cg, double cb, double ca, float depth_clear,
                       WGPURenderPipeline pipe, WGPUBindGroup bind, WGPUBuffer vbo,
                       uint32_t verts, Fb *out) {
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, NULL);
    WGPURenderPassColorAttachment ca_att = {0};
    ca_att.view = g_color_view;
    ca_att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    ca_att.loadOp = WGPULoadOp_Clear;
    ca_att.storeOp = WGPUStoreOp_Store;
    ca_att.clearValue = (WGPUColor){cr, cg, cb, ca};
    WGPURenderPassDepthStencilAttachment ds = {0};
    ds.view = g_depth_view;
    ds.depthLoadOp = WGPULoadOp_Clear;
    ds.depthStoreOp = WGPUStoreOp_Store;
    ds.depthClearValue = depth_clear;
    ds.stencilLoadOp = WGPULoadOp_Undefined;
    ds.stencilStoreOp = WGPUStoreOp_Undefined;
    WGPURenderPassDescriptor rp = {0};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &ca_att;
    rp.depthStencilAttachment = &ds;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    wgpuRenderPassEncoderSetPipeline(pass, pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, bind, 0, NULL);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, verts, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

/* vertex layouts */
typedef struct { float pos[2]; } V2;
typedef struct { float pos[2]; float col[4]; } V2C;
typedef struct { float pos[3]; } V3;
typedef struct { float pos[2]; float uv[2]; } V2UV;

/* generic pipeline builder */
static WGPURenderPipeline mk(WGPUShaderModule vs, WGPUShaderModule fs, WGPUPipelineLayout layout,
                            const WGPUVertexBufferLayout *vbl, WGPUPrimitiveTopology topo,
                            WGPUFrontFace front, WGPUCullMode cull,
                            const WGPUColorTargetState *target,
                            const WGPUDepthStencilState *depth) {
    WGPURenderPipelineDescriptor d = {0};
    d.label = "pipe";
    d.layout = layout;
    d.vertex.module = vs;
    d.vertex.entryPoint = "vs";
    d.vertex.bufferCount = 1;
    d.vertex.buffers = vbl;
    d.primitive.topology = topo;
    d.primitive.stripIndexFormat = WGPUIndexFormat_Undefined;
    d.primitive.frontFace = front;
    d.primitive.cullMode = cull;
    d.multisample.count = 1;
    d.multisample.mask = 0xFFFFFFFFu;
    d.multisample.alphaToCoverageEnabled = 0;
    d.depthStencil = depth;
    WGPUFragmentState frag = {0};
    frag.module = fs;
    frag.entryPoint = "fs";
    frag.targetCount = 1;
    frag.targets = target;
    d.fragment = &frag;
    return wgpuDeviceCreateRenderPipeline(g_dev, &d);
}

static WGPUColorTargetState no_blend(WGPUColorWriteMaskFlags mask) {
    WGPUColorTargetState t = {0};
    t.format = WGPUTextureFormat_RGBA8Unorm;
    t.blend = NULL;
    t.writeMask = mask;
    return t;
}
static WGPUBuffer make_vbo(const void *data, size_t bytes, WGPUBufferUsageFlags usage) {
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
static void set_color(WGPUBuffer ubo, float r, float g, float b, float a) {
    float rgba[4] = {r, g, b, a};
    wgpuQueueWriteBuffer(g_queue, ubo, 0, rgba, sizeof(rgba));
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
    if (!inst) {
        printf("wgpu-render-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL + 1, PASS + FAIL + 1, EXPECTED);
        printf("WGPU_RENDER_C_FULL_API FAIL\n");
        return 1;
    }
    g_inst = inst;

    /* --- request adapter (synchronous under wgpu-native) --- */
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
        printf("wgpu-render-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("WGPU_RENDER_C_FULL_API FAIL\n");
        return 1;
    }
    WGPUAdapterInfo info = {0};
    wgpuAdapterGetInfo(adapter, &info);
    const char *bname = "other";
    switch (info.backendType) {
        case WGPUBackendType_Vulkan:   bname = "Vulkan";   break;
        case WGPUBackendType_OpenGL:   bname = "OpenGL";   break;
        case WGPUBackendType_OpenGLES: bname = "OpenGLES"; break;
        default: break;
    }
    printf("wgpu adapter selected: backend=%s name=\"%s\" driver=\"%s\"\n",
           bname, info.device ? info.device : "", info.description ? info.description : "");
    ok(info.backendType == WGPUBackendType_Vulkan || info.backendType == WGPUBackendType_OpenGL ||
       info.backendType == WGPUBackendType_OpenGLES,
       "adapter backend is Vulkan or Gl");

    /* --- request device --- */
    static UncapReq uncap = {0};
    DeviceReq dreq = {0};
    WGPUDeviceDescriptor ddesc = {0};
    ddesc.label = "render-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    ddesc.uncapturedErrorCallbackInfo.userdata = &uncap;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; i++) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) {
        ok(0, "request_device yields a usable device");
        printf("wgpu-render-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("WGPU_RENDER_C_FULL_API FAIL\n");
        return 1;
    }
    g_dev = dev;
    g_queue = wgpuDeviceGetQueue(dev);

    /* --- color attachment + depth + readback plumbing --- */
    WGPUTextureDescriptor cd = {0};
    cd.label = "color";
    cd.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    cd.dimension = WGPUTextureDimension_2D;
    cd.size = (WGPUExtent3D){W, H, 1};
    cd.format = WGPUTextureFormat_RGBA8Unorm;
    cd.mipLevelCount = 1;
    cd.sampleCount = 1;
    g_color = wgpuDeviceCreateTexture(dev, &cd);
    g_color_view = wgpuTextureCreateView(g_color, NULL);

    WGPUTextureDescriptor dd = {0};
    dd.label = "depth";
    dd.usage = WGPUTextureUsage_RenderAttachment;
    dd.dimension = WGPUTextureDimension_2D;
    dd.size = (WGPUExtent3D){W, H, 1};
    dd.format = WGPUTextureFormat_Depth32Float;
    dd.mipLevelCount = 1;
    dd.sampleCount = 1;
    WGPUTexture depth_tex = wgpuDeviceCreateTexture(dev, &dd);
    g_depth_view = wgpuTextureCreateView(depth_tex, NULL);

    uint32_t unpadded = W * BPP;
    uint32_t align = 256;
    g_padded = ((unpadded + align - 1) / align) * align;
    WGPUBufferDescriptor rbd = {0};
    rbd.label = "readback";
    rbd.usage = WGPUBufferUsage_CopyDst | WGPUBufferUsage_MapRead;
    rbd.size = (uint64_t)g_padded * H;
    g_readback = wgpuDeviceCreateBuffer(dev, &rbd);

    /* --- shaders --- */
    WGPUShaderModule m_solid = make_wgsl(dev, SOLID_WGSL, "solid");
    WGPUShaderModule m_grad  = make_wgsl(dev, GRAD_WGSL,  "grad");
    WGPUShaderModule m_check = make_wgsl(dev, CHECK_WGSL, "check");
    WGPUShaderModule m_pos3  = make_wgsl(dev, POS3_WGSL,  "pos3");
    WGPUShaderModule m_tex   = make_wgsl(dev, TEX_WGSL,   "tex");

    /* uniform buffer + bind group for the solid color (replaces push constants) */
    WGPUBufferDescriptor ubd = {0};
    ubd.label = "color-ubo";
    ubd.usage = WGPUBufferUsage_Uniform | WGPUBufferUsage_CopyDst;
    ubd.size = 16;
    WGPUBuffer color_ubo = wgpuDeviceCreateBuffer(dev, &ubd);

    WGPUBindGroupLayoutEntry ubo_ble = {0};
    ubo_ble.binding = 0;
    ubo_ble.visibility = WGPUShaderStage_Fragment;
    ubo_ble.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor ubo_bld = {0};
    ubo_bld.label = "ubo-bgl";
    ubo_bld.entryCount = 1;
    ubo_bld.entries = &ubo_ble;
    WGPUBindGroupLayout ubo_bgl = wgpuDeviceCreateBindGroupLayout(dev, &ubo_bld);

    WGPUBindGroupEntry ubo_bge = {0};
    ubo_bge.binding = 0;
    ubo_bge.buffer = color_ubo;
    ubo_bge.offset = 0;
    ubo_bge.size = 16;
    WGPUBindGroupDescriptor ubo_bgd = {0};
    ubo_bgd.label = "ubo-bg";
    ubo_bgd.layout = ubo_bgl;
    ubo_bgd.entryCount = 1;
    ubo_bgd.entries = &ubo_bge;
    WGPUBindGroup ubo_bg = wgpuDeviceCreateBindGroup(dev, &ubo_bgd);

    WGPUPipelineLayoutDescriptor ubo_plld = {0};
    ubo_plld.label = "ubo-pll";
    ubo_plld.bindGroupLayoutCount = 1;
    ubo_plld.bindGroupLayouts = &ubo_bgl;
    WGPUPipelineLayout ubo_pll = wgpuDeviceCreatePipelineLayout(dev, &ubo_plld);

    WGPUPipelineLayoutDescriptor empty_plld = {0};
    empty_plld.label = "empty-pll";
    empty_plld.bindGroupLayoutCount = 0;
    WGPUPipelineLayout empty_pll = wgpuDeviceCreatePipelineLayout(dev, &empty_plld);

    /* vertex buffer layouts */
    WGPUVertexAttribute a_pos2 = {WGPUVertexFormat_Float32x2, 0, 0};
    WGPUVertexBufferLayout vbl_pos2 = {sizeof(V2), WGPUVertexStepMode_Vertex, 1, &a_pos2};

    WGPUVertexAttribute a_pos2col[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x4, 8, 1},
    };
    WGPUVertexBufferLayout vbl_pos2col = {sizeof(V2C), WGPUVertexStepMode_Vertex, 2, a_pos2col};

    WGPUVertexAttribute a_pos3 = {WGPUVertexFormat_Float32x3, 0, 0};
    WGPUVertexBufferLayout vbl_pos3 = {sizeof(V3), WGPUVertexStepMode_Vertex, 1, &a_pos3};

    WGPUVertexAttribute a_pos2uv[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x2, 8, 1},
    };
    WGPUVertexBufferLayout vbl_pos2uv = {sizeof(V2UV), WGPUVertexStepMode_Vertex, 2, a_pos2uv};

    WGPUColorTargetState tgt_all = no_blend(WGPUColorWriteMask_All);

    /* --- geometry --- */
    V2 quad[4] = {{{-1, -1}}, {{1, -1}}, {{-1, 1}}, {{1, 1}}};
    WGPUBuffer vbo = make_vbo(quad, sizeof(quad), WGPUBufferUsage_Vertex);
    V2C gquad[4] = {
        {{-1, -1}, {1, 0, 0, 1}},
        {{1, -1},  {0, 0, 1, 1}},
        {{-1, 1},  {1, 0, 0, 1}},
        {{1, 1},   {0, 0, 1, 1}},
    };
    WGPUBuffer gvbo = make_vbo(gquad, sizeof(gquad), WGPUBufferUsage_Vertex);

    /* --- base pipelines --- */
    WGPURenderPipeline pipe_solid = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
    WGPURenderPipeline pipe_grad = mk(m_grad, m_grad, empty_pll, &vbl_pos2col,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
    WGPURenderPipeline pipe_check = mk(m_check, m_check, empty_pll, &vbl_pos2,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
    ok(pipe_solid && pipe_grad && pipe_check, "base render pipelines created");

    Fb fb;
    int mapped;

    /* ============ base coverage ============ */
    /* clear (a failed readback map folds into the pixel assertion, matching the Rust cell's count) */
    {
        Draw d = {0};
        mapped = frame(0.0, 0.25, 0.5, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 0, 64, 128, 255, 2),
           "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)");
        ok(mapped && peq(&fb, 0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");
    }

    /* solid red quad */
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        Draw d = {0}; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 255, 0, 0, 255, 1), "solid red quad fills every pixel");
    }

    /* axis-aligned gradient */
    {
        Draw d = {0}; d.pipe = pipe_grad; d.vbo = gvbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int bad = 0;
        for (uint32_t y = 0; y < H; y++)
            for (uint32_t x = 0; x < W; x++) {
                float u = ((float)x + 0.5f) / (float)W;
                int r = (int)lroundf((1.0f - u) * 255.0f);
                int b = (int)lroundf(u * 255.0f);
                if (!peq(&fb, x, y, r, 0, b, 255, 4)) bad++;
            }
        ok(mapped && bad == 0, "gradient matches horizontal-linear closed-form for all pixels");
        ok(peq(&fb, 0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red");
        ok(peq(&fb, W - 1, H - 1, 0, 0, 255, 255, 8), "gradient right edge ~ blue");
        ok(peq(&fb, W / 2, H / 2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)");
    }

    /* checkerboard from @builtin(position) */
    {
        Draw d = {0}; d.pipe = pipe_check; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int bad = 0;
        for (uint32_t y = 0; y < H; y++)
            for (uint32_t x = 0; x < W; x++) {
                int white = (((x / 8) + (y / 8)) & 1) == 0;
                int w = white ? 255 : 0;
                if (!peq(&fb, x, y, w, w, w, 255, 1)) bad++;
            }
        ok(mapped && bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels");
        ok(peq(&fb, 0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white");
        ok(peq(&fb, 8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black");
    }

    /* scissor */
    set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
    {
        Draw d = {0}; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        d.has_scissor = 1; d.sx = 16; d.sy = 16; d.sw = 32; d.sh = 32;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && peq(&fb, 32, 32, 0, 255, 0, 255, 1), "scissor: inside box green");
        ok(peq(&fb, 2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)");
        ok(peq(&fb, 50, 50, 255, 0, 0, 255, 1), "scissor: past box red");
    }

    /* viewport restriction: top-left 32x32 */
    set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
    {
        Draw d = {0}; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        d.has_viewport = 1; d.vx = 0; d.vy = 0; d.vw = 32; d.vh = 32;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && peq(&fb, 8, 8, 0, 255, 0, 255, 1), "viewport: inside 32x32 green");
        ok(peq(&fb, 50, 50, 255, 0, 0, 255, 1), "viewport: outside stays clear red");
    }

    /* alpha blend: SrcAlpha / OneMinusSrcAlpha Add over all channels (alpha -> 191) */
    {
        WGPUBlendState bs = {0};
        bs.color.operation = WGPUBlendOperation_Add;
        bs.color.srcFactor = WGPUBlendFactor_SrcAlpha;
        bs.color.dstFactor = WGPUBlendFactor_OneMinusSrcAlpha;
        bs.alpha = bs.color;
        WGPUColorTargetState tgt = {0};
        tgt.format = WGPUTextureFormat_RGBA8Unorm;
        tgt.blend = &bs;
        tgt.writeMask = WGPUColorWriteMask_All;
        WGPURenderPipeline pipe_blend = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt, NULL);
        set_color(color_ubo, 0.0f, 0.0f, 1.0f, 0.5f);
        Draw d = {0}; d.pipe = pipe_blend; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 128, 0, 128, 191, 3),
           "alpha blend 0.5*blue over red -> rgb(128,0,128) a191");
        wgpuRenderPipelineRelease(pipe_blend);
    }

    /* sub-rect readback */
    set_color(color_ubo, 0.2f, 0.4f, 0.6f, 1.0f);
    {
        Draw d = {0}; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int good = mapped;
        for (uint32_t y = 10; y < 14; y++)
            for (uint32_t x = 10; x < 14; x++)
                if (!peq(&fb, x, y, 51, 102, 153, 255, 2)) good = 0;
        ok(good, "sub-rect (10,10,4x4) == (51,102,153,255)");
    }

    /* ============ topologies: all 5 WebGPU topologies (NO triangle-fan) ============ */
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        V2 tl[6] = {{{-1, -1}}, {{1, -1}}, {{-1, 1}}, {{-1, 1}}, {{1, -1}}, {{1, 1}}};
        WGPUBuffer b_tl = make_vbo(tl, sizeof(tl), WGPUBufferUsage_Vertex);
        V2 ln[2] = {{{-1, 0}}, {{1, 0}}};
        WGPUBuffer b_ln = make_vbo(ln, sizeof(ln), WGPUBufferUsage_Vertex);
        V2 pt[1] = {{{0, 0}}};
        WGPUBuffer b_pt = make_vbo(pt, sizeof(pt), WGPUBufferUsage_Vertex);

        WGPURenderPipeline p_tl = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        WGPURenderPipeline p_ll = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_LineList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        WGPURenderPipeline p_ls = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_LineStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        WGPURenderPipeline p_pt = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_PointList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        ok(p_tl && p_ll && p_ls && p_pt, "topology pipelines created");

        Draw d = {0}; d.bind = ubo_bg;
        d.pipe = p_tl; d.vbo = b_tl; d.verts = 6;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 255, 0, 0, 255, 1), "TriangleList fills quad");

        d.pipe = p_ll; d.vbo = b_ln; d.verts = 2;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        {
            int mid = 0;
            for (uint32_t x = 0; x < W; x++)
                if (peq(&fb, x, H / 2, 255, 0, 0, 255, 2) || peq(&fb, x, H / 2 - 1, 255, 0, 0, 255, 2)) mid++;
            ok(mapped && mid >= (int)W - 2, "LineList draws the middle row");
            ok(peq(&fb, 0, 0, 0, 0, 0, 255, 2), "LineList leaves top row clear");
        }

        d.pipe = p_ls; d.vbo = b_ln; d.verts = 2;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        {
            int mid = 0;
            for (uint32_t x = 0; x < W; x++)
                if (peq(&fb, x, H / 2, 255, 0, 0, 255, 2) || peq(&fb, x, H / 2 - 1, 255, 0, 0, 255, 2)) mid++;
            ok(mapped && mid >= (int)W - 2, "LineStrip draws the middle row");
        }

        d.pipe = p_pt; d.vbo = b_pt; d.verts = 1;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        {
            int hit = 0;
            for (uint32_t y = H / 2 - 2; y <= H / 2 + 2; y++)
                for (uint32_t x = W / 2 - 2; x <= W / 2 + 2; x++)
                    if (peq(&fb, x, y, 255, 0, 0, 255, 2)) hit = 1;
            ok(mapped && hit, "PointList draws a 1px point at the center");
        }

        wgpuRenderPipelineRelease(p_tl); wgpuRenderPipelineRelease(p_ll);
        wgpuRenderPipelineRelease(p_ls); wgpuRenderPipelineRelease(p_pt);
        wgpuBufferRelease(b_tl); wgpuBufferRelease(b_ln); wgpuBufferRelease(b_pt);
    }

    /* ============ blend factor + op matrix ============ */
    {
        struct { WGPUBlendFactor sc, dc; WGPUBlendOperation oc;
                 WGPUBlendFactor sa, da; WGPUBlendOperation oa;
                 float r, g, b, a; double cr, cg, cb;
                 int er, eg, eb, ea, tol; const char *name; } cases[6] = {
            {WGPUBlendFactor_One, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             WGPUBlendFactor_One, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             0, 0, 1, 1, 0.5, 0.5, 0.5, 0, 0, 255, 255, 2, "blend One/Zero: src replaces dst"},
            {WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             0, 0, 0.5, 1, 0.5, 0.0, 0.0, 128, 0, 128, 255, 2, "blend One/One Add: src+dst = (128,0,128)"},
            {WGPUBlendFactor_Zero, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             WGPUBlendFactor_Zero, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             0, 1, 0, 1, 0.2, 0.0, 0.0, 51, 0, 0, 255, 2, "blend Zero/One: dst kept (51,0,0)"},
            {WGPUBlendFactor_Dst, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             WGPUBlendFactor_Dst, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             0, 0, 1, 1, 0.5, 0.5, 0.5, 0, 0, 128, 255, 2, "blend Dst/Zero: src*dst modulate (0,0,128)"},
            {WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Max,
             WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Max,
             0.6f, 0.2f, 0.6f, 1, 0.2, 0.6, 0.2, 153, 153, 153, 255, 2, "blend op Max: per-channel max"},
            {WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_ReverseSubtract,
             WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_ReverseSubtract,
             0.25f, 0, 0, 1, 1.0, 0.0, 0.0, 191, 0, 0, 0, 3, "blend op ReverseSubtract: dst-src rgb (191,0,0) a0"},
        };
        for (int i = 0; i < 6; i++) {
            WGPUBlendState bs = {0};
            bs.color.srcFactor = cases[i].sc; bs.color.dstFactor = cases[i].dc; bs.color.operation = cases[i].oc;
            bs.alpha.srcFactor = cases[i].sa; bs.alpha.dstFactor = cases[i].da; bs.alpha.operation = cases[i].oa;
            WGPUColorTargetState tgt = {0};
            tgt.format = WGPUTextureFormat_RGBA8Unorm;
            tgt.blend = &bs;
            tgt.writeMask = WGPUColorWriteMask_All;
            WGPURenderPipeline p = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
                WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt, NULL);
            set_color(color_ubo, cases[i].r, cases[i].g, cases[i].b, cases[i].a);
            Draw d = {0}; d.pipe = p; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
            mapped = frame(cases[i].cr, cases[i].cg, cases[i].cb, 1.0, d, &fb);
            ok(mapped && all_eq(&fb, cases[i].er, cases[i].eg, cases[i].eb, cases[i].ea, cases[i].tol), cases[i].name);
            wgpuRenderPipelineRelease(p);
        }
    }

    /* ============ depth-compare matrix (all 8; z=0.5 quad vs clear-depth 0.75) ============ */
    {
        V3 dq[4] = {{{-1, -1, 0.5f}}, {{1, -1, 0.5f}}, {{-1, 1, 0.5f}}, {{1, 1, 0.5f}}};
        WGPUBuffer dvbo = make_vbo(dq, sizeof(dq), WGPUBufferUsage_Vertex);
        set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
        struct { WGPUCompareFunction cmp; int draws; const char *name; } cases[8] = {
            {WGPUCompareFunction_Always,       1, "depth Always"},
            {WGPUCompareFunction_Never,        0, "depth Never"},
            {WGPUCompareFunction_Less,         1, "depth Less"},
            {WGPUCompareFunction_LessEqual,    1, "depth LessEqual"},
            {WGPUCompareFunction_Equal,        0, "depth Equal"},
            {WGPUCompareFunction_Greater,      0, "depth Greater"},
            {WGPUCompareFunction_GreaterEqual, 0, "depth GreaterEqual"},
            {WGPUCompareFunction_NotEqual,     1, "depth NotEqual"},
        };
        for (int i = 0; i < 8; i++) {
            WGPUDepthStencilState ds = {0};
            ds.format = WGPUTextureFormat_Depth32Float;
            ds.depthWriteEnabled = 1;
            ds.depthCompare = cases[i].cmp;
            ds.stencilFront.compare = WGPUCompareFunction_Always;
            ds.stencilBack.compare = WGPUCompareFunction_Always;
            WGPURenderPipeline p = mk(m_pos3, m_pos3, ubo_pll, &vbl_pos3,
                WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, &ds);
            mapped = frame_depth(0.0, 0.0, 0.0, 1.0, 0.75f, p, ubo_bg, dvbo, 4, &fb);
            int drew = peq(&fb, W / 2, H / 2, 0, 255, 0, 255, 2);
            ok(mapped && drew == cases[i].draws, cases[i].name);
            wgpuRenderPipelineRelease(p);
        }
        wgpuBufferRelease(dvbo);
    }

    /* ============ face culling + winding ============ */
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        WGPURenderPipeline p_none = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        Draw d = {0}; d.pipe = p_none; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 255, 0, 0, 255, 1), "cull None: quad drawn");

        WGPURenderPipeline p_ccw = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_Back, &tgt_all, NULL);
        d.pipe = p_ccw;
        frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int ccw = peq(&fb, W / 2, H / 2, 255, 0, 0, 255, 2);

        WGPURenderPipeline p_cw = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CW, WGPUCullMode_Back, &tgt_all, NULL);
        d.pipe = p_cw;
        frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int cw = peq(&fb, W / 2, H / 2, 255, 0, 0, 255, 2);
        ok(ccw != cw, "cull Back: Ccw vs Cw winding flips visibility");

        WGPURenderPipeline p_front = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_Front, &tgt_all, NULL);
        d.pipe = p_front;
        frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        int front_drawn = peq(&fb, W / 2, H / 2, 255, 0, 0, 255, 2);
        ok(front_drawn != ccw, "cull Front vs cull Back (Ccw) disagree at center");

        wgpuRenderPipelineRelease(p_none); wgpuRenderPipelineRelease(p_ccw);
        wgpuRenderPipelineRelease(p_cw); wgpuRenderPipelineRelease(p_front);
    }

    /* ============ color write mask ============ */
    set_color(color_ubo, 1.0f, 1.0f, 1.0f, 1.0f);
    {
        WGPUColorTargetState tgt_red = no_blend(WGPUColorWriteMask_Red);
        WGPURenderPipeline p_r = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_red, NULL);
        Draw d = {0}; d.pipe = p_r; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 255, 0, 0, 255, 1), "colorWrites RED only: white -> (255,0,0,255)");

        WGPURenderPipeline p_all = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        d.pipe = p_all;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && all_eq(&fb, 255, 255, 255, 255, 1), "colorWrites ALL: white -> (255,255,255,255)");

        wgpuRenderPipelineRelease(p_r); wgpuRenderPipelineRelease(p_all);
    }

    /* ============ format feature + limit queries ============ */
    /* v22 webgpu.h has no wgpuAdapterGetTextureFormatFeatures; realise the two format-feature
     * assertions behaviorally: create a texture of that format with RENDER_ATTACHMENT under a
     * validation scope and require NoError (functionally proves the format is renderable). */
    {
        wgpuDevicePushErrorScope(dev, WGPUErrorFilter_Validation);
        WGPUTextureDescriptor td = {0};
        td.usage = WGPUTextureUsage_RenderAttachment;
        td.dimension = WGPUTextureDimension_2D;
        td.size = (WGPUExtent3D){W, H, 1};
        td.format = WGPUTextureFormat_RGBA8Unorm;
        td.mipLevelCount = 1; td.sampleCount = 1;
        WGPUTexture t = wgpuDeviceCreateTexture(dev, &td);
        WGPUErrorType e = pop_error_scope(dev, inst);
        ok(e == WGPUErrorType_NoError, "Rgba8Unorm supports RENDER_ATTACHMENT");
        if (t) wgpuTextureRelease(t);

        wgpuDevicePushErrorScope(dev, WGPUErrorFilter_Validation);
        td.format = WGPUTextureFormat_Depth32Float;
        WGPUTexture t2 = wgpuDeviceCreateTexture(dev, &td);
        WGPUErrorType e2 = pop_error_scope(dev, inst);
        ok(e2 == WGPUErrorType_NoError, "Depth32Float supports RENDER_ATTACHMENT");
        if (t2) wgpuTextureRelease(t2);

        WGPUSupportedLimits lim = {0};
        wgpuDeviceGetLimits(dev, &lim);
        ok(lim.limits.maxTextureDimension2D >= W, "limits.max_texture_dimension_2d >= 64");
        /* v22 wgpu-native's C accessor leaves maxColorAttachments unpopulated (reads 0), unlike the
         * Rust wgpu crate which fills it from its own default-limits table. Realise the intended
         * "at least one color attachment is supported" check behaviorally: encode a render pass with
         * one color attachment under a validation scope and require NoError. */
        wgpuDevicePushErrorScope(dev, WGPUErrorFilter_Validation);
        {
            WGPUCommandEncoder ce = wgpuDeviceCreateCommandEncoder(dev, NULL);
            WGPURenderPassColorAttachment att = {0};
            att.view = g_color_view;
            att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
            att.loadOp = WGPULoadOp_Clear;
            att.storeOp = WGPUStoreOp_Store;
            att.clearValue = (WGPUColor){0, 0, 0, 1};
            WGPURenderPassDescriptor rp = {0};
            rp.colorAttachmentCount = 1;
            rp.colorAttachments = &att;
            WGPURenderPassEncoder pe = wgpuCommandEncoderBeginRenderPass(ce, &rp);
            wgpuRenderPassEncoderEnd(pe);
            wgpuRenderPassEncoderRelease(pe);
            WGPUCommandBuffer cb = wgpuCommandEncoderFinish(ce, NULL);
            wgpuQueueSubmit(g_queue, 1, &cb);
            wgpuCommandBufferRelease(cb);
            wgpuCommandEncoderRelease(ce);
        }
        WGPUErrorType cae = pop_error_scope(dev, inst);
        ok(cae == WGPUErrorType_NoError, "limits.max_color_attachments >= 1 (one-color-attachment render pass valid)");
    }

    /* ============ 2x2 texture upload + Nearest sampling ============ */
    {
        WGPUTextureDescriptor td = {0};
        td.label = "tex2x2";
        td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
        td.dimension = WGPUTextureDimension_2D;
        td.size = (WGPUExtent3D){2, 2, 1};
        td.format = WGPUTextureFormat_RGBA8Unorm;
        td.mipLevelCount = 1; td.sampleCount = 1;
        WGPUTexture tex = wgpuDeviceCreateTexture(dev, &td);
        uint8_t texels[16] = {
            255, 0, 0, 255,   /* (0,0) red   */
            0, 255, 0, 255,   /* (1,0) green */
            0, 0, 255, 255,   /* (0,1) blue  */
            255, 255, 255, 255 /* (1,1) white */
        };
        WGPUImageCopyTexture wdst = {0};
        wdst.texture = tex; wdst.mipLevel = 0; wdst.origin = (WGPUOrigin3D){0, 0, 0};
        wdst.aspect = WGPUTextureAspect_All;
        WGPUTextureDataLayout wl = {0};
        wl.offset = 0; wl.bytesPerRow = 8; wl.rowsPerImage = 2;
        WGPUExtent3D wsz = {2, 2, 1};
        wgpuQueueWriteTexture(g_queue, &wdst, texels, sizeof(texels), &wl, &wsz);

        WGPUTextureView tview = wgpuTextureCreateView(tex, NULL);
        WGPUSamplerDescriptor sd = {0};
        sd.label = "nearest";
        sd.addressModeU = WGPUAddressMode_ClampToEdge;
        sd.addressModeV = WGPUAddressMode_ClampToEdge;
        sd.addressModeW = WGPUAddressMode_ClampToEdge;
        sd.magFilter = WGPUFilterMode_Nearest;
        sd.minFilter = WGPUFilterMode_Nearest;
        sd.mipmapFilter = WGPUMipmapFilterMode_Nearest;
        sd.lodMinClamp = 0.0f; sd.lodMaxClamp = 1.0f;
        sd.compare = WGPUCompareFunction_Undefined;
        sd.maxAnisotropy = 1;
        WGPUSampler samp = wgpuDeviceCreateSampler(dev, &sd);

        WGPUBindGroupLayoutEntry tble[2] = {0};
        tble[0].binding = 0;
        tble[0].visibility = WGPUShaderStage_Fragment;
        tble[0].texture.sampleType = WGPUTextureSampleType_Float;
        tble[0].texture.viewDimension = WGPUTextureViewDimension_2D;
        tble[0].texture.multisampled = 0;
        tble[1].binding = 1;
        tble[1].visibility = WGPUShaderStage_Fragment;
        tble[1].sampler.type = WGPUSamplerBindingType_Filtering;
        WGPUBindGroupLayoutDescriptor tbld = {0};
        tbld.label = "tex-bgl"; tbld.entryCount = 2; tbld.entries = tble;
        WGPUBindGroupLayout tex_bgl = wgpuDeviceCreateBindGroupLayout(dev, &tbld);

        WGPUBindGroupEntry tbge[2] = {0};
        tbge[0].binding = 0; tbge[0].textureView = tview;
        tbge[1].binding = 1; tbge[1].sampler = samp;
        WGPUBindGroupDescriptor tbgd = {0};
        tbgd.label = "tex-bg"; tbgd.layout = tex_bgl; tbgd.entryCount = 2; tbgd.entries = tbge;
        WGPUBindGroup tex_bg = wgpuDeviceCreateBindGroup(dev, &tbgd);

        WGPUPipelineLayoutDescriptor tplld = {0};
        tplld.label = "tex-pll"; tplld.bindGroupLayoutCount = 1; tplld.bindGroupLayouts = &tex_bgl;
        WGPUPipelineLayout tex_pll = wgpuDeviceCreatePipelineLayout(dev, &tplld);

        WGPURenderPipeline pipe_tex = mk(m_tex, m_tex, tex_pll, &vbl_pos2uv,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, NULL);
        ok(pipe_tex != NULL, "texture pipeline + bind group created");

        /* top vertices (pos.y=+1) carry v=0, bottom v=1: texture top row (red/green) at fb top */
        V2UV tq[4] = {
            {{-1, -1}, {0, 1}},
            {{1, -1},  {1, 1}},
            {{-1, 1},  {0, 0}},
            {{1, 1},   {1, 0}},
        };
        WGPUBuffer tvbo = make_vbo(tq, sizeof(tq), WGPUBufferUsage_Vertex);
        Draw d = {0}; d.pipe = pipe_tex; d.bind = tex_bg; d.vbo = tvbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && peq(&fb, W / 4, H / 4, 255, 0, 0, 255, 2), "texture Nearest top-left red");
        ok(peq(&fb, 3 * W / 4, H / 4, 0, 255, 0, 255, 2), "texture Nearest top-right green");
        ok(peq(&fb, W / 4, 3 * H / 4, 0, 0, 255, 255, 2), "texture Nearest bottom-left blue");
        ok(peq(&fb, 3 * W / 4, 3 * H / 4, 255, 255, 255, 255, 2), "texture Nearest bottom-right white");

        wgpuBufferRelease(tvbo);
        wgpuRenderPipelineRelease(pipe_tex);
        wgpuPipelineLayoutRelease(tex_pll);
        wgpuBindGroupRelease(tex_bg);
        wgpuBindGroupLayoutRelease(tex_bgl);
        wgpuSamplerRelease(samp);
        wgpuTextureViewRelease(tview);
        wgpuTextureRelease(tex);
    }

    /* ============ negative control ============ */
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        Draw d = {0}; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, &fb);
        ok(mapped && !all_eq(&fb, 0, 255, 0, 255, 2), "negative control: red buffer is NOT green");
        ok(mapped && !peq(&fb, 0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black");
    }

    wgpuDevicePoll(dev, 1, NULL);
    if (uncap.count != 0)
        fprintf(stderr, "note: %d uncaptured device error(s), last type=%d\n", uncap.count, uncap.last);

    /* teardown */
    wgpuRenderPipelineRelease(pipe_solid);
    wgpuRenderPipelineRelease(pipe_grad);
    wgpuRenderPipelineRelease(pipe_check);
    wgpuBufferRelease(vbo);
    wgpuBufferRelease(gvbo);
    wgpuPipelineLayoutRelease(ubo_pll);
    wgpuPipelineLayoutRelease(empty_pll);
    wgpuBindGroupRelease(ubo_bg);
    wgpuBindGroupLayoutRelease(ubo_bgl);
    wgpuBufferRelease(color_ubo);
    wgpuShaderModuleRelease(m_solid);
    wgpuShaderModuleRelease(m_grad);
    wgpuShaderModuleRelease(m_check);
    wgpuShaderModuleRelease(m_pos3);
    wgpuShaderModuleRelease(m_tex);
    wgpuBufferRelease(g_readback);
    wgpuTextureViewRelease(g_color_view);
    wgpuTextureViewRelease(g_depth_view);
    wgpuTextureRelease(g_color);
    wgpuTextureRelease(depth_tex);
    wgpuAdapterInfoFreeMembers(info);
    wgpuQueueRelease(g_queue);
    wgpuDeviceRelease(dev);
    wgpuAdapterRelease(adapter);
    wgpuInstanceRelease(inst);

    int TOTAL = PASS + FAIL;
    printf("wgpu-render-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) {
        printf("WGPU_RENDER_C_FULL_API OK %d\n", PASS);
        return 0;
    }
    printf("WGPU_RENDER_C_FULL_API FAIL\n");
    return 1;
}
