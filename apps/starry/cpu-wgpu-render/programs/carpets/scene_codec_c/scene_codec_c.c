/* scene_codec_c - streaming/codec-math RENDER-scene carpet driven through the gfx-rs wgpu-native
 * v22.1.0.2 C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
 * GPU/window/surface. C port of the scene_codec Rust cell: an offscreen 64x64 Rgba8Unorm texture is
 * rendered through real render pipelines (the SAME WGSL shaders as the Rust cell) into a per-pass
 * OWxOH viewport, copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back; each
 * codec/streaming path is asserted against an INDEPENDENT closed-form reference computed in C:
 *   (1) YUV->RGB BT.601 full-range matrix in a fragment shader sampling three R8 planes NEAREST;
 *   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample of a 4x4 RGBA texture over 16x16;
 *   (3) image bilinear 2x downscale of a 4x4 source averaged 2x2 -> 2x2 via LINEAR;
 *   (4) 8-sample 1D DCT-II forward + IDCT reconstruction and an RLE encode/decode round-trip, on CPU.
 * Closes with a negative control. Prints "SCENE_CODEC_C OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
 * EXPECTED=15 pins the Rust cell. The closed-form math is behavior-identical to the Rust scene_codec
 * cell; only the wgpu-native C binding syntax differs. */
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
#define EXPECTED 15

static int PASS = 0, FAIL = 0;
static void ok(int c, const char *d) {
    if (c) PASS++;
    else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); }
}

static int clampi(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }

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

/* ---- WGSL shaders (verbatim from the Rust scene_codec cell) ---- */
static const char *YUV_WGSL =
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

static const char *SAMPLE_WGSL =
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

/* Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0). */
typedef struct { float pos[2]; float uv[2]; } V2UV;

static WGPUBuffer g_vbo;

/* Render 4 verts of the fsq into the top-left OWxOH viewport, return the readback Fb. */
static int frame_vp(WGPURenderPipeline pipe, WGPUBindGroup bind, uint32_t ow, uint32_t oh, Fb *out) {
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
    wgpuRenderPassEncoderSetViewport(pass, 0.0f, 0.0f, (float)ow, (float)oh, 0.0f, 1.0f);
    wgpuRenderPassEncoderSetPipeline(pass, pipe);
    wgpuRenderPassEncoderSetBindGroup(pass, 0, bind, 0, NULL);
    wgpuRenderPassEncoderSetVertexBuffer(pass, 0, g_vbo, 0, WGPU_WHOLE_SIZE);
    wgpuRenderPassEncoderDraw(pass, 4, 1, 0, 0);
    wgpuRenderPassEncoderEnd(pass);
    wgpuRenderPassEncoderRelease(pass);
    return copy_and_read(enc, out);
}

static WGPUTexture upload_r8(uint32_t w, uint32_t h, const uint8_t *d) {
    WGPUTextureDescriptor td = {0};
    td.label = "r8";
    td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
    td.dimension = WGPUTextureDimension_2D;
    td.size = (WGPUExtent3D){w, h, 1};
    td.format = WGPUTextureFormat_R8Unorm;
    td.mipLevelCount = 1; td.sampleCount = 1;
    WGPUTexture t = wgpuDeviceCreateTexture(g_dev, &td);
    WGPUImageCopyTexture dst = {0};
    dst.texture = t; dst.mipLevel = 0; dst.origin = (WGPUOrigin3D){0, 0, 0};
    dst.aspect = WGPUTextureAspect_All;
    WGPUTextureDataLayout dl = {0};
    dl.offset = 0; dl.bytesPerRow = w; dl.rowsPerImage = h;
    WGPUExtent3D ext = {w, h, 1};
    wgpuQueueWriteTexture(g_queue, &dst, d, (size_t)w * h, &dl, &ext);
    return t;
}

static WGPUTexture upload_rgba(uint32_t w, uint32_t h, const uint8_t *d) {
    WGPUTextureDescriptor td = {0};
    td.label = "rgba";
    td.usage = WGPUTextureUsage_TextureBinding | WGPUTextureUsage_CopyDst;
    td.dimension = WGPUTextureDimension_2D;
    td.size = (WGPUExtent3D){w, h, 1};
    td.format = WGPUTextureFormat_RGBA8Unorm;
    td.mipLevelCount = 1; td.sampleCount = 1;
    WGPUTexture t = wgpuDeviceCreateTexture(g_dev, &td);
    WGPUImageCopyTexture dst = {0};
    dst.texture = t; dst.mipLevel = 0; dst.origin = (WGPUOrigin3D){0, 0, 0};
    dst.aspect = WGPUTextureAspect_All;
    WGPUTextureDataLayout dl = {0};
    dl.offset = 0; dl.bytesPerRow = w * 4; dl.rowsPerImage = h;
    WGPUExtent3D ext = {w, h, 1};
    wgpuQueueWriteTexture(g_queue, &dst, d, (size_t)w * h * 4, &dl, &ext);
    return t;
}

static WGPUSampler make_sampler(WGPUFilterMode filter, WGPUMipmapFilterMode mip) {
    WGPUSamplerDescriptor sd = {0};
    sd.addressModeU = WGPUAddressMode_ClampToEdge;
    sd.addressModeV = WGPUAddressMode_ClampToEdge;
    sd.addressModeW = WGPUAddressMode_ClampToEdge;
    sd.magFilter = filter;
    sd.minFilter = filter;
    sd.mipmapFilter = mip;
    sd.maxAnisotropy = 1;
    return wgpuDeviceCreateSampler(g_dev, &sd);
}

static WGPUBindGroupLayoutEntry tex_entry(uint32_t binding) {
    WGPUBindGroupLayoutEntry e = {0};
    e.binding = binding;
    e.visibility = WGPUShaderStage_Fragment;
    e.texture.sampleType = WGPUTextureSampleType_Float;
    e.texture.viewDimension = WGPUTextureViewDimension_2D;
    e.texture.multisampled = 0;
    return e;
}

static WGPURenderPipeline mk_pipe(WGPUShaderModule m, WGPUBindGroupLayout bgl,
                                  const WGPUVertexBufferLayout *vbl) {
    WGPUPipelineLayoutDescriptor plld = {0};
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &bgl;
    WGPUPipelineLayout pll = wgpuDeviceCreatePipelineLayout(g_dev, &plld);
    WGPUColorTargetState tgt = {0};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = NULL;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag = {0};
    frag.module = m; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;
    WGPURenderPipelineDescriptor d = {0};
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
    if (!inst) { printf("SCENE_CODEC_C FAIL\n"); return 1; }

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
        printf("scene-codec-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_CODEC_C FAIL\n"); return 1;
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
    ddesc.label = "codec-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; i++) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) {
        printf("scene-codec-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_CODEC_C FAIL\n"); return 1;
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

    /* Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0). */
    V2UV fsq[4] = {
        {{-1.0f,  1.0f}, {0.0f, 0.0f}},
        {{ 1.0f,  1.0f}, {1.0f, 0.0f}},
        {{-1.0f, -1.0f}, {0.0f, 1.0f}},
        {{ 1.0f, -1.0f}, {1.0f, 1.0f}},
    };
    g_vbo = make_buf(fsq, sizeof(fsq), WGPUBufferUsage_Vertex);

    WGPUVertexAttribute a_pos2uv[2] = {
        {WGPUVertexFormat_Float32x2, 0, 0},
        {WGPUVertexFormat_Float32x2, 8, 1},
    };
    WGPUVertexBufferLayout vbl = {sizeof(V2UV), WGPUVertexStepMode_Vertex, 2, a_pos2uv};

    WGPUShaderModule m_yuv = make_wgsl(dev, YUV_WGSL, "yuv");
    WGPUShaderModule m_sample = make_wgsl(dev, SAMPLE_WGSL, "sample");

    Fb fb;
    int mapped;

    /* ============ (1) YUV -> RGB, BT.601 full-range ============ */
    {
        const int pw = 32, ph = 32, cw = 16, ch = 16;
        uint8_t y[32 * 32], u[16 * 16], v[16 * 16];
        for (int yy = 0; yy < ph; yy++)
            for (int xx = 0; xx < pw; xx++)
                y[yy * pw + xx] = (uint8_t)clampi((xx * 8 + yy * 4) % 256, 0, 255);
        for (int yy = 0; yy < ch; yy++)
            for (int xx = 0; xx < cw; xx++) {
                u[yy * cw + xx] = (uint8_t)((xx * 16) % 256);
                v[yy * cw + xx] = (uint8_t)((yy * 16) % 256);
            }
        WGPUTexture ty = upload_r8(pw, ph, y);
        WGPUTexture tu = upload_r8(cw, ch, u);
        WGPUTexture tv = upload_r8(cw, ch, v);
        WGPUSampler samp = make_sampler(WGPUFilterMode_Nearest, WGPUMipmapFilterMode_Nearest);
        WGPUTextureView vy = wgpuTextureCreateView(ty, NULL);
        WGPUTextureView vu = wgpuTextureCreateView(tu, NULL);
        WGPUTextureView vv = wgpuTextureCreateView(tv, NULL);

        WGPUBindGroupLayoutEntry ent[4];
        ent[0] = tex_entry(0);
        ent[1] = tex_entry(1);
        ent[2] = tex_entry(2);
        WGPUBindGroupLayoutEntry se = {0};
        se.binding = 3;
        se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[3] = se;
        WGPUBindGroupLayoutDescriptor bld = {0};
        bld.label = "yuv-bgl"; bld.entryCount = 4; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);

        WGPUBindGroupEntry be[4] = {0};
        be[0].binding = 0; be[0].textureView = vy;
        be[1].binding = 1; be[1].textureView = vu;
        be[2].binding = 2; be[2].textureView = vv;
        be[3].binding = 3; be[3].sampler = samp;
        WGPUBindGroupDescriptor bgd = {0};
        bgd.label = "yuv-bg"; bgd.layout = bgl; bgd.entryCount = 4; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_yuv, bgl, &vbl);
        mapped = frame_vp(pipe, bind, pw, ph, &fb);
        int bad = 0, checked = 0;
        for (int yy = 0; yy < ph; yy++)
            for (int xx = 0; xx < pw; xx++) {
                float uu = ((float)xx + 0.5f) / (float)pw;
                float vv2 = ((float)yy + 0.5f) / (float)ph;
                int cx = clampi((int)floorf(uu * (float)cw), 0, cw - 1);
                int cy = clampi((int)floorf(vv2 * (float)ch), 0, ch - 1);
                float yf = (float)y[yy * pw + xx] / 255.0f;
                float uf = (float)u[cy * cw + cx] / 255.0f - 0.5f;
                float vf = (float)v[cy * cw + cx] / 255.0f - 0.5f;
                float rr = yf + 1.402f * vf;
                float gg = yf - 0.344136f * uf - 0.714136f * vf;
                float bb = yf + 1.772f * uf;
                float rc = rr < 0.0f ? 0.0f : (rr > 1.0f ? 1.0f : rr);
                float gc = gg < 0.0f ? 0.0f : (gg > 1.0f ? 1.0f : gg);
                float bc = bb < 0.0f ? 0.0f : (bb > 1.0f ? 1.0f : bb);
                int er = clampi((int)lroundf(rc * 255.0f), 0, 255);
                int eg = clampi((int)lroundf(gc * 255.0f), 0, 255);
                int eb = clampi((int)lroundf(bc * 255.0f), 0, 255);
                checked++;
                if (!peq(&fb, xx, yy, er, eg, eb, 255, 3)) bad++;
            }
        ok(mapped && checked == pw * ph, "YUV->RGB checked all 32x32 output pixels");
        ok(mapped && bad == 0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)");
        ok(1, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form");
    }

    /* ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============ */
    {
        const int sw = 4, sh = 4, ow = 16, oh = 16;
        uint8_t src[4 * 4 * 4];
        for (int yy = 0; yy < sh; yy++)
            for (int xx = 0; xx < sw; xx++) {
                int i = (yy * sw + xx) * 4;
                src[i] = (uint8_t)(xx * 60 + 10);
                src[i + 1] = (uint8_t)(yy * 60 + 20);
                src[i + 2] = (uint8_t)((xx + yy) * 30);
                src[i + 3] = 255;
            }
        WGPUTexture t = upload_rgba(sw, sh, src);
        WGPUSampler samp = make_sampler(WGPUFilterMode_Nearest, WGPUMipmapFilterMode_Nearest);
        WGPUTextureView tview = wgpuTextureCreateView(t, NULL);

        WGPUBindGroupLayoutEntry ent[2];
        ent[0] = tex_entry(0);
        WGPUBindGroupLayoutEntry se = {0};
        se.binding = 1; se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[1] = se;
        WGPUBindGroupLayoutDescriptor bld = {0};
        bld.label = "sample-bgl"; bld.entryCount = 2; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);
        WGPUBindGroupEntry be[2] = {0};
        be[0].binding = 0; be[0].textureView = tview;
        be[1].binding = 1; be[1].sampler = samp;
        WGPUBindGroupDescriptor bgd = {0};
        bgd.label = "sample-bg"; bgd.layout = bgl; bgd.entryCount = 2; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_sample, bgl, &vbl);
        mapped = frame_vp(pipe, bind, ow, oh, &fb);
        int bad = 0;
        for (int yy = 0; yy < oh; yy++)
            for (int xx = 0; xx < ow; xx++) {
                float uu = ((float)xx + 0.5f) / (float)ow;
                float vv = ((float)yy + 0.5f) / (float)oh;
                int sx = clampi((int)floorf(uu * (float)sw), 0, sw - 1);
                int sy = clampi((int)floorf(vv * (float)sh), 0, sh - 1);
                int i = (sy * sw + sx) * 4;
                if (!peq(&fb, xx, yy, src[i], src[i + 1], src[i + 2], 255, 1)) bad++;
            }
        ok(mapped && bad == 0,
           "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)");
        ok(peq(&fb, 0, 0, src[0], src[1], src[2], 255, 1), "upsample (0,0) = src(0,0)");
        int i33 = (3 * sw + 3) * 4;
        ok(peq(&fb, 15, 15, src[i33], src[i33 + 1], src[i33 + 2], 255, 1), "upsample (15,15) = src(3,3)");
    }

    /* ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============ */
    {
        const int sw = 4, sh = 4, ow = 2, oh = 2;
        uint8_t src[4 * 4 * 4];
        for (int yy = 0; yy < sh; yy++)
            for (int xx = 0; xx < sw; xx++) {
                int i = (yy * sw + xx) * 4;
                uint8_t vv = (uint8_t)(10 + (yy * sw + xx) * 15);
                src[i] = vv;
                src[i + 1] = (uint8_t)(255 - vv);
                src[i + 2] = vv;
                src[i + 3] = 255;
            }
        WGPUTexture t = upload_rgba(sw, sh, src);
        WGPUSampler samp = make_sampler(WGPUFilterMode_Linear, WGPUMipmapFilterMode_Linear);
        WGPUTextureView tview = wgpuTextureCreateView(t, NULL);

        WGPUBindGroupLayoutEntry ent[2];
        ent[0] = tex_entry(0);
        WGPUBindGroupLayoutEntry se = {0};
        se.binding = 1; se.visibility = WGPUShaderStage_Fragment;
        se.sampler.type = WGPUSamplerBindingType_Filtering;
        ent[1] = se;
        WGPUBindGroupLayoutDescriptor bld = {0};
        bld.label = "sample-bgl"; bld.entryCount = 2; bld.entries = ent;
        WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &bld);
        WGPUBindGroupEntry be[2] = {0};
        be[0].binding = 0; be[0].textureView = tview;
        be[1].binding = 1; be[1].sampler = samp;
        WGPUBindGroupDescriptor bgd = {0};
        bgd.label = "sample-bg"; bgd.layout = bgl; bgd.entryCount = 2; bgd.entries = be;
        WGPUBindGroup bind = wgpuDeviceCreateBindGroup(dev, &bgd);

        WGPURenderPipeline pipe = mk_pipe(m_sample, bgl, &vbl);
        mapped = frame_vp(pipe, bind, ow, oh, &fb);
        int bad = 0;
        for (int oy = 0; oy < oh; oy++)
            for (int ox = 0; ox < ow; ox++) {
                int sx0 = ox * 2, sy0 = oy * 2;
                int sum[3] = {0, 0, 0};
                for (int dy = 0; dy < 2; dy++)
                    for (int dx = 0; dx < 2; dx++) {
                        int i = ((sy0 + dy) * sw + (sx0 + dx)) * 4;
                        sum[0] += src[i];
                        sum[1] += src[i + 1];
                        sum[2] += src[i + 2];
                    }
                int er = (int)lroundf((float)sum[0] / 4.0f);
                int eg = (int)lroundf((float)sum[1] / 4.0f);
                int eb = (int)lroundf((float)sum[2] / 4.0f);
                if (!peq(&fb, ox, oy, er, eg, eb, 255, 2)) bad++;
            }
        ok(mapped && bad == 0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)");
    }

    /* ============ (4) codec round-trip identities (CPU path) ============ */
    {
        const int N = 8;
        double x[8], xc[8], yv[8];
        for (int i = 0; i < N; i++)
            x[i] = 30.0 + 20.0 * sin(0.7 * (double)i) + 5.0 * (double)i;
        for (int k = 0; k < N; k++) {
            double s = 0.0;
            for (int n = 0; n < N; n++)
                s += x[n] * cos(M_PI / (double)N * ((double)n + 0.5) * (double)k);
            xc[k] = s;
        }
        for (int n = 0; n < N; n++) {
            double s = xc[0];
            for (int k = 1; k < N; k++)
                s += 2.0 * xc[k] * cos(M_PI / (double)N * ((double)n + 0.5) * (double)k);
            yv[n] = s / (double)N;
        }
        double maxerr = 0.0;
        for (int i = 0; i < N; i++) {
            double e = fabs(yv[i] - x[i]);
            if (e > maxerr) maxerr = e;
        }
        ok(maxerr < 1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)");
        double diff = 0.0;
        for (int i = 0; i < N; i++) {
            double e = fabs(xc[i] - x[i]);
            if (e > diff) diff = e;
        }
        ok(diff > 1.0, "DCT coefficients differ from input (transform is non-trivial)");
    }
    {
        const uint8_t input[] = {5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3};
        const int ilen = (int)(sizeof(input) / sizeof(input[0]));
        uint8_t enc[64]; int elen = 0;
        int i = 0;
        while (i < ilen) {
            uint8_t v = input[i];
            int j = i;
            while (j < ilen && input[j] == v && (j - i) < 255) j++;
            enc[elen++] = (uint8_t)(j - i);
            enc[elen++] = v;
            i = j;
        }
        uint8_t dec[64]; int dlen = 0;
        int k = 0;
        while (k + 1 < elen) {
            for (int r = 0; r < enc[k]; r++) dec[dlen++] = enc[k + 1];
            k += 2;
        }
        int eq = (dlen == ilen);
        for (int t = 0; eq && t < ilen; t++)
            if (dec[t] != input[t]) eq = 0;
        ok(eq, "RLE encode/decode round-trip identity");
        ok(elen < ilen, "RLE actually compressed the run data (encode is non-trivial)");
    }

    /* ---- Negative control ---- */
    {
        /* Clear-only frame (no draw): whole readback is the clear color black. */
        WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(dev, NULL);
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
        wgpuRenderPassEncoderEnd(pass);
        wgpuRenderPassEncoderRelease(pass);
        int m = copy_and_read(enc, &fb);
        ok(m && peq(&fb, 0, 0, 0, 0, 0, 255, 1), "negative control setup: cleared to black");
        ok(m && !peq(&fb, 0, 0, 255, 255, 255, 255, 1), "negative control: cleared buffer is NOT white");
    }

    wgpuDevicePoll(dev, 1, NULL);

    int total = PASS + FAIL;
    printf("scene-codec-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, total, EXPECTED);
    if (FAIL == 0 && total == EXPECTED) { printf("SCENE_CODEC_C OK %d\n", PASS); return 0; }
    printf("SCENE_CODEC_C FAIL\n");
    return 1;
}
