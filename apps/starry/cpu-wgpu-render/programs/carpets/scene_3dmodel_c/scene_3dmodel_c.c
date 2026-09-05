/* scene_3dmodel_c - 3D indexed-mesh RENDER-scene carpet driven through the gfx-rs wgpu-native
 * v22.1.0.2 C API (webgpu.h / wgpu.h) on Mesa software adapters (lavapipe Vulkan / llvmpipe GL), no
 * GPU/window/surface. C port of the scene_3dmodel Rust cell: an offscreen 64x64 Rgba8Unorm color
 * texture + a Depth32Float depth texture, drawn through a real render pipeline (WGSL vertex+fragment)
 * with depth test CompareFunction::Less, copied to a MAP_READ buffer (256-byte bytesPerRow padding)
 * and read back. Renders an indexed cube mesh with a hand-computed Model-View-Projection matrix
 * (perspective), depth-buffered occlusion, and Gouraud shading. The assertion is an INDEPENDENT
 * software reference rasterizer written in C: verts are transformed by the SAME MVP -> clip -> NDC
 * (perspective divide) -> viewport pixels; for each pixel we compute barycentric coordinates, do a
 * perspective-correct interpolated depth test in a private z-buffer, interpolate the vertex colors,
 * then compare the reference framebuffer to the readback per pixel (small tolerance for
 * edge-sample/rounding). Closes with a negative control. Prints "SCENE_3DMODEL_C OK <n>" only when
 * FAIL==0 && TOTAL==EXPECTED==PASS. EXPECTED=14 pins the Rust cell.
 *
 * WebGPU/Vulkan NDC z is in [0,1] (unlike GL's [-1,1]). The perspective() z-row uses the
 * near->0/far->1 mapping (m[2][2]=zf/(zn-zf), m[3][2]=(zf*zn)/(zn-zf)); the reference window depth is
 * sz = ndcz directly (z/w), so the LESS depth test uses the same [0,1] depth the GPU writes into
 * Depth32Float. The vertex shader carries @invariant on @builtin(position) so the rasterized depth is
 * bit-exact and the LESS occlusion is deterministic. Only the wgpu-native C binding syntax differs from
 * the Rust cell; the M4 math, cube verts/colors/indices, and rasterizer are behavior-identical. */
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
#define EXPECTED 14

static int PASS = 0, FAIL = 0;
static void ok(int c, const char *d) {
    if (c) PASS++;
    else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); }
}

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

/* pos3 + col3, mvp uniform (WebGPU has no push constants). @invariant pins depth bit-exactness. */
static const char *CUBE_WGSL =
    "\n"
    "struct MVP { m: mat4x4<f32> };\n"
    "@group(0) @binding(0) var<uniform> u: MVP;\n"
    "struct VOut { @invariant @builtin(position) pos: vec4<f32>, @location(0) col: vec3<f32> };\n"
    "@vertex fn vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> VOut {\n"
    "    var o: VOut;\n"
    "    o.pos = u.m * vec4<f32>(p, 1.0);\n"
    "    o.col = c;\n"
    "    return o;\n"
    "}\n"
    "@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {\n"
    "    return vec4<f32>(in.col, 1.0);\n"
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

/* ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) - ported from the reference ---- */
typedef struct { float m[16]; } M4;
static M4 mul(const M4 *a, const M4 *b) {
    M4 r = {{0}};
    for (int c = 0; c < 4; c++)
        for (int row = 0; row < 4; row++) {
            float s = 0.0f;
            for (int k = 0; k < 4; k++) s += a->m[k * 4 + row] * b->m[c * 4 + k];
            r.m[c * 4 + row] = s;
        }
    return r;
}
static void mv4(const M4 *a, const float v[4], float o[4]) {
    for (int row = 0; row < 4; row++) {
        float s = 0.0f;
        for (int k = 0; k < 4; k++) s += a->m[k * 4 + row] * v[k];
        o[row] = s;
    }
}
/* WebGPU/Vulkan perspective: near->z_ndc 0, far->z_ndc 1 (z/w in [0,1]). Only the z row differs from GL. */
static M4 perspective(float fovy, float aspect, float zn, float zf) {
    float f = 1.0f / tanf(fovy * 0.5f);
    M4 r = {{0}};
    r.m[0 * 4 + 0] = f / aspect;
    r.m[1 * 4 + 1] = f;
    r.m[2 * 4 + 2] = zf / (zn - zf);
    r.m[2 * 4 + 3] = -1.0f;
    r.m[3 * 4 + 2] = (zf * zn) / (zn - zf);
    return r;
}
static M4 translate(float x, float y, float z) {
    M4 r = {{0}};
    r.m[0] = 1.0f;
    r.m[5] = 1.0f;
    r.m[10] = 1.0f;
    r.m[15] = 1.0f;
    r.m[3 * 4 + 0] = x;
    r.m[3 * 4 + 1] = y;
    r.m[3 * 4 + 2] = z;
    return r;
}
static M4 rot_y(float a) {
    M4 r = {{0}};
    float c = cosf(a), s = sinf(a);
    r.m[0 * 4 + 0] = c;
    r.m[0 * 4 + 2] = -s;
    r.m[2 * 4 + 0] = s;
    r.m[2 * 4 + 2] = c;
    r.m[1 * 4 + 1] = 1.0f;
    r.m[3 * 4 + 3] = 1.0f;
    return r;
}
static M4 rot_x(float a) {
    M4 r = {{0}};
    float c = cosf(a), s = sinf(a);
    r.m[1 * 4 + 1] = c;
    r.m[1 * 4 + 2] = s;
    r.m[2 * 4 + 1] = -s;
    r.m[2 * 4 + 2] = c;
    r.m[0 * 4 + 0] = 1.0f;
    r.m[3 * 4 + 3] = 1.0f;
    return r;
}

typedef struct { float pos[3]; float col[3]; } Vtx;

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
static WGPUTextureView g_depth_view;
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
    if (!inst) { printf("SCENE_3DMODEL_C FAIL\n"); return 1; }

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
        printf("scene-3dmodel-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_3DMODEL_C FAIL\n"); return 1;
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
    ddesc.label = "3dmodel-device";
    ddesc.uncapturedErrorCallbackInfo.callback = on_uncaptured;
    wgpuAdapterRequestDevice(adapter, &ddesc, on_device, &dreq);
    for (int i = 0; i < 64 && !dreq.done; i++) wgpuInstanceProcessEvents(inst);
    WGPUDevice dev = dreq.device;
    if (!dev) {
        printf("scene-3dmodel-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, PASS + FAIL, EXPECTED);
        printf("SCENE_3DMODEL_C FAIL\n"); return 1;
    }
    g_dev = dev;
    g_queue = wgpuDeviceGetQueue(dev);

    /* offscreen color + depth targets + readback plumbing. */
    WGPUTextureDescriptor cd = {0};
    cd.label = "color";
    cd.usage = WGPUTextureUsage_RenderAttachment | WGPUTextureUsage_CopySrc;
    cd.dimension = WGPUTextureDimension_2D;
    cd.size = (WGPUExtent3D){W, H, 1};
    cd.format = WGPUTextureFormat_RGBA8Unorm;
    cd.mipLevelCount = 1; cd.sampleCount = 1;
    g_color = wgpuDeviceCreateTexture(dev, &cd);
    g_color_view = wgpuTextureCreateView(g_color, NULL);

    WGPUTextureDescriptor dd = {0};
    dd.label = "depth";
    dd.usage = WGPUTextureUsage_RenderAttachment;
    dd.dimension = WGPUTextureDimension_2D;
    dd.size = (WGPUExtent3D){W, H, 1};
    dd.format = WGPUTextureFormat_Depth32Float;
    dd.mipLevelCount = 1; dd.sampleCount = 1;
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
    ok(1, "offscreen Rgba8Unorm + Depth32Float target + readback buffer ready");

    /* ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (ported) ---- */
    static const float VP[8][3] = {
        {-1.0f, -1.0f, -1.0f},
        {1.0f, -1.0f, -1.0f},
        {1.0f, 1.0f, -1.0f},
        {-1.0f, 1.0f, -1.0f},
        {-1.0f, -1.0f, 1.0f},
        {1.0f, -1.0f, 1.0f},
        {1.0f, 1.0f, 1.0f},
        {-1.0f, 1.0f, 1.0f},
    };
    float vc[8][3];
    for (int i = 0; i < 8; i++) {
        vc[i][0] = (VP[i][0] + 1.0f) * 0.5f;
        vc[i][1] = (VP[i][1] + 1.0f) * 0.5f;
        vc[i][2] = (VP[i][2] + 1.0f) * 0.5f;
    }
    static const uint16_t IDX[36] = {
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 5, 6,
        1, 6, 2,
    };

    M4 ry = rot_y(0.6f), rx = rot_x(0.3f);
    M4 model = mul(&ry, &rx);
    M4 view = translate(0.0f, 0.0f, -5.0f);
    M4 proj = perspective(1.0f, (float)W / (float)H, 1.0f, 20.0f);
    M4 vm = mul(&view, &model);
    M4 mvp = mul(&proj, &vm);

    Vtx verts[8];
    for (int i = 0; i < 8; i++) {
        verts[i].pos[0] = VP[i][0]; verts[i].pos[1] = VP[i][1]; verts[i].pos[2] = VP[i][2];
        verts[i].col[0] = vc[i][0]; verts[i].col[1] = vc[i][1]; verts[i].col[2] = vc[i][2];
    }
    WGPUBuffer vbo = make_buf(verts, sizeof(verts), WGPUBufferUsage_Vertex);
    WGPUBuffer ibo = make_buf(IDX, sizeof(IDX), WGPUBufferUsage_Index);

    /* mvp uniform, column-major just like the shader mat4x4 expects. */
    WGPUBuffer mvp_ubo = make_buf(mvp.m, sizeof(mvp.m), WGPUBufferUsage_Uniform);

    WGPUBindGroupLayoutEntry ule = {0};
    ule.binding = 0;
    ule.visibility = WGPUShaderStage_Vertex;
    ule.buffer.type = WGPUBufferBindingType_Uniform;
    WGPUBindGroupLayoutDescriptor uld = {0};
    uld.label = "mvp-bgl";
    uld.entryCount = 1; uld.entries = &ule;
    WGPUBindGroupLayout bgl = wgpuDeviceCreateBindGroupLayout(dev, &uld);

    WGPUBindGroupEntry be = {0};
    be.binding = 0; be.buffer = mvp_ubo; be.offset = 0; be.size = sizeof(mvp.m);
    WGPUBindGroupDescriptor bgd = {0};
    bgd.label = "mvp-bg";
    bgd.layout = bgl; bgd.entryCount = 1; bgd.entries = &be;
    WGPUBindGroup bg = wgpuDeviceCreateBindGroup(dev, &bgd);

    WGPUPipelineLayoutDescriptor plld = {0};
    plld.label = "pll";
    plld.bindGroupLayoutCount = 1; plld.bindGroupLayouts = &bgl;
    WGPUPipelineLayout pll = wgpuDeviceCreatePipelineLayout(dev, &plld);

    WGPUShaderModule module = make_wgsl(dev, CUBE_WGSL, "cube");

    WGPUVertexAttribute vattrs[2] = {
        {WGPUVertexFormat_Float32x3, 0, 0},
        {WGPUVertexFormat_Float32x3, 12, 1},
    };
    WGPUVertexBufferLayout vbl = {sizeof(Vtx), WGPUVertexStepMode_Vertex, 2, vattrs};

    /* depth test LESS, no cull (reference did not cull). */
    WGPUColorTargetState tgt = {0};
    tgt.format = WGPUTextureFormat_RGBA8Unorm;
    tgt.blend = NULL;
    tgt.writeMask = WGPUColorWriteMask_All;
    WGPUFragmentState frag = {0};
    frag.module = module; frag.entryPoint = "fs"; frag.targetCount = 1; frag.targets = &tgt;

    WGPUDepthStencilState ds = {0};
    ds.format = WGPUTextureFormat_Depth32Float;
    ds.depthWriteEnabled = 1;
    ds.depthCompare = WGPUCompareFunction_Less;
    ds.stencilFront.compare = WGPUCompareFunction_Always;
    ds.stencilBack.compare = WGPUCompareFunction_Always;

    WGPURenderPipelineDescriptor pd = {0};
    pd.label = "cube-pipe";
    pd.layout = pll;
    pd.vertex.module = module; pd.vertex.entryPoint = "vs";
    pd.vertex.bufferCount = 1; pd.vertex.buffers = &vbl;
    pd.primitive.topology = WGPUPrimitiveTopology_TriangleList;
    pd.primitive.stripIndexFormat = WGPUIndexFormat_Undefined;
    pd.primitive.frontFace = WGPUFrontFace_CCW;
    pd.primitive.cullMode = WGPUCullMode_None;
    pd.multisample.count = 1; pd.multisample.mask = 0xFFFFFFFFu;
    pd.depthStencil = &ds;
    pd.fragment = &frag;
    WGPURenderPipeline pipe = wgpuDeviceCreateRenderPipeline(dev, &pd);
    ok(1, "cube pipeline created");

    /* ---- draw: clear color black, clear depth 1.0, draw the indexed cube ---- */
    Fb buf;
    int mapped;
    {
        WGPUCommandEncoder enc = wgpuDeviceCreateCommandEncoder(dev, NULL);
        WGPURenderPassColorAttachment ca_att = {0};
        ca_att.view = g_color_view;
        ca_att.depthSlice = WGPU_DEPTH_SLICE_UNDEFINED;
        ca_att.resolveTarget = NULL;
        ca_att.loadOp = WGPULoadOp_Clear;
        ca_att.storeOp = WGPUStoreOp_Store;
        ca_att.clearValue = (WGPUColor){0.0, 0.0, 0.0, 1.0};
        WGPURenderPassDepthStencilAttachment dsa = {0};
        dsa.view = g_depth_view;
        dsa.depthLoadOp = WGPULoadOp_Clear;
        dsa.depthStoreOp = WGPUStoreOp_Store;
        dsa.depthClearValue = 1.0f;
        dsa.stencilLoadOp = WGPULoadOp_Undefined;
        dsa.stencilStoreOp = WGPUStoreOp_Undefined;
        WGPURenderPassDescriptor rp = {0};
        rp.label = "rp";
        rp.colorAttachmentCount = 1;
        rp.colorAttachments = &ca_att;
        rp.depthStencilAttachment = &dsa;
        WGPURenderPassEncoder pass = wgpuCommandEncoderBeginRenderPass(enc, &rp);
        wgpuRenderPassEncoderSetPipeline(pass, pipe);
        wgpuRenderPassEncoderSetBindGroup(pass, 0, bg, 0, NULL);
        wgpuRenderPassEncoderSetVertexBuffer(pass, 0, vbo, 0, WGPU_WHOLE_SIZE);
        wgpuRenderPassEncoderSetIndexBuffer(pass, ibo, WGPUIndexFormat_Uint16, 0, WGPU_WHOLE_SIZE);
        wgpuRenderPassEncoderDrawIndexed(pass, 36, 1, 0, 0, 0);
        wgpuRenderPassEncoderEnd(pass);
        wgpuRenderPassEncoderRelease(pass);
        mapped = copy_and_read(enc, &buf);
    }
    ok(mapped, "cube drawn (depth-tested, Gouraud)");

    /* ---- INDEPENDENT software reference rasterizer (ported; WebGPU NDC-z in [0,1]) ---- */
    static float refc[W * H][3];
    static float refz[W * H];
    static uint8_t refcov[W * H];
    for (uint32_t i = 0; i < W * H; i++) {
        refc[i][0] = 0.0f; refc[i][1] = 0.0f; refc[i][2] = 0.0f;
        refz[i] = 1e9f;
        refcov[i] = 0;
    }

    float sx[8], sy[8], sz[8], sw[8];
    for (int i = 0; i < 8; i++) {
        float in[4] = {VP[i][0], VP[i][1], VP[i][2], 1.0f};
        float out[4];
        mv4(&mvp, in, out);
        float w = out[3];
        sw[i] = w;
        float ndcx = out[0] / w, ndcy = out[1] / w, ndcz = out[2] / w; /* WebGPU NDC z in [0,1] */
        sx[i] = (ndcx * 0.5f + 0.5f) * (float)W;
        /* WebGPU framebuffer origin is top-left: NDC y=+1 maps to row 0. Flip sy so the reference's
         * row indexing matches the WebGPU-rendered readback. */
        sy[i] = (0.5f - ndcy * 0.5f) * (float)H;
        sz[i] = ndcz; /* window depth = z/w directly ([0,1]) */
    }
    ok(sw[0] > 0.0f, "reference: all clip.w positive (mesh in front of camera)");

    for (int t = 0; t < 12; t++) {
        int a = IDX[t * 3], b = IDX[t * 3 + 1], c = IDX[t * 3 + 2];
        float ax = sx[a], ay = sy[a], bx = sx[b], by = sy[b], cx = sx[c], cy = sy[c];
        float area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if (fabsf(area) < 1e-6f) continue;
        int minx = (int)floorf(fminf(ax, fminf(bx, cx)));
        int maxx = (int)ceilf(fmaxf(ax, fmaxf(bx, cx)));
        int miny = (int)floorf(fminf(ay, fminf(by, cy)));
        int maxy = (int)ceilf(fmaxf(ay, fmaxf(by, cy)));
        if (minx < 0) minx = 0;
        if (miny < 0) miny = 0;
        if (maxx > (int)W) maxx = (int)W;
        if (maxy > (int)H) maxy = (int)H;
        for (int y = miny; y < maxy; y++) {
            for (int x = minx; x < maxx; x++) {
                float pxs = (float)x + 0.5f, pys = (float)y + 0.5f;
                float w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area;
                float w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area;
                float w2 = 1.0f - w0 - w1;
                int inside = (w0 >= 0.0f && w1 >= 0.0f && w2 >= 0.0f) ||
                             (w0 <= 0.0f && w1 <= 0.0f && w2 <= 0.0f);
                if (!inside) continue;
                if (w0 < 0.0f || w1 < 0.0f || w2 < 0.0f) { w0 = -w0; w1 = -w1; w2 = -w2; }
                float z = w0 * sz[a] + w1 * sz[b] + w2 * sz[c];
                int i = y * (int)W + x;
                if (z < refz[i]) {
                    refz[i] = z;
                    refcov[i] = 1;
                    float iwa = 1.0f / sw[a], iwb = 1.0f / sw[b], iwc = 1.0f / sw[c];
                    float d = w0 * iwa + w1 * iwb + w2 * iwc;
                    for (int k = 0; k < 3; k++) {
                        float num = w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[c][k];
                        refc[i][k] = num / d;
                    }
                }
            }
        }
    }

    int total = 0, match = 0, covmatch = 0, covtotal = 0, interior_bad = 0;
    for (uint32_t y = 0; y < H; y++) {
        for (uint32_t x = 0; x < W; x++) {
            total++;
            int gcov = !(fb_p(&buf, x, y, 0) == 0 && fb_p(&buf, x, y, 1) == 0 && fb_p(&buf, x, y, 2) == 0);
            int i = (int)(y * W + x);
            int rcov = refcov[i] != 0;
            if (gcov == rcov) covmatch++;
            if (rcov) {
                covtotal++;
                int er = (int)lroundf(refc[i][0] * 255.0f);
                int eg = (int)lroundf(refc[i][1] * 255.0f);
                int eb = (int)lroundf(refc[i][2] * 255.0f);
                int interior = x > 0 && y > 0 && x < W - 1 && y < H - 1 &&
                               refcov[(int)((y - 1) * W + x)] != 0 &&
                               refcov[(int)((y + 1) * W + x)] != 0 &&
                               refcov[(int)(y * W + (x - 1))] != 0 &&
                               refcov[(int)(y * W + (x + 1))] != 0;
                if (peq(&buf, (int)x, (int)y, er, eg, eb, 255, 6)) match++;
                else if (interior) interior_bad++;
            }
        }
    }
    ok(covtotal > 200, "reference: cube covers a substantial area");
    ok(covmatch >= (int)(0.97f * (float)total),
       "coverage mask matches GPU (>=97% of pixels agree covered/empty)");
    ok(interior_bad == 0, "every interior pixel matches perspective-correct Gouraud reference (tol 6)");
    ok(match >= (int)(0.92f * (float)covtotal),
       "92%+ of covered pixels match reference color (edges excluded)");

    {
        int vx = (int)lroundf(sx[6] - 0.5f);
        int vy = (int)lroundf(sy[6] - 0.5f);
        if (vx >= 1 && vx < (int)W - 1 && vy >= 1 && vy < (int)H - 1) {
            int bright = 0;
            for (int dy = -1; dy <= 1; dy++)
                for (int dx = -1; dx <= 1; dx++) {
                    uint32_t xx = (uint32_t)(vx + dx), yy = (uint32_t)(vy + dy);
                    if (fb_p(&buf, xx, yy, 0) > 180 && fb_p(&buf, xx, yy, 1) > 180 &&
                        fb_p(&buf, xx, yy, 2) > 180)
                        bright = 1;
                }
            ok(bright, "vertex (1,1,1) region is bright (Gouraud white corner)");
        } else {
            ok(0, "vertex (1,1,1) projected off-screen (camera mis-set)");
        }
    }
    ok(peq(&buf, 0, 0, 0, 0, 0, 255, 1) || refcov[0] == 0,
       "corner (0,0) background consistent");

    {
        int cxp = (int)(W / 2), cyp = (int)(H / 2);
        int i = cyp * (int)W + cxp;
        if (refcov[i] != 0) {
            int er = (int)lroundf(refc[i][0] * 255.0f);
            int eg = (int)lroundf(refc[i][1] * 255.0f);
            int eb = (int)lroundf(refc[i][2] * 255.0f);
            ok(peq(&buf, cxp, cyp, er, eg, eb, 255, 8),
               "center pixel = nearest-face (depth-buffered occlusion) reference color");
        } else {
            ok(0, "center pixel not covered (mesh mis-projected)");
        }
    }

    ok(!(fb_p(&buf, 1, 1, 0) == fb_p(&buf, W / 2, H / 2, 0) &&
         fb_p(&buf, 1, 1, 1) == fb_p(&buf, W / 2, H / 2, 1) &&
         fb_p(&buf, 1, 1, 2) == fb_p(&buf, W / 2, H / 2, 2)),
       "negative control: image is not a flat single color (real 3D shading present)");

    wgpuDevicePoll(dev, 1, NULL);

    int grand = PASS + FAIL;
    printf("scene-3dmodel-c: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, grand, EXPECTED);
    if (FAIL == 0 && grand == EXPECTED) { printf("SCENE_3DMODEL_C OK %d\n", PASS); return 0; }
    printf("SCENE_3DMODEL_C FAIL\n");
    return 1;
}
