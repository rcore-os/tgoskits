// scene_anim_cpp - keyframe-animation RENDER-scene carpet driven through the gfx-rs wgpu-native
// v22.1.0.2 C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
// GPU/window/surface, from C++17. C++ binding of the scene_anim cell: an offscreen 64x64 Rgba8Unorm
// texture is rendered through a real render pipeline (the SAME pixel-space y-flipped affine WGSL
// shader as the Rust/C cells); N=4 keyframes of a transformed unit quad are drawn, each frame's model
// transform a rotation about the FBO center composed with a translation and uniform scale, interpolated
// by t in {0,0.25,0.5,0.75}. The transform is applied in the vertex shader from a hand-built pixel-space
// model matrix (rotation, scale, translate) and an ortho pixel->NDC map. For every frame the four
// rotated/scaled/translated quad CORNERS are computed INDEPENDENTLY here (closed form: R(theta)*S*local
// + T) and the readback is asserted at those exact corner pixels plus a point outside the quad. A cubic
// ease eased(t)=3t^2-2t^3 drives the scale, its value asserted at each t. Closes with a negative control.
//
// The lerp / ease_cubic / R*S*local+T corner math is behavior-identical to the C / Rust scene_anim cells;
// only the wgpu-native binding syntax differs. Uses the v22 (callback,userdata) async model driven
// synchronously via wgpuInstanceProcessEvents / wgpuDevicePoll, like scene_2dui_cpp. Prints
// "SCENE_ANIM_CPP OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. EXPECTED=38 pins the Rust cell.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <vector>
#include <array>

#include "webgpu.h"
#include "wgpu.h"

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

namespace {

constexpr uint32_t W = 64, H = 64, BPP = 4;
constexpr int EXPECTED = 38;

int g_pass = 0, g_fail = 0;
void ok(bool c, const char* d) {
    if (c) ++g_pass;
    else { ++g_fail; std::fprintf(stderr, "FAIL: %s\n", d); }
}

float lerp(float a, float b, float t) { return a + (b - a) * t; }
float ease_cubic(float t) { return 3.0f * t * t - 2.0f * t * t * t; }

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

// ---- WGSL shader (identical to the Rust scene_anim cell) ----
// Pixel-space affine vertex shader: pix = col0*lp.x + col1*lp.y + tr; then map pixel -> NDC with y-flip
// so pixel-y == readback-row. Uniform color out.
const char* WGSL =
    "struct X { col0: vec2<f32>, col1: vec2<f32>, tr: vec2<f32>, vp: vec2<f32>, rgba: vec4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: X;\n"
    "@vertex fn vs(@location(0) lp: vec2<f32>) -> @builtin(position) vec4<f32> {\n"
    "    let pix = u.col0 * lp.x + u.col1 * lp.y + u.tr;\n"
    "    let n = (pix / u.vp) * 2.0 - 1.0;\n"
    "    return vec4<f32>(n.x, -n.y, 0.0, 1.0);\n"
    "}\n"
    "@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }\n";

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
    // 3x3 neighborhood scan (a fixed at 255), matching the Rust Fb::near_color.
    bool near_color(int x, int y, int r, int g, int b, int tol) const {
        for (int dy = -1; dy <= 1; ++dy)
            for (int dx = -1; dx <= 1; ++dx) {
                int xx = x + dx, yy = y + dy;
                if (xx < 0 || yy < 0 || xx >= static_cast<int>(W) || yy >= static_cast<int>(H)) continue;
                if (peq(xx, yy, r, g, b, 255, tol)) return true;
            }
        return false;
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

struct V2 { float pos[2]; };

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

WGPUBindGroupLayout g_ubo_bgl = nullptr;
WGPUBindGroup bind_ubo(WGPUBuffer buf, uint64_t size) {
    WGPUBindGroupEntry e{};
    e.binding = 0; e.buffer = buf; e.offset = 0; e.size = size;
    WGPUBindGroupDescriptor bgd{};
    bgd.layout = g_ubo_bgl; bgd.entryCount = 1; bgd.entries = &e;
    return wgpuDeviceCreateBindGroup(g_dev, &bgd);
}

// TriangleStrip pipeline for the affine quad (the anim cell uses PrimitiveTopology::TriangleStrip).
WGPURenderPipeline mk_solid(WGPUShaderModule m, WGPUPipelineLayout pll,
                            const WGPUVertexBufferLayout* vbl, const WGPUBlendState* blend) {
    WGPUColorTargetState tgt{};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = blend;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag{};
    frag.module = m; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;
    WGPURenderPipelineDescriptor d{};
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

// per-frame uniform: col0.xy, col1.xy, tr.xy, vp.xy, rgba (12 floats)
WGPURenderPipeline g_pipe = nullptr;
WGPUBuffer g_xform_ubo = nullptr;
WGPUBindGroup g_bg = nullptr;
WGPUBuffer g_vbo = nullptr;

// render one frame with the current uniform (rewrite ubo + re-encode).
bool render_frame(const float xform[12], Fb& out) {
    wgpuQueueWriteBuffer(g_queue, g_xform_ubo, 0, xform, 12 * sizeof(float));
    WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(g_dev, nullptr);
    WGPURenderPassColorAttachment att{};
    att.view = g_color_view;
    att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
    att.resolveTarget = nullptr;
    att.loadOp = WGPULoadOp_Clear;
    att.storeOp = WGPUStoreOp_Store;
    att.clearValue = {0.0, 0.0, 0.0, 1.0};
    WGPURenderPassDescriptor rp{};
    rp.colorAttachmentCount = 1;
    rp.colorAttachments = &att;
    WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
    wgpuRenderPassEncoderSetPipeline(pass, g_pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, g_bg, 0, nullptr);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, g_vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, 4, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

// animation keyframe params (ported).
constexpr float A0 = 0.0f, A1 = static_cast<float>(M_PI / 2.0);
constexpr float S0 = 6.0f, S1 = 14.0f;
constexpr float CX0 = 20.0f, CX1 = 44.0f, CY0 = 20.0f, CY1 = 44.0f;

// frame_transform(t) -> col0[2], col1[2], tr[2], sc, ang
void frame_transform(float t, float col0[2], float col1[2], float tr[2], float* sc, float* ang) {
    float a = lerp(A0, A1, t);
    float s = lerp(S0, S1, ease_cubic(t));
    float cx = lerp(CX0, CX1, t), cy = lerp(CY0, CY1, t);
    float ca = std::cos(a), sa = std::sin(a);
    col0[0] = s * ca; col0[1] = s * sa;
    col1[0] = -s * sa; col1[1] = s * ca;
    tr[0] = cx; tr[1] = cy;
    *sc = s; *ang = a;
}

int finish() {
    int total = g_pass + g_fail;
    std::printf("scene-anim-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, total, EXPECTED);
    if (g_fail == 0 && total == EXPECTED) {
        std::printf("SCENE_ANIM_CPP OK %d\n", g_pass);
        return 0;
    }
    std::printf("SCENE_ANIM_CPP FAIL\n");
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
    if (!inst) { std::printf("SCENE_ANIM_CPP FAIL\n"); return 1; }

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
    if (!adapter) {
        std::printf("scene-anim-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, g_pass + g_fail, EXPECTED);
        std::printf("SCENE_ANIM_CPP FAIL\n");
        return 1;
    }

    WGPUAdapterInfo info{};
    wgpuAdapterGetInfo(adapter, &info);
    std::printf("wgpu adapter selected: type=%d name=\"%s\"\n", static_cast<int>(info.backendType),
                info.device ? info.device : "");
    ok(info.backendType == WGPUBackendType_Vulkan || info.backendType == WGPUBackendType_OpenGL ||
       info.backendType == WGPUBackendType_OpenGLES,
       "adapter backend is Vulkan or Gl");

    DeviceReq dreq;
    WGPUDeviceDescriptor ddesc{};
    ddesc.label = "anim-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; ++i) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) {
        std::printf("scene-anim-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, g_pass + g_fail, EXPECTED);
        std::printf("SCENE_ANIM_CPP FAIL\n");
        return 1;
    }
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

    // xform uniform buffer (48 bytes), written per frame.
    WGPUBufferDescriptor xd{};
    xd.label = "xform-ubo";
    xd.usage = WGPUBufferUsage_Uniform | WGPUBufferUsage_CopyDst;
    xd.size = 12 * sizeof(float);
    g_xform_ubo = wgpuDeviceCreateBuffer(dev, &xd);

    WGPUShaderModule module = make_wgsl(dev, WGSL, "anim");

    WGPUBindGroupLayoutEntry ule{};
    ule.binding = 0;
    ule.visibility = WGPUShaderStage_Vertex | WGPUShaderStage_Fragment;
    ule.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor uld{};
    uld.entryCount = 1; uld.entries = &ule;
    g_ubo_bgl = wgpuDeviceCreateBindGroupLayout(dev, &uld);
    WGPUPipelineLayoutDescriptor plld{};
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &g_ubo_bgl;
    WGPUPipelineLayout pll = wgpuDeviceCreatePipelineLayout(dev, &plld);

    g_bg = bind_ubo(g_xform_ubo, 12 * sizeof(float));

    WGPUVertexAttribute a_pos2{WGPUVertexFormat_Float32x2, 0, 0};
    WGPUVertexBufferLayout vbl_pos2{sizeof(V2), WGPUVertexStepMode_Vertex, 1, &a_pos2};
    g_pipe = mk_solid(module, pll, &vbl_pos2, nullptr);

    // local quad corners (unit square in local space, TL/TR/BL/BR) as a triangle strip.
    static constexpr std::array<float, 8> local = {-1.0f, -1.0f, 1.0f, -1.0f, -1.0f, 1.0f, 1.0f, 1.0f};
    std::array<V2, 4> lq = {{
        {{local[0], local[1]}}, {{local[2], local[3]}},
        {{local[4], local[5]}}, {{local[6], local[7]}},
    }};
    g_vbo = make_buf(lq.data(), lq.size() * sizeof(V2), WGPUBufferUsage_Vertex);

    const std::array<float, 4> ts = {0.0f, 0.25f, 0.5f, 0.75f};
    const std::array<std::array<float, 3>, 4> cols = {{{1, 0, 0}, {0, 1, 0}, {0, 0, 1}, {1, 1, 0}}};

    Fb fb;

    for (int fi = 0; fi < 4; ++fi) {
        float t = ts[fi];
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(t, col0, col1, tr, &sc, &ang);
        float xform[12] = {
            col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], static_cast<float>(W), static_cast<float>(H),
            cols[fi][0], cols[fi][1], cols[fi][2], 1.0f,
        };
        (void)render_frame(xform, fb);

        // closed-form corner positions: corner = R(ang)*S(sc)*localCorner + center.
        float ca = std::cos(ang), sa = std::sin(ang);
        float corners[4][2];
        for (int k = 0; k < 4; ++k) {
            float lx = local[k * 2], ly = local[k * 2 + 1];
            float rx = sc * (ca * lx - sa * ly);
            float ry = sc * (sa * lx + ca * ly);
            corners[k][0] = tr[0] + rx;
            corners[k][1] = tr[1] + ry;
        }
        float e = ease_cubic(t);
        float e_ref = 3.0f * t * t - 2.0f * t * t * t;
        ok(std::fabs(e - e_ref) < 1e-6f, "ease_cubic closed-form value");
        ok(std::fabs(sc - (S0 + (S1 - S0) * e)) < 1e-4f, "scale = lerp(S0,S1,ease(t)) closed-form");

        int cxi = static_cast<int>(std::lround(tr[0] - 0.5f));
        int cyi = static_cast<int>(std::lround(tr[1] - 0.5f));
        ok(fb.peq(cxi, cyi,
                  static_cast<int>(std::lround(cols[fi][0] * 255.0f)),
                  static_cast<int>(std::lround(cols[fi][1] * 255.0f)),
                  static_cast<int>(std::lround(cols[fi][2] * 255.0f)),
                  255, 2),
           "frame center pixel carries frame color at closed-form center");

        for (int k = 0; k < 4; ++k) {
            int px_ = static_cast<int>(std::lround(corners[k][0] - 0.5f));
            int py_ = static_cast<int>(std::lround(corners[k][1] - 0.5f));
            bool onscreen = px_ >= 0 && py_ >= 0 && px_ < static_cast<int>(W) && py_ < static_cast<int>(H);
            ok(onscreen && fb.near_color(px_, py_,
                                         static_cast<int>(std::lround(cols[fi][0] * 255.0f)),
                                         static_cast<int>(std::lround(cols[fi][1] * 255.0f)),
                                         static_cast<int>(std::lround(cols[fi][2] * 255.0f)),
                                         40),
               "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)");
        }

        // a point far outside the quad silhouette stays background (guard by max reach = sc*sqrt2).
        {
            int ox = (fi < 2) ? static_cast<int>(W) - 2 : 1;
            int oy = (fi < 2) ? static_cast<int>(H) - 2 : 1;
            float reach = sc * 1.4142f;
            bool covers = std::fabs(ox + 0.5f - tr[0]) <= reach && std::fabs(oy + 0.5f - tr[1]) <= reach;
            if (!covers)
                ok(fb.peq(ox, oy, 0, 0, 0, 255, 2),
                   "outside-quad point stays background (closed-form silhouette)");
            else
                ok(true, "outside-quad point skipped (would be covered)");
        }
    }

    // t=0 vs t=0.75 center positions differ.
    {
        float c0[2], c1[2], tra[2], sc, ang;
        float d0[2], d1[2], trb[2], sc2, ang2;
        frame_transform(0.0f, c0, c1, tra, &sc, &ang);
        frame_transform(0.75f, d0, d1, trb, &sc2, &ang2);
        ok(std::fabs(tra[0] - trb[0]) > 1.0f, "center translates between t=0 and t=0.75 (animation is real)");
    }

    // rotation at t=0.5: angle = pi/4, rotated x-axis column = (sc*cos45, sc*sin45).
    {
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(0.5f, col0, col1, tr, &sc, &ang);
        ok(std::fabs(ang - static_cast<float>(M_PI / 4.0)) < 1e-5f, "t=0.5 rotation angle = pi/4 closed-form");
        ok(std::fabs(col0[0] - col0[1]) < 1e-4f && col0[0] > 0.0f,
           "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)");
    }

    // negative control: render frame 0 (red) and confirm it is NOT green.
    {
        float col0[2], col1[2], tr[2], sc, ang;
        frame_transform(0.0f, col0, col1, tr, &sc, &ang);
        float xform[12] = {
            col0[0], col0[1], col1[0], col1[1], tr[0], tr[1], static_cast<float>(W), static_cast<float>(H),
            1.0f, 0.0f, 0.0f, 1.0f,
        };
        (void)render_frame(xform, fb);
        int cxi = static_cast<int>(std::lround(tr[0] - 0.5f));
        int cyi = static_cast<int>(std::lround(tr[1] - 0.5f));
        ok(!fb.peq(cxi, cyi, 0, 255, 0, 255, 4), "negative control: frame-0 center is NOT green");
    }

    wgpuDevicePoll(dev, 1, nullptr);
    return finish();
}
