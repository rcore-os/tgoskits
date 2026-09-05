// wgpu_render_cpp_full_api - WebGPU/wgpu RENDER carpet on Mesa lavapipe (software Vulkan on the CPU,
// no GPU/window/surface/swapchain), driven through the C webgpu.h / wgpu.h API of gfx-rs wgpu-native
// v22.1.0.2 (matches the wgpu="22" crate the Rust reference cell links) from C++17. It renders
// offscreen into a 64x64 RGBA8Unorm texture (RENDER_ATTACHMENT | COPY_SRC) through real render
// pipelines with WGSL vertex+fragment shaders, copies the texture into a MAP_READ buffer honouring
// the 256-byte bytesPerRow alignment (rows are padded on copy then unpadded on readback), maps it,
// and hard-asserts every pixel against a closed-form reference.
//
// Coverage mirrors the verified Rust reference cell 1:1 (56 assertions, same closed-form pixel
// references): render-pass clear, a solid quad (uniform-buffer color; WebGPU has no push constants by
// default), a per-vertex axis-aligned linear gradient, an @builtin(position) checkerboard, a scissor
// rect, a viewport restriction, alpha blend a=191, a sub-rectangle readback; then exhaustive per-API
// coverage: all 5 WebGPU primitive topologies (PointList 1px / LineList / LineStrip / TriangleList /
// TriangleStrip - WebGPU has NO triangle-fan), a blend factor+op matrix (One/Zero, One/One Add,
// Zero/One, Dst/Zero, One/One Max, One/One ReverseSubtract alpha=0), the full 8-way
// WGPUCompareFunction depth matrix (Depth32Float attachment; a z=0.5 quad vs clear-depth 0.75 draws
// only under {Always,Less,LessEqual,NotEqual}), face culling + winding (cull None vs Back with
// FrontFace CCW vs CW, cull Front vs Back), a colour write mask (RED vs ALL), format+limit queries, a
// 2x2 RGBA8 texture upload + Nearest sampling through a sampler + bind group (corners TL red / TR
// green / BL blue / BR white), closing with a negative control.
//
// v22 API note vs the newer sibling compute cell: this header is the OLD callback model -
// wgpuInstanceRequestAdapter/wgpuAdapterRequestDevice take a bare (callback, userdata) pair whose
// callback receives a `char const *` message (not the WGPUFuture/WGPUStringView model), WGSL modules
// chain a WGPUShaderModuleWGSLDescriptor, and wgpuDevicePopErrorScope uses a WGPUErrorCallback. The
// async requests resolve synchronously under wgpu-native, driven with wgpuInstanceProcessEvents /
// wgpuDevicePoll(device, wait=true, NULL). Backend is selected via WGPU_BACKEND
// (vulkan=lavapipe / gl=llvmpipe) like the compute cell.
//
// Format-feature / limit query calibration: wgpu-native v22's webgpu.h exposes NO
// wgpuAdapterGetTextureFormatFeatures (the Rust wgpu::Adapter::get_texture_format_features has no
// C-API counterpart in this header). To keep the assertion count at the pinned 56 with genuinely
// equivalent checks, the two format-feature assertions are realised behaviorally: a texture of that
// format is created with RENDER_ATTACHMENT usage under a validation error scope and the popped error
// must be NoError. Likewise, v22 wgpu-native's C wgpuDeviceGetLimits leaves maxColorAttachments
// unpopulated (reads 0, unlike the Rust wgpu crate's default-limits table), so the
// "max_color_attachments >= 1" assertion is realised by validating a one-color-attachment render pass
// under an error scope. Both keep the count at 56.
//
// Prints "WGPU_RENDER_CPP_FULL_API OK 56" with FAIL=0 only when every assertion passes and the count
// equals the pinned EXPECTED total.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <string>
#include <vector>
#include <array>

#include "webgpu.h"
#include "wgpu.h"

namespace {

constexpr uint32_t W = 64, H = 64, BPP = 4;
constexpr int EXPECTED = 56;

int g_pass = 0, g_fail = 0;
void ok(bool c, const char* d) {
    if (c) ++g_pass;
    else { ++g_fail; std::fprintf(stderr, "FAIL: %s\n", d); }
}

// ---- synchronous adapter/device request via the v22 (callback, userdata) model ----
struct AdapterReq { WGPUAdapter adapter = nullptr; WGPURequestAdapterStatus status{}; bool done = false; };
void on_adapter(WGPURequestAdapterStatus status, WGPUAdapter adapter, char const* message, void* ud) {
    (void)message;
    auto* r = static_cast<AdapterReq*>(ud);
    r->status = status; r->adapter = adapter; r->done = true;
}
struct DeviceReq { WGPUDevice device = nullptr; WGPURequestDeviceStatus status{}; bool done = false; };
void on_device(WGPURequestDeviceStatus status, WGPUDevice device, char const* message, void* ud) {
    (void)message;
    auto* r = static_cast<DeviceReq*>(ud);
    r->status = status; r->device = device; r->done = true;
}
struct MapReq { WGPUBufferMapAsyncStatus status{}; bool done = false; };
void on_map(WGPUBufferMapAsyncStatus status, void* ud) {
    auto* r = static_cast<MapReq*>(ud);
    r->status = status; r->done = true;
}
struct ScopeReq { WGPUErrorType type{}; bool done = false; };
void on_scope(WGPUErrorType type, char const* message, void* ud) {
    (void)message;
    auto* r = static_cast<ScopeReq*>(ud);
    r->type = type; r->done = true;
}
struct UncapReq { int count = 0; WGPUErrorType last{}; };
void on_uncaptured(WGPUErrorType type, char const* message, void* ud) {
    (void)message;
    auto* r = static_cast<UncapReq*>(ud);
    ++r->count; r->last = type;
}

// ---- WGSL shaders (identical to the Rust reference cell) ----
const char* SOLID_WGSL = R"(
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
)";

const char* GRAD_WGSL = R"(
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.col = c;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return in.col; }
)";

const char* CHECK_WGSL = R"(
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
)";

// @invariant on the clip-space position makes @builtin(position).z bit-exact, so an exact-EQUAL /
// NotEqual depth compare against the cleared depth is deterministic across pipelines/runs (this is
// the informational invariance warning wgpu-native emits for depth-tested pipelines).
const char* POS3_WGSL = R"(
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec3<f32>) -> @invariant @builtin(position) vec4<f32> {
    return vec4<f32>(p, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
)";

const char* TEX_WGSL = R"(
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
)";

// ---- globals wired through the frame helpers ----
WGPUDevice   g_dev = nullptr;
WGPUInstance g_inst = nullptr;
WGPUQueue    g_queue = nullptr;
WGPUTexture  g_color = nullptr;
WGPUBuffer   g_readback = nullptr;
WGPUTextureView g_color_view = nullptr, g_depth_view = nullptr;
uint32_t     g_padded = 0;

WGPUShaderModule make_wgsl(WGPUDevice dev, const char* code, const char* label) {
    WGPUShaderModuleWGSLDescriptor wgsl{};
    wgsl.chain.sType = WGPUSType_ShaderModuleWGSLDescriptor;
    wgsl.code = code;
    WGPUShaderModuleDescriptor sd{};
    sd.nextInChain = reinterpret_cast<WGPUChainedStruct*>(&wgsl);
    sd.label = label;
    return wgpuDeviceCreateShaderModule(dev, &sd);
}

WGPUErrorType pop_error_scope(WGPUDevice dev, WGPUInstance inst) {
    ScopeReq sr{};
    wgpuDevicePopErrorScope(dev, on_scope, &sr);
    for (int i = 0; i < 256 && !sr.done; ++i) {
        wgpuDevicePoll(dev, 1, nullptr);
        wgpuInstanceProcessEvents(inst);
    }
    return sr.done ? sr.type : WGPUErrorType_Unknown;
}

// ---- readback framebuffer ----
struct Fb {
    std::array<uint8_t, H * W * BPP> px{};
    uint8_t p(uint32_t x, uint32_t y, unsigned c) const { return px[(y * W + x) * BPP + c]; }
    bool peq(uint32_t x, uint32_t y, int r, int g, int b, int a, int tol) const {
        auto d = [](int v, int t) { return std::abs(v - t) <= 0; };
        (void)d;
        return std::abs(static_cast<int>(p(x, y, 0)) - r) <= tol &&
               std::abs(static_cast<int>(p(x, y, 1)) - g) <= tol &&
               std::abs(static_cast<int>(p(x, y, 2)) - b) <= tol &&
               std::abs(static_cast<int>(p(x, y, 3)) - a) <= tol;
    }
    bool all_eq(int r, int g, int b, int a, int tol) const {
        for (uint32_t y = 0; y < H; ++y)
            for (uint32_t x = 0; x < W; ++x)
                if (!peq(x, y, r, g, b, a, tol)) return false;
        return true;
    }
};

bool copy_and_read(WGPUCommandEncoder enc, Fb& out) {
    WGPUImageCopyTexture src{};
    src.texture = g_color;
    src.mipLevel = 0;
    src.origin = {0, 0, 0};
    src.aspect = WGPUTextureAspect_All;
    WGPUImageCopyBuffer dst{};
    dst.buffer = g_readback;
    dst.layout.offset = 0;
    dst.layout.bytesPerRow = g_padded;
    dst.layout.rowsPerImage = H;
    WGPUExtent3D ext{W, H, 1};
    wgpuCommandEncoderCopyTextureToBuffer(enc, &src, &dst, &ext);
    WGPUCommandBuffer cmd = wgpuCommandEncoderFinish(enc, nullptr);
    wgpuQueueSubmit(g_queue, 1, &cmd);

    MapReq mr{};
    size_t bytes = static_cast<size_t>(g_padded) * H;
    wgpuBufferMapAsync(g_readback, WGPUMapMode_Read, 0, bytes, on_map, &mr);
    for (int i = 0; i < 256 && !mr.done; ++i) wgpuDevicePoll(g_dev, 1, nullptr);
    bool good = mr.done && mr.status == WGPUBufferMapAsyncStatus_Success;
    if (good) {
        const auto* p = static_cast<const uint8_t*>(wgpuBufferGetConstMappedRange(g_readback, 0, bytes));
        if (!p) good = false;
        else
            for (uint32_t y = 0; y < H; ++y)
                std::memcpy(&out.px[y * W * BPP], &p[y * g_padded], W * BPP);
        wgpuBufferUnmap(g_readback);
    }
    wgpuCommandBufferRelease(cmd);
    wgpuCommandEncoderRelease(enc);
    return good;
}

struct Draw {
    WGPURenderPipeline pipe = nullptr;
    WGPUBindGroup bind = nullptr;
    WGPUBuffer vbo = nullptr;
    uint32_t verts = 0;
    bool has_scissor = false; uint32_t sx = 0, sy = 0, sw = 0, sh = 0;
    bool has_viewport = false; float vx = 0, vy = 0, vw = 0, vh = 0;
};

bool frame(double cr, double cg, double cb, double ca, const Draw& d, Fb& out) {
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, nullptr);
    WGPURenderPassColorAttachment att{};
    att.view = g_color_view;
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.resolveTarget = nullptr;
    att.loadOp = WGPULoadOp_Clear;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = {cr, cg, cb, ca};
    WGPURenderPassDescriptor rp{};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &att;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    if (d.pipe) {
        wgpuRenderPassEncoderSetPipeline(pass, d.pipe);
        if (d.has_viewport)
            wgpuRenderPassEncoderSetViewport(pass, d.vx, d.vy, d.vw, d.vh, 0.0f, 1.0f);
        if (d.has_scissor)
            wgpuRenderPassEncoderSetScissorRect(pass, d.sx, d.sy, d.sw, d.sh);
        if (d.bind) wgpuRenderPassEncoderSetBindGroup(pass, 0, d.bind, 0, nullptr);
        if (d.vbo) wgpuRenderPassEncoderSetVertexBuffer(pass, 0, d.vbo, 0, WGPU_WHOLE_SIZE);
        wgpuRenderPassEncoderDraw(pass, d.verts, 1, 0, 0);
    }
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

bool frame_depth(double cr, double cg, double cb, double ca, float depth_clear,
                 WGPURenderPipeline pipe, WGPUBindGroup bind, WGPUBuffer vbo,
                 uint32_t verts, Fb& out) {
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, nullptr);
    WGPURenderPassColorAttachment att{};
    att.view = g_color_view;
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.loadOp = WGPULoadOp_Clear;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = {cr, cg, cb, ca};
    WGPURenderPassDepthStencilAttachment ds{};
    ds.view = g_depth_view;
    ds.depthLoadOp = WGPULoadOp_Clear;
    ds.depthStoreOp = WGPUStoreOp_Store;
    ds.depthClearValue = depth_clear;
    ds.stencilLoadOp = WGPULoadOp_Undefined;
    ds.stencilStoreOp = WGPUStoreOp_Undefined;
    WGPURenderPassDescriptor rp{};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &att;
    rp.depthStencilAttachment = &ds;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    wgpuRenderPassEncoderSetPipeline(pass, pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, bind, 0, nullptr);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, verts, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

// vertex layouts
struct V2 { float pos[2]; };
struct V2C { float pos[2]; float col[4]; };
struct V3 { float pos[3]; };
struct V2UV { float pos[2]; float uv[2]; };

WGPURenderPipeline mk(WGPUShaderModule vs, WGPUShaderModule fs, WGPUPipelineLayout layout,
                     const WGPUVertexBufferLayout* vbl, WGPUPrimitiveTopology topo,
                     WGPUFrontFace front, WGPUCullMode cull,
                     const WGPUColorTargetState* target,
                     const WGPUDepthStencilState* depth) {
    WGPURenderPipelineDescriptor d{};
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
    WGPUFragmentState frag{};
    frag.module = fs;
    frag.entryPoint = "fs";
    frag.targetCount = 1;
    frag.targets = target;
    d.fragment = &frag;
    return wgpuDeviceCreateRenderPipeline(g_dev, &d);
}

WGPUColorTargetState no_blend(WGPUColorWriteMaskFlags mask) {
    WGPUColorTargetState t{};
    t.format = WGPUTextureFormat_RGBA8Unorm;
    t.blend = nullptr;
    t.writeMask = mask;
    return t;
}
WGPUBuffer make_vbo(const void* data, size_t bytes, WGPUBufferUsageFlags usage) {
    WGPUBufferDescriptor bd{};
    bd.usage = usage;
    bd.size = bytes;
    bd.mappedAtCreation = 1;
    WGPUBuffer buf = wgpuDeviceCreateBuffer(g_dev, &bd);
    void* p = wgpuBufferGetMappedRange(buf, 0, bytes);
    std::memcpy(p, data, bytes);
    wgpuBufferUnmap(buf);
    return buf;
}
void set_color(WGPUBuffer ubo, float r, float g, float b, float a) {
    float rgba[4] = {r, g, b, a};
    wgpuQueueWriteBuffer(g_queue, ubo, 0, rgba, sizeof(rgba));
}

int finish() {
    int total = g_pass + g_fail;
    std::printf("wgpu-render-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, total, EXPECTED);
    if (g_fail == 0 && total == EXPECTED) {
        std::printf("WGPU_RENDER_CPP_FULL_API OK %d\n", g_pass);
        return 0;
    }
    std::printf("WGPU_RENDER_CPP_FULL_API FAIL\n");
    return 1;
}

} // namespace

int main() {
    const char* backend = std::getenv("WGPU_BACKEND");
    WGPUInstanceBackendFlags flags = WGPUInstanceBackend_Vulkan | WGPUInstanceBackend_GL;
    if (backend && (std::strcmp(backend, "gl") == 0 || std::strcmp(backend, "gles") == 0))
        flags = WGPUInstanceBackend_GL;
    else if (backend && std::strcmp(backend, "vulkan") == 0)
        flags = WGPUInstanceBackend_Vulkan;

    WGPUInstanceExtras extras{};
    extras.chain.sType = static_cast<WGPUSType>(WGPUSType_InstanceExtras);
    extras.backends = flags;
    WGPUInstanceDescriptor idesc{};
    idesc.nextInChain = reinterpret_cast<WGPUChainedStruct*>(&extras);
    WGPUInstance inst = wgpuCreateInstance(&idesc);
    if (!inst) { ok(false, "wgpuCreateInstance"); return finish(); }
    g_inst = inst;

    // --- request adapter (synchronous under wgpu-native) ---
    AdapterReq areq;
    WGPURequestAdapterOptions aopts{};
    aopts.powerPreference = WGPUPowerPreference_LowPower;
    aopts.backendType = WGPUBackendType_Undefined;
    aopts.forceFallbackAdapter = 0;
    wgpuInstanceRequestAdapter(inst, &aopts, on_adapter, &areq);
    for (int i = 0; i < 64 && !areq.done; ++i) wgpuInstanceProcessEvents(inst);
    WGPUAdapter adapter = areq.adapter;
    ok(areq.done && areq.status == WGPURequestAdapterStatus_Success && adapter != nullptr,
       "request_adapter yields a usable adapter");
    if (!adapter) return finish();

    WGPUAdapterInfo info{};
    wgpuAdapterGetInfo(adapter, &info);
    const char* bname = "other";
    switch (info.backendType) {
        case WGPUBackendType_Vulkan:   bname = "Vulkan";   break;
        case WGPUBackendType_OpenGL:   bname = "OpenGL";   break;
        case WGPUBackendType_OpenGLES: bname = "OpenGLES"; break;
        default: break;
    }
    std::printf("wgpu adapter selected: backend=%s name=\"%s\" driver=\"%s\"\n",
                bname, info.device ? info.device : "", info.description ? info.description : "");
    ok(info.backendType == WGPUBackendType_Vulkan || info.backendType == WGPUBackendType_OpenGL ||
       info.backendType == WGPUBackendType_OpenGLES,
       "adapter backend is Vulkan or Gl");

    // --- request device ---
    static UncapReq uncap;
    DeviceReq dreq;
    WGPUDeviceDescriptor ddesc{};
    ddesc.label = "render-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    ddesc.uncapturedErrorCallbackInfo.userdata = &uncap;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; ++i) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) { ok(false, "request_device yields a usable device"); return finish(); }
    g_dev = dev;
    g_queue = wgpuDeviceGetQueue(dev);

    // --- color attachment + depth + readback plumbing ---
    WGPUTextureDescriptor cd{};
    cd.label = "color";
    cd.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    cd.dimension = WGPUTextureDimension_2D;
    cd.size = {W, H, 1};
    cd.format = WGPUTextureFormat_RGBA8Unorm;
    cd.mipLevelCount = 1;
    cd.sampleCount = 1;
    g_color = wgpuDeviceCreateTexture(dev, &cd);
    g_color_view = wgpuTextureCreateView(g_color, nullptr);

    WGPUTextureDescriptor dd{};
    dd.label = "depth";
    dd.usage = WGPUTextureUsage_RenderAttachment;
    dd.dimension = WGPUTextureDimension_2D;
    dd.size = {W, H, 1};
    dd.format = WGPUTextureFormat_Depth32Float;
    dd.mipLevelCount = 1;
    dd.sampleCount = 1;
    WGPUTexture depth_tex = wgpuDeviceCreateTexture(dev, &dd);
    g_depth_view = wgpuTextureCreateView(depth_tex, nullptr);

    uint32_t unpadded = W * BPP;
    uint32_t align = 256;
    g_padded = ((unpadded + align - 1) / align) * align;
    WGPUBufferDescriptor rbd{};
    rbd.label = "readback";
    rbd.usage = WGPUBufferUsage_CopyDst | WGPUBufferUsage_MapRead;
    rbd.size = static_cast<uint64_t>(g_padded) * H;
    g_readback = wgpuDeviceCreateBuffer(dev, &rbd);

    // --- shaders ---
    WGPUShaderModule m_solid = make_wgsl(dev, SOLID_WGSL, "solid");
    WGPUShaderModule m_grad  = make_wgsl(dev, GRAD_WGSL,  "grad");
    WGPUShaderModule m_check = make_wgsl(dev, CHECK_WGSL, "check");
    WGPUShaderModule m_pos3  = make_wgsl(dev, POS3_WGSL,  "pos3");
    WGPUShaderModule m_tex   = make_wgsl(dev, TEX_WGSL,   "tex");

    // uniform buffer + bind group for the solid color
    WGPUBufferDescriptor ubd{};
    ubd.label = "color-ubo";
    ubd.usage = WGPUBufferUsage_Uniform | WGPUBufferUsage_CopyDst;
    ubd.size = 16;
    WGPUBuffer color_ubo = wgpuDeviceCreateBuffer(dev, &ubd);

    WGPUBindGroupLayoutEntry ubo_ble{};
    ubo_ble.binding = 0;
    ubo_ble.visibility = WGPUShaderStage_Fragment;
    ubo_ble.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor ubo_bld{};
    ubo_bld.label = "ubo-bgl";
    ubo_bld.entryCount = 1;
    ubo_bld.entries = &ubo_ble;
    WGPUBindGroupLayout ubo_bgl = wgpuDeviceCreateBindGroupLayout(dev, &ubo_bld);

    WGPUBindGroupEntry ubo_bge{};
    ubo_bge.binding = 0;
    ubo_bge.buffer = color_ubo;
    ubo_bge.offset = 0;
    ubo_bge.size = 16;
    WGPUBindGroupDescriptor ubo_bgd{};
    ubo_bgd.label = "ubo-bg";
    ubo_bgd.layout = ubo_bgl;
    ubo_bgd.entryCount = 1;
    ubo_bgd.entries = &ubo_bge;
    WGPUBindGroup ubo_bg = wgpuDeviceCreateBindGroup(dev, &ubo_bgd);

    WGPUPipelineLayoutDescriptor ubo_plld{};
    ubo_plld.label = "ubo-pll";
    ubo_plld.bindGroupLayoutCount = 1;
    ubo_plld.bindGroupLayouts = &ubo_bgl;
    WGPUPipelineLayout ubo_pll = wgpuDeviceCreatePipelineLayout(dev, &ubo_plld);

    WGPUPipelineLayoutDescriptor empty_plld{};
    empty_plld.label = "empty-pll";
    empty_plld.bindGroupLayoutCount = 0;
    WGPUPipelineLayout empty_pll = wgpuDeviceCreatePipelineLayout(dev, &empty_plld);

    // vertex buffer layouts
    WGPUVertexAttribute a_pos2{WGPUVertexFormat_Float32x2, 0, 0};
    WGPUVertexBufferLayout vbl_pos2{sizeof(V2), WGPUVertexStepMode_Vertex, 1, &a_pos2};

    WGPUVertexAttribute a_pos2col[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x4, 8, 1},
    };
    WGPUVertexBufferLayout vbl_pos2col{sizeof(V2C), WGPUVertexStepMode_Vertex, 2, a_pos2col};

    WGPUVertexAttribute a_pos3{WGPUVertexFormat_Float32x3, 0, 0};
    WGPUVertexBufferLayout vbl_pos3{sizeof(V3), WGPUVertexStepMode_Vertex, 1, &a_pos3};

    WGPUVertexAttribute a_pos2uv[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x2, 8, 1},
    };
    WGPUVertexBufferLayout vbl_pos2uv{sizeof(V2UV), WGPUVertexStepMode_Vertex, 2, a_pos2uv};

    WGPUColorTargetState tgt_all = no_blend(WGPUColorWriteMask_All);

    // --- geometry ---
    V2 quad[4] = {{{-1, -1}}, {{1, -1}}, {{-1, 1}}, {{1, 1}}};
    WGPUBuffer vbo = make_vbo(quad, sizeof(quad), WGPUBufferUsage_Vertex);
    V2C gquad[4] = {
        {{-1, -1}, {1, 0, 0, 1}},
        {{1, -1},  {0, 0, 1, 1}},
        {{-1, 1},  {1, 0, 0, 1}},
        {{1, 1},   {0, 0, 1, 1}},
    };
    WGPUBuffer gvbo = make_vbo(gquad, sizeof(gquad), WGPUBufferUsage_Vertex);

    // --- base pipelines ---
    WGPURenderPipeline pipe_solid = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
    WGPURenderPipeline pipe_grad = mk(m_grad, m_grad, empty_pll, &vbl_pos2col,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
    WGPURenderPipeline pipe_check = mk(m_check, m_check, empty_pll, &vbl_pos2,
        WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
    ok(pipe_solid && pipe_grad && pipe_check, "base render pipelines created");

    Fb fb;
    bool mapped;

    // ============ base coverage ============
    {
        Draw d;
        mapped = frame(0.0, 0.25, 0.5, 1.0, d, fb);
        ok(mapped && fb.all_eq(0, 64, 128, 255, 2),
           "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)");
        ok(mapped && fb.peq(0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");
    }

    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        Draw d; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(255, 0, 0, 255, 1), "solid red quad fills every pixel");
    }

    {
        Draw d; d.pipe = pipe_grad; d.vbo = gvbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        int bad = 0;
        for (uint32_t y = 0; y < H; ++y)
            for (uint32_t x = 0; x < W; ++x) {
                float u = (static_cast<float>(x) + 0.5f) / static_cast<float>(W);
                int r = static_cast<int>(std::lround((1.0f - u) * 255.0f));
                int b = static_cast<int>(std::lround(u * 255.0f));
                if (!fb.peq(x, y, r, 0, b, 255, 4)) ++bad;
            }
        ok(mapped && bad == 0, "gradient matches horizontal-linear closed-form for all pixels");
        ok(fb.peq(0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red");
        ok(fb.peq(W - 1, H - 1, 0, 0, 255, 255, 8), "gradient right edge ~ blue");
        ok(fb.peq(W / 2, H / 2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)");
    }

    {
        Draw d; d.pipe = pipe_check; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        int bad = 0;
        for (uint32_t y = 0; y < H; ++y)
            for (uint32_t x = 0; x < W; ++x) {
                bool white = (((x / 8) + (y / 8)) & 1) == 0;
                int w = white ? 255 : 0;
                if (!fb.peq(x, y, w, w, w, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "checkerboard matches (x/8+y/8) parity for all pixels");
        ok(fb.peq(0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white");
        ok(fb.peq(8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black");
    }

    set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
    {
        Draw d; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        d.has_scissor = true; d.sx = 16; d.sy = 16; d.sw = 32; d.sh = 32;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.peq(32, 32, 0, 255, 0, 255, 1), "scissor: inside box green");
        ok(fb.peq(2, 2, 255, 0, 0, 255, 1), "scissor: outside box red (clear)");
        ok(fb.peq(50, 50, 255, 0, 0, 255, 1), "scissor: past box red");
    }

    set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
    {
        Draw d; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        d.has_viewport = true; d.vx = 0; d.vy = 0; d.vw = 32; d.vh = 32;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.peq(8, 8, 0, 255, 0, 255, 1), "viewport: inside 32x32 green");
        ok(fb.peq(50, 50, 255, 0, 0, 255, 1), "viewport: outside stays clear red");
    }

    {
        WGPUBlendState bs{};
        bs.color.operation = WGPUBlendOperation_Add;
        bs.color.srcFactor = WGPUBlendFactor_SrcAlpha;
        bs.color.dstFactor = WGPUBlendFactor_OneMinusSrcAlpha;
        bs.alpha = bs.color;
        WGPUColorTargetState tgt{};
        tgt.format = WGPUTextureFormat_RGBA8Unorm;
        tgt.blend = &bs;
        tgt.writeMask = WGPUColorWriteMask_All;
        WGPURenderPipeline pipe_blend = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt, nullptr);
        set_color(color_ubo, 0.0f, 0.0f, 1.0f, 0.5f);
        Draw d; d.pipe = pipe_blend; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(1.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(128, 0, 128, 191, 3),
           "alpha blend 0.5*blue over red -> rgb(128,0,128) a191");
        wgpuRenderPipelineRelease(pipe_blend);
    }

    set_color(color_ubo, 0.2f, 0.4f, 0.6f, 1.0f);
    {
        Draw d; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        bool good = mapped;
        for (uint32_t y = 10; y < 14; ++y)
            for (uint32_t x = 10; x < 14; ++x)
                if (!fb.peq(x, y, 51, 102, 153, 255, 2)) good = false;
        ok(good, "sub-rect (10,10,4x4) == (51,102,153,255)");
    }

    // ============ topologies ============
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        V2 tl[6] = {{{-1, -1}}, {{1, -1}}, {{-1, 1}}, {{-1, 1}}, {{1, -1}}, {{1, 1}}};
        WGPUBuffer b_tl = make_vbo(tl, sizeof(tl), WGPUBufferUsage_Vertex);
        V2 ln[2] = {{{-1, 0}}, {{1, 0}}};
        WGPUBuffer b_ln = make_vbo(ln, sizeof(ln), WGPUBufferUsage_Vertex);
        V2 pt[1] = {{{0, 0}}};
        WGPUBuffer b_pt = make_vbo(pt, sizeof(pt), WGPUBufferUsage_Vertex);

        WGPURenderPipeline p_tl = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        WGPURenderPipeline p_ll = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_LineList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        WGPURenderPipeline p_ls = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_LineStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        WGPURenderPipeline p_pt = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_PointList, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        ok(p_tl && p_ll && p_ls && p_pt, "topology pipelines created");

        Draw d; d.bind = ubo_bg;
        d.pipe = p_tl; d.vbo = b_tl; d.verts = 6;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(255, 0, 0, 255, 1), "TriangleList fills quad");

        d.pipe = p_ll; d.vbo = b_ln; d.verts = 2;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        {
            int mid = 0;
            for (uint32_t x = 0; x < W; ++x)
                if (fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2)) ++mid;
            ok(mapped && mid >= static_cast<int>(W) - 2, "LineList draws the middle row");
            ok(fb.peq(0, 0, 0, 0, 0, 255, 2), "LineList leaves top row clear");
        }

        d.pipe = p_ls; d.vbo = b_ln; d.verts = 2;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        {
            int mid = 0;
            for (uint32_t x = 0; x < W; ++x)
                if (fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2)) ++mid;
            ok(mapped && mid >= static_cast<int>(W) - 2, "LineStrip draws the middle row");
        }

        d.pipe = p_pt; d.vbo = b_pt; d.verts = 1;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        {
            bool hit = false;
            for (uint32_t y = H / 2 - 2; y <= H / 2 + 2; ++y)
                for (uint32_t x = W / 2 - 2; x <= W / 2 + 2; ++x)
                    if (fb.peq(x, y, 255, 0, 0, 255, 2)) hit = true;
            ok(mapped && hit, "PointList draws a 1px point at the center");
        }

        wgpuRenderPipelineRelease(p_tl); wgpuRenderPipelineRelease(p_ll);
        wgpuRenderPipelineRelease(p_ls); wgpuRenderPipelineRelease(p_pt);
        wgpuBufferRelease(b_tl); wgpuBufferRelease(b_ln); wgpuBufferRelease(b_pt);
    }

    // ============ blend factor + op matrix ============
    {
        struct Case {
            WGPUBlendFactor sc, dc; WGPUBlendOperation oc;
            WGPUBlendFactor sa, da; WGPUBlendOperation oa;
            float r, g, b, a; double cr, cg, cb;
            int er, eg, eb, ea, tol; const char* name;
        };
        const Case cases[6] = {
            {WGPUBlendFactor_One, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             WGPUBlendFactor_One, WGPUBlendFactor_Zero, WGPUBlendOperation_Add,
             0, 0, 1, 1, 0.5, 0.5, 0.5, 0, 0, 255, 255, 2, "blend One/Zero: src replaces dst"},
            {WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             WGPUBlendFactor_One, WGPUBlendFactor_One, WGPUBlendOperation_Add,
             0, 0, 0.5f, 1, 0.5, 0.0, 0.0, 128, 0, 128, 255, 2, "blend One/One Add: src+dst = (128,0,128)"},
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
        for (const auto& c : cases) {
            WGPUBlendState bs{};
            bs.color.srcFactor = c.sc; bs.color.dstFactor = c.dc; bs.color.operation = c.oc;
            bs.alpha.srcFactor = c.sa; bs.alpha.dstFactor = c.da; bs.alpha.operation = c.oa;
            WGPUColorTargetState tgt{};
            tgt.format = WGPUTextureFormat_RGBA8Unorm;
            tgt.blend = &bs;
            tgt.writeMask = WGPUColorWriteMask_All;
            WGPURenderPipeline p = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
                WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt, nullptr);
            set_color(color_ubo, c.r, c.g, c.b, c.a);
            Draw d; d.pipe = p; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
            mapped = frame(c.cr, c.cg, c.cb, 1.0, d, fb);
            ok(mapped && fb.all_eq(c.er, c.eg, c.eb, c.ea, c.tol), c.name);
            wgpuRenderPipelineRelease(p);
        }
    }

    // ============ depth-compare matrix ============
    {
        V3 dq[4] = {{{-1, -1, 0.5f}}, {{1, -1, 0.5f}}, {{-1, 1, 0.5f}}, {{1, 1, 0.5f}}};
        WGPUBuffer dvbo = make_vbo(dq, sizeof(dq), WGPUBufferUsage_Vertex);
        set_color(color_ubo, 0.0f, 1.0f, 0.0f, 1.0f);
        struct DCase { WGPUCompareFunction cmp; bool draws; const char* name; };
        const DCase cases[8] = {
            {WGPUCompareFunction_Always,       true,  "depth Always"},
            {WGPUCompareFunction_Never,        false, "depth Never"},
            {WGPUCompareFunction_Less,         true,  "depth Less"},
            {WGPUCompareFunction_LessEqual,    true,  "depth LessEqual"},
            {WGPUCompareFunction_Equal,        false, "depth Equal"},
            {WGPUCompareFunction_Greater,      false, "depth Greater"},
            {WGPUCompareFunction_GreaterEqual, false, "depth GreaterEqual"},
            {WGPUCompareFunction_NotEqual,     true,  "depth NotEqual"},
        };
        for (const auto& c : cases) {
            WGPUDepthStencilState ds{};
            ds.format = WGPUTextureFormat_Depth32Float;
            ds.depthWriteEnabled = 1;
            ds.depthCompare = c.cmp;
            ds.stencilFront.compare = WGPUCompareFunction_Always;
            ds.stencilBack.compare = WGPUCompareFunction_Always;
            WGPURenderPipeline p = mk(m_pos3, m_pos3, ubo_pll, &vbl_pos3,
                WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, &ds);
            mapped = frame_depth(0.0, 0.0, 0.0, 1.0, 0.75f, p, ubo_bg, dvbo, 4, fb);
            bool drew = fb.peq(W / 2, H / 2, 0, 255, 0, 255, 2);
            ok(mapped && drew == c.draws, c.name);
            wgpuRenderPipelineRelease(p);
        }
        wgpuBufferRelease(dvbo);
    }

    // ============ face culling + winding ============
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        WGPURenderPipeline p_none = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        Draw d; d.pipe = p_none; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(255, 0, 0, 255, 1), "cull None: quad drawn");

        WGPURenderPipeline p_ccw = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_Back, &tgt_all, nullptr);
        d.pipe = p_ccw;
        frame(0.0, 0.0, 0.0, 1.0, d, fb);
        bool ccw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);

        WGPURenderPipeline p_cw = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CW, WGPUCullMode_Back, &tgt_all, nullptr);
        d.pipe = p_cw;
        frame(0.0, 0.0, 0.0, 1.0, d, fb);
        bool cw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);
        ok(ccw != cw, "cull Back: Ccw vs Cw winding flips visibility");

        WGPURenderPipeline p_front = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_Front, &tgt_all, nullptr);
        d.pipe = p_front;
        frame(0.0, 0.0, 0.0, 1.0, d, fb);
        bool front_drawn = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);
        ok(front_drawn != ccw, "cull Front vs cull Back (Ccw) disagree at center");

        wgpuRenderPipelineRelease(p_none); wgpuRenderPipelineRelease(p_ccw);
        wgpuRenderPipelineRelease(p_cw); wgpuRenderPipelineRelease(p_front);
    }

    // ============ color write mask ============
    set_color(color_ubo, 1.0f, 1.0f, 1.0f, 1.0f);
    {
        WGPUColorTargetState tgt_red = no_blend(WGPUColorWriteMask_Red);
        WGPURenderPipeline p_r = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_red, nullptr);
        Draw d; d.pipe = p_r; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(255, 0, 0, 255, 1), "colorWrites RED only: white -> (255,0,0,255)");

        WGPURenderPipeline p_all = mk(m_solid, m_solid, ubo_pll, &vbl_pos2,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        d.pipe = p_all;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.all_eq(255, 255, 255, 255, 1), "colorWrites ALL: white -> (255,255,255,255)");

        wgpuRenderPipelineRelease(p_r); wgpuRenderPipelineRelease(p_all);
    }

    // ============ format feature + limit queries ============
    // v22 webgpu.h has no wgpuAdapterGetTextureFormatFeatures; realise the two format-feature
    // assertions behaviorally: create a texture of that format with RENDER_ATTACHMENT under a
    // validation scope and require NoError (functionally proves the format is renderable).
    {
        wgpuDevicePushErrorScope(dev, WGPUErrorFilter_Validation);
        WGPUTextureDescriptor td{};
        td.usage = WGPUTextureUsage_RenderAttachment;
        td.dimension = WGPUTextureDimension_2D;
        td.size = {W, H, 1};
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

        WGPUSupportedLimits lim{};
        wgpuDeviceGetLimits(dev, &lim);
        ok(lim.limits.maxTextureDimension2D >= W, "limits.max_texture_dimension_2d >= 64");
        // v22 wgpu-native's C accessor leaves maxColorAttachments unpopulated (reads 0), unlike the
        // Rust wgpu crate. Realise the intended check behaviorally: encode a render pass with one
        // color attachment under a validation scope and require NoError.
        wgpuDevicePushErrorScope(dev, WGPUErrorFilter_Validation);
        {
            WGPUCommandEncoder ce = wgpuDeviceCreateCommandEncoder(dev, nullptr);
            WGPURenderPassColorAttachment att{};
            att.view = g_color_view;
            att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
            att.loadOp = WGPULoadOp_Clear;
            att.storeOp = WGPUStoreOp_Store;
            att.clearValue = {0, 0, 0, 1};
            WGPURenderPassDescriptor rp{};
            rp.colorAttachmentCount = 1;
            rp.colorAttachments = &att;
            WGPURenderPassEncoder pe = wgpuCommandEncoderBeginRenderPass(ce, &rp);
            wgpuRenderPassEncoderEnd(pe);
            wgpuRenderPassEncoderRelease(pe);
            WGPUCommandBuffer cb = wgpuCommandEncoderFinish(ce, nullptr);
            wgpuQueueSubmit(g_queue, 1, &cb);
            wgpuCommandBufferRelease(cb);
            wgpuCommandEncoderRelease(ce);
        }
        WGPUErrorType cae = pop_error_scope(dev, inst);
        ok(cae == WGPUErrorType_NoError, "limits.max_color_attachments >= 1 (one-color-attachment render pass valid)");
    }

    // ============ 2x2 texture upload + Nearest sampling ============
    {
        WGPUTextureDescriptor td{};
        td.label = "tex2x2";
        td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
        td.dimension = WGPUTextureDimension_2D;
        td.size = {2, 2, 1};
        td.format = WGPUTextureFormat_RGBA8Unorm;
        td.mipLevelCount = 1; td.sampleCount = 1;
        WGPUTexture tex = wgpuDeviceCreateTexture(dev, &td);
        uint8_t texels[16] = {
            255, 0, 0, 255,     // (0,0) red
            0, 255, 0, 255,     // (1,0) green
            0, 0, 255, 255,     // (0,1) blue
            255, 255, 255, 255  // (1,1) white
        };
        WGPUImageCopyTexture wdst{};
        wdst.texture = tex; wdst.mipLevel = 0; wdst.origin = {0, 0, 0};
        wdst.aspect = WGPUTextureAspect_All;
        WGPUTextureDataLayout wl{};
        wl.offset = 0; wl.bytesPerRow = 8; wl.rowsPerImage = 2;
        WGPUExtent3D wsz{2, 2, 1};
        wgpuQueueWriteTexture(g_queue, &wdst, texels, sizeof(texels), &wl, &wsz);

        WGPUTextureView tview = wgpuTextureCreateView(tex, nullptr);
        WGPUSamplerDescriptor sd{};
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

        WGPUBindGroupLayoutEntry tble[2]{};
        tble[0].binding = 0;
        tble[0].visibility = WGPUShaderStage_Fragment;
        tble[0].texture.sampleType = WGPUTextureSampleType_Float;
        tble[0].texture.viewDimension = WGPUTextureViewDimension_2D;
        tble[0].texture.multisampled = 0;
        tble[1].binding = 1;
        tble[1].visibility = WGPUShaderStage_Fragment;
        tble[1].sampler.type = WGPUSamplerBindingType_Filtering;
        WGPUBindGroupLayoutDescriptor tbld{};
        tbld.label = "tex-bgl"; tbld.entryCount = 2; tbld.entries = tble;
        WGPUBindGroupLayout tex_bgl = wgpuDeviceCreateBindGroupLayout(dev, &tbld);

        WGPUBindGroupEntry tbge[2]{};
        tbge[0].binding = 0; tbge[0].textureView = tview;
        tbge[1].binding = 1; tbge[1].sampler = samp;
        WGPUBindGroupDescriptor tbgd{};
        tbgd.label = "tex-bg"; tbgd.layout = tex_bgl; tbgd.entryCount = 2; tbgd.entries = tbge;
        WGPUBindGroup tex_bg = wgpuDeviceCreateBindGroup(dev, &tbgd);

        WGPUPipelineLayoutDescriptor tplld{};
        tplld.label = "tex-pll"; tplld.bindGroupLayoutCount = 1; tplld.bindGroupLayouts = &tex_bgl;
        WGPUPipelineLayout tex_pll = wgpuDeviceCreatePipelineLayout(dev, &tplld);

        WGPURenderPipeline pipe_tex = mk(m_tex, m_tex, tex_pll, &vbl_pos2uv,
            WGPUPrimitiveTopology_TriangleStrip, WGPUFrontFace_CCW, WGPUCullMode_None, &tgt_all, nullptr);
        ok(pipe_tex != nullptr, "texture pipeline + bind group created");

        V2UV tq[4] = {
            {{-1, -1}, {0, 1}},
            {{1, -1},  {1, 1}},
            {{-1, 1},  {0, 0}},
            {{1, 1},   {1, 0}},
        };
        WGPUBuffer tvbo = make_vbo(tq, sizeof(tq), WGPUBufferUsage_Vertex);
        Draw d; d.pipe = pipe_tex; d.bind = tex_bg; d.vbo = tvbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && fb.peq(W / 4, H / 4, 255, 0, 0, 255, 2), "texture Nearest top-left red");
        ok(fb.peq(3 * W / 4, H / 4, 0, 255, 0, 255, 2), "texture Nearest top-right green");
        ok(fb.peq(W / 4, 3 * H / 4, 0, 0, 255, 255, 2), "texture Nearest bottom-left blue");
        ok(fb.peq(3 * W / 4, 3 * H / 4, 255, 255, 255, 255, 2), "texture Nearest bottom-right white");

        wgpuBufferRelease(tvbo);
        wgpuRenderPipelineRelease(pipe_tex);
        wgpuPipelineLayoutRelease(tex_pll);
        wgpuBindGroupRelease(tex_bg);
        wgpuBindGroupLayoutRelease(tex_bgl);
        wgpuSamplerRelease(samp);
        wgpuTextureViewRelease(tview);
        wgpuTextureRelease(tex);
    }

    // ============ negative control ============
    set_color(color_ubo, 1.0f, 0.0f, 0.0f, 1.0f);
    {
        Draw d; d.pipe = pipe_solid; d.bind = ubo_bg; d.vbo = vbo; d.verts = 4;
        mapped = frame(0.0, 0.0, 0.0, 1.0, d, fb);
        ok(mapped && !fb.all_eq(0, 255, 0, 255, 2), "negative control: red buffer is NOT green");
        ok(mapped && !fb.peq(0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black");
    }

    wgpuDevicePoll(dev, 1, nullptr);
    if (uncap.count != 0)
        std::fprintf(stderr, "note: %d uncaptured device error(s), last type=%d\n", uncap.count, uncap.last);

    // teardown
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

    return finish();
}
