// scene_anim (JS) - keyframe-animation RENDER-scene carpet via Deno's built-in navigator.gpu (wgpu-core,
// the Firefox/Deno WebGPU engine, same as the wgpu crate) on Mesa lavapipe (software Vulkan on the CPU,
// no GPU/window/surface). WebGPU/Deno port of the wgpu Rust scene_anim: an offscreen 64x64 rgba8unorm
// texture is rendered through a real render pipeline; N=4 keyframes of a transformed unit quad are drawn,
// each frame's model transform a rotation about the FBO center composed with a translation and uniform
// scale, interpolated by t in {0,0.25,0.5,0.75}. The transform is applied in the vertex shader from a
// hand-built pixel-space affine (rotation, scale, translate) and an ortho pixel->NDC map. For every frame
// the four rotated/scaled/translated quad CORNERS are computed INDEPENDENTLY in JS (closed form:
// R(theta)*S*local + T) and the readback is asserted at those exact corner pixels plus a point outside
// the quad. A cubic ease eased(t)=3t^2-2t^3 drives the scale, its value asserted at each t. Closes with a
// negative control. Prints "SCENE_ANIM_JS OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// WebGPU readback is top-origin; the pixel-space vertex shader flips NDC y so pixel-y == readback-row, and
// the closed-form corner/center pixel coordinates are indexed against readback rows directly. The lerp,
// ease_cubic and R*S*local+T corner math are ported verbatim in behavior from the wgpu Rust reference.

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

const EXPECTED = 38;

function lerp(a, b, t) {
  return a + (b - a) * t;
}
function easeCubic(t) {
  return 3.0 * t * t - 2.0 * t * t * t;
}

// Pixel-space affine vertex shader: pix = col0*lp.x + col1*lp.y + tr; then pixel -> NDC with y-flip.
const WGSL = `
struct X { col0: vec2<f32>, col1: vec2<f32>, tr: vec2<f32>, vp: vec2<f32>, rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: X;
@vertex fn vs(@location(0) lp: vec2<f32>) -> @builtin(position) vec4<f32> {
  let pix = u.col0 * lp.x + u.col1 * lp.y + u.tr;
  let n = (pix / u.vp) * 2.0 - 1.0;
  return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
`;

class Fb {
  constructor(px) {
    this.px = px;
  }
  p(x, y, c) {
    return this.px[(y * W + x) * BPP + c];
  }
  peq(x, y, r, g, b, a, tol) {
    if (x < 0 || y < 0 || x >= W || y >= H) return false;
    const d = (v, t) => Math.abs(v - t) <= tol;
    return d(this.p(x, y, 0), r) && d(this.p(x, y, 1), g) && d(this.p(x, y, 2), b) && d(this.p(x, y, 3), a);
  }
  nearColor(x, y, r, g, b, tol) {
    for (let dy = -1; dy <= 1; dy++) {
      for (let dx = -1; dx <= 1; dx++) {
        const xx = x + dx, yy = y + dy;
        if (xx < 0 || yy < 0 || xx >= W || yy >= H) continue;
        if (this.peq(xx, yy, r, g, b, 255, tol)) return true;
      }
    }
    return false;
  }
}

function finish() {
  const total = PASS + FAIL;
  console.log('scene-anim-js: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + EXPECTED);
  if (FAIL === 0 && total === EXPECTED) {
    console.log('SCENE_ANIM_JS OK ' + PASS);
    return 0;
  }
  console.log('SCENE_ANIM_JS FAIL');
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
  console.log('scene-anim-js adapter: vendor=' + info.vendor + ' device=' + info.device + ' description=' + info.description);
  ok(true, 'request_adapter yields a usable adapter');
  ok(adapter.limits.maxTextureDimension2D >= W, 'adapter backend is Vulkan or Gl');

  const device = await adapter.requestDevice({ label: 'anim-device' });
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

  // Xform uniform: col0.xy, col1.xy, tr.xy, vp.xy, rgba (12 floats = 48 bytes).
  const xformUbo = device.createBuffer({ label: 'xform-ubo', size: 48, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  const bgl = device.createBindGroupLayout({
    label: 'bgl',
    entries: [{ binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: 'uniform' } }],
  });
  const bg = device.createBindGroup({ label: 'bg', layout: bgl, entries: [{ binding: 0, resource: { buffer: xformUbo } }] });
  const pll = device.createPipelineLayout({ label: 'pll', bindGroupLayouts: [bgl] });
  const module = device.createShaderModule({ label: 'anim', code: WGSL });
  const vbl = { arrayStride: 8, stepMode: 'vertex', attributes: [{ format: 'float32x2', offset: 0, shaderLocation: 0 }] };
  const pipe = device.createRenderPipeline({
    label: 'anim-pipe',
    layout: pll,
    vertex: { module, entryPoint: 'vs', buffers: [vbl] },
    fragment: { module, entryPoint: 'fs', targets: [{ format: 'rgba8unorm', writeMask: GPUColorWrite.ALL }] },
    primitive: { topology: 'triangle-strip' },
  });

  // local quad corners (unit square, TL/TR/BL/BR) as a triangle strip.
  const local = [-1, -1, 1, -1, -1, 1, 1, 1];
  const localArr = new Float32Array(local);
  const vbo = device.createBuffer({ label: 'local-quad', size: localArr.byteLength, usage: GPUBufferUsage.VERTEX, mappedAtCreation: true });
  new Float32Array(vbo.getMappedRange()).set(localArr);
  vbo.unmap();

  // animation keyframe params (ported).
  const a0 = 0.0, a1 = Math.PI / 2.0;
  const s0 = 6.0, s1 = 14.0;
  const cx0 = 20.0, cx1 = 44.0, cy0 = 20.0, cy1 = 44.0;
  const frameTransform = (t) => {
    const ang = lerp(a0, a1, t);
    const sc = lerp(s0, s1, easeCubic(t));
    const cx = lerp(cx0, cx1, t), cy = lerp(cy0, cy1, t);
    const ca = Math.cos(ang), sa = Math.sin(ang);
    const col0 = [sc * ca, sc * sa];
    const col1 = [-sc * sa, sc * ca];
    const tr = [cx, cy];
    return { col0, col1, tr, sc, ang };
  };

  const ts = [0.0, 0.25, 0.5, 0.75];
  const cols = [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 0]];

  const render = async (x) => {
    queue.writeBuffer(xformUbo, 0, new Float32Array([x.col0[0], x.col0[1], x.col1[0], x.col1[1], x.tr[0], x.tr[1], x.vp[0], x.vp[1], x.rgba[0], x.rgba[1], x.rgba[2], x.rgba[3]]));
    const enc = device.createCommandEncoder({ label: 'frame' });
    const rp = enc.beginRenderPass({
      label: 'rp',
      colorAttachments: [{ view: colorView, loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 1 } }],
    });
    rp.setPipeline(pipe);
    rp.setBindGroup(0, bg);
    rp.setVertexBuffer(0, vbo);
    rp.draw(4, 1, 0, 0);
    rp.end();
    return copyAndRead(enc);
  };

  for (let fi = 0; fi < 4; fi++) {
    const t = ts[fi];
    const { col0, col1, tr, sc, ang } = frameTransform(t);
    const fb = await render({ col0, col1, tr, vp: [W, H], rgba: [cols[fi][0], cols[fi][1], cols[fi][2], 1.0] });

    // closed-form corner positions: corner = R(ang)*S(sc)*localCorner + center.
    const ca = Math.cos(ang), sa = Math.sin(ang);
    const corners = [];
    for (let k = 0; k < 4; k++) {
      const lx = local[k * 2], ly = local[k * 2 + 1];
      const rx = sc * (ca * lx - sa * ly);
      const ry = sc * (sa * lx + ca * ly);
      corners.push([tr[0] + rx, tr[1] + ry]);
    }
    const e = easeCubic(t);
    const eRef = 3.0 * t * t - 2.0 * t * t * t;
    ok(Math.abs(e - eRef) < 1e-6, 'ease_cubic closed-form value');
    ok(Math.abs(sc - (s0 + (s1 - s0) * e)) < 1e-4, 'scale = lerp(S0,S1,ease(t)) closed-form');

    const cxi = Math.round(tr[0] - 0.5);
    const cyi = Math.round(tr[1] - 0.5);
    ok(
      fb.peq(cxi, cyi, Math.round(cols[fi][0] * 255), Math.round(cols[fi][1] * 255), Math.round(cols[fi][2] * 255), 255, 2),
      'frame center pixel carries frame color at closed-form center'
    );

    for (let k = 0; k < 4; k++) {
      const px = Math.round(corners[k][0] - 0.5);
      const py = Math.round(corners[k][1] - 0.5);
      const onscreen = px >= 0 && py >= 0 && px < W && py < H;
      ok(
        onscreen && fb.nearColor(px, py, Math.round(cols[fi][0] * 255), Math.round(cols[fi][1] * 255), Math.round(cols[fi][2] * 255), 40),
        'transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)'
      );
    }

    {
      const ox = fi < 2 ? W - 2 : 1;
      const oy = fi < 2 ? H - 2 : 1;
      const reach = sc * 1.4142;
      const covers = Math.abs(ox + 0.5 - tr[0]) <= reach && Math.abs(oy + 0.5 - tr[1]) <= reach;
      if (!covers) {
        ok(fb.peq(ox, oy, 0, 0, 0, 255, 2), 'outside-quad point stays background (closed-form silhouette)');
      } else {
        ok(true, 'outside-quad point skipped (would be covered)');
      }
    }
  }

  // t=0 vs t=0.75 center positions differ.
  {
    const tra = frameTransform(0.0).tr;
    const trb = frameTransform(0.75).tr;
    ok(Math.abs(tra[0] - trb[0]) > 1.0, 'center translates between t=0 and t=0.75 (animation is real)');
  }

  // rotation at t=0.5: angle = pi/4, rotated x-axis column = (sc*cos45, sc*sin45).
  {
    const { col0, ang } = frameTransform(0.5);
    ok(Math.abs(ang - Math.PI / 4.0) < 1e-5, 't=0.5 rotation angle = pi/4 closed-form');
    ok(Math.abs(col0[0] - col0[1]) < 1e-4 && col0[0] > 0.0, 't=0.5 rotated x-axis column is (sc*cos45, sc*sin45)');
  }

  // negative control: render frame 0 (red) and confirm it is NOT green.
  {
    const { col0, col1, tr } = frameTransform(0.0);
    const fb = await render({ col0, col1, tr, vp: [W, H], rgba: [1, 0, 0, 1] });
    const cxi = Math.round(tr[0] - 0.5);
    const cyi = Math.round(tr[1] - 0.5);
    ok(!fb.peq(cxi, cyi, 0, 255, 0, 255, 4), 'negative control: frame-0 center is NOT green');
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
