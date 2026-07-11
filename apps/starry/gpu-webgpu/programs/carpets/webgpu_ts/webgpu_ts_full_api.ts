// webgpu_ts_full_api - full WebGPU compute-API carpet on Mesa lavapipe, driven by the dawn-based
// `webgpu` node package, written in TypeScript and type-checked with @webgpu/types. Walks the WebGPU
// object graph - gpu / adapter (features/limits/info) / device (limits/features/lost) / queue /
// shader-module (createShaderModule + getCompilationInfo + a broken-WGSL compile-error path) /
// buffer (mappedAtCreation / mapAsync / getMappedRange / unmap / writeBuffer / mapState / destroy) /
// bind-group-layout / pipeline-layout / compute-pipeline (sync + createComputePipelineAsync) /
// bind-group (static + dynamic offsets) / command-encoder / compute-pass / dispatchWorkgroups /
// dispatchWorkgroupsIndirect / copyBufferToBuffer / clearBuffer / querySet+timestampWrites+
// resolveQuerySet (feature-gated) / pushErrorScope+popErrorScope / device.destroy - with every
// handle typed (GPUAdapter, GPUDevice, GPUBuffer, GPUComputePipeline, GPUBindGroup, ...) and asserts
// vadd/saxpy/mul results per element against a TypeScript-computed reference. A negative-control
// block corrupts real device output and proves the equality checkers flag it against an independent
// reference. Boundary cases cover dispatchWorkgroups(0), a zero-byte buffer, and a large element-wise
// verified run. Prints "WEBGPU_TS_FULL_API OK <n>" only when every assertion passes and the count
// equals the pinned total.

import { create, globals } from 'webgpu';

Object.assign(globalThis, globals);

let PASS = 0;
let FAIL = 0;

function ok(cond: boolean, desc: string): void {
  if (cond) {
    PASS += 1;
  } else {
    FAIL += 1;
    process.stderr.write('FAIL: ' + desc + '\n');
  }
}

// f32 rounding makes scaled results inexact vs a double reference; use a relative tolerance there and
// exact equality for the +/* cases that round-trip through f32 identically.
function feq(a: number, b: number): boolean {
  return Math.abs(a - b) <= 1e-4 * (1.0 + Math.abs(b));
}

function allEq(got: Float32Array, ref: Float32Array): boolean {
  if (got.length !== ref.length) return false;
  for (let i = 0; i < ref.length; i++) {
    if (got[i] !== ref[i]) return false;
  }
  return true;
}

function allFeq(got: Float32Array, ref: Float32Array): boolean {
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

const N = 2048;
const WG = 64;
const NBYTES = N * 4;
const GROUPS = Math.ceil(N / WG);

function packParams(alpha: number, n: number): ArrayBuffer {
  const buf = new ArrayBuffer(16);
  new Float32Array(buf, 0, 1)[0] = alpha;
  new Uint32Array(buf, 4, 1)[0] = n;
  return buf;
}

function storageEntry(binding: number, readOnly: boolean): GPUBindGroupLayoutEntry {
  return {
    binding,
    visibility: GPUShaderStage.COMPUTE,
    buffer: { type: readOnly ? 'read-only-storage' : 'storage' },
  };
}

function finish(): number {
  const expected = 77;
  const total = PASS + FAIL;
  console.log(
    'webgpu-ts: PASS=' + PASS + ' FAIL=' + FAIL + ' TOTAL=' + total + ' EXPECTED=' + expected
  );
  if (FAIL === 0 && total === expected) {
    console.log('WEBGPU_TS_FULL_API OK ' + PASS);
    return 0;
  }
  console.log('WEBGPU_TS_FULL_API FAIL');
  return 1;
}

async function run(): Promise<number> {
  // --- gpu entry + adapter --------------------------------------------------------------------
  const gpu: GPU = create([]);
  ok(gpu != null, 'create([]) returns gpu');
  ok(typeof gpu.requestAdapter === 'function', 'gpu.requestAdapter is function');

  const adapter: GPUAdapter | null = await gpu.requestAdapter({ powerPreference: 'low-power' });
  if (adapter == null) {
    ok(false, 'requestAdapter non-null');
    return finish();
  }

  const info: GPUAdapterInfo = adapter.info;
  console.log(
    'webgpu-ts adapter selected: vendor=' + info.vendor +
    ' architecture=' + info.architecture +
    ' device=' + info.device +
    ' description=' + info.description
  );
  ok(info != null, 'adapter.info present');
  ok(String(info.description || info.device || '').length > 0, 'adapter.info description/device non-empty');

  // adapter capability queries
  const feats: GPUSupportedFeatures = adapter.features;
  ok(typeof feats.has === 'function', 'adapter.features is set-like');
  const featCount = [...feats].length;
  ok(featCount === feats.size, 'adapter.features spread length equals set size');
  const alim: GPUSupportedLimits = adapter.limits;
  ok(alim.maxComputeWorkgroupSizeX >= 64, 'adapter.limits maxComputeWorkgroupSizeX>=64');
  ok(alim.maxStorageBuffersPerShaderStage >= 3, 'adapter.limits maxStorageBuffersPerShaderStage>=3');
  ok(alim.maxBindGroups >= 1, 'adapter.limits maxBindGroups>=1');
  ok(alim.maxComputeInvocationsPerWorkgroup >= 64, 'adapter.limits maxComputeInvocationsPerWorkgroup>=64');

  // --- device + queue -------------------------------------------------------------------------
  // opt into timestamp-query when the adapter advertises it so the query-set path below is live.
  const hasTimestamp = adapter.features.has('timestamp-query');
  const wantFeatures: GPUFeatureName[] = hasTimestamp ? ['timestamp-query'] : [];
  const device: GPUDevice = await adapter.requestDevice({ label: 'carpet-device', requiredFeatures: wantFeatures });
  ok(device != null, 'requestDevice non-null');
  const dlim: GPUSupportedLimits = device.limits;
  ok(dlim.maxComputeInvocationsPerWorkgroup >= 64, 'device.limits maxComputeInvocationsPerWorkgroup>=64');
  ok(typeof device.features.has === 'function', 'device.features is set-like');
  const queue: GPUQueue = device.queue;
  ok(queue != null, 'device.queue present');
  ok(typeof queue.writeBuffer === 'function', 'queue.writeBuffer is function');

  // surface any uncaptured validation/oom error loudly
  device.addEventListener('uncapturederror', (ev: Event) => {
    const uerr = (ev as GPUUncapturedErrorEvent).error;
    process.stderr.write('UNCAPTURED webgpu error: ' + uerr.message + '\n');
  });

  // --- CPU reference data ---------------------------------------------------------------------
  const a = new Float32Array(N);
  const b = new Float32Array(N);
  for (let i = 0; i < N; i++) {
    a[i] = i * 0.5;
    b[i] = 2.0 * i + 1.0;
  }

  // seed a STORAGE|COPY_DST buffer by writing its mapped range at creation time
  function makeSeeded(data: Float32Array, extraUsage: GPUBufferUsageFlags): GPUBuffer {
    const buf: GPUBuffer = device.createBuffer({
      size: NBYTES,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | extraUsage,
      mappedAtCreation: true,
    });
    new Float32Array(buf.getMappedRange()).set(data);
    buf.unmap();
    return buf;
  }

  // --- buffers --------------------------------------------------------------------------------
  const bufA: GPUBuffer = makeSeeded(a, GPUBufferUsage.COPY_SRC);
  ok(bufA.size === NBYTES, 'buffer A size');
  ok((bufA.usage & GPUBufferUsage.STORAGE) !== 0, 'buffer A usage has STORAGE');
  const bufB: GPUBuffer = makeSeeded(b, 0);
  ok(bufB.size === NBYTES, 'buffer B size');

  const bufC: GPUBuffer = device.createBuffer({
    size: NBYTES,
    usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST,
  });
  ok(bufC.size === NBYTES, 'buffer C size');
  ok((bufC.usage & GPUBufferUsage.COPY_SRC) !== 0, 'buffer C usage has COPY_SRC');

  const pbuf: GPUBuffer = device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
  ok((pbuf.usage & GPUBufferUsage.UNIFORM) !== 0, 'params buffer usage has UNIFORM');

  const staging: GPUBuffer = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
  ok((staging.usage & GPUBufferUsage.MAP_READ) !== 0, 'staging buffer usage has MAP_READ');

  // --- shader modules + saxpy layout/pipeline/bind, all under one validation scope -------------
  // Dawn returns non-null handles even on a deferred validation error, so a bare handle!=null is
  // vacuous. Wrap the whole saxpy create-group in a single error scope and assert it popped clean;
  // every created object is exercised downstream and its correctness verified by the compute result.
  device.pushErrorScope('validation');
  const saxpyMod: GPUShaderModule = device.createShaderModule({ label: 'saxpy', code: SAXPY_WGSL });
  const mulMod: GPUShaderModule = device.createShaderModule({ label: 'mul', code: MUL_WGSL });
  const bgl: GPUBindGroupLayout = device.createBindGroupLayout({
    label: 'saxpy-bgl',
    entries: [
      storageEntry(0, true),
      storageEntry(1, true),
      storageEntry(2, false),
      { binding: 3, visibility: GPUShaderStage.COMPUTE, buffer: { type: 'uniform' } },
    ],
  });
  const pll: GPUPipelineLayout = device.createPipelineLayout({ label: 'saxpy-pll', bindGroupLayouts: [bgl] });
  const saxpyPipe: GPUComputePipeline = device.createComputePipeline({
    label: 'saxpy-pipe',
    layout: pll,
    compute: { module: saxpyMod, entryPoint: 'main' },
  });
  const bind: GPUBindGroup = device.createBindGroup({
    label: 'saxpy-bind',
    layout: bgl,
    entries: [
      { binding: 0, resource: { buffer: bufA } },
      { binding: 1, resource: { buffer: bufB } },
      { binding: 2, resource: { buffer: bufC } },
      { binding: 3, resource: { buffer: pbuf } },
    ],
  });
  ok((await device.popErrorScope()) == null, 'saxpy create-group (modules+layout+pipeline+bind) raises no validation error');

  // copy staging -> mapAsync(READ) -> Float32Array copy -> unmap. Returns a fresh Float32Array.
  async function readBack(src: GPUBuffer): Promise<Float32Array> {
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    enc.copyBufferToBuffer(src, 0, staging, 0, NBYTES);
    queue.submit([enc.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const out = new Float32Array(staging.getMappedRange().slice(0));
    staging.unmap();
    return out;
  }

  function dispatch(pipe: GPUComputePipeline, bg: GPUBindGroup): void {
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    const pass: GPUComputePassEncoder = enc.beginComputePass();
    pass.setPipeline(pipe);
    pass.setBindGroup(0, bg);
    pass.dispatchWorkgroups(GROUPS);
    pass.end();
    const cmd: GPUCommandBuffer = enc.finish();
    queue.submit([cmd]);
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

  // --- second pipeline: element-wise multiply, again under one validation scope ---------------
  device.pushErrorScope('validation');
  const bgl2: GPUBindGroupLayout = device.createBindGroupLayout({
    label: 'mul-bgl',
    entries: [storageEntry(0, true), storageEntry(1, true), storageEntry(2, false)],
  });
  const pll2: GPUPipelineLayout = device.createPipelineLayout({ label: 'mul-pll', bindGroupLayouts: [bgl2] });
  const mulPipe: GPUComputePipeline = device.createComputePipeline({
    label: 'mul-pipe',
    layout: pll2,
    compute: { module: mulMod, entryPoint: 'main' },
  });
  const bind2: GPUBindGroup = device.createBindGroup({
    label: 'mul-bind',
    layout: bgl2,
    entries: [
      { binding: 0, resource: { buffer: bufA } },
      { binding: 1, resource: { buffer: bufB } },
      { binding: 2, resource: { buffer: bufC } },
    ],
  });
  ok((await device.popErrorScope()) == null, 'mul create-group (layout+pipeline+bind) raises no validation error');
  dispatch(mulPipe, bind2);
  {
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = a[i] * b[i];
    ok(allEq(got, ref), 'mul c==a*b (every element)');
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
  const mid: GPUBuffer = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
  {
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 0, mid, 0, NBYTES);
    queue.submit([enc.finish()]);
    const got = await readBack(mid);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = 4.0 + b[i];
    ok(allEq(got, ref), 'copy chain preserves c (every element)');
  }

  // --- windowed copy: skip element 0, copy the tail into staging and verify -------------------
  {
    const enc: GPUCommandEncoder = device.createCommandEncoder();
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
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    enc.clearBuffer(bufC);
    queue.submit([enc.finish()]);
    const got = await readBack(bufC);
    const zeros = new Float32Array(N);
    ok(allEq(got, zeros), 'clearBuffer zeros c');
  }

  // --- mappedAtCreation write path verified end to end via a copy -----------------------------
  {
    const seeded: GPUBuffer = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_SRC, mappedAtCreation: true });
    const view = new Float32Array(seeded.getMappedRange());
    for (let i = 0; i < N; i++) view[i] = i + 100.0;
    seeded.unmap();
    const enc: GPUCommandEncoder = device.createCommandEncoder();
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
    const err: GPUError | null = await device.popErrorScope();
    ok(err != null, 'bad bind-group (missing bindings) reports validation error');
    ok(err != null && /entr|bind|match/i.test(err.message), 'validation error message mentions entries');
  }

  // --- validation error on out-of-bounds copy -------------------------------------------------
  {
    device.pushErrorScope('validation');
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    enc.copyBufferToBuffer(bufC, 0, staging, 0, NBYTES * 4);
    queue.submit([enc.finish()]);
    const err: GPUError | null = await device.popErrorScope();
    ok(err != null, 'oversized copyBufferToBuffer reports validation error');
  }

  // --- NEGATIVE CONTROL: prove allEq/feq actually catch a wrong answer -------------------------
  // Recompute a clean vadd on device, then corrupt exactly one element of the device output and an
  // independent CPU reference and assert the checkers FLAG the mismatch. Without this the passing
  // equality assertions above are unfalsifiable.
  queue.writeBuffer(bufA, 0, a);            // restore bufA (writeBuffer test above set it to 4.0)
  queue.writeBuffer(pbuf, 0, packParams(1.0, N));
  dispatch(saxpyPipe, bind);
  {
    const good = await readBack(bufC);      // real device output c==a+b
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = a[i] + b[i];
    ok(allEq(good, ref), 'neg-control baseline: clean device output matches reference');

    // corrupt ONE element of a copy of the real device output; an independent reference is unchanged.
    const corrupt = Float32Array.from(good);
    const j = 1234;
    corrupt[j] = good[j] + 1.0;             // a genuinely wrong value at one index
    ok(!allEq(corrupt, ref), 'neg-control: allEq flags a single corrupted output element');
    ok(!allFeq(corrupt, ref), 'neg-control: allFeq flags a single corrupted output element');

    // and the inverse: corrupt the REFERENCE instead of the output, still caught.
    const badRef = Float32Array.from(ref);
    badRef[N - 1] = ref[N - 1] - 2.0;
    ok(!allEq(good, badRef), 'neg-control: allEq flags a wrong reference vs clean output');
  }

  // --- compile-error path: malformed WGSL surfaces via getCompilationInfo ----------------------
  // A valid module first: getCompilationInfo carries zero error-type diagnostics.
  {
    const info: GPUCompilationInfo = await saxpyMod.getCompilationInfo();
    const errCount = [...info.messages].filter((m) => m.type === 'error').length;
    ok(errCount === 0, 'getCompilationInfo on valid WGSL has no error diagnostics');
  }
  // A deliberately broken shader: a real error diagnostic must surface, both through
  // getCompilationInfo() messages and through a validation error scope around the create.
  {
    device.pushErrorScope('validation');
    const broken: GPUShaderModule = device.createShaderModule({
      label: 'broken',
      code: '@compute @workgroup_size(64)\nfn main( { this is not valid wgsl ;',
    });
    const info: GPUCompilationInfo = await broken.getCompilationInfo();
    const msgs = [...info.messages];
    const errs = msgs.filter((m) => m.type === 'error');
    ok(errs.length > 0, 'broken WGSL: getCompilationInfo reports >=1 error diagnostic');
    ok(errs[0].message.length > 0, 'broken WGSL: error diagnostic carries a message');
    ok(errs[0].lineNum >= 1, 'broken WGSL: error diagnostic carries a line number');
    const scopeErr: GPUError | null = await device.popErrorScope();
    ok(scopeErr != null, 'broken WGSL: createShaderModule raises a validation error scope');
    ok(scopeErr != null && /pars|wgsl|expected/i.test(scopeErr.message), 'broken WGSL: scope message names a parse error');
  }

  // --- createComputePipelineAsync: async compile of the mul pipeline ---------------------------
  {
    device.pushErrorScope('validation');
    const asyncPipe: GPUComputePipeline = await device.createComputePipelineAsync({
      label: 'mul-pipe-async',
      layout: pll2,
      compute: { module: mulMod, entryPoint: 'main' },
    });
    const scopeErr: GPUError | null = await device.popErrorScope();
    ok(scopeErr == null, 'createComputePipelineAsync compiles without validation error');
    // drive it and check it yields the same a*b result as the sync mul pipeline.
    const asyncBind: GPUBindGroup = device.createBindGroup({
      layout: asyncPipe.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: bufA } },
        { binding: 1, resource: { buffer: bufB } },
        { binding: 2, resource: { buffer: bufC } },
      ],
    });
    dispatch(asyncPipe, asyncBind);
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = a[i] * b[i];
    ok(allEq(got, ref), 'async pipeline c==a*b (every element)');
  }

  // --- dispatchWorkgroupsIndirect: same result as a direct dispatch ---------------------------
  {
    const indirect: GPUBuffer = device.createBuffer({
      size: 12,
      usage: GPUBufferUsage.INDIRECT | GPUBufferUsage.COPY_DST,
      mappedAtCreation: true,
    });
    new Uint32Array(indirect.getMappedRange()).set([GROUPS, 1, 1]);
    indirect.unmap();
    ok((indirect.usage & GPUBufferUsage.INDIRECT) !== 0, 'indirect buffer usage has INDIRECT');

    queue.writeBuffer(pbuf, 0, packParams(2.0, N)); // c = 2a + b
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    const pass: GPUComputePassEncoder = enc.beginComputePass();
    pass.setPipeline(saxpyPipe);
    pass.setBindGroup(0, bind);
    pass.dispatchWorkgroupsIndirect(indirect, 0);
    pass.end();
    queue.submit([enc.finish()]);
    const got = await readBack(bufC);
    const ref = new Float32Array(N);
    for (let i = 0; i < N; i++) ref[i] = 2.0 * a[i] + b[i];
    ok(allFeq(got, ref), 'indirect dispatch c==2a+b (every element)');
    ok(allEq(got, ref), 'indirect dispatch exact f32 match');
  }

  // --- dynamic-offset bind group: one uniform buffer, two param blocks selected by offset ------
  {
    const dynWgsl = `
struct P { off: f32, n: u32 };
@group(0) @binding(0) var<storage, read>       src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;
@group(0) @binding(2) var<uniform>             q: P;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i < q.n) { dst[i] = src[i] + q.off; }
}`;
    const dynMod: GPUShaderModule = device.createShaderModule({ label: 'dyn', code: dynWgsl });
    const dynBgl: GPUBindGroupLayout = device.createBindGroupLayout({
      label: 'dyn-bgl',
      entries: [
        storageEntry(0, true),
        storageEntry(1, false),
        { binding: 2, visibility: GPUShaderStage.COMPUTE, buffer: { type: 'uniform', hasDynamicOffset: true } },
      ],
    });
    const dynPll: GPUPipelineLayout = device.createPipelineLayout({ bindGroupLayouts: [dynBgl] });
    device.pushErrorScope('validation');
    const dynPipe: GPUComputePipeline = device.createComputePipeline({
      label: 'dyn-pipe',
      layout: dynPll,
      compute: { module: dynMod, entryPoint: 'main' },
    });
    ok((await device.popErrorScope()) == null, 'dynamic-offset pipeline builds without validation error');

    // two param blocks, aligned to minUniformBufferOffsetAlignment, in one buffer.
    const align = Math.max(device.limits.minUniformBufferOffsetAlignment, 16);
    const dpbuf: GPUBuffer = device.createBuffer({ size: align * 2, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(dpbuf, 0, packParams(10.0, N));
    queue.writeBuffer(dpbuf, align, packParams(100.0, N));
    const dynBind: GPUBindGroup = device.createBindGroup({
      layout: dynBgl,
      entries: [
        { binding: 0, resource: { buffer: bufA } },
        { binding: 1, resource: { buffer: bufC } },
        { binding: 2, resource: { buffer: dpbuf, size: 16 } },
      ],
    });

    async function dynDispatch(dynOff: number): Promise<Float32Array> {
      const enc: GPUCommandEncoder = device.createCommandEncoder();
      const pass: GPUComputePassEncoder = enc.beginComputePass();
      pass.setPipeline(dynPipe);
      pass.setBindGroup(0, dynBind, [dynOff]);
      pass.dispatchWorkgroups(GROUPS);
      pass.end();
      queue.submit([enc.finish()]);
      return readBack(bufC);
    }
    const g0 = await dynDispatch(0);
    const ref0 = new Float32Array(N);
    for (let i = 0; i < N; i++) ref0[i] = a[i] + 10.0;
    ok(allEq(g0, ref0), 'dynamic offset 0 -> dst==src+10 (every element)');
    const g1 = await dynDispatch(align);
    const ref1 = new Float32Array(N);
    for (let i = 0; i < N; i++) ref1[i] = a[i] + 100.0;
    ok(allEq(g1, ref1), 'dynamic offset align -> dst==src+100 (every element)');
    ok(g0[3] !== g1[3], 'dynamic offset actually selects a different param block');
  }

  // --- zero-size boundaries: dispatchWorkgroups(0) is a no-op; a 0-byte buffer is valid ---------
  {
    // seed a known pattern, then a zero-workgroup dispatch must leave it untouched.
    queue.writeBuffer(bufA, 0, a);
    queue.writeBuffer(pbuf, 0, packParams(7.0, N));
    dispatch(saxpyPipe, bind);            // c = 7a + b
    const before = await readBack(bufC);
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    const pass: GPUComputePassEncoder = enc.beginComputePass();
    pass.setPipeline(saxpyPipe);
    pass.setBindGroup(0, bind);
    pass.dispatchWorkgroups(0);           // zero workgroups: no invocation runs
    pass.end();
    queue.submit([enc.finish()]);
    const after = await readBack(bufC);
    ok(allEq(after, before), 'dispatchWorkgroups(0) leaves output unchanged');

    device.pushErrorScope('validation');
    const zeroBuf: GPUBuffer = device.createBuffer({ size: 0, usage: GPUBufferUsage.STORAGE });
    ok((await device.popErrorScope()) == null, 'zero-byte buffer creates without validation error');
    ok(zeroBuf.size === 0, 'zero-byte buffer reports size 0');
    zeroBuf.destroy();
  }

  // --- large run: verify EVERY element against a closed-form reference -------------------------
  // NON-COUNTING ceiling note: this Mesa lavapipe build (25.2.8 / LLVM 20) aborts the process
  // ("futex facility returned an unexpected error code" / SIGSEGV) on any single storage buffer
  // whose map crosses ~512KB (>=131072 f32). A >=1,000,000-element single dispatch therefore
  // cannot run here - it is a backend limit, not a carpet-logic limit. We verify element-wise at
  // the largest size this backend maps reliably (BIG f32 = 256KB, 10/10 stable after full state).
  console.log('webgpu-ts NON-COUNTING: lavapipe aborts on >~512KB buffer maps; large run capped at BIG f32');
  {
    const BIG = 65536;
    const bigBytes = BIG * 4;
    const bigGroups = Math.ceil(BIG / WG);
    const ba = new Float32Array(BIG);
    const bb = new Float32Array(BIG);
    for (let i = 0; i < BIG; i++) {
      ba[i] = (i % 4096) * 0.25;          // bounded so f32 stays exact for a*b
      bb[i] = i % 2048;
    }
    // writeBuffer (not mappedAtCreation) + a full queue drain before mapAsync is the map path this
    // backend handles without aborting.
    const bigA: GPUBuffer = device.createBuffer({ size: bigBytes, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(bigA, 0, ba);
    const bigB: GPUBuffer = device.createBuffer({ size: bigBytes, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
    queue.writeBuffer(bigB, 0, bb);
    const bigC: GPUBuffer = device.createBuffer({ size: bigBytes, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const bigBind: GPUBindGroup = device.createBindGroup({
      layout: mulPipe.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: bigA } },
        { binding: 1, resource: { buffer: bigB } },
        { binding: 2, resource: { buffer: bigC } },
      ],
    });
    await queue.onSubmittedWorkDone();
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    const pass: GPUComputePassEncoder = enc.beginComputePass();
    pass.setPipeline(mulPipe);
    pass.setBindGroup(0, bigBind);
    pass.dispatchWorkgroups(bigGroups);
    pass.end();
    const bigStaging: GPUBuffer = device.createBuffer({ size: bigBytes, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    enc.copyBufferToBuffer(bigC, 0, bigStaging, 0, bigBytes);
    queue.submit([enc.finish()]);
    await queue.onSubmittedWorkDone();
    await bigStaging.mapAsync(GPUMapMode.READ);
    const got = new Float32Array(bigStaging.getMappedRange().slice(0));
    bigStaging.unmap();
    ok(got.length === BIG, 'large run readback length == 65536');
    let mism = -1;
    for (let i = 0; i < BIG; i++) {
      if (got[i] !== ba[i] * bb[i]) { mism = i; break; }
    }
    ok(mism === -1, 'large run: all 65536 elements == a*b (element-wise verified)');
    bigA.destroy();
    bigB.destroy();
    bigC.destroy();
    bigStaging.destroy();
  }

  // --- query-set / timestamp path (feature-gated) ---------------------------------------------
  if (hasTimestamp) {
    const qset: GPUQuerySet = device.createQuerySet({ type: 'timestamp', count: 2 });
    ok(qset.type === 'timestamp', 'createQuerySet type==timestamp');
    ok(qset.count === 2, 'createQuerySet count==2');
    const resolve: GPUBuffer = device.createBuffer({ size: 16, usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC });
    const tsRead: GPUBuffer = device.createBuffer({ size: 16, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const enc: GPUCommandEncoder = device.createCommandEncoder();
    const pass: GPUComputePassEncoder = enc.beginComputePass({
      timestampWrites: { querySet: qset, beginningOfPassWriteIndex: 0, endOfPassWriteIndex: 1 },
    });
    pass.setPipeline(saxpyPipe);
    pass.setBindGroup(0, bind);
    pass.dispatchWorkgroups(GROUPS);
    pass.end();
    enc.resolveQuerySet(qset, 0, 2, resolve, 0);
    enc.copyBufferToBuffer(resolve, 0, tsRead, 0, 16);
    queue.submit([enc.finish()]);
    await tsRead.mapAsync(GPUMapMode.READ);
    const ts = new BigUint64Array(tsRead.getMappedRange().slice(0));
    tsRead.unmap();
    ok(ts[1] >= ts[0], 'timestamp end >= begin (monotonic)');
    ok(ts[0] > 0n || ts[1] > 0n, 'timestamp values non-zero');
    qset.destroy();
  } else {
    console.log('webgpu-ts NON-COUNTING: timestamp-query feature unavailable on this backend');
  }

  // --- buffer.mapState transitions -------------------------------------------------------------
  {
    const mb: GPUBuffer = device.createBuffer({ size: NBYTES, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    ok(mb.mapState === 'unmapped', 'mapState initial == unmapped');
    const p = mb.mapAsync(GPUMapMode.READ);
    ok(mb.mapState === 'pending', 'mapState during mapAsync == pending');
    await p;
    ok(mb.mapState === 'mapped', 'mapState after mapAsync == mapped');
    mb.unmap();
    ok(mb.mapState === 'unmapped', 'mapState after unmap == unmapped');
    mb.destroy();
  }

  // drain and confirm the queue is idle
  await queue.onSubmittedWorkDone();

  // --- explicit resource cleanup: buffer.destroy then device.destroy + device.lost -------------
  // buffer.destroy is idempotent-safe here; a destroyed buffer's usage/size stay queryable.
  bufA.destroy();
  ok(bufA.size === NBYTES, 'buffer.destroy leaves size queryable');
  mid.destroy();
  ok(mid.mapState === 'unmapped', 'buffer.destroy leaves mid mapState==unmapped');
  // device.destroy is terminal - it must be the last device operation. It resolves device.lost
  // with reason 'destroyed'.
  device.destroy();
  const lost: GPUDeviceLostInfo = await device.lost;
  ok(lost.reason === 'destroyed', 'device.destroy resolves device.lost with reason destroyed');
  ok(lost.message.length > 0, 'device.lost carries a non-empty message');

  return finish();
}

run()
  .then((code) => process.exit(code))
  .catch((e: unknown) => {
    const err = e as Error;
    process.stderr.write('FATAL: ' + (err && err.stack ? err.stack : String(e)) + '\n');
    process.exit(1);
  });
