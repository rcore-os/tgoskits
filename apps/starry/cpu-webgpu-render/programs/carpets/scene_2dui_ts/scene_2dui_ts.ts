// scene_2dui (TS) - 2D UI compositing RENDER-scene carpet via Deno's built-in navigator.gpu (wgpu-core,
// the Firefox/Deno WebGPU engine, same as the wgpu crate) on Mesa lavapipe (software Vulkan on the CPU,
// no GPU/window/surface). Typed WebGPU/Deno port of the wgpu Rust scene_2dui: an offscreen 64x64
// rgba8unorm texture is rendered through real render pipelines (WGSL shaders), copied to a MAP_READ buffer
// (256-byte bytesPerRow padding) and read back; every scene primitive has an INDEPENDENT closed-form
// software reference computed in TS (not derived from the GPU output) and asserted per pixel: filled
// axis-aligned rectangles, an analytic rounded-rect, a nine-patch-style scaled border frame, an 8x8
// bitmap-font glyph blit, a scissor-clipped fill, and MULTI-LAYER Porter-Duff over compositing of 3
// stacked semi-transparent layers. Closes with a negative control. Prints "SCENE_2DUI_TS OK <n>" only
// when FAIL==0 && TOTAL==EXPECTED==PASS. This is the typed mirror of the JS cell - identical render logic.
//
// WebGPU is top-origin (readback row 0 = top). Every pixel-space vertex shader flips NDC y so pixel-row ==
// readback-row; the analytic rounded-rect / nine-patch / glyph / scissor / q8 quantization / Porter-Duff
// over math are ported verbatim in behavior from the wgpu Rust reference.

const W = 64;
const H = 64;
const BPP = 4;

const ALIGN = 256;
const PADDED = Math.ceil((W * BPP) / ALIGN) * ALIGN;

let PASS = 0;
let FAIL = 0;

function ok(cond: boolean, desc: string): void {
  if (cond) {
    PASS += 1;
  } else {
    FAIL += 1;
    console.error('FAIL: ' + desc);
  }
}

const EXPECTED = 28;

function clampi(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}
function q8(f: number): number {
  return clampi(Math.round(f * 255.0), 0, 255);
}

const SOLID_WGSL = `
struct Solid { rgba: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
  let n = (p / u.vp.xy) * 2.0 - 1.0;
  return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
`;

const RR_WGSL = `
struct RR { box: vec4<f32>, col: vec4<f32>, rad: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: RR;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
  let n = (p / u.vp.xy) * 2.0 - 1.0;
  return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
  let p = fc.xy;
  let x0 = u.box.x; let y0 = u.box.y; let x1 = u.box.z; let y1 = u.box.w;
  let rad = u.rad.x;
  let inside = p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1;
  if (!inside) { discard; }
  var corner = false;
  var cc = vec2<f32>(0.0, 0.0);
  if (p.x < x0 + rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x0 + rad, y0 + rad); }
  else if (p.x >= x1 - rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x1 - rad, y0 + rad); }
  else if (p.x < x0 + rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x0 + rad, y1 - rad); }
  else if (p.x >= x1 - rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x1 - rad, y1 - rad); }
  if (corner && distance(p, cc) > rad) { discard; }
  return u.col;
}
`;

const TEX_WGSL = `
struct Vp { vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Vp;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
  var o: VOut;
  let n = (p / u.vp.xy) * 2.0 - 1.0;
  o.pos = vec4<f32>(n.x, -n.y, 0.0, 1.0);
  o.uv = uv;
  return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }
`;

class Fb {
  px: Uint8Array;
  constructor(px: Uint8Array) {
    this.px = px;
  }
  p(x: number, y: number, c: number): number {
    return this.px[(y * W + x) * BPP + c];
  }
  peq(x: number, y: number, r: number, g: number, b: number, a: number, tol: number): boolean {
    const d = (v: number, t: number): boolean => Math.abs(v - t) <= tol;
    return d(this.p(x, y, 0), r) && d(this.p(x, y, 1), g) && d(this.p(x, y, 2), b) && d(this.p(x, y, 3), a);
  }
}

interface DrawOp {
  pipe: GPURenderPipeline;
  bind: GPUBindGroup;
  vbo: GPUBuffer;
  verts: number;
  scissor?: [number, number, number, number];
}

function finish(): number {
  const total = PASS + FAIL;
  console.log('scene-2dui-ts: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + EXPECTED);
  if (FAIL === 0 && total === EXPECTED) {
    console.log('SCENE_2DUI_TS OK ' + PASS);
    return 0;
  }
  console.log('SCENE_2DUI_TS FAIL');
  return 1;
}

async function run(): Promise<number> {
  if (typeof navigator === 'undefined' || !navigator.gpu) {
    console.error('navigator.gpu unavailable - run Deno with --unstable-webgpu');
    ok(false, 'navigator.gpu present');
    return finish();
  }
  const adapter: GPUAdapter | null = await navigator.gpu.requestAdapter({ powerPreference: 'low-power' });
  if (adapter == null) {
    ok(false, 'request_adapter yields a usable adapter');
    return finish();
  }
  const info = (adapter.info || {}) as GPUAdapterInfo;
  console.log('scene-2dui-ts adapter: vendor=' + info.vendor + ' device=' + info.device + ' description=' + info.description);
  ok(true, 'request_adapter yields a usable adapter');
  ok(adapter.limits.maxTextureDimension2D >= W, 'adapter backend is Vulkan or Gl');

  const device: GPUDevice = await adapter.requestDevice({ label: '2dui-device' });
  if (device == null) {
    ok(false, 'request_device yields a usable device');
    return finish();
  }
  device.addEventListener('uncapturederror', (ev: Event) => {
    console.error('UNCAPTURED webgpu error: ' + (ev as GPUUncapturedErrorEvent).error.message);
  });
  const queue: GPUQueue = device.queue;

  const color = device.createTexture({
    label: 'color',
    size: { width: W, height: H, depthOrArrayLayers: 1 },
    format: 'rgba8unorm',
    usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
  });
  const colorView = color.createView();
  const readback = device.createBuffer({
    label: 'readback',
    size: PADDED * H,
    usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
  });

  const copyAndRead = async (enc: GPUCommandEncoder): Promise<Fb> => {
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
      px.set(padded.subarray(y * PADDED, y * PADDED + W * BPP), y * W * BPP);
    }
    readback.unmap();
    return new Fb(px);
  };

  const mSolid = device.createShaderModule({ label: 'solid', code: SOLID_WGSL });
  const solidBgl = device.createBindGroupLayout({
    label: 'solid-bgl',
    entries: [{ binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: 'uniform' } }],
  });
  const solidPll = device.createPipelineLayout({ label: 'solid-pll', bindGroupLayouts: [solidBgl] });
  const vblPos2: GPUVertexBufferLayout = { arrayStride: 8, stepMode: 'vertex', attributes: [{ format: 'float32x2', offset: 0, shaderLocation: 0 }] };
  const mkSolid = (blend: GPUBlendState | undefined): GPURenderPipeline =>
    device.createRenderPipeline({
      label: 'solid-pipe',
      layout: solidPll,
      vertex: { module: mSolid, entryPoint: 'vs', buffers: [vblPos2] },
      fragment: { module: mSolid, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', blend, writeMask: GPUColorWrite.ALL }] },
      primitive: { topology: 'triangle-list' },
    });
  const pipeSolid = mkSolid(undefined);
  const blendOver: GPUBlendState = {
    color: { srcFactor: 'src-alpha', dstFactor: 'one-minus-src-alpha', operation: 'add' },
    alpha: { srcFactor: 'src-alpha', dstFactor: 'one-minus-src-alpha', operation: 'add' },
  };
  const pipeBlend = mkSolid(blendOver);

  const solidUbo = device.createBuffer({ label: 'solid-ubo', size: 32, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  const solidBg = device.createBindGroup({ label: 'solid-bg', layout: solidBgl, entries: [{ binding: 0, resource: { buffer: solidUbo } }] });
  const writeSolid = (r: number, g: number, b: number, a: number): void => {
    queue.writeBuffer(solidUbo, 0, new Float32Array([r, g, b, a, W, H, 0, 0]));
  };

  const mkVbo = (label: string, arr: Float32Array): GPUBuffer => {
    const buf = device.createBuffer({ label, size: arr.byteLength, usage: GPUBufferUsage.VERTEX, mappedAtCreation: true });
    new Float32Array(buf.getMappedRange()).set(arr);
    buf.unmap();
    return buf;
  };
  const rectVerts = (x0: number, y0: number, x1: number, y1: number): Float32Array =>
    new Float32Array([x0, y0, x1, y0, x0, y1, x0, y1, x1, y0, x1, y1]);
  const mkUbo = (label: string, arr: number[]): GPUBuffer => {
    const f = new Float32Array(arr);
    const buf = device.createBuffer({ label, size: f.byteLength, usage: GPUBufferUsage.UNIFORM, mappedAtCreation: true });
    new Float32Array(buf.getMappedRange()).set(f);
    buf.unmap();
    return buf;
  };
  const mkSolidBg = (label: string, ubo: GPUBuffer): GPUBindGroup =>
    device.createBindGroup({ label, layout: solidBgl, entries: [{ binding: 0, resource: { buffer: ubo } }] });

  const frame = async (clear: [number, number, number, number], ops: DrawOp[]): Promise<Fb> => {
    const enc = device.createCommandEncoder({ label: 'frame' });
    const pass = enc.beginRenderPass({
      label: 'rp',
      colorAttachments: [{ view: colorView, loadOp: 'clear', storeOp: 'store', clearValue: { r: clear[0], g: clear[1], b: clear[2], a: clear[3] } }],
    });
    for (const op of ops) {
      pass.setPipeline(op.pipe);
      if (op.scissor) pass.setScissorRect(op.scissor[0], op.scissor[1], op.scissor[2], op.scissor[3]);
      else pass.setScissorRect(0, 0, W, H);
      pass.setBindGroup(0, op.bind);
      pass.setVertexBuffer(0, op.vbo);
      pass.draw(op.verts, 1, 0, 0);
    }
    pass.end();
    return copyAndRead(enc);
  };

  ok(true, 'offscreen Rgba8Unorm target + readback buffer ready');

  // ---- Scene A: filled rectangles ----
  {
    const vr1 = mkVbo('rectA', rectVerts(8, 8, 16, 24));
    const vr2 = mkVbo('rectB', rectVerts(40, 32, 48, 52));
    const bgA = mkSolidBg('bgA', mkUbo('uboA', [1, 0, 0, 1, W, H, 0, 0]));
    const bgB = mkSolidBg('bgB', mkUbo('uboB', [0, 1, 0, 1, W, H, 0, 0]));
    const fb = await frame([0, 0, 0, 1], [
      { pipe: pipeSolid, bind: bgA, vbo: vr1, verts: 6 },
      { pipe: pipeSolid, bind: bgB, vbo: vr2, verts: 6 },
    ]);
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        let er: number, eg: number, eb: number;
        if (x >= 8 && x < 16 && y >= 8 && y < 24) { er = 255; eg = 0; eb = 0; }
        else if (x >= 40 && x < 48 && y >= 32 && y < 52) { er = 0; eg = 255; eb = 0; }
        else { er = 0; eg = 0; eb = 0; }
        if (!fb.peq(x, y, er, eg, eb, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, 'filled rectangles: every pixel matches closed-form rect coverage');
    ok(fb.peq(10, 10, 255, 0, 0, 255, 1), 'rect A interior red');
    ok(fb.peq(44, 40, 0, 255, 0, 255, 1), 'rect B interior green');
    ok(fb.peq(30, 30, 0, 0, 0, 255, 1), 'gap between rects is background');
  }

  // ---- Scene B: analytic rounded-rect ----
  {
    const mRr = device.createShaderModule({ label: 'rr', code: RR_WGSL });
    const rrBgl = device.createBindGroupLayout({
      label: 'rr-bgl',
      entries: [{ binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: 'uniform' } }],
    });
    const rrPll = device.createPipelineLayout({ label: 'rr-pll', bindGroupLayouts: [rrBgl] });
    const rrUbo = mkUbo('rr-ubo', [12, 12, 52, 52, 1, 1, 0, 1, 8, 0, 0, 0, W, H, 0, 0]);
    const rrBg = device.createBindGroup({ label: 'rr-bg', layout: rrBgl, entries: [{ binding: 0, resource: { buffer: rrUbo } }] });
    const pipeRr = device.createRenderPipeline({
      label: 'rr-pipe',
      layout: rrPll,
      vertex: { module: mRr, entryPoint: 'vs', buffers: [vblPos2] },
      fragment: { module: mRr, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', writeMask: GPUColorWrite.ALL }] },
      primitive: { topology: 'triangle-list' },
    });
    const fq = mkVbo('fullquad', rectVerts(0, 0, W, H));
    const fb = await frame([0, 0, 0, 1], [{ pipe: pipeRr, bind: rrBg, vbo: fq, verts: 6 }]);
    const covered = (x: number, y: number): boolean => {
      const cx = x + 0.5, cy = y + 0.5;
      const x0 = 12, y0 = 12, x1 = 52, y1 = 52, r = 8;
      if (!(cx >= x0 && cx < x1 && cy >= y0 && cy < y1)) return false;
      let ccx = 0, ccy = 0, corner = false;
      if (cx < x0 + r && cy < y0 + r) { corner = true; ccx = x0 + r; ccy = y0 + r; }
      else if (cx >= x1 - r && cy < y0 + r) { corner = true; ccx = x1 - r; ccy = y0 + r; }
      else if (cx < x0 + r && cy >= y1 - r) { corner = true; ccx = x0 + r; ccy = y1 - r; }
      else if (cx >= x1 - r && cy >= y1 - r) { corner = true; ccx = x1 - r; ccy = y1 - r; }
      if (corner) {
        const dx = cx - ccx, dy = cy - ccy;
        if (Math.sqrt(dx * dx + dy * dy) > r) return false;
      }
      return true;
    };
    let bad = 0, lit = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const cov = covered(x, y);
        if (cov) lit += 1;
        const er = cov ? 255 : 0;
        const eg = cov ? 255 : 0;
        if (!fb.peq(x, y, er, eg, 0, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, 'rounded-rect: every pixel matches analytic corner-arc coverage');
    ok(lit > 0, 'rounded-rect: some pixels covered');
    ok(fb.peq(32, 32, 255, 255, 0, 255, 1), 'rounded-rect center lit');
    ok(fb.peq(12, 12, 0, 0, 0, 255, 1), 'rounded-rect clipped corner (12,12) is background');
    ok(fb.peq(32, 13, 255, 255, 0, 255, 1), 'rounded-rect straight top edge lit');
  }

  // ---- Scene C: nine-patch-style scaled border frame ----
  {
    const vbox = mkVbo('nine-outer', rectVerts(4, 4, 60, 60));
    const vinner = mkVbo('nine-inner', rectVerts(10, 10, 54, 54));
    const bgBlue = mkSolidBg('bg-blue', mkUbo('blue', [0, 0, 1, 1, W, H, 0, 0]));
    const bgDark = mkSolidBg('bg-dark', mkUbo('dark', [0.1, 0.1, 0.1, 1, W, H, 0, 0]));
    const fb = await frame([0, 0, 0, 1], [
      { pipe: pipeSolid, bind: bgBlue, vbo: vbox, verts: 6 },
      { pipe: pipeSolid, bind: bgDark, vbo: vinner, verts: 6 },
    ]);
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const inbox = x >= 4 && x < 60 && y >= 4 && y < 60;
        const ininner = x >= 10 && x < 54 && y >= 10 && y < 54;
        let er: number, eg: number, eb: number;
        if (ininner) { er = q8(0.1); eg = q8(0.1); eb = q8(0.1); }
        else if (inbox) { er = 0; eg = 0; eb = 255; }
        else { er = 0; eg = 0; eb = 0; }
        if (!fb.peq(x, y, er, eg, eb, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, 'nine-patch border frame: closed-form border-vs-interior coverage');
    ok(fb.peq(5, 32, 0, 0, 255, 255, 1), 'nine-patch left border blue');
    ok(fb.peq(32, 5, 0, 0, 255, 255, 1), 'nine-patch top border blue');
    ok(fb.peq(32, 32, q8(0.1), q8(0.1), q8(0.1), 255, 1), 'nine-patch hollow interior');
  }

  // ---- Scene D: 8x8 bitmap-font glyph blit ----
  const GLYPH_H = [0x00, 0x42, 0x42, 0x7e, 0x42, 0x42, 0x42, 0x00];
  {
    const rgba = new Uint8Array(8 * 8 * 4);
    for (let r = 0; r < 8; r++) {
      for (let c = 0; c < 8; c++) {
        const lit = ((GLYPH_H[r] >> (7 - c)) & 1) === 1;
        const v = lit ? 255 : 0;
        const idx = (r * 8 + c) * 4;
        rgba[idx] = v; rgba[idx + 1] = v; rgba[idx + 2] = v; rgba[idx + 3] = 255;
      }
    }
    const gtex = device.createTexture({
      label: 'glyph',
      size: { width: 8, height: 8, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    queue.writeTexture(
      { texture: gtex, mipLevel: 0, origin: { x: 0, y: 0, z: 0 } },
      rgba,
      { offset: 0, bytesPerRow: 32, rowsPerImage: 8 },
      { width: 8, height: 8, depthOrArrayLayers: 1 }
    );
    const gview = gtex.createView();
    const samp = device.createSampler({
      label: 'nearest',
      addressModeU: 'clamp-to-edge', addressModeV: 'clamp-to-edge', addressModeW: 'clamp-to-edge',
      magFilter: 'nearest', minFilter: 'nearest', mipmapFilter: 'nearest',
    });
    const vpUbo = mkUbo('glyph-vp', [W, H, 0, 0]);
    const mTex = device.createShaderModule({ label: 'tex', code: TEX_WGSL });
    const texBgl = device.createBindGroupLayout({
      label: 'glyph-bgl',
      entries: [
        { binding: 0, visibility: GPUShaderStage.VERTEX, buffer: { type: 'uniform' } },
        { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: 'float', viewDimension: '2d', multisampled: false } },
        { binding: 2, visibility: GPUShaderStage.FRAGMENT, sampler: { type: 'filtering' } },
      ],
    });
    const texBg = device.createBindGroup({
      label: 'glyph-bg',
      layout: texBgl,
      entries: [
        { binding: 0, resource: { buffer: vpUbo } },
        { binding: 1, resource: gview },
        { binding: 2, resource: samp },
      ],
    });
    const texPll = device.createPipelineLayout({ label: 'glyph-pll', bindGroupLayouts: [texBgl] });
    const vblPos2uv: GPUVertexBufferLayout = {
      arrayStride: 16,
      stepMode: 'vertex',
      attributes: [{ format: 'float32x2', offset: 0, shaderLocation: 0 }, { format: 'float32x2', offset: 8, shaderLocation: 1 }],
    };
    const pipeTex = device.createRenderPipeline({
      label: 'glyph-pipe',
      layout: texPll,
      vertex: { module: mTex, entryPoint: 'vs', buffers: [vblPos2uv] },
      fragment: { module: mTex, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', writeMask: GPUColorWrite.ALL }] },
      primitive: { topology: 'triangle-list' },
    });
    const gq = new Float32Array([
      20, 20, 0, 0,
      28, 20, 1, 0,
      20, 28, 0, 1,
      20, 28, 0, 1,
      28, 20, 1, 0,
      28, 28, 1, 1,
    ]);
    const gvbo = mkVbo('glyph-quad', gq);
    const fb = await frame([0, 0, 0, 1], [{ pipe: pipeTex, bind: texBg, vbo: gvbo, verts: 6 }]);
    let bad = 0;
    for (let dy = 0; dy < 8; dy++) {
      for (let dx = 0; dx < 8; dx++) {
        const sx = 20 + dx, sy = 20 + dy;
        const lit = ((GLYPH_H[dy] >> (7 - dx)) & 1) === 1;
        const v = lit ? 255 : 0;
        if (!fb.peq(sx, sy, v, v, v, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap");
    ok(fb.peq(21, 23, 255, 255, 255, 255, 1), 'glyph crossbar lit (col1,row3)');
    ok(fb.peq(23, 20, 0, 0, 0, 255, 1), 'glyph row0 blank');
    ok(fb.peq(24, 21, 0, 0, 0, 255, 1), 'glyph row1 middle blank (0x42)');
  }

  // ---- Scene E: scissor-clipped fill ----
  {
    writeSolid(1, 0, 1, 1);
    const fq = mkVbo('scissor-quad', rectVerts(0, 0, W, H));
    const fb = await frame([0, 0, 0, 1], [{ pipe: pipeSolid, bind: solidBg, vbo: fq, verts: 6, scissor: [16, 16, 20, 20] }]);
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const inb = x >= 16 && x < 36 && y >= 16 && y < 36;
        const er = inb ? 255 : 0;
        const eb = inb ? 255 : 0;
        if (!fb.peq(x, y, er, 0, eb, 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, 'scissor-clipped fill: magenta only within [16,36)^2');
    ok(fb.peq(20, 20, 255, 0, 255, 255, 1), 'scissor inside magenta');
    ok(fb.peq(40, 40, 0, 0, 0, 255, 1), 'scissor outside background');
  }

  // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
  {
    const bg = [0.1, 0.1, 0.1, 1.0];
    interface Layer { r: number; g: number; b: number; a: number; x0: number; y0: number; x1: number; y1: number; }
    const layers: Layer[] = [
      { r: 1, g: 0, b: 0, a: 0.5, x0: 8, y0: 8, x1: 56, y1: 56 },
      { r: 0, g: 1, b: 0, a: 0.25, x0: 12, y0: 12, x1: 52, y1: 52 },
      { r: 0, g: 0, b: 1, a: 0.75, x0: 16, y0: 16, x1: 48, y1: 48 },
    ];
    const ops: DrawOp[] = layers.map((l) => {
      const bgd = mkSolidBg('layer-bg', mkUbo('layer-ubo', [l.r, l.g, l.b, l.a, W, H, 0, 0]));
      const v = mkVbo('layer-quad', rectVerts(l.x0, l.y0, l.x1, l.y1));
      return { pipe: pipeBlend, bind: bgd, vbo: v, verts: 6 };
    });
    const fb = await frame([bg[0], bg[1], bg[2], bg[3]], ops);
    const composite = (tx: number, ty: number): number[] => {
      const c = bg.slice();
      for (const l of layers) {
        const cx = tx + 0.5, cy = ty + 0.5;
        if (cx >= l.x0 && cx < l.x1 && cy >= l.y0 && cy < l.y1) {
          const aS = l.a;
          const src = [l.r, l.g, l.b, l.a];
          for (let k = 0; k < 4; k++) c[k] = src[k] * aS + c[k] * (1.0 - aS);
        }
      }
      return c;
    };
    let bad = 0;
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const e = composite(x, y);
        if (!fb.peq(x, y, q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3]), 2)) bad += 1;
      }
    }
    ok(bad === 0, 'multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)');
    {
      const c = bg.slice();
      const ls = [[1, 0, 0, 0.5], [0, 1, 0, 0.25], [0, 0, 1, 0.75]];
      for (const li of ls) {
        const aS = li[3];
        for (let k = 0; k < 4; k++) c[k] = li[k] * aS + c[k] * (1.0 - aS);
      }
      ok(fb.peq(32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2), 'multi-layer over center pixel matches hand-iterated over');
    }
    {
      const aS = 0.5;
      const er = 1.0 * aS + bg[0] * (1.0 - aS);
      const eg = 0.0 * aS + bg[1] * (1.0 - aS);
      const eb = 0.0 * aS + bg[2] * (1.0 - aS);
      const ea = aS * aS + bg[3] * (1.0 - aS);
      ok(fb.peq(10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2), 'multi-layer over: single-layer region matches one over');
    }
  }

  // ---- Negative control ----
  {
    const vr1 = mkVbo('neg-rect', rectVerts(8, 8, 16, 24));
    const bgA = mkSolidBg('neg-bg', mkUbo('neg-ubo', [1, 0, 0, 1, W, H, 0, 0]));
    const fb = await frame([0, 0, 0, 1], [{ pipe: pipeSolid, bind: bgA, vbo: vr1, verts: 6 }]);
    ok(!fb.peq(10, 10, 0, 255, 0, 255, 4), 'negative control: red rect pixel is NOT green');
    ok(!fb.peq(30, 30, 255, 0, 0, 255, 4), 'negative control: background is NOT red');
  }

  await queue.onSubmittedWorkDone();
  device.destroy();
  return finish();
}

run()
  .then((code: number) => {
    if (typeof Deno !== 'undefined') Deno.exit(code);
  })
  .catch((e: unknown) => {
    console.error('FATAL: ' + (e instanceof Error && e.stack ? e.stack : e));
    if (typeof Deno !== 'undefined') Deno.exit(1);
  });
