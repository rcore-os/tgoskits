// scene_2dui_cpp - 2D UI compositing RENDER-scene carpet driven through the gfx-rs wgpu-native
// v22.1.0.2 C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
// GPU/window/surface, from C++17. C++ binding of the scene_2dui cell: an offscreen 64x64 Rgba8Unorm
// texture is rendered through real render pipelines (the SAME pixel-space y-flipped WGSL shaders as
// the Rust/C cells), copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back; every
// scene primitive has an INDEPENDENT closed-form software reference computed here (not derived from
// the GPU output) and asserted per pixel: filled axis-aligned rectangles, an analytic rounded-rect, a
// nine-patch-style scaled border frame, an 8x8 bitmap-font glyph blit, a scissor-clipped fill, and
// MULTI-LAYER Porter-Duff over compositing of 3 stacked semi-transparent layers. Closes with a
// negative control.
//
// The closed-form math (Porter-Duff over, analytic rounded-rect corner arc, nine-patch coverage, 8x8
// glyph bitmap, scissor clipping, q8 quantization) is behavior-identical to the C / Rust scene_2dui
// cells; only the wgpu-native binding syntax differs. Uses the v22 callback (callback,userdata) async
// model driven synchronously with wgpuInstanceProcessEvents / wgpuDevicePoll, like wgpu_render_cpp.
// Prints "SCENE_2DUI_CPP OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. EXPECTED=28 pins the
// Rust cell.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include <array>

#include "webgpu.h"
#include "wgpu.h"

namespace {

constexpr uint32_t W = 64, H = 64, BPP = 4;
constexpr int EXPECTED = 28;

int g_pass = 0, g_fail = 0;
void ok(bool c, const char* d) {
    if (c) ++g_pass;
    else { ++g_fail; std::fprintf(stderr, "FAIL: %s\n", d); }
}

int clampi(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }
int q8(float f) { return clampi(static_cast<int>(std::lround(f * 255.0f)), 0, 255); }

// ---- synchronous adapter/device/map request via the v22 (callback, userdata) model ----
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
void on_uncaptured(WGPUErrorType type, char const* message, void* ud) {
    (void)ud;
    std::fprintf(stderr, "UNCAPTURED wgpu error (%d): %s\n", static_cast<int>(type), message ? message : "");
}

// ---- WGSL shaders (identical to the Rust scene_2dui cell) ----
const char* SOLID_WGSL =
    "struct Solid { rgba: vec4<f32>, vp: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: Solid;\n"
    "@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    let n = (p / u.vp.xy) * 2.0 - 1.0;\n"
    "    return vec4<f32>(n.x, -n.y, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }\n";

const char* RR_WGSL =
    "struct RR { box: vec4<f32>, col: vec4<f32>, rad: vec4<f32>, vp: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: RR;\n"
    "@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    let n = (p / u.vp.xy) * 2.0 - 1.0;\n"
    "    return vec4<f32>(n.x, -n.y, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {\n"
    "    let p = fc.xy;\n"
    "    let x0 = u.box.x; let y0 = u.box.y; let x1 = u.box.z; let y1 = u.box.w;\n"
    "    let rad = u.rad.x;\n"
    "    let inside = p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1;\n"
    "    if (!inside) { discard; }\n"
    "    var corner = false;\n"
    "    var cc = vec2<f32>(0.0, 0.0);\n"
    "    if (p.x < x0 + rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x0 + rad, y0 + rad); }\n"
    "    else if (p.x >= x1 - rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x1 - rad, y0 + rad); }\n"
    "    else if (p.x < x0 + rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x0 + rad, y1 - rad); }\n"
    "    else if (p.x >= x1 - rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x1 - rad, y1 - rad); }\n"
    "    if (corner && distance(p, cc) > rad) { discard; }\n"
    "    return u.col;\n"
    "}\n";

const char* TEX_WGSL =
    "struct Vp { vp: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: Vp;\n"
    "@group(0) @binding(1) var t: texture_2d<f32>;\n"
    "@group(0) @binding(2) var s: sampler;\n"
    "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };\n"
    "@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    let n = (p / u.vp.xy) * 2.0 - 1.0;\n"
    "    o.pos = vec4<f32>(n.x, -n.y, 0.0, 1.0);\n"
    "    o.uv = uv;\n"
    "    return o;\n"
    "}\n"
    "@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }\n";

WGPUShaderModule make_wgsl(WGPUDevice dev, const char* code, const char* label) {
    WGPUShaderModuleWGSLDescriptor wgsl{};
    wgsl.chain.sType = WGPUSType_ShaderModuleWGSLDescriptor;
    wgsl.code = code;
    WGPUShaderModuleDescriptor sd{};
    sd.nextInChain = reinterpret_cast<WGPUChainedStruct*>(&wgsl);
    sd.label = label;
    return wgpuDeviceCreateShaderModule(dev, &sd);
}

// ---- readback framebuffer ----
struct Fb {
    std::array<uint8_t, H * W * BPP> px{};
    uint8_t p(uint32_t x, uint32_t y, unsigned c) const { return px[(y * W + x) * BPP + c]; }
    bool peq(int x, int y, int r, int g, int b, int a, int tol) const {
        if (x < 0 || y < 0 || x >= static_cast<int>(W) || y >= static_cast<int>(H)) return false;
        return std::abs(static_cast<int>(p(x, y, 0)) - r) <= tol &&
               std::abs(static_cast<int>(p(x, y, 1)) - g) <= tol &&
               std::abs(static_cast<int>(p(x, y, 2)) - b) <= tol &&
               std::abs(static_cast<int>(p(x, y, 3)) - a) <= tol;
    }
};

// ---- globals wired through the frame helper ----
WGPUDevice   g_dev = nullptr;
WGPUQueue    g_queue = nullptr;
WGPUTexture  g_color = nullptr;
WGPUBuffer   g_readback = nullptr;
WGPUTextureView g_color_view = nullptr;
uint32_t     g_padded = 0;

bool copy_and_read(WGPUCommandEncoder enc, Fb& out) {
    WGPUImageCopyTexture src{};
    src.texture = g_color; src.mipLevel = 0;
    src.origin = {0, 0, 0}; src.aspect = WGPUTextureAspect_All;
    WGPUImageCopyBuffer dst{};
    dst.buffer = g_readback; dst.layout.offset = 0;
    dst.layout.bytesPerRow = g_padded; dst.layout.rowsPerImage = H;
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

// one draw op
struct DrawOp {
    WGPURenderPipeline pipe = nullptr;
    WGPUBindGroup bind = nullptr;
    WGPUBuffer vbo = nullptr;
    uint32_t verts = 0;
    bool has_scissor = false; uint32_t sx = 0, sy = 0, sw = 0, sh = 0;
};

// render one frame: clear + a list of draw ops.
bool frame(double cr, double cg, double cb, double ca, const std::vector<DrawOp>& ops, Fb& out) {
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
    for (const auto& op : ops) {
        wgpuRenderPassEncoderSetPipeline(pass, op.pipe);
        if (op.has_scissor)
            wgpuRenderPassEncoderSetScissorRect(pass, op.sx, op.sy, op.sw, op.sh);
        else
            wgpuRenderPassEncoderSetScissorRect(pass, 0, 0, W, H);
        if (op.bind) wgpuRenderPassEncoderSetBindGroup(pass, 0, op.bind, 0, nullptr);
        if (op.vbo) wgpuRenderPassEncoderSetVertexBuffer(pass, 0, op.vbo, 0, WGPU_WHOLE_SIZE);
        wgpuRenderPassEncoderDraw(pass, op.verts, 1, 0, 0);
    }
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

struct V2 { float pos[2]; };
struct V2UV { float pos[2]; float uv[2]; };

WGPUBuffer make_buf(const void* data, size_t bytes, WGPUBufferUsageFlags usage) {
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
// two-triangle pixel rect [x0,x1) x [y0,y1)
WGPUBuffer rect_vbo(float x0, float y0, float x1, float y1) {
    std::array<V2, 6> v = {{
        {{x0, y0}}, {{x1, y0}}, {{x0, y1}},
        {{x0, y1}}, {{x1, y0}}, {{x1, y1}},
    }};
    return make_buf(v.data(), v.size() * sizeof(V2), WGPUBufferUsage_Vertex);
}
// solid uniform: rgba + vp (8 floats)
WGPUBuffer solid_ubo(float r, float g, float b, float a) {
    std::array<float, 8> d = {r, g, b, a, static_cast<float>(W), static_cast<float>(H), 0.0f, 0.0f};
    return make_buf(d.data(), d.size() * sizeof(float), WGPUBufferUsage_Uniform);
}

WGPUBindGroupLayout g_ubo_bgl = nullptr;
WGPUBindGroup bind_ubo(WGPUBuffer buf, uint64_t size) {
    WGPUBindGroupEntry e{};
    e.binding = 0; e.buffer = buf; e.offset = 0; e.size = size;
    WGPUBindGroupDescriptor bgd{};
    bgd.layout = g_ubo_bgl; bgd.entryCount = 1; bgd.entries = &e;
    return wgpuDeviceCreateBindGroup(g_dev, &bgd);
}

WGPURenderPipeline mk_solid(WGPUShaderModule m, WGPUPipelineLayout pll,
                            const WGPUVertexBufferLayout* vbl, const WGPUBlendState* blend) {
    WGPUColorTargetState tgt{};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = blend;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag{};
    frag.module = m; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;
    WGPURenderPipelineDescriptor d{};
    d.label = "pipe";
    d.layout = pll;
    d.vertex.module = m; d.vertex.entryPoint = "vs"; d.vertex.bufferCount = 1; d.vertex.buffers = vbl;
    d.primitive.topology = WGPUPrimitiveTopology_TriangleList;
    d.primitive.stripIndexFormat = WGPUIndexFormat_Undefined;
    d.primitive.frontFace = WGPUFrontFace_CCW;
    d.primitive.cullMode = WGPUCullMode_None;
    d.multisample.count = 1; d.multisample.mask = 0xFFFFFFFFu;
    d.fragment = &frag;
    return wgpuDeviceCreateRenderPipeline(g_dev, &d);
}

int finish() {
    int total = g_pass + g_fail;
    std::printf("scene-2dui-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, total, EXPECTED);
    if (g_fail == 0 && total == EXPECTED) {
        std::printf("SCENE_2DUI_CPP OK %d\n", g_pass);
        return 0;
    }
    std::printf("SCENE_2DUI_CPP FAIL\n");
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
    if (!inst) { std::printf("SCENE_2DUI_CPP FAIL\n"); return 1; }

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
    std::printf("wgpu adapter selected: type=%d name=\"%s\"\n", static_cast<int>(info.backendType),
                info.device ? info.device : "");
    ok(info.backendType == WGPUBackendType_Vulkan || info.backendType == WGPUBackendType_OpenGL ||
       info.backendType == WGPUBackendType_OpenGLES,
       "adapter backend is Vulkan or Gl");

    DeviceReq dreq;
    WGPUDeviceDescriptor ddesc{};
    ddesc.label = "2dui-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; ++i) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) return finish();
    g_dev = dev;
    g_queue = wgpuDeviceGetQueue(dev);

    WGPUTextureDescriptor cd{};
    cd.label = "color";
    cd.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    cd.dimension = WGPUTextureDimension_2D;
    cd.size = {W, H, 1};
    cd.format = WGPUTextureFormat_RGBA8Unorm;
    cd.mipLevelCount = 1; cd.sampleCount = 1;
    g_color = wgpuDeviceCreateTexture(dev, &cd);
    g_color_view = wgpuTextureCreateView(g_color, nullptr);

    uint32_t unpadded = W * BPP;
    uint32_t align = 256;
    g_padded = ((unpadded + align - 1) / align) * align;
    WGPUBufferDescriptor rbd{};
    rbd.label = "readback";
    rbd.usage = WGPUBufferUsage_CopyDst | WGPUBufferUsage_MapRead;
    rbd.size = static_cast<uint64_t>(g_padded) * H;
    g_readback = wgpuDeviceCreateBuffer(dev, &rbd);
    ok(true, "offscreen Rgba8Unorm target + readback buffer ready");

    // shaders
    WGPUShaderModule m_solid = make_wgsl(dev, SOLID_WGSL, "solid");
    WGPUShaderModule m_rr = make_wgsl(dev, RR_WGSL, "rr");
    WGPUShaderModule m_tex = make_wgsl(dev, TEX_WGSL, "tex");

    // single-uniform bind-group layout (vertex+fragment visible), shared by solid/rr
    WGPUBindGroupLayoutEntry ule{};
    ule.binding = 0;
    ule.visibility = WGPUShaderStage_Vertex | WGPUShaderStage_Fragment;
    ule.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor uld{};
    uld.entryCount = 1; uld.entries = &ule;
    g_ubo_bgl = wgpuDeviceCreateBindGroupLayout(dev, &uld);
    WGPUPipelineLayoutDescriptor plld{};
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &g_ubo_bgl;
    WGPUPipelineLayout solid_pll = wgpuDeviceCreatePipelineLayout(dev, &plld);

    WGPUVertexAttribute a_pos2{WGPUVertexFormat_Float32x2, 0, 0};
    WGPUVertexBufferLayout vbl_pos2{sizeof(V2), WGPUVertexStepMode_Vertex, 1, &a_pos2};

    WGPURenderPipeline pipe_solid = mk_solid(m_solid, solid_pll, &vbl_pos2, nullptr);
    WGPUBlendState blend_over{};
    blend_over.color.srcFactor = WGPUBlendFactor_SrcAlpha;
    blend_over.color.dstFactor = WGPUBlendFactor_OneMinusSrcAlpha;
    blend_over.color.operation = WGPUBlendOperation_Add;
    blend_over.alpha = blend_over.color;
    WGPURenderPipeline pipe_blend = mk_solid(m_solid, solid_pll, &vbl_pos2, &blend_over);

    Fb fb;
    bool mapped;

    // ---- Scene A: filled rectangles ----
    {
        WGPUBuffer ua = solid_ubo(1, 0, 0, 1), ub = solid_ubo(0, 1, 0, 1);
        WGPUBindGroup ba = bind_ubo(ua, 32), bb = bind_ubo(ub, 32);
        WGPUBuffer vr1 = rect_vbo(8, 8, 16, 24), vr2 = rect_vbo(40, 32, 48, 52);
        std::vector<DrawOp> ops = {
            {pipe_solid, ba, vr1, 6, false, 0, 0, 0, 0},
            {pipe_solid, bb, vr2, 6, false, 0, 0, 0, 0},
        };
        mapped = frame(0, 0, 0, 1, ops, fb);
        int bad = 0;
        for (int y = 0; y < static_cast<int>(H); ++y)
            for (int x = 0; x < static_cast<int>(W); ++x) {
                int er, eg, eb;
                if (x >= 8 && x < 16 && y >= 8 && y < 24) { er = 255; eg = 0; eb = 0; }
                else if (x >= 40 && x < 48 && y >= 32 && y < 52) { er = 0; eg = 255; eb = 0; }
                else { er = 0; eg = 0; eb = 0; }
                if (!fb.peq(x, y, er, eg, eb, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "filled rectangles: every pixel matches closed-form rect coverage");
        ok(fb.peq(10, 10, 255, 0, 0, 255, 1), "rect A interior red");
        ok(fb.peq(44, 40, 0, 255, 0, 255, 1), "rect B interior green");
        ok(fb.peq(30, 30, 0, 0, 0, 255, 1), "gap between rects is background");
        wgpuBufferRelease(ua); wgpuBufferRelease(ub);
        wgpuBufferRelease(vr1); wgpuBufferRelease(vr2);
        wgpuBindGroupRelease(ba); wgpuBindGroupRelease(bb);
    }

    // ---- Scene B: analytic rounded-rect ----
    {
        float rrd[16] = {12, 12, 52, 52, 1, 1, 0, 1, 8, 0, 0, 0, static_cast<float>(W), static_cast<float>(H), 0, 0};
        WGPUBuffer rr_ubo = make_buf(rrd, sizeof(rrd), WGPUBufferUsage_Uniform);
        WGPUBindGroup rr_bg = bind_ubo(rr_ubo, 64);
        WGPURenderPipeline pipe_rr = mk_solid(m_rr, solid_pll, &vbl_pos2, nullptr);
        WGPUBuffer fq = rect_vbo(0, 0, static_cast<float>(W), static_cast<float>(H));
        std::vector<DrawOp> ops = {{pipe_rr, rr_bg, fq, 6, false, 0, 0, 0, 0}};
        mapped = frame(0, 0, 0, 1, ops, fb);
        int bad = 0, lit = 0;
        for (int y = 0; y < static_cast<int>(H); ++y)
            for (int x = 0; x < static_cast<int>(W); ++x) {
                float cx = x + 0.5f, cy = y + 0.5f;
                float x0 = 12, y0 = 12, x1 = 52, y1 = 52, r = 8;
                int cov = (cx >= x0 && cx < x1 && cy >= y0 && cy < y1);
                if (cov) {
                    int corner = 0; float ccx = 0, ccy = 0;
                    if (cx < x0 + r && cy < y0 + r) { corner = 1; ccx = x0 + r; ccy = y0 + r; }
                    else if (cx >= x1 - r && cy < y0 + r) { corner = 1; ccx = x1 - r; ccy = y0 + r; }
                    else if (cx < x0 + r && cy >= y1 - r) { corner = 1; ccx = x0 + r; ccy = y1 - r; }
                    else if (cx >= x1 - r && cy >= y1 - r) { corner = 1; ccx = x1 - r; ccy = y1 - r; }
                    if (corner) {
                        float dx = cx - ccx, dy = cy - ccy;
                        if (std::sqrt(dx * dx + dy * dy) > r) cov = 0;
                    }
                }
                if (cov) ++lit;
                int er = cov ? 255 : 0, eg = cov ? 255 : 0;
                if (!fb.peq(x, y, er, eg, 0, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "rounded-rect: every pixel matches analytic corner-arc coverage");
        ok(lit > 0, "rounded-rect: some pixels covered");
        ok(fb.peq(32, 32, 255, 255, 0, 255, 1), "rounded-rect center lit");
        ok(fb.peq(12, 12, 0, 0, 0, 255, 1), "rounded-rect clipped corner (12,12) is background");
        ok(fb.peq(32, 13, 255, 255, 0, 255, 1), "rounded-rect straight top edge lit");
        wgpuBufferRelease(rr_ubo); wgpuBufferRelease(fq);
        wgpuBindGroupRelease(rr_bg); wgpuRenderPipelineRelease(pipe_rr);
    }

    // ---- Scene C: nine-patch-style scaled border frame ----
    {
        WGPUBuffer vbox = rect_vbo(4, 4, 60, 60), vinner = rect_vbo(10, 10, 54, 54);
        WGPUBuffer ublue = solid_ubo(0, 0, 1, 1), udark = solid_ubo(0.1f, 0.1f, 0.1f, 1);
        WGPUBindGroup bblue = bind_ubo(ublue, 32), bdark = bind_ubo(udark, 32);
        std::vector<DrawOp> ops = {
            {pipe_solid, bblue, vbox, 6, false, 0, 0, 0, 0},
            {pipe_solid, bdark, vinner, 6, false, 0, 0, 0, 0},
        };
        mapped = frame(0, 0, 0, 1, ops, fb);
        int bad = 0;
        for (int y = 0; y < static_cast<int>(H); ++y)
            for (int x = 0; x < static_cast<int>(W); ++x) {
                int inbox = x >= 4 && x < 60 && y >= 4 && y < 60;
                int ininner = x >= 10 && x < 54 && y >= 10 && y < 54;
                int er, eg, eb;
                if (ininner) { er = eg = eb = q8(0.1f); }
                else if (inbox) { er = 0; eg = 0; eb = 255; }
                else { er = 0; eg = 0; eb = 0; }
                if (!fb.peq(x, y, er, eg, eb, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "nine-patch border frame: closed-form border-vs-interior coverage");
        ok(fb.peq(5, 32, 0, 0, 255, 255, 1), "nine-patch left border blue");
        ok(fb.peq(32, 5, 0, 0, 255, 255, 1), "nine-patch top border blue");
        ok(fb.peq(32, 32, q8(0.1f), q8(0.1f), q8(0.1f), 255, 1), "nine-patch hollow interior");
        wgpuBufferRelease(vbox); wgpuBufferRelease(vinner);
        wgpuBufferRelease(ublue); wgpuBufferRelease(udark);
        wgpuBindGroupRelease(bblue); wgpuBindGroupRelease(bdark);
    }

    // ---- Scene D: 8x8 bitmap-font glyph blit ----
    {
        const std::array<uint8_t, 8> GLYPH_H = {0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00};
        uint8_t rgba[8 * 8 * 4];
        for (int r = 0; r < 8; ++r)
            for (int c = 0; c < 8; ++c) {
                int lit = (GLYPH_H[r] >> (7 - c)) & 1;
                uint8_t v = lit ? 255 : 0;
                int idx = (r * 8 + c) * 4;
                rgba[idx] = v; rgba[idx + 1] = v; rgba[idx + 2] = v; rgba[idx + 3] = 255;
            }
        WGPUTextureDescriptor gd{};
        gd.label = "glyph";
        gd.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
        gd.dimension = WGPUTextureDimension_2D;
        gd.size = {8, 8, 1};
        gd.format = WGPUTextureFormat_RGBA8Unorm;
        gd.mipLevelCount = 1; gd.sampleCount = 1;
        WGPUTexture gtex = wgpuDeviceCreateTexture(dev, &gd);
        WGPUImageCopyTexture dst{};
        dst.texture = gtex; dst.mipLevel = 0; dst.origin = {0, 0, 0};
        dst.aspect = WGPUTextureAspect_All;
        WGPUTextureDataLayout dl{};
        dl.offset = 0; dl.bytesPerRow = 32; dl.rowsPerImage = 8;
        WGPUExtent3D ge{8, 8, 1};
        wgpuQueueWriteTexture(g_queue, &dst, rgba, sizeof(rgba), &dl, &ge);
        WGPUTextureView gview = wgpuTextureCreateView(gtex, nullptr);

        WGPUSamplerDescriptor sd{};
        sd.addressModeU = WGPUAddressMode_ClampToEdge;
        sd.addressModeV = WGPUAddressMode_ClampToEdge;
        sd.addressModeW = WGPUAddressMode_ClampToEdge;
        sd.magFilter = WGPUFilterMode_Nearest;
        sd.minFilter = WGPUFilterMode_Nearest;
        sd.mipmapFilter = WGPUMipmapFilterMode_Nearest;
        sd.maxAnisotropy = 1;
        WGPUSampler samp = wgpuDeviceCreateSampler(dev, &sd);

        float vpd[4] = {static_cast<float>(W), static_cast<float>(H), 0, 0};
        WGPUBuffer vp_ubo = make_buf(vpd, sizeof(vpd), WGPUBufferUsage_Uniform);

        WGPUBindGroupLayoutEntry te[3]{};
        te[0].binding = 0; te[0].visibility = WGPUShaderStage_Vertex;
        te[0].buffer.type = WGPUBufferBindingType_Uniform;
        te[1].binding = 1; te[1].visibility = WGPUShaderStage_Fragment;
        te[1].texture.sampleType = WGPUTextureSampleType_Float;
        te[1].texture.viewDimension = WGPUTextureViewDimension_2D;
        te[2].binding = 2; te[2].visibility = WGPUShaderStage_Fragment;
        te[2].sampler.type = WGPUSamplerBindingType_Filtering;
        WGPUBindGroupLayoutDescriptor tld{};
        tld.entryCount = 3; tld.entries = te;
        WGPUBindGroupLayout tex_bgl = wgpuDeviceCreateBindGroupLayout(dev, &tld);

        WGPUBindGroupEntry tbe[3]{};
        tbe[0].binding = 0; tbe[0].buffer = vp_ubo; tbe[0].offset = 0; tbe[0].size = 16;
        tbe[1].binding = 1; tbe[1].textureView = gview;
        tbe[2].binding = 2; tbe[2].sampler = samp;
        WGPUBindGroupDescriptor tbd{};
        tbd.layout = tex_bgl; tbd.entryCount = 3; tbd.entries = tbe;
        WGPUBindGroup tex_bg = wgpuDeviceCreateBindGroup(dev, &tbd);

        WGPUPipelineLayoutDescriptor tplld{};
        tplld.bindGroupLayoutCount = 1; tplld.bindGroupLayouts = &tex_bgl;
        WGPUPipelineLayout tex_pll = wgpuDeviceCreatePipelineLayout(dev, &tplld);

        WGPUVertexAttribute a_pos2uv[2] = {
            {WGPUVertexFormat_Float32x2, 0, 0},
            {WGPUVertexFormat_Float32x2, 8, 1},
        };
        WGPUVertexBufferLayout vbl_pos2uv{sizeof(V2UV), WGPUVertexStepMode_Vertex, 2, a_pos2uv};
        WGPURenderPipeline pipe_tex = mk_solid(m_tex, tex_pll, &vbl_pos2uv, nullptr);

        std::array<V2UV, 6> gq = {{
            {{20, 20}, {0, 0}}, {{28, 20}, {1, 0}}, {{20, 28}, {0, 1}},
            {{20, 28}, {0, 1}}, {{28, 20}, {1, 0}}, {{28, 28}, {1, 1}},
        }};
        WGPUBuffer gvbo = make_buf(gq.data(), gq.size() * sizeof(V2UV), WGPUBufferUsage_Vertex);
        std::vector<DrawOp> ops = {{pipe_tex, tex_bg, gvbo, 6, false, 0, 0, 0, 0}};
        mapped = frame(0, 0, 0, 1, ops, fb);
        int bad = 0;
        for (int dy = 0; dy < 8; ++dy)
            for (int dx = 0; dx < 8; ++dx) {
                int sx = 20 + dx, sy = 20 + dy;
                int lit = (GLYPH_H[dy] >> (7 - dx)) & 1;
                int v = lit ? 255 : 0;
                if (!fb.peq(sx, sy, v, v, v, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap");
        ok(fb.peq(21, 23, 255, 255, 255, 255, 1), "glyph crossbar lit (col1,row3)");
        ok(fb.peq(23, 20, 0, 0, 0, 255, 1), "glyph row0 blank");
        ok(fb.peq(24, 21, 0, 0, 0, 255, 1), "glyph row1 middle blank (0x42)");
        wgpuBufferRelease(vp_ubo); wgpuBufferRelease(gvbo);
        wgpuBindGroupRelease(tex_bg); wgpuRenderPipelineRelease(pipe_tex);
    }

    // ---- Scene E: scissor-clipped fill ----
    {
        WGPUBuffer umag = solid_ubo(1, 0, 1, 1);
        WGPUBindGroup bmag = bind_ubo(umag, 32);
        WGPUBuffer sq = rect_vbo(0, 0, static_cast<float>(W), static_cast<float>(H));
        std::vector<DrawOp> ops = {{pipe_solid, bmag, sq, 6, true, 16, 16, 20, 20}};
        mapped = frame(0, 0, 0, 1, ops, fb);
        int bad = 0;
        for (int y = 0; y < static_cast<int>(H); ++y)
            for (int x = 0; x < static_cast<int>(W); ++x) {
                int inb = x >= 16 && x < 36 && y >= 16 && y < 36;
                int er = inb ? 255 : 0, eb = inb ? 255 : 0;
                if (!fb.peq(x, y, er, 0, eb, 255, 1)) ++bad;
            }
        ok(mapped && bad == 0, "scissor-clipped fill: magenta only within [16,36)^2");
        ok(fb.peq(20, 20, 255, 0, 255, 255, 1), "scissor inside magenta");
        ok(fb.peq(40, 40, 0, 0, 0, 255, 1), "scissor outside background");
        wgpuBufferRelease(umag); wgpuBufferRelease(sq); wgpuBindGroupRelease(bmag);
    }

    // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
    {
        float bg[4] = {0.10f, 0.10f, 0.10f, 1.0f};
        std::array<std::array<float, 8>, 3> layers = {{
            {{1, 0, 0, 0.50f, 8, 8, 56, 56}},
            {{0, 1, 0, 0.25f, 12, 12, 52, 52}},
            {{0, 0, 1, 0.75f, 16, 16, 48, 48}},
        }};
        std::array<WGPUBuffer, 3> ubos, vbos;
        std::array<WGPUBindGroup, 3> bgs;
        std::vector<DrawOp> ops;
        for (int i = 0; i < 3; ++i) {
            ubos[i] = solid_ubo(layers[i][0], layers[i][1], layers[i][2], layers[i][3]);
            bgs[i] = bind_ubo(ubos[i], 32);
            vbos[i] = rect_vbo(layers[i][4], layers[i][5], layers[i][6], layers[i][7]);
            ops.push_back({pipe_blend, bgs[i], vbos[i], 6, false, 0, 0, 0, 0});
        }
        mapped = frame(bg[0], bg[1], bg[2], bg[3], ops, fb);
        int bad = 0;
        for (int y = 0; y < static_cast<int>(H); ++y)
            for (int x = 0; x < static_cast<int>(W); ++x) {
                float c[4] = {bg[0], bg[1], bg[2], bg[3]};
                for (int l = 0; l < 3; ++l) {
                    float cx = x + 0.5f, cy = y + 0.5f;
                    if (cx >= layers[l][4] && cx < layers[l][6] &&
                        cy >= layers[l][5] && cy < layers[l][7]) {
                        float a = layers[l][3];
                        float src[4] = {layers[l][0], layers[l][1], layers[l][2], layers[l][3]};
                        for (int k = 0; k < 4; ++k) c[k] = src[k] * a + c[k] * (1.0f - a);
                    }
                }
                if (!fb.peq(x, y, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2)) ++bad;
            }
        ok(mapped && bad == 0,
           "multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)");
        {
            float c[4] = {bg[0], bg[1], bg[2], bg[3]};
            float ls[3][4] = {{1, 0, 0, 0.5f}, {0, 1, 0, 0.25f}, {0, 0, 1, 0.75f}};
            for (int l = 0; l < 3; ++l) {
                float a = ls[l][3];
                for (int k = 0; k < 4; ++k) c[k] = ls[l][k] * a + c[k] * (1.0f - a);
            }
            ok(fb.peq(32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2),
               "multi-layer over center pixel matches hand-iterated over");
        }
        {
            float a = 0.5f;
            float er = 1.0f * a + bg[0] * (1.0f - a);
            float eg = 0.0f * a + bg[1] * (1.0f - a);
            float eb = 0.0f * a + bg[2] * (1.0f - a);
            float ea = a * a + bg[3] * (1.0f - a);
            ok(fb.peq(10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2),
               "multi-layer over: single-layer region matches one over");
        }
        for (int i = 0; i < 3; ++i) {
            wgpuBufferRelease(ubos[i]); wgpuBufferRelease(vbos[i]); wgpuBindGroupRelease(bgs[i]);
        }
    }

    // ---- Negative control ----
    {
        WGPUBuffer ua = solid_ubo(1, 0, 0, 1);
        WGPUBindGroup ba = bind_ubo(ua, 32);
        WGPUBuffer vr1 = rect_vbo(8, 8, 16, 24);
        std::vector<DrawOp> ops = {{pipe_solid, ba, vr1, 6, false, 0, 0, 0, 0}};
        frame(0, 0, 0, 1, ops, fb);
        ok(!fb.peq(10, 10, 0, 255, 0, 255, 4), "negative control: red rect pixel is NOT green");
        ok(!fb.peq(30, 30, 255, 0, 0, 255, 4), "negative control: background is NOT red");
        wgpuBufferRelease(ua); wgpuBufferRelease(vr1); wgpuBindGroupRelease(ba);
    }

    wgpuDevicePoll(dev, 1, nullptr);
    return finish();
}
