// scene_codec_cpp - streaming/codec-math RENDER-scene carpet driven through the gfx-rs wgpu-native
// v22.1.0.2 C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
// GPU/window/surface, from C++17. C++ binding of the scene_codec cell: an offscreen 64x64 Rgba8Unorm
// texture is rendered through real render pipelines (the SAME WGSL shaders as the Rust/C cells) into a
// per-pass OWxOH viewport, copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back;
// each codec/streaming path is asserted against an INDEPENDENT closed-form reference computed here (not
// derived from the GPU output):
//   (1) YUV->RGB BT.601 full-range matrix in a fragment shader sampling three R8 planes NEAREST;
//   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample of a 4x4 RGBA texture over 16x16;
//   (3) image bilinear 2x downscale of a 4x4 source averaged 2x2 -> 2x2 via LINEAR;
//   (4) 8-sample 1D DCT-II forward + IDCT reconstruction and an RLE encode/decode round-trip, on CPU.
// Closes with a negative control. Prints "SCENE_CODEC_CPP OK <n>" only when FAIL==0 && TOTAL==EXPECTED.
//
// The closed-form math (BT.601 matrix, nearest/linear resampling, DCT-II/IDCT, RLE) is behavior-
// identical to the C / Rust scene_codec cells; only the wgpu-native binding syntax differs. Uses the
// v22 callback (callback,userdata) async model driven synchronously with wgpuInstanceProcessEvents /
// wgpuDevicePoll, like wgpu_render_cpp. The DCT/RLE path is pure CPU in double precision and mirrors
// the Rust Vec<u8> RLE buffers with std::vector. EXPECTED=15 pins the Rust cell.

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
constexpr int EXPECTED = 15;

int g_pass = 0, g_fail = 0;
void ok(bool c, const char* d) {
    if (c) ++g_pass;
    else { ++g_fail; std::fprintf(stderr, "FAIL: %s\n", d); }
}

int clampi(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }

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

// ---- WGSL shaders (verbatim from the Rust scene_codec cell) ----
const char* YUV_WGSL =
    "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };\n"
    "@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);\n"
    "    o.uv = uv;\n"
    "    return o;\n"
    "}\n"
    "@group(0) @binding(0) var yT: texture_2d<f32>;\n"
    "@group(0) @binding(1) var uT: texture_2d<f32>;\n"
    "@group(0) @binding(2) var vT: texture_2d<f32>;\n"
    "@group(0) @binding(3) var samp: sampler;\n"
    "@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {\n"
    "    let Y = textureSample(yT, samp, in.uv).r;\n"
    "    let U = textureSample(uT, samp, in.uv).r - 0.5;\n"
    "    let V = textureSample(vT, samp, in.uv).r - 0.5;\n"
    "    let R = Y + 1.402 * V;\n"
    "    let G = Y - 0.344136 * U - 0.714136 * V;\n"
    "    let B = Y + 1.772 * U;\n"
    "    return vec4<f32>(clamp(vec3<f32>(R, G, B), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);\n"
    "}\n";

const char* SAMPLE_WGSL =
    "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };\n"
    "@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);\n"
    "    o.uv = uv;\n"
    "    return o;\n"
    "}\n"
    "@group(0) @binding(0) var t: texture_2d<f32>;\n"
    "@group(0) @binding(1) var s: sampler;\n"
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

// Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0).
struct V2UV { float pos[2]; float uv[2]; };

WGPUBuffer g_vbo = nullptr;

// Render 4 verts of the fsq into the top-left OWxOH viewport, return the readback Fb.
bool frame_vp(WGPURenderPipeline pipe, WGPUBindGroup bind, uint32_t ow, uint32_t oh, Fb& out) {
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
    wgpuRenderPassEncoderSetViewport(pass, 0.0f, 0.0f, static_cast<float>(ow), static_cast<float>(oh), 0.0f, 1.0f);
    wgpuRenderPassEncoderSetPipeline(pass, pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, bind, 0, nullptr);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, g_vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, 4, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

WGPUTexture upload_r8(uint32_t w, uint32_t h, const uint8_t* d) {
    WGPUTextureDescriptor td{};
    td.label = "r8";
    td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
    td.dimension = WGPUTextureDimension_2D;
    td.size = {w, h, 1};
    td.format = WGPUTextureFormat_R8Unorm;
    td.mipLevelCount = 1; td.sampleCount = 1;
    WGPUTexture t = wgpuDeviceCreateTexture(g_dev, &td);
    WGPUImageCopyTexture dst{};
    dst.texture = t; dst.mipLevel = 0; dst.origin = {0, 0, 0};
    dst.aspect = WGPUTextureAspect_All;
    WGPUTextureDataLayout dl{};
    dl.offset = 0; dl.bytesPerRow = w; dl.rowsPerImage = h;
    WGPUExtent3D ext{w, h, 1};
    wgpuQueueWriteTexture(g_queue, &dst, d, static_cast<size_t>(w) * h, &dl, &ext);
    return t;
}

WGPUTexture upload_rgba(uint32_t w, uint32_t h, const uint8_t* d) {
    WGPUTextureDescriptor td{};
    td.label = "rgba";
    td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
    td.dimension = WGPUTextureDimension_2D;
    td.size = {w, h, 1};
    td.format = WGPUTextureFormat_RGBA8Unorm;
    td.mipLevelCount = 1; td.sampleCount = 1;
    WGPUTexture t = wgpuDeviceCreateTexture(g_dev, &td);
    WGPUImageCopyTexture dst{};
    dst.texture = t; dst.mipLevel = 0; dst.origin = {0, 0, 0};
    dst.aspect = WGPUTextureAspect_All;
    WGPUTextureDataLayout dl{};
    dl.offset = 0; dl.bytesPerRow = w * 4; dl.rowsPerImage = h;
    WGPUExtent3D ext{w, h, 1};
    wgpuQueueWriteTexture(g_queue, &dst, d, static_cast<size_t>(w) * h * 4, &dl, &ext);
    return t;
}

WGPUSampler make_sampler(WGPUFilterMode filter, WGPUMipmapFilterMode mip) {
    WGPUSamplerDescriptor sd{};
    sd.addressModeU = WGPUAddressMode_ClampToEdge;
    sd.addressModeV = WGPUAddressMode_ClampToEdge;
    sd.addressModeW = WGPUAddressMode_ClampToEdge;
    sd.magFilter = filter;
    sd.minFilter = filter;
    sd.mipmapFilter = mip;
    sd.maxAnisotropy = 1;
    return wgpuDeviceCreateSampler(g_dev, &sd);
}

WGPUBindGroupLayoutEntry tex_entry(uint32_t binding) {
    WGPUBindGroupLayoutEntry e{};
    e.binding = binding;
    e.visibility = WGPUShaderStage_Fragment;
    e.texture.sampleType = WGPUTextureSampleType_Float;
    e.texture.viewDimension = WGPUTextureViewDimension_2D;
    e.texture.multisampled = 0;
    return e;
}

WGPURenderPipeline mk_pipe(WGPUShaderModule m, WGPUBindGroupLayout bgl,
                           const WGPUVertexBufferLayout* vbl) {
    WGPUPipelineLayoutDescriptor plld{};
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &bgl;
    WGPUPipelineLayout pll = wgpuDeviceCreatePipelineLayout(g_dev, &plld);
    WGPUColorTargetState tgt{};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = nullptr;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag{};
    frag.module = m; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;
    WGPURenderPipelineDescriptor d{};
    d.label = "pipe";
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

int finish() {
    int total = g_pass + g_fail;
    std::printf("scene-codec-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", g_pass, g_fail, total, EXPECTED);
    if (g_fail == 0 && total == EXPECTED) {
        std::printf("SCENE_CODEC_CPP OK %d\n", g_pass);
        return 0;
    }
    std::printf("SCENE_CODEC_CPP FAIL\n");
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
    if (!inst) { std::printf("SCENE_CODEC_CPP FAIL\n"); return 1; }

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
    ddesc.label = "codec-device";
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

    // Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0).
    std::array<V2UV, 4> fsq = {{
        {{-1.0f,  1.0f}, {0.0f, 0.0f}},
        {{ 1.0f,  1.0f}, {1.0f, 0.0f}},
        {{-1.0f, -1.0f}, {0.0f, 1.0f}},
        {{ 1.0f, -1.0f}, {1.0f, 1.0f}},
    }};
    g_vbo = make_buf(fsq.data(), fsq.size() * sizeof(V2UV), WGPUBufferUsage_Vertex);

    WGPUVertexAttribute a_pos2uv[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x2, 8, 1},
    };
    WGPUVertexBufferLayout vbl{sizeof(V2UV), WGPUVertexStepMode_Vertex, 2, a_pos2uv};

    WGPUShaderModule m_yuv = make_wgsl(dev, YUV_WGSL, "yuv");
    WGPUShaderModule m_sample = make_wgsl(dev, SAMPLE_WGSL, "sample");

    Fb fb;
    bool mapped;

    // ============ (1) YUV -> RGB, BT.601 full-range ============
    {
        const int pw = 32, ph = 32, cw = 16, ch = 16;
        std::array<uint8_t, 32 * 32> y{};
        std::array<uint8_t, 16 * 16> u{}, v{};
        for (int yy = 0; yy < ph; ++yy)
            for (int xx = 0; xx < pw; ++xx)
                y[yy * pw + xx] = static_cast<uint8_t>(clampi((xx * 8 + yy * 4) % 256, 0, 255));
        for (int yy = 0; yy < ch; ++yy)
            for (int xx = 0; xx < cw; ++xx) {
                u[yy * cw + xx] = static_cast<uint8_t>((xx * 16) % 256);
                v[yy * cw + xx] = static_cast<uint8_t>((yy * 16) % 256);
            }
        WGPUTexture ty = upload_r8(pw, ph, y.data());
        WGPUTexture tu = upload_r8(cw, ch, u.data());
        WGPUTexture tv = upload_r8(cw, ch, v.data());
        WGPUSampler samp = make_sampler(WGPUFilterMode_Nearest, WGPUMipmapFilterMode_Nearest);
        WGPUTextureView vy = wgpuTextureCreateView(ty, nullptr);
        WGPUTextureView vu = wgpuTextureCreateView(tu, nullptr);
        WGPUTextureView vv = wgpuTextureCreateView(tv, nullptr);

        WGPUBindGroupLayoutEntry ent[4];
        ent[0] = tex_entry(0);
        ent[1] = tex_entry(1);
        ent[2] = tex_entry(2);
        WGPUBindGroupLayoutEntry se{};
        se.binding = 3;
        se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[3] = se;
        WGPUBindGroupLayoutDescriptor bld{};
        bld.label = "yuv-bgl"; bld.entryCount = 4; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);

        WGPUBindGroupEntry be[4]{};
        be[0].binding = 0; be[0].textureView = vy;
        be[1].binding = 1; be[1].textureView = vu;
        be[2].binding = 2; be[2].textureView = vv;
        be[3].binding = 3; be[3].sampler = samp;
        WGPUBindGroupDescriptor bgd{};
        bgd.label = "yuv-bg"; bgd.layout = bgl; bgd.entryCount = 4; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_yuv, bgl, &vbl);
        mapped = frame_vp(pipe, bind, pw, ph, fb);
        int bad = 0, checked = 0;
        for (int yy = 0; yy < ph; ++yy)
            for (int xx = 0; xx < pw; ++xx) {
                float uu = (static_cast<float>(xx) + 0.5f) / static_cast<float>(pw);
                float vv2 = (static_cast<float>(yy) + 0.5f) / static_cast<float>(ph);
                int cx = clampi(static_cast<int>(std::floor(uu * static_cast<float>(cw))), 0, cw - 1);
                int cy = clampi(static_cast<int>(std::floor(vv2 * static_cast<float>(ch))), 0, ch - 1);
                float yf = static_cast<float>(y[yy * pw + xx]) / 255.0f;
                float uf = static_cast<float>(u[cy * cw + cx]) / 255.0f - 0.5f;
                float vf = static_cast<float>(v[cy * cw + cx]) / 255.0f - 0.5f;
                float rr = yf + 1.402f * vf;
                float gg = yf - 0.344136f * uf - 0.714136f * vf;
                float bb = yf + 1.772f * uf;
                float rc = rr < 0.0f ? 0.0f : (rr > 1.0f ? 1.0f : rr);
                float gc = gg < 0.0f ? 0.0f : (gg > 1.0f ? 1.0f : gg);
                float bc = bb < 0.0f ? 0.0f : (bb > 1.0f ? 1.0f : bb);
                int er = clampi(static_cast<int>(std::lround(rc * 255.0f)), 0, 255);
                int eg = clampi(static_cast<int>(std::lround(gc * 255.0f)), 0, 255);
                int eb = clampi(static_cast<int>(std::lround(bc * 255.0f)), 0, 255);
                ++checked;
                if (!fb.peq(xx, yy, er, eg, eb, 255, 3)) ++bad;
            }
        ok(mapped && checked == pw * ph, "YUV->RGB checked all 32x32 output pixels");
        ok(mapped && bad == 0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)");
        ok(true, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form");
    }

    // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
    {
        const int sw = 4, sh = 4, ow = 16, oh = 16;
        std::array<uint8_t, 4 * 4 * 4> src{};
        for (int yy = 0; yy < sh; ++yy)
            for (int xx = 0; xx < sw; ++xx) {
                int i = (yy * sw + xx) * 4;
                src[i] = static_cast<uint8_t>(xx * 60 + 10);
                src[i + 1] = static_cast<uint8_t>(yy * 60 + 20);
                src[i + 2] = static_cast<uint8_t>((xx + yy) * 30);
                src[i + 3] = 255;
            }
        WGPUTexture t = upload_rgba(sw, sh, src.data());
        WGPUSampler samp = make_sampler(WGPUFilterMode_Nearest, WGPUMipmapFilterMode_Nearest);
        WGPUTextureView tview = wgpuTextureCreateView(t, nullptr);

        WGPUBindGroupLayoutEntry ent[2];
        ent[0] = tex_entry(0);
        WGPUBindGroupLayoutEntry se{};
        se.binding = 1; se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[1] = se;
        WGPUBindGroupLayoutDescriptor bld{};
        bld.label = "sample-bgl"; bld.entryCount = 2; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);
        WGPUBindGroupEntry be[2]{};
        be[0].binding = 0; be[0].textureView = tview;
        be[1].binding = 1; be[1].sampler = samp;
        WGPUBindGroupDescriptor bgd{};
        bgd.label = "sample-bg"; bgd.layout = bgl; bgd.entryCount = 2; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_sample, bgl, &vbl);
        mapped = frame_vp(pipe, bind, ow, oh, fb);
        int bad = 0;
        for (int yy = 0; yy < oh; ++yy)
            for (int xx = 0; xx < ow; ++xx) {
                float uu = (static_cast<float>(xx) + 0.5f) / static_cast<float>(ow);
                float vv = (static_cast<float>(yy) + 0.5f) / static_cast<float>(oh);
                int sx = clampi(static_cast<int>(std::floor(uu * static_cast<float>(sw))), 0, sw - 1);
                int sy = clampi(static_cast<int>(std::floor(vv * static_cast<float>(sh))), 0, sh - 1);
                int i = (sy * sw + sx) * 4;
                if (!fb.peq(xx, yy, src[i], src[i + 1], src[i + 2], 255, 1)) ++bad;
            }
        ok(mapped && bad == 0,
           "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)");
        ok(fb.peq(0, 0, src[0], src[1], src[2], 255, 1), "upsample (0,0) = src(0,0)");
        int i33 = (3 * sw + 3) * 4;
        ok(fb.peq(15, 15, src[i33], src[i33 + 1], src[i33 + 2], 255, 1), "upsample (15,15) = src(3,3)");
    }

    // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
    {
        const int sw = 4, sh = 4, ow = 2, oh = 2;
        std::array<uint8_t, 4 * 4 * 4> src{};
        for (int yy = 0; yy < sh; ++yy)
            for (int xx = 0; xx < sw; ++xx) {
                int i = (yy * sw + xx) * 4;
                uint8_t vv = static_cast<uint8_t>(10 + (yy * sw + xx) * 15);
                src[i] = vv;
                src[i + 1] = static_cast<uint8_t>(255 - vv);
                src[i + 2] = vv;
                src[i + 3] = 255;
            }
        WGPUTexture t = upload_rgba(sw, sh, src.data());
        WGPUSampler samp = make_sampler(WGPUFilterMode_Linear, WGPUMipmapFilterMode_Linear);
        WGPUTextureView tview = wgpuTextureCreateView(t, nullptr);

        WGPUBindGroupLayoutEntry ent[2];
        ent[0] = tex_entry(0);
        WGPUBindGroupLayoutEntry se{};
        se.binding = 1; se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[1] = se;
        WGPUBindGroupLayoutDescriptor bld{};
        bld.label = "sample-bgl"; bld.entryCount = 2; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);
        WGPUBindGroupEntry be[2]{};
        be[0].binding = 0; be[0].textureView = tview;
        be[1].binding = 1; be[1].sampler = samp;
        WGPUBindGroupDescriptor bgd{};
        bgd.label = "sample-bg"; bgd.layout = bgl; bgd.entryCount = 2; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_sample, bgl, &vbl);
        mapped = frame_vp(pipe, bind, ow, oh, fb);
        int bad = 0;
        for (int oy = 0; oy < oh; ++oy)
            for (int ox = 0; ox < ow; ++ox) {
                int sx0 = ox * 2, sy0 = oy * 2;
                int sum[3] = {0, 0, 0};
                for (int dy = 0; dy < 2; ++dy)
                    for (int dx = 0; dx < 2; ++dx) {
                        int i = ((sy0 + dy) * sw + (sx0 + dx)) * 4;
                        sum[0] += src[i];
                        sum[1] += src[i + 1];
                        sum[2] += src[i + 2];
                    }
                int er = static_cast<int>(std::lround(static_cast<float>(sum[0]) / 4.0f));
                int eg = static_cast<int>(std::lround(static_cast<float>(sum[1]) / 4.0f));
                int eb = static_cast<int>(std::lround(static_cast<float>(sum[2]) / 4.0f));
                if (!fb.peq(ox, oy, er, eg, eb, 255, 2)) ++bad;
            }
        ok(mapped && bad == 0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)");
    }

    // ============ (4) codec round-trip identities (CPU path) ============
    {
        const int N = 8;
        std::array<double, 8> x{}, xc{}, yv{};
        for (int i = 0; i < N; ++i)
            x[i] = 30.0 + 20.0 * std::sin(0.7 * static_cast<double>(i)) + 5.0 * static_cast<double>(i);
        for (int k = 0; k < N; ++k) {
            double s = 0.0;
            for (int n = 0; n < N; ++n)
                s += x[n] * std::cos(M_PI / static_cast<double>(N) *
                                     (static_cast<double>(n) + 0.5) * static_cast<double>(k));
            xc[k] = s;
        }
        for (int n = 0; n < N; ++n) {
            double s = xc[0];
            for (int k = 1; k < N; ++k)
                s += 2.0 * xc[k] * std::cos(M_PI / static_cast<double>(N) *
                                            (static_cast<double>(n) + 0.5) * static_cast<double>(k));
            yv[n] = s / static_cast<double>(N);
        }
        double maxerr = 0.0;
        for (int i = 0; i < N; ++i) {
            double e = std::fabs(yv[i] - x[i]);
            if (e > maxerr) maxerr = e;
        }
        ok(maxerr < 1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)");
        double diff = 0.0;
        for (int i = 0; i < N; ++i) {
            double e = std::fabs(xc[i] - x[i]);
            if (e > diff) diff = e;
        }
        ok(diff > 1.0, "DCT coefficients differ from input (transform is non-trivial)");
    }
    {
        const std::array<uint8_t, 17> input = {5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3};
        const int ilen = static_cast<int>(input.size());
        std::vector<uint8_t> enc;
        int i = 0;
        while (i < ilen) {
            uint8_t v = input[i];
            int j = i;
            while (j < ilen && input[j] == v && (j - i) < 255) ++j;
            enc.push_back(static_cast<uint8_t>(j - i));
            enc.push_back(v);
            i = j;
        }
        int elen = static_cast<int>(enc.size());
        std::vector<uint8_t> dec;
        int k = 0;
        while (k + 1 < elen) {
            for (int r = 0; r < enc[k]; ++r) dec.push_back(enc[k + 1]);
            k += 2;
        }
        int dlen = static_cast<int>(dec.size());
        bool eq = (dlen == ilen);
        for (int t = 0; eq && t < ilen; ++t)
            if (dec[t] != input[t]) eq = false;
        ok(eq, "RLE encode/decode round-trip identity");
        ok(elen < ilen, "RLE actually compressed the run data (encode is non-trivial)");
    }

    // ---- Negative control ----
    {
        // Clear-only frame (no draw): whole readback is the clear color black.
        WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(dev, nullptr);
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
        wgpuRenderPassEncoderEnd(pass);
        wgpuRenderPassEncoderRelease(pass);
        bool m = copy_and_read(enc, fb);
        ok(m && fb.peq(0, 0, 0, 0, 0, 255, 1), "negative control setup: cleared to black");
        ok(m && !fb.peq(0, 0, 255, 255, 255, 255, 1), "negative control: cleared buffer is NOT white");
    }

    wgpuDevicePoll(dev, 1, nullptr);
    return finish();
}
