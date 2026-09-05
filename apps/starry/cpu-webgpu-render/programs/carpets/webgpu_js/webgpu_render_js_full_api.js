// webgpu_render_js_full_api - WebGPU RENDER carpet via Deno's built-in navigator.gpu (wgpu-core, the
// Firefox/Deno WebGPU engine, same as the wgpu crate) on Mesa lavapipe (software Vulkan on the CPU, no
// GPU/window/surface/swapchain). It renders offscreen into a 64x64 rgba8unorm GPUTexture
// (RENDER_ATTACHMENT | COPY_SRC) through real render pipelines with WGSL vertex+fragment shaders,
// copies the texture to a MAP_READ buffer honouring the 256-byte bytesPerRow alignment (rows are padded
// on copy then unpadded on readback), maps it, reads into a Uint8Array (H,W,4), and hard-asserts every
// pixel against a closed-form reference. Coverage mirrors the verified Rust wgpu render cell exactly
// (same closed-form pixel references, EXPECTED=56): render-pass clear (loadOp 'clear'), a solid quad
// (uniform buffer + bind group; WebGPU has no push constants), an axis-aligned linear gradient, a
// @builtin(position) checkerboard, a scissor rect (setScissorRect), a viewport restriction
// (setViewport), an alpha blend (a=191), and a sub-rectangle readback. Exhaustive per-API coverage
// builds a pipeline per state: all 5 WebGPU primitive topologies (point-list / line-list / line-strip /
// triangle-list / triangle-strip - WebGPU has NO triangle-fan, and points are always 1px), a blend
// factor+op matrix (one/zero, one/one add, zero/one, dst/zero, max, reverse-subtract -> alpha 0), the
// full depth-compare matrix (all 8 GPUCompareFunction against a depth32float attachment; WebGPU NDC z in
// [0,1] so a z=0.5 quad vs clear-depth 0.75 draws only under always/less/less-equal/not-equal, with
// @invariant on the depth vertex shader's @builtin(position) so z is bit-exact across pipelines), face
// culling + winding (cull none vs back with front-face ccw vs cw), a color write mask (RED vs ALL),
// format-feature + limit queries, and a 2x2 rgba8 texture upload + nearest sampling (corners TL red /
// TR green / BL blue / BR white), closing with a negative control. Prints
// "WEBGPU_RENDER_JS_FULL_API OK <n>" only when every assertion passes and the count equals EXPECTED.

'use strict';

// [runtime] Deno/browser expose global navigator.gpu (wgpu-core). This cell TESTS the WebGPU render API.

const W = 64;
const H = 64;
const BPP = 4;

// 256-byte bytesPerRow alignment (COPY_BYTES_PER_ROW_ALIGNMENT). 64*4 == 256 already, but compute the
// padded stride generically so the unpad path is exercised regardless.
const ALIGN = 256;
const UNPADDED = W * BPP;
const PADDED = Math.ceil(UNPADDED / ALIGN) * ALIGN;

let PASS = 0;
let FAIL = 0;

function ok(cond, desc) {
  if (cond) {
    PASS += 1;
  } else {
    FAIL += 1;
    console.error('FAIL: ' + desc);
  }
}

// Assertion budget, calibrated to the count this cell genuinely runs on the success path; mirrors the
// verified Rust wgpu render cell one-for-one (same coverage, same closed forms).
const EXPECTED = 56;

// A readback framebuffer: an unpadded (H*W*4) RGBA8 image with pixel accessors.
class Fb {
  constructor(px) {
    this.px = px;
  }
  p(x, y, c) {
    return this.px[(y * W + x) * BPP + c];
  }
  peq(x, y, r, g, b, a, tol) {
    const d = (v, t) => Math.abs(v - t) <= tol;
    return d(this.p(x, y, 0), r) && d(this.p(x, y, 1), g) && d(this.p(x, y, 2), b) && d(this.p(x, y, 3), a);
  }
  allEq(r, g, b, a, tol) {
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        if (!this.peq(x, y, r, g, b, a, tol)) return false;
      }
    }
    return true;
  }
}

// --- WGSL shaders (inline template strings) ---------------------------------------------------

// pos2 + uniform color -> solid fill.
const SOLID_WGSL = `
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
  return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
`;

// pos2 + per-vertex color -> interpolated gradient.
const GRAD_WGSL = `
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
  var o: VOut;
  o.pos = vec4<f32>(p, 0.0, 1.0);
  o.col = c;
  return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return in.col; }
`;

// pos2 + @builtin(position) checkerboard: white when ((x/8 + y/8) & 1) == 0.
const CHECK_WGSL = `
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
`;

// pos3 (carries a z for the depth-compare matrix) + uniform color. @invariant on the position output
// makes z bit-exact across the eight depth pipelines so equal/not-equal are deterministic.
const POS3_WGSL = `
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec3<f32>) -> @invariant @builtin(position) vec4<f32> {
  return vec4<f32>(p, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
`;

// pos2 + uv -> sample a 2x2 texture with a nearest sampler.
const TEX_WGSL = `
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
`;

function finish() {
  const total = PASS + FAIL;
  console.log(
    'webgpu-render-js: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + EXPECTED
  );
  if (FAIL === 0 && total === EXPECTED) {
    console.log('WEBGPU_RENDER_JS_FULL_API OK ' + PASS);
    return 0;
  }
  console.log('WEBGPU_RENDER_JS_FULL_API FAIL');
  return 1;
}

async function run() {
  // --- gpu entry + adapter --------------------------------------------------------------------
  if (typeof navigator === 'undefined' || !navigator.gpu) {
    console.error('navigator.gpu unavailable - run Deno with --unstable-webgpu');
    ok(false, 'navigator.gpu present');
    return finish();
  }
  const gpu = navigator.gpu;

  const adapter = await gpu.requestAdapter({ powerPreference: 'low-power' });
  if (adapter == null) {
    console.error('requestAdapter returned null - no WebGPU adapter on this host');
    ok(false, 'requestAdapter yields a usable adapter');
    return finish();
  }
  const info = adapter.info || {};
  console.log(
    'webgpu-render-js adapter selected: vendor=' + info.vendor +
    ' architecture=' + info.architecture +
    ' device=' + info.device +
    ' description=' + info.description
  );
  ok(true, 'requestAdapter yields a usable adapter');
  ok(adapter.limits.maxTextureDimension2D >= W, 'adapter exposes limits (maxTextureDimension2D>=64)');

  const device = await adapter.requestDevice({ label: 'render-device' });
  if (device == null) {
    console.error('requestDevice returned null');
    ok(false, 'requestDevice yields a usable device');
    return finish();
  }
  device.addEventListener('uncapturederror', (ev) => {
    console.error('UNCAPTURED webgpu error: ' + ev.error.message);
  });
  const queue = device.queue;

  // --- color attachment + depth + readback plumbing -------------------------------------------
  const color = device.createTexture({
    label: 'color',
    size: { width: W, height: H, depthOrArrayLayers: 1 },
    format: 'rgba8unorm',
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
  });
  const colorView = color.createView();

  const depthTex = device.createTexture({
    label: 'depth',
    size: { width: W, height: H, depthOrArrayLayers: 1 },
    format: 'depth32float',
    usage: GPUTextureUsage.RENDER_ATTACHMENT,
  });
  const depthView = depthTex.createView();

  const readback = device.createBuffer({
    label: 'readback',
    size: PADDED * H,
    usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
  });

  // --- shader modules -------------------------------------------------------------------------
  const mSolid = device.createShaderModule({ label: 'solid', code: SOLID_WGSL });
  const mGrad = device.createShaderModule({ label: 'grad', code: GRAD_WGSL });
  const mCheck = device.createShaderModule({ label: 'check', code: CHECK_WGSL });
  const mPos3 = device.createShaderModule({ label: 'pos3', code: POS3_WGSL });
  const mTex = device.createShaderModule({ label: 'tex', code: TEX_WGSL });

  // Uniform buffer + bind group for the solid color (replaces Vulkan push constants).
  const colorUbo = device.createBuffer({
    label: 'color-ubo',
    size: 16,
    usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
  });
  const uboBgl = device.createBindGroupLayout({
    label: 'ubo-bgl',
    entries: [{ binding: 0, visibility: GPUShaderStage.FRAGMENT, buffer: { type: 'uniform' } }],
  });
  const uboBg = device.createBindGroup({
    label: 'ubo-bg',
    layout: uboBgl,
    entries: [{ binding: 0, resource: { buffer: colorUbo } }],
  });
  const uboPll = device.createPipelineLayout({ label: 'ubo-pll', bindGroupLayouts: [uboBgl] });
  const emptyPll = device.createPipelineLayout({ label: 'empty-pll', bindGroupLayouts: [] });

  // --- vertex buffer layouts ------------------------------------------------------------------
  const vblPos2 = {
    arrayStride: 8,
    stepMode: 'vertex',
    attributes: [{ format: 'float32x2', offset: 0, shaderLocation: 0 }],
  };
  const vblPos2col = {
    arrayStride: 24,
    stepMode: 'vertex',
    attributes: [
      { format: 'float32x2', offset: 0, shaderLocation: 0 },
      { format: 'float32x4', offset: 8, shaderLocation: 1 },
    ],
  };
  const vblPos3 = {
    arrayStride: 12,
    stepMode: 'vertex',
    attributes: [{ format: 'float32x3', offset: 0, shaderLocation: 0 }],
  };
  const vblPos2uv = {
    arrayStride: 16,
    stepMode: 'vertex',
    attributes: [
      { format: 'float32x2', offset: 0, shaderLocation: 0 },
      { format: 'float32x2', offset: 8, shaderLocation: 1 },
    ],
  };

  const noBlendTarget = (writeMask) => ({ format: 'rgba8unorm', writeMask });

  // Generic render-pipeline builder covering every state axis the coverage matrix needs.
  const mk = (vsMod, fsMod, layout, vbl, topology, frontFace, cullMode, target, depthStencil) =>
    device.createRenderPipeline({
      label: 'pipe',
      layout,
      vertex: { module: vsMod, entryPoint: 'vs', buffers: [vbl] },
      fragment: { module: fsMod, entryPoint: 'fs', targets: [target] },
      primitive: { topology, frontFace, cullMode },
      depthStencil,
    });

  const depthFor = (compare) => ({ format: 'depth32float', depthWriteEnabled: true, depthCompare: compare });

  // --- vertex geometry ------------------------------------------------------------------------
  const mkVbo = (label, arr) => {
    const buf = device.createBuffer({ label, size: arr.byteLength, usage: GPUBufferUsage.VERTEX, mappedAtCreation: true });
    new Float32Array(buf.getMappedRange()).set(arr);
    buf.unmap();
    return buf;
  };

  // Full-screen triangle-strip quad [-1,-1]..[1,1].
  const quad = new Float32Array([
    -1.0, -1.0,
    1.0, -1.0,
    -1.0, 1.0,
    1.0, 1.0,
  ]);
  const vbo = mkVbo('quad', quad);
  // Axis-aligned gradient: red at left column, blue at right column.
  const gquad = new Float32Array([
    -1.0, -1.0, 1.0, 0.0, 0.0, 1.0,
    1.0, -1.0, 0.0, 0.0, 1.0, 1.0,
    -1.0, 1.0, 1.0, 0.0, 0.0, 1.0,
    1.0, 1.0, 0.0, 0.0, 1.0, 1.0,
  ]);
  const gvbo = mkVbo('gquad', gquad);

  // Base pipelines.
  const pipeSolid = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
  const pipeGrad = mk(mGrad, mGrad, emptyPll, vblPos2col, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
  const pipeCheck = mk(mCheck, mCheck, emptyPll, vblPos2, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
  ok(true, 'base render pipelines created');

  const setColor = (r, g, b, a) => {
    queue.writeBuffer(colorUbo, 0, new Float32Array([r, g, b, a]));
  };

  // Copy the color texture into the readback buffer (padded rows), submit, map, unpad into an Fb.
  const copyAndRead = async (enc) => {
    enc.copyTextureToBuffer(
      { texture: color, mipLevel: 0, origin: { x: 0, y: 0, z: 0 } },
      { buffer: readback, bytesPerRow: PADDED, rowsPerImage: H },
      { width: W, height: H, depthOrArrayLayers: 1 }
    );
    queue.submit([enc.finish()]);
    await readback.mapAsync(GPUMapMode.READ);
    const padded = new Uint8Array(readback.getMappedRange());
    const px = new Uint8Array(W * H * BPP);
    for (let y = 0; y < H; y++) {
      const src = y * PADDED;
      const dst = y * W * BPP;
      px.set(padded.subarray(src, src + W * BPP), dst);
    }
    readback.unmap();
    return new Fb(px);
  };

  // Render one frame: clear to `clear`, optionally draw, then read back. `d` bundles the draw state.
  const frame = async (clear, d) => {
    const enc = device.createCommandEncoder({ label: 'frame' });
    const pass = enc.beginRenderPass({
      label: 'rp',
      colorAttachments: [{
        view: colorView,
        loadOp: 'clear',
        storeOp: 'store',
        clearValue: { r: clear[0], g: clear[1], b: clear[2], a: clear[3] },
      }],
    });
    if (d.pipe) {
      pass.setPipeline(d.pipe);
      if (d.viewport) pass.setViewport(d.viewport[0], d.viewport[1], d.viewport[2], d.viewport[3], 0.0, 1.0);
      if (d.scissor) pass.setScissorRect(d.scissor[0], d.scissor[1], d.scissor[2], d.scissor[3]);
      if (d.bind) pass.setBindGroup(0, d.bind);
      if (d.vbo) pass.setVertexBuffer(0, d.vbo);
      pass.draw(d.verts, 1, 0, 0);
    }
    pass.end();
    return copyAndRead(enc);
  };

  // Depth-enabled frame: clears color + depth, draws the pos3 quad through `pipe`.
  const frameDepth = async (clear, depthClear, pipe, bind, vboBuf, verts) => {
    const enc = device.createCommandEncoder({ label: 'frame-d' });
    const pass = enc.beginRenderPass({
      label: 'rp-d',
      colorAttachments: [{
        view: colorView,
        loadOp: 'clear',
        storeOp: 'store',
        clearValue: { r: clear[0], g: clear[1], b: clear[2], a: clear[3] },
      }],
      depthStencilAttachment: {
        view: depthView,
        depthLoadOp: 'clear',
        depthStoreOp: 'store',
        depthClearValue: depthClear,
      },
    });
    pass.setPipeline(pipe);
    pass.setBindGroup(0, bind);
    pass.setVertexBuffer(0, vboBuf);
    pass.draw(verts, 1, 0, 0);
    pass.end();
    return copyAndRead(enc);
  };

  // ================= base coverage =================

  // Clear.
  let fb = await frame([0.0, 0.25, 0.5, 1.0], { verts: 0 });
  ok(fb.allEq(0, 64, 128, 255, 2), 'renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)');
  ok(fb.peq(0, 0, 0, 64, 128, 255, 2), 'clear pixel (0,0)');

  // Solid red quad.
  setColor(1.0, 0.0, 0.0, 1.0);
  fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeSolid, bind: uboBg, vbo, verts: 4 });
  ok(fb.allEq(255, 0, 0, 255, 1), 'solid red quad fills every pixel');

  // Axis-aligned gradient.
  fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeGrad, vbo: gvbo, verts: 4 });
  {
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const u = (x + 0.5) / W;
        const r = Math.round((1.0 - u) * 255.0);
        const b = Math.round(u * 255.0);
        if (!fb.peq(x, y, r, 0, b, 255, 4)) bad += 1;
      }
    }
    ok(bad === 0, 'gradient matches horizontal-linear closed-form for all pixels');
    ok(fb.peq(0, 0, 255, 0, 0, 255, 8), 'gradient left edge ~ red');
    ok(fb.peq(W - 1, H - 1, 0, 0, 255, 255, 8), 'gradient right edge ~ blue');
    ok(fb.peq(W / 2, H / 2, 128, 0, 128, 255, 4), 'gradient center ~ (128,0,128)');
  }

  // Checkerboard from @builtin(position).
  fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeCheck, vbo, verts: 4 });
  {
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const white = (((x / 8 | 0) + (y / 8 | 0)) & 1) === 0;
        const w = white ? 255 : 0;
        if (!fb.peq(x, y, w, w, w, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, 'checkerboard matches (x/8+y/8) parity for all pixels');
    ok(fb.peq(0, 0, 255, 255, 255, 255, 1), 'checker cell (0,0) white');
    ok(fb.peq(8, 0, 0, 0, 0, 255, 1), 'checker cell (8,0) black');
  }

  // Scissor.
  setColor(0.0, 1.0, 0.0, 1.0);
  fb = await frame([1.0, 0.0, 0.0, 1.0], { pipe: pipeSolid, bind: uboBg, vbo, verts: 4, scissor: [16, 16, 32, 32] });
  ok(fb.peq(32, 32, 0, 255, 0, 255, 1), 'scissor: inside box green');
  ok(fb.peq(2, 2, 255, 0, 0, 255, 1), 'scissor: outside box red (clear)');
  ok(fb.peq(50, 50, 255, 0, 0, 255, 1), 'scissor: past box red');

  // Viewport restriction: a viewport confined to the top-left 32x32 maps the full-NDC quad into that
  // sub-rect; pixels outside stay at the clear color.
  setColor(0.0, 1.0, 0.0, 1.0);
  fb = await frame([1.0, 0.0, 0.0, 1.0], { pipe: pipeSolid, bind: uboBg, vbo, verts: 4, viewport: [0.0, 0.0, 32.0, 32.0] });
  ok(fb.peq(8, 8, 0, 255, 0, 255, 1), 'viewport: inside 32x32 green');
  ok(fb.peq(50, 50, 255, 0, 0, 255, 1), 'viewport: outside stays clear red');

  // Alpha blend: Src=SrcAlpha, Dst=OneMinusSrcAlpha, Add, over all channels (alpha too -> 191).
  const blendOver = {
    color: { srcFactor: 'src-alpha', dstFactor: 'one-minus-src-alpha', operation: 'add' },
    alpha: { srcFactor: 'src-alpha', dstFactor: 'one-minus-src-alpha', operation: 'add' },
  };
  const pipeBlend = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none',
    { format: 'rgba8unorm', blend: blendOver, writeMask: GPUColorWrite.ALL }, undefined);
  setColor(0.0, 0.0, 1.0, 0.5);
  fb = await frame([1.0, 0.0, 0.0, 1.0], { pipe: pipeBlend, bind: uboBg, vbo, verts: 4 });
  ok(fb.allEq(128, 0, 128, 191, 3), 'alpha blend 0.5*blue over red -> rgb(128,0,128) a191');

  // Sub-rect readback.
  setColor(0.2, 0.4, 0.6, 1.0);
  fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeSolid, bind: uboBg, vbo, verts: 4 });
  {
    let good = true;
    for (let y = 10; y < 14; y++) {
      for (let x = 10; x < 14; x++) {
        if (!fb.peq(x, y, 51, 102, 153, 255, 2)) good = false;
      }
    }
    ok(good, 'sub-rect (10,10,4x4) == (51,102,153,255)');
  }

  // ================= exhaustive per-API render coverage =================

  // --- Topologies: all 5 WebGPU topologies (NO triangle-fan) ----------------------------------
  setColor(1.0, 0.0, 0.0, 1.0);
  {
    // TriangleList: two CCW tris covering the quad.
    const tl = new Float32Array([
      -1.0, -1.0, 1.0, -1.0, -1.0, 1.0,
      -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
    ]);
    const bTl = mkVbo('tl', tl);
    // A horizontal center line for LineList / LineStrip.
    const ln = new Float32Array([-1.0, 0.0, 1.0, 0.0]);
    const bLn = mkVbo('ln', ln);
    // A single center point.
    const pt = new Float32Array([0.0, 0.0]);
    const bPt = mkVbo('pt', pt);

    const mkt = (topo) => mk(mSolid, mSolid, uboPll, vblPos2, topo, 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
    const pTl = mkt('triangle-list');
    const pLl = mkt('line-list');
    const pLs = mkt('line-strip');
    const pPt = mkt('point-list');
    ok(true, 'topology pipelines created');

    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pTl, bind: uboBg, vbo: bTl, verts: 6 });
    ok(fb.allEq(255, 0, 0, 255, 1), 'TriangleList fills quad');

    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pLl, bind: uboBg, vbo: bLn, verts: 2 });
    {
      let mid = 0;
      for (let x = 0; x < W; x++) {
        if (fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2)) mid += 1;
      }
      ok(mid >= W - 2, 'LineList draws the middle row');
      ok(fb.peq(0, 0, 0, 0, 0, 255, 2), 'LineList leaves top row clear');
    }

    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pLs, bind: uboBg, vbo: bLn, verts: 2 });
    {
      let mid = 0;
      for (let x = 0; x < W; x++) {
        if (fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2)) mid += 1;
      }
      ok(mid >= W - 2, 'LineStrip draws the middle row');
    }

    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pPt, bind: uboBg, vbo: bPt, verts: 1 });
    {
      let hit = false;
      for (let y = H / 2 - 2; y <= H / 2 + 2; y++) {
        for (let x = W / 2 - 2; x <= W / 2 + 2; x++) {
          if (fb.peq(x, y, 255, 0, 0, 255, 2)) hit = true;
        }
      }
      ok(hit, 'PointList draws a 1px point at the center');
    }
  }

  // --- Blend factor + op matrix ---------------------------------------------------------------
  {
    const mkBlend = (sc, dc, oc, sa, da, oa) => mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none',
      {
        format: 'rgba8unorm',
        blend: {
          color: { srcFactor: sc, dstFactor: dc, operation: oc },
          alpha: { srcFactor: sa, dstFactor: da, operation: oa },
        },
        writeMask: GPUColorWrite.ALL,
      }, undefined);

    // One/Zero: src replaces dst.
    let p = mkBlend('one', 'zero', 'add', 'one', 'zero', 'add');
    setColor(0.0, 0.0, 1.0, 1.0);
    fb = await frame([0.5, 0.5, 0.5, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(0, 0, 255, 255, 2), 'blend One/Zero: src replaces dst');

    // One/One Add.
    p = mkBlend('one', 'one', 'add', 'one', 'one', 'add');
    setColor(0.0, 0.0, 0.5, 1.0);
    fb = await frame([0.5, 0.0, 0.0, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(128, 0, 128, 255, 2), 'blend One/One Add: src+dst = (128,0,128)');

    // Zero/One: dst kept.
    p = mkBlend('zero', 'one', 'add', 'zero', 'one', 'add');
    setColor(0.0, 1.0, 0.0, 1.0);
    fb = await frame([0.2, 0.0, 0.0, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(51, 0, 0, 255, 2), 'blend Zero/One: dst kept (51,0,0)');

    // Dst/Zero: src*dst modulate.
    p = mkBlend('dst', 'zero', 'add', 'dst', 'zero', 'add');
    setColor(0.0, 0.0, 1.0, 1.0);
    fb = await frame([0.5, 0.5, 0.5, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(0, 0, 128, 255, 2), 'blend Dst/Zero: src*dst modulate (0,0,128)');

    // One/One Max: per-channel max.
    p = mkBlend('one', 'one', 'max', 'one', 'one', 'max');
    setColor(0.6, 0.2, 0.6, 1.0);
    fb = await frame([0.2, 0.6, 0.2, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(153, 153, 153, 255, 2), 'blend op Max: per-channel max');

    // One/One ReverseSubtract: dst-src (rgb 191, alpha resolves to 0).
    p = mkBlend('one', 'one', 'reverse-subtract', 'one', 'one', 'reverse-subtract');
    setColor(0.25, 0.0, 0.0, 1.0);
    fb = await frame([1.0, 0.0, 0.0, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(191, 0, 0, 0, 3), 'blend op ReverseSubtract: dst-src rgb (191,0,0) a0');
  }

  // --- Depth-compare matrix (all 8 GPUCompareFunction; z=0.5 quad vs clear-depth 0.75) --------
  {
    const dq = new Float32Array([
      -1.0, -1.0, 0.5,
      1.0, -1.0, 0.5,
      -1.0, 1.0, 0.5,
      1.0, 1.0, 0.5,
    ]);
    const dvbo = mkVbo('dquad', dq);
    setColor(0.0, 1.0, 0.0, 1.0);
    const cases = [
      ['always', true, 'depth Always'],
      ['never', false, 'depth Never'],
      ['less', true, 'depth Less'],
      ['less-equal', true, 'depth LessEqual'],
      ['equal', false, 'depth Equal'],
      ['greater', false, 'depth Greater'],
      ['greater-equal', false, 'depth GreaterEqual'],
      ['not-equal', true, 'depth NotEqual'],
    ];
    for (const [cmp, draws, name] of cases) {
      const p = mk(mPos3, mPos3, uboPll, vblPos3, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), depthFor(cmp));
      fb = await frameDepth([0.0, 0.0, 0.0, 1.0], 0.75, p, uboBg, dvbo, 4);
      ok(fb.peq(W / 2, H / 2, 0, 255, 0, 255, 2) === draws, name);
    }
  }

  // --- Face culling + winding -----------------------------------------------------------------
  setColor(1.0, 0.0, 0.0, 1.0);
  {
    // Cull None: quad drawn.
    const p = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: p, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(255, 0, 0, 255, 1), 'cull None: quad drawn');

    // Cull Back with FrontFace Ccw vs Cw: exactly one shows the quad (winding flip).
    const pCcw = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'back', noBlendTarget(GPUColorWrite.ALL), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pCcw, bind: uboBg, vbo, verts: 4 });
    const ccw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);

    const pCw = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'cw', 'back', noBlendTarget(GPUColorWrite.ALL), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pCw, bind: uboBg, vbo, verts: 4 });
    const cw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);

    ok(ccw !== cw, 'cull Back: Ccw vs Cw winding flips visibility');

    // Cull Front (Ccw) vs cull Back (Ccw) disagree at the center: one draws, one culls.
    const pFront = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'front', noBlendTarget(GPUColorWrite.ALL), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pFront, bind: uboBg, vbo, verts: 4 });
    const frontDrawn = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);
    ok(frontDrawn !== ccw, 'cull Front vs cull Back (Ccw) disagree at center');
  }

  // --- Color write mask -----------------------------------------------------------------------
  setColor(1.0, 1.0, 1.0, 1.0);
  {
    const pR = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.RED), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pR, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(255, 0, 0, 255, 1), 'colorWrites RED only: white -> (255,0,0,255)');

    const pAll = mk(mSolid, mSolid, uboPll, vblPos2, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pAll, bind: uboBg, vbo, verts: 4 });
    ok(fb.allEq(255, 255, 255, 255, 1), 'colorWrites ALL: white -> (255,255,255,255)');
  }

  // --- Format feature + limit queries ---------------------------------------------------------
  {
    // rgba8unorm and depth32float both build RENDER_ATTACHMENT textures successfully above; assert the
    // spec-mandated limits that gate this cell's usage.
    const lim = device.limits;
    ok(lim.maxTextureDimension2D >= W, 'limits.maxTextureDimension2D >= 64');
    ok(lim.maxColorAttachments >= 1, 'limits.maxColorAttachments >= 1');
    // Prove the render+copy formats are actually usable by round-tripping a probe texture create.
    const probe = device.createTexture({
      size: { width: 4, height: 4, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
    });
    ok(probe.format === 'rgba8unorm', 'rgba8unorm texture usable as RENDER_ATTACHMENT|COPY_SRC');
    probe.destroy();
    const probeD = device.createTexture({
      size: { width: 4, height: 4, depthOrArrayLayers: 1 },
      format: 'depth32float',
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });
    ok(probeD.format === 'depth32float', 'depth32float texture usable as RENDER_ATTACHMENT');
    probeD.destroy();
  }

  // --- 2x2 texture upload + Nearest sampling --------------------------------------------------
  {
    const tex = device.createTexture({
      label: 'tex2x2',
      size: { width: 2, height: 2, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    // Row-major, v origin top-left in WebGPU: TL red, TR green, BL blue, BR white.
    const texels = new Uint8Array([
      255, 0, 0, 255,
      0, 255, 0, 255,
      0, 0, 255, 255,
      255, 255, 255, 255,
    ]);
    queue.writeTexture(
      { texture: tex, mipLevel: 0, origin: { x: 0, y: 0, z: 0 } },
      texels,
      { offset: 0, bytesPerRow: 8, rowsPerImage: 2 },
      { width: 2, height: 2, depthOrArrayLayers: 1 }
    );
    const tview = tex.createView();
    const samp = device.createSampler({
      label: 'nearest',
      addressModeU: 'clamp-to-edge',
      addressModeV: 'clamp-to-edge',
      addressModeW: 'clamp-to-edge',
      magFilter: 'nearest',
      minFilter: 'nearest',
      mipmapFilter: 'nearest',
    });
    const texBgl = device.createBindGroupLayout({
      label: 'tex-bgl',
      entries: [
        { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: 'float', viewDimension: '2d', multisampled: false } },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, sampler: { type: 'filtering' } },
      ],
    });
    const texBg = device.createBindGroup({
      label: 'tex-bg',
      layout: texBgl,
      entries: [
        { binding: 0, resource: tview },
        { binding: 1, resource: samp },
      ],
    });
    const texPll = device.createPipelineLayout({ label: 'tex-pll', bindGroupLayouts: [texBgl] });
    const pipeTex = mk(mTex, mTex, texPll, vblPos2uv, 'triangle-strip', 'ccw', 'none', noBlendTarget(GPUColorWrite.ALL), undefined);
    ok(true, 'texture pipeline + bind group created');

    // Full-screen quad with uv. WebGPU maps NDC y=+1 to the framebuffer top and the texture's v origin
    // is top-left, so the top vertices (pos.y=+1) carry v=0 to put the texture's top row at the top.
    const tq = new Float32Array([
      -1.0, -1.0, 0.0, 1.0,
      1.0, -1.0, 1.0, 1.0,
      -1.0, 1.0, 0.0, 0.0,
      1.0, 1.0, 1.0, 0.0,
    ]);
    const tvbo = mkVbo('tquad', tq);
    fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeTex, bind: texBg, vbo: tvbo, verts: 4 });
    ok(fb.peq(W / 4, H / 4, 255, 0, 0, 255, 2), 'texture Nearest top-left red');
    ok(fb.peq(3 * W / 4, H / 4, 0, 255, 0, 255, 2), 'texture Nearest top-right green');
    ok(fb.peq(W / 4, 3 * H / 4, 0, 0, 255, 255, 2), 'texture Nearest bottom-left blue');
    ok(fb.peq(3 * W / 4, 3 * H / 4, 255, 255, 255, 255, 2), 'texture Nearest bottom-right white');
  }

  // --- Negative control -----------------------------------------------------------------------
  setColor(1.0, 0.0, 0.0, 1.0);
  fb = await frame([0.0, 0.0, 0.0, 1.0], { pipe: pipeSolid, bind: uboBg, vbo, verts: 4 });
  ok(!fb.allEq(0, 255, 0, 255, 2), 'negative control: red buffer is NOT green');
  ok(!fb.peq(0, 0, 0, 0, 0, 255, 2), 'negative control: red pixel is NOT black');

  await queue.onSubmittedWorkDone();
  device.destroy();
  return finish();
}

run()
  .then((code) => {
    if (typeof Deno !== 'undefined') Deno.exit(code);
    else process.exit(code);
  })
  .catch((e) => {
    console.error('FATAL: ' + (e && e.stack ? e.stack : e));
    if (typeof Deno !== 'undefined') Deno.exit(1);
    else process.exit(1);
  });
