// webgpu_js_full_api - full WebGPU JS compute-API carpet on Mesa lavapipe, driven by the dawn-based
// `webgpu` node package. Walks the WebGPU object graph - gpu / adapter / device / queue / shader-module
// / buffer / bind-group-layout / pipeline-layout / compute-pipeline / bind-group / command-encoder /
// compute-pass / dispatch / copy-buffer-to-buffer / mapAsync - and asserts vadd/saxpy/mul results per
// element against a JS-computed reference. Prints "WEBGPU_JS_FULL_API OK <n>" only when every assertion
// passes and the count equals the pinned EXPECTED total.

'use strict';

const { create, globals } = require('webgpu');
Object.assign(globalThis, globals);

let PASS = 0;
let FAIL = 0;

function ok(cond, desc) {
  if (cond) {
    PASS += 1;
  } else {
    FAIL += 1;
    process.stderr.write('FAIL: ' + desc + '\n');
  }
}

// f32 rounding makes scaled results inexact vs a JS double reference; use a relative tolerance there and
// exact equality for the +/* cases that round-trip through f32 identically.
function feq(a, b) {
  return Math.abs(a - b) <= 1e-4 * (1.0 + Math.abs(b));
}

function allEq(got, ref) {
  if (got.length !== ref.length) return false;
  for (let i = 0; i < ref.length; i++) {
    if (got[i] !== ref[i]) return false;
  }
  return true;
}

function allFeq(got, ref) {
  if (got.length !== ref.length) return false;
  for (let i = 0; i < ref.length; i++) {
    if (!feq(got[i], ref[i])) return false;
  }
  return true;
}

// WGSL compute shader: c[i] = alpha*a[i] + b[i]. alpha and n come from a uniform block so the same
// pipeline drives both vadd (alpha=1) and saxpy (alpha=k).
const SAXPY_WGSL = `
struct Params { alpha: f32, n: u32 };
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@group(0) @binding(3) var<uniform>             p: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i < p.n) {
    c[i] = p.alpha * a[i] + b[i];
  }
}
`;

// Second shader (separate module + pipeline): c[i] = a[i] * b[i], no uniform - exercises a distinct
// bind-group-layout (3 storage bindings) and a second compute pipeline.
const MUL_WGSL = `
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i < arrayLength(&a)) {
    c[i] = a[i] * b[i];
  }
}
`;

// Deliberately-invalid WGSL: unterminated statement + unknown builtin. Used to exercise the
// compile-error diagnostic path (getCompilationInfo + validation error scope).
const BROKEN_WGSL = `
@compute @workgroup_size(64)
fn main() {
  let x = ;
  totally_not_a_function(x)
}
`;

// Single-storage-binding add-one shader for the large-N multi-workgroup grid and the
// dynamic-offset / indirect-dispatch coverage.
const ADD1_WGSL = `
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read_write> c: array<f32>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i < arrayLength(&a)) {
    c[i] = a[i] + 1.0;
  }
}
`;

const N = 2048;
const WG = 64;
const NBYTES = N * 4;
const GROUPS = Math.ceil(N / WG);

function packParams(alpha, n) {
  const buf = new ArrayBuffer(16);
  new Float32Array(buf, 0, 1)[0] = alpha;
  new Uint32Array(buf, 4, 1)[0] = n;
  return buf;
}

function storageEntry(binding, readOnly) {
  return {
    binding,
    visibility: GPUShaderStage.COMPUTE,
    buffer: { type: readOnly ? 'read-only-storage' : 'storage' },
  };
}

function finish() {
  const expected = 78;
  const total = PASS + FAIL;
  console.log(
    'webgpu-js: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + expected
  );
  if (FAIL === 0 && total === expected) {
    console.log('WEBGPU_JS_FULL_API OK ' + PASS);
    return 0;
  }
  console.log('WEBGPU_JS_FULL_API FAIL');
  return 1;
}

async function run() {
  // --- gpu entry + adapter --------------------------------------------------------------------
  const gpu = create([]);
  ok(gpu != null, 'create([]) returns gpu');
  ok(typeof gpu.requestAdapter === 'function', 'gpu.requestAdapter is function');

  const adapter = await gpu.requestAdapter({ powerPreference: 'low-power' });
  ok(adapter != null, 'requestAdapter non-null');
  if (adapter == null) {
    return finish();
  }

  const info = adapter.info;
  console.log(
    'webgpu-js adapter selected: vendor=' + info.vendor +
    ' architecture=' + info.architecture +
    ' device=' + info.device +
    ' description=' + info.description
  );
  ok(info != null, 'adapter.info present');
  ok(String(info.description || info.device || '').length > 0, 'adapter.info description/device non-empty');

  // adapter capability queries
  const feats = adapter.features;
  ok(typeof feats.has === 'function', 'adapter.features is set-like');
  const featCount = [...feats].length;
  ok(featCount === feats.size, 'adapter.features spread length equals set size');
  const alim = adapter.limits;
  ok(alim.maxComputeWorkgroupSizeX >= 64, 'adapter.limits maxComputeWorkgroupSizeX>=64');
  ok(alim.maxStorageBuffersPerShaderStage >= 3, 'adapter.limits maxStorageBuffersPerShaderStage>=3');
  ok(alim.maxBindGroups >= 1, 'adapter.limits maxBindGroups>=1');
  ok(alim.maxComputeInvocationsPerWorkgroup >= 64, 'adapter.limits maxComputeInvocationsPerWorkgroup>=64');
  ok(alim.maxComputeWorkgroupsPerDimension >= 65535, 'adapter.limits maxComputeWorkgroupsPerDimension>=65535');

  // --- device + queue -------------------------------------------------------------------------
  // requestDevice with requiredFeatures + requiredLimits: only ask for timestamp-query when the
  // adapter advertises it (lavapipe does), and floor a limit we actually consume so a device that
  // cannot honour it is rejected up front rather than failing later.
  const hasTimestamp = feats.has('timestamp-query');
  const requiredFeatures = hasTimestamp ? ['timestamp-query'] : [];
  const device = await adapter.requestDevice({
    label: 'carpet-device',
    requiredFeatures,
    requiredLimits: { maxComputeInvocationsPerWorkgroup: 64 },
  });
  ok(device != null, 'requestDevice non-null');
  ok(device.limits.maxComputeInvocationsPerWorkgroup >= 64, 'requiredLimits honoured (maxComputeInvocationsPerWorkgroup>=64)');
  ok(!hasTimestamp || device.features.has('timestamp-query'), 'requiredFeatures granted: device.features has timestamp-query');
  const dlim = device.limits;
  ok(dlim.maxComputeInvocationsPerWorkgroup >= 64, 'device.limits maxComputeInvocationsPerWorkgroup>=64');
  ok(typeof device.features.has === 'function', 'device.features is set-like');
  const queue = device.queue;
  ok(queue != null, 'device.queue present');
  ok(typeof queue.writeBuffer === 'function', 'queue.writeBuffer is function');

  // surface any uncaptured validation/oom error loudly
  device.addEventListener('uncapturederror', (ev) => {
    process.stderr.write('UNCAPTURED webgpu error: ' + ev.error.message + '\n');
  });

  // --- CPU reference data ---------------------------------------------------------------------
  const a = new Float32Array(N);
  const b = new Float32Array(N);
  for (let i = 0; i < N; i++) {
    a[i] = i * 0.5;
    b[i] = 2.0 * i + 1.0;
  }

  // seed a STORAGE|COPY_DST buffer by writing its mapped range at creation time
  function makeSeeded(data, extraUsage) {
    const buf = device.createBuffer({
      size: NBYTES,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | (extraUsage || 0),
      mappedAtCreation: true,
    });
    new Float32Array(buf.getMappedRange()).set(data);
    buf.unmap();
    return buf;
  }

  // --- buffers --------------------------------------------------------------------------------
  const bufA = makeSeeded(a, GPUBufferUsage.COPY_SRC);
  ok(bufA.size === NBYTES, 'buffer A size');
  ok((bufA.usage & GPUBufferUsage.STORAGE) !== 0, 'buffer A usage has STORAGE');
  const bufB = makeSeeded(b, 0);
  ok(bufB.size === NBYTES, 'buffer B size');

  const bufC = device.createBuffer({
    size: NBYTES,
    usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST,
  });
  ok(bufC.size === NBYTES, 'buffer C size');
  ok((bufC.usage & GPUBufferUsage.COPY_SRC) !== 0, 'buffer C usage has COPY_SRC');

  const pbuf = device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  ok((pbuf.usage & GPUBufferUsage.UNIFORM) !== 0, 'params buffer usage has UNIFORM');

  const staging = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
  ok((staging.usage & GPUBufferUsage.MAP_READ) !== 0, 'staging buffer usage has MAP_READ');

  // --- shader modules + saxpy pipeline objects, proven clean via a single validation scope -----
  // Dawn returns a non-null handle even on a deferred validation error, so `handle != null` proves
  // nothing. Instead wrap the whole create-group in one validation error scope and assert it popped
  // clean - a single genuine check that shader-module/layout/pipeline/bind-group all built without a
  // deferred validation error. The objects' functional correctness is verified by the compute results.
  device.pushErrorScope('validation');
  const saxpyMod = device.createShaderModule({ label: 'saxpy', code: SAXPY_WGSL });
  const mulMod = device.createShaderModule({ label: 'mul', code: MUL_WGSL });
  const bgl = device.createBindGroupLayout({
    label: 'saxpy-bgl',
    entries: [
      storageEntry(0, true),
      storageEntry(1, true),
      storageEntry(2, false),
      { binding: 3, visibility: GPUShaderStage.COMPUTE, buffer: { type: 'uniform' } },
    ],
  });
  const pll = device.createPipelineLayout({ label: 'saxpy-pll', bindGroupLayouts: [bgl] });
  const saxpyPipe = device.createComputePipeline({
    label: 'saxpy-pipe',
    layout: pll,
    compute: { module: saxpyMod, entryPoint: 'main' },
  });
  const bind = device.createBindGroup({
    label: 'saxpy-bind',
    layout: bgl,
    entries: [
      { binding: 0, resource: { buffer: bufA } },
      { binding: 1, resource: { buffer: bufB } },
      { binding: 2, resource: { buffer: bufC } },
      { binding: 3, resource: { buffer: pbuf } },
    ],
  });
  const saxpyCreateErr = await device.popErrorScope();
  ok(saxpyCreateErr == null, 'saxpy create-group (modules+layout+pipeline+bind) raises no validation error');

  // copy staging -> mapAsync(READ) -> Float32Array copy -> unmap. Returns a fresh Float32Array.
  async function readBack(src) {
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(src, 0, staging, 0, NBYTES);
    queue.submit([enc.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const out = new Float32Array(staging.getMappedRange().slice(0));
    staging.unmap();
    return out;
  }

  function dispatch(pipe, bg) {
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(pipe);
    pass.setBindGroup(0, bg);
    pass.dispatchWorkgroups(GROUPS);
    pass.end();
    queue.submit([enc.finish()]);
  }

  // --- vadd: alpha=1 --------------------------------------------------------------------------
  queue.writeBuffer(pbuf, 0, packParams(1.0, N));
  dispatch(saxpyPipe, bind);
  {
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = a[i] + b[i];
    ok(got.length === N, 'vadd readback length');
    ok(allEq(got, ref), 'vadd c==a+b (every element)');
    ok(got[0] === ref[0], 'vadd element[0]');
    ok(got[N / 2] === ref[N / 2], 'vadd element[N/2]');
    ok(got[N - 1] === ref[N - 1], 'vadd element[N-1]');

    // negative control: build a deliberately-wrong independent reference (a+b+1) and prove the
    // comparators actually REJECT it. If readback silently returned reference-like data the earlier
    // allEq() would pass vacuously; this asserts the mismatch is caught, so the correctness checks
    // above are load-bearing.
    const wrongRef = new Float32Array(N);
    for (let i = 0; i < N; i++) wrongRef[i] = a[i] + b[i] + 1.0;
    ok(allEq(got, wrongRef) === false, 'negative control: allEq rejects wrong reference (a+b+1)');
    ok(allFeq(got, wrongRef) === false, 'negative control: allFeq rejects wrong reference (a+b+1)');

    // negative control #2: corrupt one real device-output element and confirm the element-wise
    // check flags exactly that index against the untouched correct reference.
    const corrupt = Float32Array.from(got);
    corrupt[123] = corrupt[123] + 7.0;
    ok(allEq(corrupt, ref) === false, 'negative control: single corrupted element flagged vs reference');
    let flaggedIdx = -1;
    for (let i = 0; i < N; i++) { if (corrupt[i] !== ref[i]) { flaggedIdx = i; break; } }
    ok(flaggedIdx === 123, 'negative control: correctness check pinpoints the corrupted index');
  }

  // --- saxpy: alpha=3 -------------------------------------------------------------------------
  const k = 3.0;
  queue.writeBuffer(pbuf, 0, packParams(k, N));
  dispatch(saxpyPipe, bind);
  {
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = k * a[i] + b[i];
    ok(allFeq(got, ref), 'saxpy c==3*a+b (every element, tol)');
    ok(allEq(got, ref), 'saxpy exact f32 match');
  }

  // --- saxpy alpha=0 -> c == b (edge) ---------------------------------------------------------
  queue.writeBuffer(pbuf, 0, packParams(0.0, N));
  dispatch(saxpyPipe, bind);
  {
    const got = await readBack(bufC);
    ok(allEq(got, b), 'saxpy alpha=0 c==b (every element)');
  }

  // --- partial n: only first half written; tail keeps prior alpha=0 result (==b) --------------
  const half = N / 2;
  queue.writeBuffer(pbuf, 0, packParams(5.0, half));
  dispatch(saxpyPipe, bind);
  {
    const got = await readBack(bufC);
    const ref = Float32Array.from(b);
    for (let i = 0; i < half; i++) ref[i] = 5.0 * a[i] + b[i];
    let headOk = true;
    for (let i = 0; i < half; i++) headOk = headOk && feq(got[i], ref[i]);
    let tailOk = true;
    for (let i = half; i < N; i++) tailOk = tailOk && got[i] === b[i];
    ok(headOk, 'partial-n head c==5*a+b');
    ok(tailOk, 'partial-n tail untouched ==b');
  }

  // --- second pipeline: element-wise multiply (create-group proven clean via one scope) --------
  device.pushErrorScope('validation');
  const bgl2 = device.createBindGroupLayout({
    label: 'mul-bgl',
    entries: [storageEntry(0, true), storageEntry(1, true), storageEntry(2, false)],
  });
  const pll2 = device.createPipelineLayout({ label: 'mul-pll', bindGroupLayouts: [bgl2] });
  const mulPipe = device.createComputePipeline({
    label: 'mul-pipe',
    layout: pll2,
    compute: { module: mulMod, entryPoint: 'main' },
  });
  const bind2 = device.createBindGroup({
    label: 'mul-bind',
    layout: bgl2,
    entries: [
      { binding: 0, resource: { buffer: bufA } },
      { binding: 1, resource: { buffer: bufB } },
      { binding: 2, resource: { buffer: bufC } },
    ],
  });
  const mulCreateErr = await device.popErrorScope();
  ok(mulCreateErr == null, 'mul create-group (layout+pipeline+bind) raises no validation error');
  dispatch(mulPipe, bind2);
  {
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = a[i] * b[i];
    ok(allEq(got, ref), 'mul c==a*b (every element)');
    ok(got[7] === a[7] * b[7], 'mul element[7]');
  }

  // --- buffer update then re-dispatch (writeBuffer to a STORAGE buffer) ------------------------
  const a2 = new Float32Array(N).fill(4.0);
  queue.writeBuffer(bufA, 0, a2);
  queue.writeBuffer(pbuf, 0, packParams(1.0, N));
  dispatch(saxpyPipe, bind);
  {
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = 4.0 + b[i];
    ok(allEq(got, ref), 'vadd after writeBuffer c==4+b (every element)');
  }

  // --- copy_buffer_to_buffer chain: c -> mid -> staging ---------------------------------------
  const mid = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
  {
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 0, mid, 0, NBYTES);
    queue.submit([enc.finish()]);
    const got = await readBack(mid);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = 4.0 + b[i];
    ok(allEq(got, ref), 'copy chain preserves c (every element)');
  }

  // --- windowed copy: skip element 0, copy the tail into staging and verify -------------------
  {
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 4, staging, 0, (N - 1) * 4);
    queue.submit([enc.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const win = new Float32Array(staging.getMappedRange(0, (N - 1) * 4).slice(0));
    staging.unmap();
    const ref = new Float32Array(N - 1);
    for (let i = 0; i < N - 1; i++) ref[i] = 4.0 + b[i + 1];
    ok(win.length === N - 1, 'windowed copy size');
    ok(allEq(win, ref), 'windowed copy values (offset 1)');
  }

  // --- clearBuffer then verify zeros ----------------------------------------------------------
  {
    const enc = device.createCommandEncoder();
    enc.clearBuffer(bufC);
    queue.submit([enc.finish()]);
    const got = await readBack(bufC);
    const zeros = new Float32Array(N);
    ok(allEq(got, zeros), 'clearBuffer zeros c');
  }

  // --- mappedAtCreation write path verified end to end via a copy -----------------------------
  {
    const seeded = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_SRC, mappedAtCreation: true });
    const view = new Float32Array(seeded.getMappedRange());
    for (let i = 0; i < N; i++) view[i] = i + 100.0;
    seeded.unmap();
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(seeded, 0, staging, 0, NBYTES);
    queue.submit([enc.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const got = new Float32Array(staging.getMappedRange().slice(0));
    staging.unmap();
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = i + 100.0;
    ok(allEq(got, ref), 'mappedAtCreation values (every element)');
  }

  // --- validation error surfaces on a bad bind-group (missing bindings) -----------------------
  // Dawn defers bind-group validation to an async error scope rather than throwing synchronously.
  {
    device.pushErrorScope('validation');
    device.createBindGroup({
      layout: bgl2,
      entries: [{ binding: 0, resource: { buffer: bufA } }],
    });
    const err = await device.popErrorScope();
    ok(err != null, 'bad bind-group (missing bindings) reports validation error');
    ok(err != null && /entr|bind|match/i.test(err.message), 'validation error message mentions entries');
  }

  // --- validation error on out-of-bounds copy -------------------------------------------------
  {
    device.pushErrorScope('validation');
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 0, staging, 0, NBYTES * 4);
    queue.submit([enc.finish()]);
    const err = await device.popErrorScope();
    ok(err != null, 'oversized copyBufferToBuffer reports validation error');
  }

  // --- create-success proof via error scope (no validation error on a valid create) -----------
  // Instead of asserting a handle is non-null, wrap a real create in a validation scope and assert
  // it popped clean - a genuine check the object was built without a deferred validation error.
  {
    device.pushErrorScope('validation');
    const probeBgl = device.createBindGroupLayout({
      label: 'probe-bgl',
      entries: [storageEntry(0, false)],
    });
    const scopeErr = await device.popErrorScope();
    ok(scopeErr == null, 'valid createBindGroupLayout raises no validation error in scope');
    void probeBgl;
  }

  // --- shader compile-error path: broken WGSL -------------------------------------------------
  // A genuinely invalid module surfaces (a) a deferred validation error via the scope and
  // (b) an "error" message from getCompilationInfo(). Assert the real diagnostics, not ok(true).
  {
    device.pushErrorScope('validation');
    const badMod = device.createShaderModule({ label: 'broken', code: BROKEN_WGSL });
    const compileErr = await device.popErrorScope();
    ok(compileErr != null, 'broken WGSL surfaces a validation error via error scope');
    const ci = await badMod.getCompilationInfo();
    const errMsgs = ci.messages.filter((m) => m.type === 'error');
    ok(errMsgs.length >= 1, 'getCompilationInfo reports >=1 error message for broken WGSL');
    ok(errMsgs.some((m) => m.message.length > 0), 'compilation error message is non-empty');
  }

  // control: a valid module reports zero error diagnostics from getCompilationInfo
  {
    const goodCi = await saxpyMod.getCompilationInfo();
    ok(goodCi.messages.filter((m) => m.type === 'error').length === 0,
      'getCompilationInfo reports no errors for valid WGSL');
  }

  // --- createComputePipelineAsync (async pipeline creation, then run it) -----------------------
  const add1Mod = device.createShaderModule({ label: 'add1', code: ADD1_WGSL });
  const add1Bgl = device.createBindGroupLayout({
    label: 'add1-bgl',
    entries: [storageEntry(0, true), storageEntry(1, false)],
  });
  const add1Pll = device.createPipelineLayout({ bindGroupLayouts: [add1Bgl] });
  const add1Pipe = await device.createComputePipelineAsync({
    label: 'add1-async',
    layout: add1Pll,
    compute: { module: add1Mod, entryPoint: 'main' },
  });
  {
    // run the async pipeline: c = a + 1, element-wise verified against an independent reference
    const src = new Float32Array(N);
    for (let i = 0; i < N; i++) src[i] = i * 0.25 - 3.0;
    const inBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(inBuf, 0, src);
    const outBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const bgAdd = device.createBindGroup({
      layout: add1Bgl,
      entries: [{ binding: 0, resource: { buffer: inBuf } }, { binding: 1, resource: { buffer: outBuf } }],
    });
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(add1Pipe);
    pass.setBindGroup(0, bgAdd);
    pass.dispatchWorkgroups(Math.ceil(N / 256));
    pass.end();
    queue.submit([enc.finish()]);
    const got = await readBack(outBuf);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = src[i] + 1.0;
    ok(allEq(got, ref), 'async pipeline c==a+1 (every element)');
    inBuf.destroy();
    outBuf.destroy();
  }

  // --- dispatchWorkgroupsIndirect: workgroup count sourced from an INDIRECT buffer -------------
  {
    const src = new Float32Array(N);
    for (let i = 0; i < N; i++) src[i] = 10.0 + i;
    const inBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(inBuf, 0, src);
    const outBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const bgAdd = device.createBindGroup({
      layout: add1Bgl,
      entries: [{ binding: 0, resource: { buffer: inBuf } }, { binding: 1, resource: { buffer: outBuf } }],
    });
    const groups = Math.ceil(N / 256);
    const indirect = device.createBuffer({ size: 12, usage: GPUBufferUsage.INDIRECT | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(indirect, 0, new Uint32Array([groups, 1, 1]));
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(add1Pipe);
    pass.setBindGroup(0, bgAdd);
    pass.dispatchWorkgroupsIndirect(indirect, 0);
    pass.end();
    queue.submit([enc.finish()]);
    const got = await readBack(outBuf);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = src[i] + 1.0;
    ok(allEq(got, ref), 'dispatchWorkgroupsIndirect c==a+1 (every element)');
    ok(got[N - 1] === ref[N - 1], 'indirect dispatch reached the last element');
    inBuf.destroy();
    outBuf.destroy();
    indirect.destroy();
  }

  // --- dynamic offsets: one storage buffer holding two windows, selected via setBindGroup offset -
  {
    // Two N/2 windows packed in one buffer; a dynamic-offset bind group points binding 0 at either
    // half, and the shader writes a[i]+1 for that half. Dynamic offsets must be 256-byte aligned.
    const halfN = N / 2;
    const align = device.limits.minStorageBufferOffsetAlignment || 256;
    const stride = Math.ceil((halfN * 4) / align) * align;
    const packed = device.createBuffer({ size: stride + halfN * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    const w0 = new Float32Array(halfN);
    const w1 = new Float32Array(halfN);
    for (let i = 0; i < halfN; i++) { w0[i] = i; w1[i] = 1000 + i; }
    queue.writeBuffer(packed, 0, w0);
    queue.writeBuffer(packed, stride, w1);
    const outBuf = device.createBuffer({ size: halfN * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const dynBgl = device.createBindGroupLayout({
      label: 'dyn-bgl',
      entries: [
        { binding: 0, visibility: GPUShaderStage.COMPUTE, buffer: { type: 'read-only-storage', hasDynamicOffset: true, minBindingSize: halfN * 4 } },
        storageEntry(1, false),
      ],
    });
    const dynPll = device.createPipelineLayout({ bindGroupLayouts: [dynBgl] });
    const dynPipe = device.createComputePipeline({ layout: dynPll, compute: { module: add1Mod, entryPoint: 'main' } });
    const dynBg = device.createBindGroup({
      layout: dynBgl,
      entries: [
        { binding: 0, resource: { buffer: packed, offset: 0, size: halfN * 4 } },
        { binding: 1, resource: { buffer: outBuf } },
      ],
    });
    const runWindow = async (dynOff, window) => {
      const enc = device.createCommandEncoder();
      const pass = enc.beginComputePass();
      pass.setPipeline(dynPipe);
      pass.setBindGroup(0, dynBg, [dynOff]);
      pass.dispatchWorkgroups(Math.ceil(halfN / 256));
      pass.end();
      enc.copyBufferToBuffer(outBuf, 0, staging, 0, halfN * 4);
      queue.submit([enc.finish()]);
      await staging.mapAsync(GPUMapMode.READ);
      const out = new Float32Array(staging.getMappedRange(0, halfN * 4).slice(0));
      staging.unmap();
      let good = true;
      for (let i = 0; i < halfN; i++) good = good && out[i] === window[i] + 1.0;
      return good;
    };
    ok(await runWindow(0, w0), 'dynamic offset window 0 c==a+1');
    ok(await runWindow(stride, w1), 'dynamic offset window 1 (offset) c==a+1');
    packed.destroy();
    outBuf.destroy();
  }

  // --- timestamp querySet + resolveQuerySet (feature-gated; NON-COUNTING when unsupported) ------
  if (hasTimestamp) {
    const qset = device.createQuerySet({ type: 'timestamp', count: 2 });
    ok(qset.count === 2, 'createQuerySet(timestamp) count===2');
    ok(qset.type === 'timestamp', 'querySet type is timestamp');
    const resolveBuf = device.createBuffer({ size: 16, usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC });
    const tsStaging = device.createBuffer({ size: 16, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    device.pushErrorScope('validation');
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass({
      timestampWrites: { querySet: qset, beginningOfPassWriteIndex: 0, endOfPassWriteIndex: 1 },
    });
    pass.setPipeline(saxpyPipe);
    pass.setBindGroup(0, bind);
    pass.dispatchWorkgroups(GROUPS);
    pass.end();
    enc.resolveQuerySet(qset, 0, 2, resolveBuf, 0);
    enc.copyBufferToBuffer(resolveBuf, 0, tsStaging, 0, 16);
    queue.submit([enc.finish()]);
    const tsErr = await device.popErrorScope();
    ok(tsErr == null, 'timestampWrites + resolveQuerySet raise no validation error');
    await tsStaging.mapAsync(GPUMapMode.READ);
    const ts = new BigUint64Array(tsStaging.getMappedRange().slice(0));
    tsStaging.unmap();
    // lavapipe may report zero-delta timestamps; assert they were written and are ordered/equal.
    ok(ts.length === 2 && ts[1] >= ts[0], 'resolved timestamps are ordered (end>=begin)');
    qset.destroy();
  } else {
    console.log('webgpu-js NON-COUNTING: timestamp-query feature unavailable on this adapter, skipping querySet path');
  }

  // --- boundary: dispatchWorkgroups(0) is a no-op ----------------------------------------------
  {
    const src = new Float32Array(N);
    for (let i = 0; i < N; i++) src[i] = 42.0 + i;
    const inBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(inBuf, 0, src);
    const outBuf = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
    // pre-seed output with a sentinel so a no-op dispatch leaves it verifiably untouched
    const sentinel = new Float32Array(N).fill(-1.0);
    queue.writeBuffer(outBuf, 0, sentinel);
    const bgAdd = device.createBindGroup({
      layout: add1Bgl,
      entries: [{ binding: 0, resource: { buffer: inBuf } }, { binding: 1, resource: { buffer: outBuf } }],
    });
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(add1Pipe);
    pass.setBindGroup(0, bgAdd);
    pass.dispatchWorkgroups(0);
    pass.end();
    queue.submit([enc.finish()]);
    const got = await readBack(outBuf);
    ok(allEq(got, sentinel), 'dispatchWorkgroups(0) leaves output untouched (==sentinel)');
    inBuf.destroy();
    outBuf.destroy();
  }

  // --- boundary: large N (>= 1<<20) multi-workgroup grid, every element verified ----------------
  {
    const NL = 1 << 20;
    const src = new Float32Array(NL);
    for (let i = 0; i < NL; i++) src[i] = (i % 4096) * 0.5;
    const inBuf = device.createBuffer({ size: NL * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(inBuf, 0, src);
    const outBuf = device.createBuffer({ size: NL * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const bgAdd = device.createBindGroup({
      layout: add1Bgl,
      entries: [{ binding: 0, resource: { buffer: inBuf } }, { binding: 1, resource: { buffer: outBuf } }],
    });
    const groups = Math.ceil(NL / 256);
    ok(groups <= device.limits.maxComputeWorkgroupsPerDimension, 'large-N grid within maxComputeWorkgroupsPerDimension');
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(add1Pipe);
    pass.setBindGroup(0, bgAdd);
    pass.dispatchWorkgroups(groups);
    pass.end();
    const bigStaging = device.createBuffer({ size: NL * 4, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    enc.copyBufferToBuffer(outBuf, 0, bigStaging, 0, NL * 4);
    queue.submit([enc.finish()]);
    await bigStaging.mapAsync(GPUMapMode.READ);
    const got = new Float32Array(bigStaging.getMappedRange().slice(0));
    bigStaging.unmap();
    let mism = 0;
    for (let i = 0; i < NL; i++) { if (got[i] !== src[i] + 1.0) { mism++; } }
    ok(got.length === NL, 'large-N readback length == 1<<20');
    ok(mism === 0, 'large-N c==a+1 for all 1048576 elements');
    ok(got[NL - 1] === src[NL - 1] + 1.0, 'large-N last element correct');
    inBuf.destroy();
    outBuf.destroy();
    bigStaging.destroy();
  }

  // --- boundary: zero-size buffer is a valid (empty) allocation --------------------------------
  {
    const zb = device.createBuffer({ size: 0, usage: GPUBufferUsage.STORAGE });
    ok(zb.size === 0, 'zero-size buffer reports size===0');
    zb.destroy();
  }

  // --- boundary/error: getMappedRange out of range throws OperationError -----------------------
  {
    const m = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    await m.mapAsync(GPUMapMode.READ);
    let threw = false;
    try {
      m.getMappedRange(0, NBYTES * 4);
    } catch (e) {
      threw = true;
    }
    ok(threw, 'getMappedRange with out-of-range size throws');
    m.unmap();
    m.destroy();
  }

  // --- lifecycle: buffer.destroy then use-after-destroy surfaces a validation error -------------
  {
    const victim = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    victim.destroy();
    device.pushErrorScope('validation');
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 0, victim, 0, NBYTES);
    queue.submit([enc.finish()]);
    const err = await device.popErrorScope();
    ok(err != null, 'use-after-destroy (copy into destroyed buffer) reports validation error');
    ok(err != null && /destroy/i.test(err.message), 'use-after-destroy error message mentions destroyed');
  }

  // drain and confirm the queue is idle
  await queue.onSubmittedWorkDone();

  // --- lifecycle teardown: destroy remaining buffers, then device.destroy + device.lost --------
  bufA.destroy();
  bufB.destroy();
  bufC.destroy();
  pbuf.destroy();
  mid.destroy();
  staging.destroy();
  device.destroy();
  const lost = await device.lost;
  ok(lost != null, 'device.lost resolves after device.destroy');
  ok(lost.reason === 'destroyed', "device.lost reason is 'destroyed'");

  return finish();
}

run()
  .then((code) => process.exit(code))
  .catch((e) => {
    process.stderr.write('FATAL: ' + (e && e.stack ? e.stack : e) + '\n');
    process.exit(1);
  });
