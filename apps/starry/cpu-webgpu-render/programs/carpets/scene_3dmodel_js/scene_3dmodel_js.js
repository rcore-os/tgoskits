// scene_3dmodel (JS) - 3D indexed-mesh RENDER-scene carpet via Deno's built-in navigator.gpu (wgpu-core,
// the Firefox/Deno WebGPU engine, same as the wgpu crate) on Mesa lavapipe (software Vulkan on the CPU,
// no GPU/window/surface). WebGPU/Deno port of the wgpu Rust scene_3dmodel: an offscreen rgba8unorm color
// texture + a depth32float depth texture, drawn through a real render pipeline (WGSL vertex+fragment)
// with depth compare 'less', copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back.
// Renders an indexed cube mesh with a hand-computed perspective MVP, depth-buffered occlusion, and
// Gouraud shading. The assertion is an INDEPENDENT software reference rasterizer written in JS: verts
// transformed by the SAME MVP -> clip -> NDC (perspective divide) -> viewport pixels; per pixel we
// compute barycentric coordinates, do a perspective-correct depth test in a private z-buffer, interpolate
// vertex colors, then compare the reference framebuffer to the readback per pixel. Closes with a negative
// control. Prints "SCENE_3DMODEL_JS OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// WebGPU NDC z is in [0,1] (near->0, far->1). perspective() uses m[2][2]=zf/(zn-zf),
// m[3][2]=(zf*zn)/(zn-zf); the reference window depth is z/w directly. @invariant on the vertex
// @builtin(position) pins depth bit-exactness so the LESS occlusion is deterministic. The column-major
// M4 math, cube verts/colors/indices, model/view, barycentric rasterizer and perspective-correct color
// interpolation are ported byte-identical in behavior from the wgpu Rust reference.

'use strict';

const W = 64;
const H = 64;
const BPP = 4;

const ALIGN = 256;
const PADDED = Math.ceil((W * BPP) / ALIGN) * ALIGN;

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

const EXPECTED = 14;

// pos3 + col3, mvp uniform. @invariant pins depth bit-exactness.
const CUBE_WGSL = `
struct MVP { m: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: MVP;
struct VOut { @invariant @builtin(position) pos: vec4<f32>, @location(0) col: vec3<f32> };
@vertex fn vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> VOut {
  var o: VOut;
  o.pos = u.m * vec4<f32>(p, 1.0);
  o.col = c;
  return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
  return vec4<f32>(in.col, 1.0);
}
`;

// ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) - ported from the reference ----
function mul(a, b) {
  const r = new Array(16).fill(0);
  for (let c = 0; c < 4; c++) {
    for (let row = 0; row < 4; row++) {
      let s = 0;
      for (let k = 0; k < 4; k++) s += a[k * 4 + row] * b[c * 4 + k];
      r[c * 4 + row] = s;
    }
  }
  return r;
}
function mv4(a, v) {
  const o = [0, 0, 0, 0];
  for (let row = 0; row < 4; row++) {
    let s = 0;
    for (let k = 0; k < 4; k++) s += a[k * 4 + row] * v[k];
    o[row] = s;
  }
  return o;
}
// WebGPU/Vulkan perspective: near->z_ndc 0, far->z_ndc 1. Only the z row differs from GL.
function perspective(fovy, aspect, zn, zf) {
  const f = 1.0 / Math.tan(fovy * 0.5);
  const r = new Array(16).fill(0);
  r[0 * 4 + 0] = f / aspect;
  r[1 * 4 + 1] = f;
  r[2 * 4 + 2] = zf / (zn - zf);
  r[2 * 4 + 3] = -1.0;
  r[3 * 4 + 2] = (zf * zn) / (zn - zf);
  return r;
}
function translate(x, y, z) {
  const r = new Array(16).fill(0);
  r[0] = 1; r[5] = 1; r[10] = 1; r[15] = 1;
  r[3 * 4 + 0] = x; r[3 * 4 + 1] = y; r[3 * 4 + 2] = z;
  return r;
}
function rotY(a) {
  const r = new Array(16).fill(0);
  const c = Math.cos(a), s = Math.sin(a);
  r[0 * 4 + 0] = c; r[0 * 4 + 2] = -s; r[2 * 4 + 0] = s; r[2 * 4 + 2] = c;
  r[1 * 4 + 1] = 1; r[3 * 4 + 3] = 1;
  return r;
}
function rotX(a) {
  const r = new Array(16).fill(0);
  const c = Math.cos(a), s = Math.sin(a);
  r[1 * 4 + 1] = c; r[1 * 4 + 2] = s; r[2 * 4 + 1] = -s; r[2 * 4 + 2] = c;
  r[0 * 4 + 0] = 1; r[3 * 4 + 3] = 1;
  return r;
}

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
}

function finish() {
  const total = PASS + FAIL;
  console.log('scene-3dmodel-js: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + EXPECTED);
  if (FAIL === 0 && total === EXPECTED) {
    console.log('SCENE_3DMODEL_JS OK ' + PASS);
    return 0;
  }
  console.log('SCENE_3DMODEL_JS FAIL');
  return 1;
}

async function run() {
  if (typeof navigator === 'undefined' || !navigator.gpu) {
    console.error('navigator.gpu unavailable - run Deno with --unstable-webgpu');
    ok(false, 'navigator.gpu present');
    return finish();
  }
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'low-power' });
  if (adapter == null) {
    ok(false, 'request_adapter yields a usable adapter');
    return finish();
  }
  const info = adapter.info || {};
  console.log('scene-3dmodel-js adapter: vendor=' + info.vendor + ' device=' + info.device + ' description=' + info.description);
  ok(true, 'request_adapter yields a usable adapter');
  ok(adapter.limits.maxTextureDimension2D >= W, 'adapter backend is Vulkan or Gl');

  const device = await adapter.requestDevice({ label: '3dmodel-device' });
  if (device == null) {
    ok(false, 'request_device yields a usable device');
    return finish();
  }
  device.addEventListener('uncapturederror', (ev) => {
    console.error('UNCAPTURED webgpu error: ' + ev.error.message);
  });
  const queue = device.queue;

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
  ok(true, 'offscreen Rgba8Unorm + Depth32Float target + readback buffer ready');

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
      px.set(padded.subarray(y * PADDED, y * PADDED + W * BPP), y * W * BPP);
    }
    readback.unmap();
    return new Fb(px);
  };

  // ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (ported) ----
  const VP = [
    [-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
    [-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1],
  ];
  const vc = [];
  for (let i = 0; i < 8; i++) {
    vc.push([(VP[i][0] + 1) * 0.5, (VP[i][1] + 1) * 0.5, (VP[i][2] + 1) * 0.5]);
  }
  const IDX = [
    0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 5, 6, 1, 6, 2,
  ];

  const model = mul(rotY(0.6), rotX(0.3));
  const view = translate(0, 0, -5);
  const proj = perspective(1.0, W / H, 1.0, 20.0);
  const mvp = mul(proj, mul(view, model));

  // interleaved pos3 + col3 vertex buffer.
  const verts = new Float32Array(8 * 6);
  for (let i = 0; i < 8; i++) {
    verts[i * 6 + 0] = VP[i][0]; verts[i * 6 + 1] = VP[i][1]; verts[i * 6 + 2] = VP[i][2];
    verts[i * 6 + 3] = vc[i][0]; verts[i * 6 + 4] = vc[i][1]; verts[i * 6 + 5] = vc[i][2];
  }
  const vbo = device.createBuffer({ label: 'cube-vbo', size: verts.byteLength, usage: GPUBufferUsage.VERTEX, mappedAtCreation: true });
  new Float32Array(vbo.getMappedRange()).set(verts);
  vbo.unmap();
  const idxArr = new Uint16Array(IDX);
  const ibo = device.createBuffer({ label: 'cube-ibo', size: idxArr.byteLength, usage: GPUBufferUsage.INDEX, mappedAtCreation: true });
  new Uint16Array(ibo.getMappedRange()).set(idxArr);
  ibo.unmap();

  const mvpArr = new Float32Array(mvp);
  const mvpUbo = device.createBuffer({ label: 'mvp', size: mvpArr.byteLength, usage: GPUBufferUsage.UNIFORM, mappedAtCreation: true });
  new Float32Array(mvpUbo.getMappedRange()).set(mvpArr);
  mvpUbo.unmap();

  const bgl = device.createBindGroupLayout({
    label: 'mvp-bgl',
    entries: [{ binding: 0, visibility: GPUShaderStage.VERTEX, buffer: { type: 'uniform' } }],
  });
  const bg = device.createBindGroup({ label: 'mvp-bg', layout: bgl, entries: [{ binding: 0, resource: { buffer: mvpUbo } }] });
  const pll = device.createPipelineLayout({ label: 'pll', bindGroupLayouts: [bgl] });
  const module = device.createShaderModule({ label: 'cube', code: CUBE_WGSL });
  const vbl = {
    arrayStride: 24,
    stepMode: 'vertex',
    attributes: [{ format: 'float32x3', offset: 0, shaderLocation: 0 }, { format: 'float32x3', offset: 12, shaderLocation: 1 }],
  };
  const pipe = device.createRenderPipeline({
    label: 'cube-pipe',
    layout: pll,
    vertex: { module, entryPoint: 'vs', buffers: [vbl] },
    fragment: { module, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', writeMask: GPUColorWrite.ALL }] },
    primitive: { topology: 'triangle-list', frontFace: 'ccw', cullMode: 'none' },
    depthStencil: { format: 'depth32float', depthWriteEnabled: true, depthCompare: 'less' },
  });
  ok(true, 'cube pipeline created');

  // ---- draw: clear color black, clear depth 1.0, draw the indexed cube ----
  const buf = await (async () => {
    const enc = device.createCommandEncoder({ label: 'frame' });
    const rp = enc.beginRenderPass({
      label: 'rp',
      colorAttachments: [{ view: colorView, loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 1 } }],
      depthStencilAttachment: { view: depthView, depthLoadOp: 'clear', depthStoreOp: 'store', depthClearValue: 1.0 },
    });
    rp.setPipeline(pipe);
    rp.setBindGroup(0, bg);
    rp.setVertexBuffer(0, vbo);
    rp.setIndexBuffer(ibo, 'uint16');
    rp.drawIndexed(36, 1, 0, 0, 0);
    rp.end();
    return copyAndRead(enc);
  })();
  ok(true, 'cube drawn (depth-tested, Gouraud)');

  // ---- INDEPENDENT software reference rasterizer (ported; WebGPU NDC-z in [0,1]) ----
  const refc = new Array(W * H);
  const refz = new Float32Array(W * H).fill(1e9);
  const refcov = new Uint8Array(W * H);
  for (let i = 0; i < W * H; i++) refc[i] = [0, 0, 0];
  const idx2 = (x, y) => y * W + x;

  const sx = new Array(8), sy = new Array(8), sz = new Array(8), sw = new Array(8);
  for (let i = 0; i < 8; i++) {
    const out = mv4(mvp, [VP[i][0], VP[i][1], VP[i][2], 1.0]);
    const w = out[3];
    sw[i] = w;
    const ndcx = out[0] / w, ndcy = out[1] / w, ndcz = out[2] / w;
    sx[i] = (ndcx * 0.5 + 0.5) * W;
    sy[i] = (0.5 - ndcy * 0.5) * H;
    sz[i] = ndcz;
  }
  ok(sw[0] > 0.0, 'reference: all clip.w positive (mesh in front of camera)');

  for (let t = 0; t < 12; t++) {
    const a = IDX[t * 3], b = IDX[t * 3 + 1], c = IDX[t * 3 + 2];
    const ax = sx[a], ay = sy[a], bx = sx[b], by = sy[b], cx = sx[c], cy = sy[c];
    const area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if (Math.abs(area) < 1e-6) continue;
    let minx = Math.floor(Math.min(ax, bx, cx));
    let maxx = Math.ceil(Math.max(ax, bx, cx));
    let miny = Math.floor(Math.min(ay, by, cy));
    let maxy = Math.ceil(Math.max(ay, by, cy));
    minx = Math.max(minx, 0); miny = Math.max(miny, 0);
    maxx = Math.min(maxx, W); maxy = Math.min(maxy, H);
    for (let y = miny; y < maxy; y++) {
      for (let x = minx; x < maxx; x++) {
        const pxs = x + 0.5, pys = y + 0.5;
        let w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area;
        let w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area;
        let w2 = 1.0 - w0 - w1;
        const inside = (w0 >= 0 && w1 >= 0 && w2 >= 0) || (w0 <= 0 && w1 <= 0 && w2 <= 0);
        if (!inside) continue;
        if (w0 < 0 || w1 < 0 || w2 < 0) { w0 = -w0; w1 = -w1; w2 = -w2; }
        const z = w0 * sz[a] + w1 * sz[b] + w2 * sz[c];
        const i = idx2(x, y);
        if (z < refz[i]) {
          refz[i] = z;
          refcov[i] = 1;
          const iwa = 1.0 / sw[a], iwb = 1.0 / sw[b], iwc = 1.0 / sw[c];
          const d = w0 * iwa + w1 * iwb + w2 * iwc;
          for (let k = 0; k < 3; k++) {
            const num = w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[c][k];
            refc[i][k] = num / d;
          }
        }
      }
    }
  }

  let total = 0, match = 0, covmatch = 0, covtotal = 0, interiorBad = 0;
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      total += 1;
      const gcov = !(buf.p(x, y, 0) === 0 && buf.p(x, y, 1) === 0 && buf.p(x, y, 2) === 0);
      const i = idx2(x, y);
      const rcov = refcov[i] !== 0;
      if (gcov === rcov) covmatch += 1;
      if (rcov) {
        covtotal += 1;
        const er = Math.round(refc[i][0] * 255.0);
        const eg = Math.round(refc[i][1] * 255.0);
        const eb = Math.round(refc[i][2] * 255.0);
        const interior =
          x > 0 && y > 0 && x < W - 1 && y < H - 1 &&
          refcov[idx2(x, y - 1)] !== 0 && refcov[idx2(x, y + 1)] !== 0 &&
          refcov[idx2(x - 1, y)] !== 0 && refcov[idx2(x + 1, y)] !== 0;
        if (buf.peq(x, y, er, eg, eb, 255, 6)) match += 1;
        else if (interior) interiorBad += 1;
      }
    }
  }
  ok(covtotal > 200, 'reference: cube covers a substantial area');
  ok(covmatch >= Math.floor(0.97 * total), 'coverage mask matches GPU (>=97% of pixels agree covered/empty)');
  ok(interiorBad === 0, 'every interior pixel matches perspective-correct Gouraud reference (tol 6)');
  ok(match >= Math.floor(0.92 * covtotal), '92%+ of covered pixels match reference color (edges excluded)');

  {
    const vx = Math.round(sx[6] - 0.5);
    const vy = Math.round(sy[6] - 0.5);
    if (vx >= 1 && vx < W - 1 && vy >= 1 && vy < H - 1) {
      let bright = false;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          const xx = vx + dx, yy = vy + dy;
          if (buf.p(xx, yy, 0) > 180 && buf.p(xx, yy, 1) > 180 && buf.p(xx, yy, 2) > 180) bright = true;
        }
      }
      ok(bright, 'vertex (1,1,1) region is bright (Gouraud white corner)');
    } else {
      ok(false, 'vertex (1,1,1) projected off-screen (camera mis-set)');
    }
  }
  ok(buf.peq(0, 0, 0, 0, 0, 255, 1) || refcov[idx2(0, 0)] === 0, 'corner (0,0) background consistent');

  {
    const cxp = W / 2, cyp = H / 2;
    const i = idx2(cxp, cyp);
    if (refcov[i] !== 0) {
      const er = Math.round(refc[i][0] * 255.0);
      const eg = Math.round(refc[i][1] * 255.0);
      const eb = Math.round(refc[i][2] * 255.0);
      ok(buf.peq(cxp, cyp, er, eg, eb, 255, 8), 'center pixel = nearest-face (depth-buffered occlusion) reference color');
    } else {
      ok(false, 'center pixel not covered (mesh mis-projected)');
    }
  }

  ok(
    !(buf.p(1, 1, 0) === buf.p(W / 2, H / 2, 0) && buf.p(1, 1, 1) === buf.p(W / 2, H / 2, 1) && buf.p(1, 1, 2) === buf.p(W / 2, H / 2, 2)),
    'negative control: image is not a flat single color (real 3D shading present)'
  );

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
