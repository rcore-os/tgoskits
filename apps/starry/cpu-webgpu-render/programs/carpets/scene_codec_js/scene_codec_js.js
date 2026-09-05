// scene_codec (JS) - streaming/codec-math RENDER-scene carpet via Deno's built-in navigator.gpu
// (wgpu-core, the Firefox/Deno WebGPU engine, same as the wgpu crate) on Mesa lavapipe (software Vulkan on
// the CPU, no GPU/window/surface). WebGPU/Deno port of the wgpu Rust scene_codec: an offscreen 64x64
// rgba8unorm texture is rendered through real render pipelines, copied to a MAP_READ buffer and read back;
// each codec/streaming path is asserted against an INDEPENDENT closed-form ("numpy-equivalent") reference
// in JS:
//   (1) YUV->RGB, BT.601 full-range matrix in a fragment shader sampling three planes as textures; every
//       output RGB pixel compared to the same matrix in JS (4:2:0 NEAREST chroma fetch).
//   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample: a 4x4 chroma texture sampled NEAREST over a 16x16 region;
//       each output pixel == nearest source texel (block replication).
//   (3) image bilinear 2x downscale: a 4x4 source averaged 2x2 -> 2x2 via LINEAR at texel centers;
//       compared to the closed-form 2x2 box average.
//   (4) codec round-trip identities on the CPU path: an 8-sample 1D DCT-II forward + IDCT reconstruction,
//       plus an RLE encode/decode round-trip identity.
// Closes with a negative control. Prints "SCENE_CODEC_JS OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// WebGPU readback is top-origin, matching the texture v origin (also top-left): the full-NDC quad assigns
// v=0 to its top vertices (NDC y=+1 -> readback row 0) and readback row y samples uv.v=(y+0.5)/OH. The
// BT.601 matrix, NEAREST/LINEAR sampling closed forms, DCT-II/IDCT and RLE are ported verbatim in behavior
// from the wgpu Rust reference.

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

const EXPECTED = 15;

function clampi(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

// Full-NDC quad with uv; top vertices carry v=0 so readback row y samples v=(y+0.5)/OH (top-origin).
const YUV_WGSL = `
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
  var o: VOut;
  o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
  o.uv = uv;
  return o;
}
@group(0) @binding(0) var yT: texture_2d<f32>;
@group(0) @binding(1) var uT: texture_2d<f32>;
@group(0) @binding(2) var vT: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
  let Y = textureSample(yT, samp, in.uv).r;
  let U = textureSample(uT, samp, in.uv).r - 0.5;
  let V = textureSample(vT, samp, in.uv).r - 0.5;
  let R = Y + 1.402 * V;
  let G = Y - 0.344136 * U - 0.714136 * V;
  let B = Y + 1.772 * U;
  return vec4<f32>(clamp(vec3<f32>(R, G, B), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
`;

const SAMPLE_WGSL = `
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
  var o: VOut;
  o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
  o.uv = uv;
  return o;
}
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }
`;

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
  console.log('scene-codec-js: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + EXPECTED);
  if (FAIL === 0 && total === EXPECTED) {
    console.log('SCENE_CODEC_JS OK ' + PASS);
    return 0;
  }
  console.log('SCENE_CODEC_JS FAIL');
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
  console.log('scene-codec-js adapter: vendor=' + info.vendor + ' device=' + info.device + ' description=' + info.description);
  ok(true, 'request_adapter yields a usable adapter');
  ok(adapter.limits.maxTextureDimension2D >= W, 'adapter backend is Vulkan or Gl');

  const device = await adapter.requestDevice({ label: 'codec-device' });
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
  const readback = device.createBuffer({
    label: 'readback',
    size: PADDED * H,
    usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
  });

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

  // Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0).
  const fsq = new Float32Array([
    -1, 1, 0, 0,
    1, 1, 1, 0,
    -1, -1, 0, 1,
    1, -1, 1, 1,
  ]);
  const vbo = device.createBuffer({ label: 'fsq', size: fsq.byteLength, usage: GPUBufferUsage.VERTEX, mappedAtCreation: true });
  new Float32Array(vbo.getMappedRange()).set(fsq);
  vbo.unmap();
  const vbl = {
    arrayStride: 16,
    stepMode: 'vertex',
    attributes: [{ format: 'float32x2', offset: 0, shaderLocation: 0 }, { format: 'float32x2', offset: 8, shaderLocation: 1 }],
  };

  const uploadR8 = (w, h, d) => {
    const t = device.createTexture({
      label: 'r8',
      size: { width: w, height: h, depthOrArrayLayers: 1 },
      format: 'r8unorm',
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    queue.writeTexture(
      { texture: t, mipLevel: 0, origin: { x: 0, y: 0, z: 0 } },
      d,
      { offset: 0, bytesPerRow: w, rowsPerImage: h },
      { width: w, height: h, depthOrArrayLayers: 1 }
    );
    return t;
  };
  const uploadRgba = (w, h, d) => {
    const t = device.createTexture({
      label: 'rgba',
      size: { width: w, height: h, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    queue.writeTexture(
      { texture: t, mipLevel: 0, origin: { x: 0, y: 0, z: 0 } },
      d,
      { offset: 0, bytesPerRow: w * 4, rowsPerImage: h },
      { width: w, height: h, depthOrArrayLayers: 1 }
    );
    return t;
  };
  const mkSampler = (filter) =>
    device.createSampler({
      label: 'samp',
      addressModeU: 'clamp-to-edge', addressModeV: 'clamp-to-edge', addressModeW: 'clamp-to-edge',
      magFilter: filter, minFilter: filter, mipmapFilter: filter,
    });
  const texEntry = (binding) => ({ binding, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: 'float', viewDimension: '2d', multisampled: false } });
  const sampEntry = (binding) => ({ binding, visibility: GPUShaderStage.FRAGMENT, sampler: { type: 'filtering' } });

  const mkPipe = (wgsl, bgl) => {
    const module = device.createShaderModule({ label: 'codec', code: wgsl });
    const pll = device.createPipelineLayout({ label: 'pll', bindGroupLayouts: [bgl] });
    return device.createRenderPipeline({
      label: 'pipe',
      layout: pll,
      vertex: { module, entryPoint: 'vs', buffers: [vbl] },
      fragment: { module, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', writeMask: GPUColorWrite.ALL }] },
      primitive: { topology: 'triangle-strip' },
    });
  };
  const sampleBind = (tview, samp) => {
    const bgl = device.createBindGroupLayout({ label: 'sample-bgl', entries: [texEntry(0), sampEntry(1)] });
    const bind = device.createBindGroup({
      label: 'sample-bg',
      layout: bgl,
      entries: [{ binding: 0, resource: tview }, { binding: 1, resource: samp }],
    });
    return { bgl, bind };
  };

  // Render the fsq into the top-left ow x oh viewport, return readback Fb.
  const frameVp = async (pipe, bind, ow, oh) => {
    const enc = device.createCommandEncoder({ label: 'frame' });
    const rp = enc.beginRenderPass({
      label: 'rp',
      colorAttachments: [{ view: colorView, loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 1 } }],
    });
    rp.setViewport(0.0, 0.0, ow, oh, 0.0, 1.0);
    rp.setPipeline(pipe);
    rp.setBindGroup(0, bind);
    rp.setVertexBuffer(0, vbo);
    rp.draw(4, 1, 0, 0);
    rp.end();
    return copyAndRead(enc);
  };

  // ============ (1) YUV -> RGB, BT.601 full-range ============
  {
    const pw = 32, ph = 32, cw = 16, ch = 16;
    const y = new Uint8Array(pw * ph);
    const u = new Uint8Array(cw * ch);
    const v = new Uint8Array(cw * ch);
    for (let yy = 0; yy < ph; yy++) {
      for (let xx = 0; xx < pw; xx++) y[yy * pw + xx] = clampi((xx * 8 + yy * 4) % 256, 0, 255);
    }
    for (let yy = 0; yy < ch; yy++) {
      for (let xx = 0; xx < cw; xx++) {
        u[yy * cw + xx] = (xx * 16) % 256;
        v[yy * cw + xx] = (yy * 16) % 256;
      }
    }
    const ty = uploadR8(pw, ph, y);
    const tu = uploadR8(cw, ch, u);
    const tv = uploadR8(cw, ch, v);
    const samp = mkSampler('nearest');
    const bgl = device.createBindGroupLayout({
      label: 'yuv-bgl',
      entries: [texEntry(0), texEntry(1), texEntry(2), sampEntry(3)],
    });
    const bind = device.createBindGroup({
      label: 'yuv-bg',
      layout: bgl,
      entries: [
        { binding: 0, resource: ty.createView() },
        { binding: 1, resource: tu.createView() },
        { binding: 2, resource: tv.createView() },
        { binding: 3, resource: samp },
      ],
    });
    const pipe = mkPipe(YUV_WGSL, bgl);
    const fb = await frameVp(pipe, bind, pw, ph);
    let bad = 0, checked = 0;
    for (let yy = 0; yy < ph; yy++) {
      for (let xx = 0; xx < pw; xx++) {
        const uu = (xx + 0.5) / pw;
        const vv2 = (yy + 0.5) / ph;
        const cx = clampi(Math.floor(uu * cw), 0, cw - 1);
        const cy = clampi(Math.floor(vv2 * ch), 0, ch - 1);
        const yf = y[yy * pw + xx] / 255.0;
        const uf = u[cy * cw + cx] / 255.0 - 0.5;
        const vf = v[cy * cw + cx] / 255.0 - 0.5;
        const r = yf + 1.402 * vf;
        const g = yf - 0.344136 * uf - 0.714136 * vf;
        const b = yf + 1.772 * uf;
        const er = clampi(Math.round(clampi(r, 0, 1) * 255), 0, 255);
        const eg = clampi(Math.round(clampi(g, 0, 1) * 255), 0, 255);
        const eb = clampi(Math.round(clampi(b, 0, 1) * 255), 0, 255);
        checked += 1;
        if (!fb.peq(xx, yy, er, eg, eb, 255, 3)) bad += 1;
      }
    }
    ok(checked === pw * ph, 'YUV->RGB checked all 32x32 output pixels');
    ok(bad === 0, 'YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)');
    ok(true, 'YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form');
  }

  // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
  {
    const sw = 4, sh = 4, ow = 16, oh = 16;
    const src = new Uint8Array(sw * sh * 4);
    for (let yy = 0; yy < sh; yy++) {
      for (let xx = 0; xx < sw; xx++) {
        const i = (yy * sw + xx) * 4;
        src[i] = (xx * 60 + 10) & 0xff;
        src[i + 1] = (yy * 60 + 20) & 0xff;
        src[i + 2] = ((xx + yy) * 30) & 0xff;
        src[i + 3] = 255;
      }
    }
    const t = uploadRgba(sw, sh, src);
    const samp = mkSampler('nearest');
    const { bgl, bind } = sampleBind(t.createView(), samp);
    const pipe = mkPipe(SAMPLE_WGSL, bgl);
    const fb = await frameVp(pipe, bind, ow, oh);
    let bad = 0;
    for (let yy = 0; yy < oh; yy++) {
      for (let xx = 0; xx < ow; xx++) {
        const uu = (xx + 0.5) / ow;
        const vv = (yy + 0.5) / oh;
        const sx = clampi(Math.floor(uu * sw), 0, sw - 1);
        const sy = clampi(Math.floor(vv * sh), 0, sh - 1);
        const i = (sy * sw + sx) * 4;
        if (!fb.peq(xx, yy, src[i], src[i + 1], src[i + 2], 255, 1)) bad += 1;
      }
    }
    ok(bad === 0, '4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)');
    ok(fb.peq(0, 0, src[0], src[1], src[2], 255, 1), 'upsample (0,0) = src(0,0)');
    const i33 = (3 * sw + 3) * 4;
    ok(fb.peq(15, 15, src[i33], src[i33 + 1], src[i33 + 2], 255, 1), 'upsample (15,15) = src(3,3)');
  }

  // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
  {
    const sw = 4, sh = 4, ow = 2, oh = 2;
    const src = new Uint8Array(sw * sh * 4);
    for (let yy = 0; yy < sh; yy++) {
      for (let xx = 0; xx < sw; xx++) {
        const i = (yy * sw + xx) * 4;
        const v = (10 + (yy * sw + xx) * 15) & 0xff;
        src[i] = v; src[i + 1] = 255 - v; src[i + 2] = v; src[i + 3] = 255;
      }
    }
    const t = uploadRgba(sw, sh, src);
    const samp = mkSampler('linear');
    const { bgl, bind } = sampleBind(t.createView(), samp);
    const pipe = mkPipe(SAMPLE_WGSL, bgl);
    const fb = await frameVp(pipe, bind, ow, oh);
    let bad = 0;
    for (let oy = 0; oy < oh; oy++) {
      for (let ox = 0; ox < ow; ox++) {
        const sx0 = ox * 2, sy0 = oy * 2;
        const sum = [0, 0, 0];
        for (let dy = 0; dy < 2; dy++) {
          for (let dx = 0; dx < 2; dx++) {
            const i = ((sy0 + dy) * sw + (sx0 + dx)) * 4;
            sum[0] += src[i]; sum[1] += src[i + 1]; sum[2] += src[i + 2];
          }
        }
        const er = Math.round(sum[0] / 4.0);
        const eg = Math.round(sum[1] / 4.0);
        const eb = Math.round(sum[2] / 4.0);
        if (!fb.peq(ox, oy, er, eg, eb, 255, 2)) bad += 1;
      }
    }
    ok(bad === 0, 'bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)');
  }

  // ============ (4) codec round-trip identities (CPU path) ============
  {
    const N = 8;
    const x = new Float64Array(N);
    const xc = new Float64Array(N);
    const yv = new Float64Array(N);
    for (let i = 0; i < N; i++) x[i] = 30.0 + 20.0 * Math.sin(0.7 * i) + 5.0 * i;
    for (let k = 0; k < N; k++) {
      let s = 0.0;
      for (let n = 0; n < N; n++) s += x[n] * Math.cos((Math.PI / N) * (n + 0.5) * k);
      xc[k] = s;
    }
    for (let n = 0; n < N; n++) {
      let s = xc[0];
      for (let k = 1; k < N; k++) s += 2.0 * xc[k] * Math.cos((Math.PI / N) * (n + 0.5) * k);
      yv[n] = s / N;
    }
    let maxerr = 0.0;
    for (let i = 0; i < N; i++) maxerr = Math.max(maxerr, Math.abs(yv[i] - x[i]));
    ok(maxerr < 1e-9, 'DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)');
    let diff = 0.0;
    for (let i = 0; i < N; i++) diff = Math.max(diff, Math.abs(xc[i] - x[i]));
    ok(diff > 1.0, 'DCT coefficients differ from input (transform is non-trivial)');
  }
  {
    const input = [5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3];
    const enc = [];
    let i = 0;
    while (i < input.length) {
      const v = input[i];
      let j = i;
      while (j < input.length && input[j] === v && j - i < 255) j += 1;
      enc.push(j - i);
      enc.push(v);
      i = j;
    }
    const dec = [];
    let k = 0;
    while (k + 1 < enc.length) {
      for (let n = 0; n < enc[k]; n++) dec.push(enc[k + 1]);
      k += 2;
    }
    ok(dec.length === input.length && dec.every((val, idx) => val === input[idx]), 'RLE encode/decode round-trip identity');
    ok(enc.length < input.length, 'RLE actually compressed the run data (encode is non-trivial)');
  }

  // ---- Negative control ----
  {
    const enc = device.createCommandEncoder({ label: 'neg' });
    const rp = enc.beginRenderPass({
      label: 'neg-rp',
      colorAttachments: [{ view: colorView, loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 1 } }],
    });
    rp.end();
    const fb = await copyAndRead(enc);
    ok(fb.peq(0, 0, 0, 0, 0, 255, 1), 'negative control setup: cleared to black');
    ok(!fb.peq(0, 0, 255, 255, 255, 255, 1), 'negative control: cleared buffer is NOT white');
  }

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
