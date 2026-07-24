// webgpu_kotlin - full WebGPU compute-API carpet written in Kotlin, compiled to JavaScript with the
// Kotlin/JS IR backend and run on Node against the dawn-based `webgpu` npm package on Mesa lavapipe.
// This is the "webgpu x Kotlin" cartesian cell: the exact same runtime and API surface as the JS/TS
// carpets, but driven from Kotlin via `external` declarations and `dynamic` interop.
//
// Walks the WebGPU object graph - gpu / adapter / device / queue / shader-module / buffer /
// bind-group-layout / pipeline-layout / compute-pipeline / bind-group / command-encoder /
// compute-pass / dispatch / copy-buffer-to-buffer / mapAsync - and asserts vadd/saxpy/mul results per
// element against a reference computed independently in Kotlin. Prints
// "WEBGPU_KOTLIN_FULL_API OK <n>" only when every assertion passes and the count equals the pinned
// EXPECTED total.

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.coroutines.startCoroutine
import kotlin.coroutines.suspendCoroutine

// --- Node / JS interop --------------------------------------------------------------------------
external fun require(module: String): dynamic
external val globalThis: dynamic
external val process: dynamic
external val console: dynamic
external val Math: dynamic

// Typed arrays as external classes so construction/length/set/slice are strongly typed at the call
// sites; element access goes through asDynamic() indexing.
external class Float32Array {
    constructor(length: Int)
    constructor(buffer: dynamic)
    constructor(buffer: dynamic, byteOffset: Int, length: Int)
    val length: Int
    fun set(src: dynamic)
    fun slice(begin: Int): Float32Array
}

external class Uint32Array {
    constructor(buffer: dynamic, byteOffset: Int, length: Int)
    constructor(elements: Array<Int>)
    val length: Int
}

external class BigUint64Array {
    constructor(buffer: dynamic)
    val length: Int
}

external class ArrayBuffer(size: Int)

// --- Promise -> Kotlin suspend bridge -----------------------------------------------------------
// The dawn-based `webgpu` addon resolves its promises from inside native event-processing callbacks.
// Resuming a Kotlin continuation with a raw `.then` callback re-enters Dawn (createCommandEncoder /
// submit / destroy) from within that native callback frame, which corrupts native state and
// segfaults. A native async trampoline - a real JS `async function` that `await`s the Dawn promise -
// hands the settled value back only after V8's genuine promise-job queue drains the native frame,
// exactly like plain async/await. It lives in a required .js file because the Kotlin `js()` intrinsic
// rejects async/await literals.
val awaitHelper: dynamic = require("./webgpu_kotlin_await.js")

// A `dynamic` value that is actually a JS Promise; awaiting it yields dynamic.
suspend fun awaitDyn(p: dynamic): dynamic = suspendCoroutine { cont ->
    awaitHelper.awaitVia(
        p,
        { value: dynamic -> cont.resume(value) },
        { err: dynamic -> cont.resumeWithException((err as? Throwable) ?: RuntimeException("$err")) }
    )
}

fun launch(block: suspend () -> Unit) {
    block.startCoroutine(object : Continuation<Unit> {
        override val context = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) {
            result.onFailure { e ->
                process.stderr.write("FATAL: " + (e.asDynamic().stack ?: e.message) + "\n")
                process.exit(1)
            }
        }
    })
}

// --- assertion accounting -----------------------------------------------------------------------
var PASS = 0
var FAIL = 0

fun ok(cond: Boolean, desc: String) {
    if (cond) {
        PASS += 1
    } else {
        FAIL += 1
        process.stderr.write("FAIL: $desc\n")
    }
}

// f32 rounding makes scaled results inexact vs a Kotlin Double reference; use a relative tolerance
// there and exact equality for the +/* cases that round-trip through f32 identically.
fun feq(a: Double, b: Double): Boolean = kotlin.math.abs(a - b) <= 1e-4 * (1.0 + kotlin.math.abs(b))

// Compare a device-returned Float32Array (dynamic-indexed) against a Kotlin DoubleArray reference.
fun allEq(got: Float32Array, ref: DoubleArray): Boolean {
    if (got.length != ref.size) return false
    val g = got.asDynamic()
    for (i in ref.indices) {
        val v: Double = g[i]
        if (v != ref[i]) return false
    }
    return true
}

fun allFeq(got: Float32Array, ref: DoubleArray): Boolean {
    if (got.length != ref.size) return false
    val g = got.asDynamic()
    for (i in ref.indices) {
        val v: Double = g[i]
        if (!feq(v, ref[i])) return false
    }
    return true
}

// Compare two Float32Arrays exactly (both dynamic-indexed).
fun allEqArr(got: Float32Array, ref: Float32Array): Boolean {
    if (got.length != ref.length) return false
    val g = got.asDynamic()
    val r = ref.asDynamic()
    for (i in 0 until ref.length) {
        val gv: Double = g[i]
        val rv: Double = r[i]
        if (gv != rv) return false
    }
    return true
}

fun allFeqArr(got: Float32Array, ref: Float32Array): Boolean {
    if (got.length != ref.length) return false
    val g = got.asDynamic()
    val r = ref.asDynamic()
    for (i in 0 until ref.length) {
        val gv: Double = g[i]
        val rv: Double = r[i]
        if (!feq(gv, rv)) return false
    }
    return true
}

fun ftpl(get: Float32Array, i: Int): Double = get.asDynamic()[i]

// --- WGSL sources -------------------------------------------------------------------------------
// c[i] = alpha*a[i] + b[i]; alpha and n come from a uniform block so one pipeline drives both vadd
// (alpha=1) and saxpy (alpha=k).
val SAXPY_WGSL = """
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
"""

// c[i] = a[i] * b[i]; distinct bind-group-layout (3 storage bindings) + a second pipeline.
val MUL_WGSL = """
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
"""

// Deliberately-invalid WGSL: exercises the compile-error diagnostic path.
val BROKEN_WGSL = """
@compute @workgroup_size(64)
fn main() {
  let x = ;
  totally_not_a_function(x)
}
"""

// Single-storage-binding add-one shader for the large-N grid + indirect / dynamic-offset coverage.
val ADD1_WGSL = """
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read_write> c: array<f32>;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i < arrayLength(&a)) {
    c[i] = a[i] + 1.0;
  }
}
"""

const val N = 2048
const val WG = 64
const val NBYTES = N * 4
val GROUPS: Int get() = Math.ceil(N.toDouble() / WG) as Int

// WebGPU usage/stage constants pulled from the module `globals` (assigned onto globalThis).
val GPUShaderStage: dynamic get() = globalThis.GPUShaderStage
val GPUBufferUsage: dynamic get() = globalThis.GPUBufferUsage
val GPUMapMode: dynamic get() = globalThis.GPUMapMode

// 16-byte Params uniform block: f32 alpha then u32 n.
fun packParams(alpha: Double, n: Int): ArrayBuffer {
    val buf = ArrayBuffer(16)
    val f = Float32Array(buf.asDynamic(), 0, 1)
    f.asDynamic()[0] = alpha
    val u = Uint32Array(buf.asDynamic(), 4, 1)
    u.asDynamic()[0] = n
    return buf
}

fun storageEntry(binding: Int, readOnly: Boolean): dynamic {
    val e: dynamic = js("({})")
    e.binding = binding
    e.visibility = GPUShaderStage.COMPUTE
    e.buffer = js("({})")
    e.buffer.type = if (readOnly) "read-only-storage" else "storage"
    return e
}

fun finish(): Int {
    val expected = 78
    val total = PASS + FAIL
    console.log("webgpu-kotlin: PASS=$PASS FAIL=$FAIL TOTAL=$total EXPECTED=$expected")
    return if (FAIL == 0 && total == expected) {
        console.log("WEBGPU_KOTLIN_FULL_API OK $PASS")
        0
    } else {
        console.log("WEBGPU_KOTLIN_FULL_API FAIL")
        1
    }
}

suspend fun run(): Int {
    val webgpu = require("webgpu")
    js("Object").assign(globalThis, webgpu.globals)

    // --- gpu entry + adapter --------------------------------------------------------------------
    val gpu = webgpu.create(js("[]"))
    ok(gpu != null, "create([]) returns gpu")
    ok(jsTypeOf(gpu.requestAdapter) == "function", "gpu.requestAdapter is function")

    val adapterReq: dynamic = js("({})"); adapterReq.powerPreference = "low-power"
    val adapter = awaitDyn(gpu.requestAdapter(adapterReq))
    ok(adapter != null, "requestAdapter non-null")
    if (adapter == null) return finish()

    val info = adapter.info
    console.log(
        "webgpu-kotlin adapter selected: vendor=" + info.vendor +
            " architecture=" + info.architecture +
            " device=" + info.device +
            " description=" + info.description
    )
    ok(info != null, "adapter.info present")
    val descLen: Int = js("String")(info.description ?: info.device ?: "").length
    ok(descLen > 0, "adapter.info description/device non-empty")

    // adapter capability queries
    val feats = adapter.features
    ok(jsTypeOf(feats.has) == "function", "adapter.features is set-like")
    val featSpread: dynamic = js("Array.from")(feats)
    ok((featSpread.length as Int) == (feats.size as Int), "adapter.features spread length equals set size")
    val alim = adapter.limits
    ok((alim.maxComputeWorkgroupSizeX as Int) >= 64, "adapter.limits maxComputeWorkgroupSizeX>=64")
    ok((alim.maxStorageBuffersPerShaderStage as Int) >= 3, "adapter.limits maxStorageBuffersPerShaderStage>=3")
    ok((alim.maxBindGroups as Int) >= 1, "adapter.limits maxBindGroups>=1")
    ok((alim.maxComputeInvocationsPerWorkgroup as Int) >= 64, "adapter.limits maxComputeInvocationsPerWorkgroup>=64")
    ok((alim.maxComputeWorkgroupsPerDimension as Int) >= 65535, "adapter.limits maxComputeWorkgroupsPerDimension>=65535")

    // --- device + queue -------------------------------------------------------------------------
    val hasTimestamp: Boolean = feats.has("timestamp-query") as Boolean
    val reqDevice: dynamic = js("({})")
    reqDevice.label = "carpet-device"
    reqDevice.requiredFeatures = if (hasTimestamp) js("['timestamp-query']") else js("[]")
    reqDevice.requiredLimits = js("({maxComputeInvocationsPerWorkgroup: 64})")
    val device = awaitDyn(adapter.requestDevice(reqDevice))
    ok(device != null, "requestDevice non-null")
    ok((device.limits.maxComputeInvocationsPerWorkgroup as Int) >= 64, "requiredLimits honoured (maxComputeInvocationsPerWorkgroup>=64)")
    ok(!hasTimestamp || (device.features.has("timestamp-query") as Boolean), "requiredFeatures granted: device.features has timestamp-query")
    val dlim = device.limits
    ok((dlim.maxComputeInvocationsPerWorkgroup as Int) >= 64, "device.limits maxComputeInvocationsPerWorkgroup>=64")
    ok(jsTypeOf(device.features.has) == "function", "device.features is set-like")
    val queue = device.queue
    ok(queue != null, "device.queue present")
    ok(jsTypeOf(queue.writeBuffer) == "function", "queue.writeBuffer is function")

    device.addEventListener("uncapturederror") { ev: dynamic ->
        process.stderr.write("UNCAPTURED webgpu error: " + ev.error.message + "\n")
    }

    // --- CPU reference data (computed independently in Kotlin) -----------------------------------
    val a = Float32Array(N)
    val b = Float32Array(N)
    run {
        val ad = a.asDynamic(); val bd = b.asDynamic()
        for (i in 0 until N) {
            ad[i] = i * 0.5
            bd[i] = 2.0 * i + 1.0
        }
    }

    // seed a STORAGE|COPY_DST buffer by writing its mapped range at creation time
    fun makeSeeded(data: Float32Array, extraUsage: Int): dynamic {
        val desc: dynamic = js("({})")
        desc.size = NBYTES
        desc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int) or extraUsage
        desc.mappedAtCreation = true
        val buf = device.createBuffer(desc)
        Float32Array(buf.getMappedRange()).set(data)
        buf.unmap()
        return buf
    }

    // --- buffers --------------------------------------------------------------------------------
    val bufA = makeSeeded(a, GPUBufferUsage.COPY_SRC as Int)
    ok((bufA.size as Int) == NBYTES, "buffer A size")
    ok(((bufA.usage as Int) and (GPUBufferUsage.STORAGE as Int)) != 0, "buffer A usage has STORAGE")
    val bufB = makeSeeded(b, 0)
    ok((bufB.size as Int) == NBYTES, "buffer B size")

    val bufCDesc: dynamic = js("({})")
    bufCDesc.size = NBYTES
    bufCDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int) or (GPUBufferUsage.COPY_DST as Int)
    val bufC = device.createBuffer(bufCDesc)
    ok((bufC.size as Int) == NBYTES, "buffer C size")
    ok(((bufC.usage as Int) and (GPUBufferUsage.COPY_SRC as Int)) != 0, "buffer C usage has COPY_SRC")

    val pbufDesc: dynamic = js("({})")
    pbufDesc.size = 16
    pbufDesc.usage = (GPUBufferUsage.UNIFORM as Int) or (GPUBufferUsage.COPY_DST as Int)
    val pbuf = device.createBuffer(pbufDesc)
    ok(((pbuf.usage as Int) and (GPUBufferUsage.UNIFORM as Int)) != 0, "params buffer usage has UNIFORM")

    val stagingDesc: dynamic = js("({})")
    stagingDesc.size = NBYTES
    stagingDesc.usage = (GPUBufferUsage.COPY_DST as Int) or (GPUBufferUsage.MAP_READ as Int)
    val staging = device.createBuffer(stagingDesc)
    ok(((staging.usage as Int) and (GPUBufferUsage.MAP_READ as Int)) != 0, "staging buffer usage has MAP_READ")

    // --- shader modules + saxpy pipeline objects, proven clean via a single validation scope -----
    device.pushErrorScope("validation")
    val saxpyMod = device.createShaderModule(shaderDesc("saxpy", SAXPY_WGSL))
    val mulMod = device.createShaderModule(shaderDesc("mul", MUL_WGSL))
    val bglDesc: dynamic = js("({})")
    bglDesc.label = "saxpy-bgl"
    val uniformEntry: dynamic = js("({})")
    uniformEntry.binding = 3; uniformEntry.visibility = GPUShaderStage.COMPUTE; uniformEntry.buffer = js("({type:'uniform'})")
    bglDesc.entries = arrayOf(storageEntry(0, true), storageEntry(1, true), storageEntry(2, false), uniformEntry)
    val bgl = device.createBindGroupLayout(bglDesc)
    val pll = device.createPipelineLayout(pllDesc("saxpy-pll", arrayOf(bgl)))
    val saxpyPipeDesc: dynamic = js("({})")
    saxpyPipeDesc.label = "saxpy-pipe"; saxpyPipeDesc.layout = pll
    saxpyPipeDesc.compute = computeStage(saxpyMod, "main")
    val saxpyPipe = device.createComputePipeline(saxpyPipeDesc)
    val bindDesc: dynamic = js("({})")
    bindDesc.label = "saxpy-bind"; bindDesc.layout = bgl
    bindDesc.entries = arrayOf(
        bufEntry(0, bufA), bufEntry(1, bufB), bufEntry(2, bufC), bufEntry(3, pbuf)
    )
    val bind = device.createBindGroup(bindDesc)
    val saxpyCreateErr = awaitDyn(device.popErrorScope())
    ok(saxpyCreateErr == null, "saxpy create-group (modules+layout+pipeline+bind) raises no validation error")

    // copy staging -> mapAsync(READ) -> Float32Array copy -> unmap.
    suspend fun readBack(src: dynamic): Float32Array {
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(src, 0, staging, 0, NBYTES)
        queue.submit(arrayOf(enc.finish()))
        awaitDyn(staging.mapAsync(GPUMapMode.READ))
        val out = Float32Array(staging.getMappedRange().slice(0))
        staging.unmap()
        return out
    }

    fun dispatch(pipe: dynamic, bg: dynamic) {
        val enc = device.createCommandEncoder()
        val pass = enc.beginComputePass()
        pass.setPipeline(pipe)
        pass.setBindGroup(0, bg)
        pass.dispatchWorkgroups(GROUPS)
        pass.end()
        queue.submit(arrayOf(enc.finish()))
    }

    // --- vadd: alpha=1 --------------------------------------------------------------------------
    queue.writeBuffer(pbuf, 0, packParams(1.0, N))
    dispatch(saxpyPipe, bind)
    run {
        val got = readBackSync(::readBack, bufC)
        val ref = DoubleArray(N) { i -> (i * 0.5) + (2.0 * i + 1.0) }
        ok(got.length == N, "vadd readback length")
        ok(allEq(got, ref), "vadd c==a+b (every element)")
        ok(ftpl(got, 0) == ref[0], "vadd element[0]")
        ok(ftpl(got, N / 2) == ref[N / 2], "vadd element[N/2]")
        ok(ftpl(got, N - 1) == ref[N - 1], "vadd element[N-1]")

        // negative control: independent wrong reference (a+b+1) must be REJECTED by the comparators.
        val wrongRef = DoubleArray(N) { i -> (i * 0.5) + (2.0 * i + 1.0) + 1.0 }
        ok(!allEq(got, wrongRef), "negative control: allEq rejects wrong reference (a+b+1)")
        ok(!allFeq(got, wrongRef), "negative control: allFeq rejects wrong reference (a+b+1)")

        // negative control #2: corrupt one real device-output element; element-wise check must flag it.
        val corrupt = Float32Array(got.slice(0).asDynamic())
        corrupt.asDynamic()[123] = (corrupt.asDynamic()[123] as Double) + 7.0
        ok(!allEq(corrupt, ref), "negative control: single corrupted element flagged vs reference")
        var flaggedIdx = -1
        val cd = corrupt.asDynamic()
        for (i in 0 until N) { val v: Double = cd[i]; if (v != ref[i]) { flaggedIdx = i; break } }
        ok(flaggedIdx == 123, "negative control: correctness check pinpoints the corrupted index")
    }

    // --- saxpy: alpha=3 -------------------------------------------------------------------------
    val k = 3.0
    queue.writeBuffer(pbuf, 0, packParams(k, N))
    dispatch(saxpyPipe, bind)
    run {
        val got = readBackSync(::readBack, bufC)
        val ref = DoubleArray(N) { i -> k * (i * 0.5) + (2.0 * i + 1.0) }
        ok(allFeq(got, ref), "saxpy c==3*a+b (every element, tol)")
        ok(allEq(got, ref), "saxpy exact f32 match")
    }

    // --- saxpy alpha=0 -> c == b (edge) ---------------------------------------------------------
    queue.writeBuffer(pbuf, 0, packParams(0.0, N))
    dispatch(saxpyPipe, bind)
    run {
        val got = readBackSync(::readBack, bufC)
        ok(allEqArr(got, b), "saxpy alpha=0 c==b (every element)")
    }

    // --- partial n: only first half written; tail keeps prior alpha=0 result (==b) --------------
    val half = N / 2
    queue.writeBuffer(pbuf, 0, packParams(5.0, half))
    dispatch(saxpyPipe, bind)
    run {
        val got = readBackSync(::readBack, bufC)
        val bd = b.asDynamic()
        val ref = DoubleArray(N) { i -> if (i < half) 5.0 * (i * 0.5) + (bd[i] as Double) else (bd[i] as Double) }
        var headOk = true
        for (i in 0 until half) headOk = headOk && feq(ftpl(got, i), ref[i])
        var tailOk = true
        for (i in half until N) tailOk = tailOk && (ftpl(got, i) == (bd[i] as Double))
        ok(headOk, "partial-n head c==5*a+b")
        ok(tailOk, "partial-n tail untouched ==b")
    }

    // --- second pipeline: element-wise multiply (create-group proven clean via one scope) --------
    device.pushErrorScope("validation")
    val bgl2Desc: dynamic = js("({})")
    bgl2Desc.label = "mul-bgl"
    bgl2Desc.entries = arrayOf(storageEntry(0, true), storageEntry(1, true), storageEntry(2, false))
    val bgl2 = device.createBindGroupLayout(bgl2Desc)
    val pll2 = device.createPipelineLayout(pllDesc("mul-pll", arrayOf(bgl2)))
    val mulPipeDesc: dynamic = js("({})")
    mulPipeDesc.label = "mul-pipe"; mulPipeDesc.layout = pll2
    mulPipeDesc.compute = computeStage(mulMod, "main")
    val mulPipe = device.createComputePipeline(mulPipeDesc)
    val bind2Desc: dynamic = js("({})")
    bind2Desc.label = "mul-bind"; bind2Desc.layout = bgl2
    bind2Desc.entries = arrayOf(bufEntry(0, bufA), bufEntry(1, bufB), bufEntry(2, bufC))
    val bind2 = device.createBindGroup(bind2Desc)
    val mulCreateErr = awaitDyn(device.popErrorScope())
    ok(mulCreateErr == null, "mul create-group (layout+pipeline+bind) raises no validation error")
    dispatch(mulPipe, bind2)
    run {
        val got = readBackSync(::readBack, bufC)
        val ref = DoubleArray(N) { i -> (i * 0.5) * (2.0 * i + 1.0) }
        ok(allEq(got, ref), "mul c==a*b (every element)")
        ok(ftpl(got, 7) == (7 * 0.5) * (2.0 * 7 + 1.0), "mul element[7]")
    }

    // --- buffer update then re-dispatch (writeBuffer to a STORAGE buffer) ------------------------
    val a2 = Float32Array(N)
    run { val d = a2.asDynamic(); for (i in 0 until N) d[i] = 4.0 }
    queue.writeBuffer(bufA, 0, a2)
    queue.writeBuffer(pbuf, 0, packParams(1.0, N))
    dispatch(saxpyPipe, bind)
    run {
        val got = readBackSync(::readBack, bufC)
        val bd = b.asDynamic()
        val ref = DoubleArray(N) { i -> 4.0 + (bd[i] as Double) }
        ok(allEq(got, ref), "vadd after writeBuffer c==4+b (every element)")
    }

    // --- copy_buffer_to_buffer chain: c -> mid -> staging ---------------------------------------
    val midDesc: dynamic = js("({})")
    midDesc.size = NBYTES
    midDesc.usage = (GPUBufferUsage.COPY_SRC as Int) or (GPUBufferUsage.COPY_DST as Int)
    val mid = device.createBuffer(midDesc)
    run {
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(bufC, 0, mid, 0, NBYTES)
        queue.submit(arrayOf(enc.finish()))
        val got = readBackSync(::readBack, mid)
        val bd = b.asDynamic()
        val ref = DoubleArray(N) { i -> 4.0 + (bd[i] as Double) }
        ok(allEq(got, ref), "copy chain preserves c (every element)")
    }

    // --- windowed copy: skip element 0, copy the tail into staging and verify -------------------
    run {
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(bufC, 4, staging, 0, (N - 1) * 4)
        queue.submit(arrayOf(enc.finish()))
        awaitDyn(staging.mapAsync(GPUMapMode.READ))
        val win = Float32Array(staging.getMappedRange(0, (N - 1) * 4).slice(0))
        staging.unmap()
        val bd = b.asDynamic()
        val ref = DoubleArray(N - 1) { i -> 4.0 + (bd[i + 1] as Double) }
        ok(win.length == N - 1, "windowed copy size")
        ok(allEq(win, ref), "windowed copy values (offset 1)")
    }

    // --- clearBuffer then verify zeros ----------------------------------------------------------
    run {
        val enc = device.createCommandEncoder()
        enc.clearBuffer(bufC)
        queue.submit(arrayOf(enc.finish()))
        val got = readBackSync(::readBack, bufC)
        val zeros = DoubleArray(N) { 0.0 }
        ok(allEq(got, zeros), "clearBuffer zeros c")
    }

    // --- mappedAtCreation write path verified end to end via a copy -----------------------------
    run {
        val seededDesc: dynamic = js("({})")
        seededDesc.size = NBYTES; seededDesc.usage = GPUBufferUsage.COPY_SRC; seededDesc.mappedAtCreation = true
        val seeded = device.createBuffer(seededDesc)
        val view = Float32Array(seeded.getMappedRange())
        val vd = view.asDynamic()
        for (i in 0 until N) vd[i] = i + 100.0
        seeded.unmap()
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(seeded, 0, staging, 0, NBYTES)
        queue.submit(arrayOf(enc.finish()))
        awaitDyn(staging.mapAsync(GPUMapMode.READ))
        val got = Float32Array(staging.getMappedRange().slice(0))
        staging.unmap()
        val ref = DoubleArray(N) { i -> i + 100.0 }
        ok(allEq(got, ref), "mappedAtCreation values (every element)")
    }

    // --- validation error surfaces on a bad bind-group (missing bindings) -----------------------
    run {
        device.pushErrorScope("validation")
        val badDesc: dynamic = js("({})")
        badDesc.layout = bgl2
        badDesc.entries = arrayOf(bufEntry(0, bufA))
        device.createBindGroup(badDesc)
        val err = awaitDyn(device.popErrorScope())
        ok(err != null, "bad bind-group (missing bindings) reports validation error")
        ok(err != null && js("/entr|bind|match/i").test(err.message) as Boolean, "validation error message mentions entries")
    }

    // --- validation error on out-of-bounds copy -------------------------------------------------
    run {
        device.pushErrorScope("validation")
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(bufC, 0, staging, 0, NBYTES * 4)
        queue.submit(arrayOf(enc.finish()))
        val err = awaitDyn(device.popErrorScope())
        ok(err != null, "oversized copyBufferToBuffer reports validation error")
    }

    // --- create-success proof via error scope (no validation error on a valid create) -----------
    run {
        device.pushErrorScope("validation")
        val probeDesc: dynamic = js("({})")
        probeDesc.label = "probe-bgl"; probeDesc.entries = arrayOf(storageEntry(0, false))
        device.createBindGroupLayout(probeDesc)
        val scopeErr = awaitDyn(device.popErrorScope())
        ok(scopeErr == null, "valid createBindGroupLayout raises no validation error in scope")
    }

    // --- shader compile-error path: broken WGSL -------------------------------------------------
    run {
        device.pushErrorScope("validation")
        val badMod = device.createShaderModule(shaderDesc("broken", BROKEN_WGSL))
        val compileErr = awaitDyn(device.popErrorScope())
        ok(compileErr != null, "broken WGSL surfaces a validation error via error scope")
        val ci = awaitDyn(badMod.getCompilationInfo())
        val errMsgs = ci.messages.filter({ m: dynamic -> m.type == "error" })
        ok((errMsgs.length as Int) >= 1, "getCompilationInfo reports >=1 error message for broken WGSL")
        ok(errMsgs.some({ m: dynamic -> (m.message.length as Int) > 0 }) as Boolean, "compilation error message is non-empty")
    }

    // control: a valid module reports zero error diagnostics
    run {
        val goodCi = awaitDyn(saxpyMod.getCompilationInfo())
        val errs = goodCi.messages.filter({ m: dynamic -> m.type == "error" })
        ok((errs.length as Int) == 0, "getCompilationInfo reports no errors for valid WGSL")
    }

    // --- createComputePipelineAsync (async pipeline creation, then run it) -----------------------
    val add1Mod = device.createShaderModule(shaderDesc("add1", ADD1_WGSL))
    val add1BglDesc: dynamic = js("({})")
    add1BglDesc.label = "add1-bgl"; add1BglDesc.entries = arrayOf(storageEntry(0, true), storageEntry(1, false))
    val add1Bgl = device.createBindGroupLayout(add1BglDesc)
    val add1Pll = device.createPipelineLayout(pllDesc(null, arrayOf(add1Bgl)))
    val add1PipeDesc: dynamic = js("({})")
    add1PipeDesc.label = "add1-async"; add1PipeDesc.layout = add1Pll
    add1PipeDesc.compute = computeStage(add1Mod, "main")
    val add1Pipe = awaitDyn(device.createComputePipelineAsync(add1PipeDesc))
    run {
        val src = Float32Array(N)
        val sd = src.asDynamic()
        for (i in 0 until N) sd[i] = i * 0.25 - 3.0
        val inDesc: dynamic = js("({})"); inDesc.size = NBYTES
        inDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val inBuf = device.createBuffer(inDesc)
        queue.writeBuffer(inBuf, 0, src)
        val outDesc: dynamic = js("({})"); outDesc.size = NBYTES
        outDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int)
        val outBuf = device.createBuffer(outDesc)
        val bgDesc: dynamic = js("({})"); bgDesc.layout = add1Bgl
        bgDesc.entries = arrayOf(bufEntry(0, inBuf), bufEntry(1, outBuf))
        val bgAdd = device.createBindGroup(bgDesc)
        val enc = device.createCommandEncoder()
        val pass = enc.beginComputePass()
        pass.setPipeline(add1Pipe); pass.setBindGroup(0, bgAdd)
        pass.dispatchWorkgroups(Math.ceil(N.toDouble() / 256))
        pass.end()
        queue.submit(arrayOf(enc.finish()))
        val got = readBackSync(::readBack, outBuf)
        val ref = DoubleArray(N) { i -> (i * 0.25 - 3.0) + 1.0 }
        ok(allEq(got, ref), "async pipeline c==a+1 (every element)")
        inBuf.destroy(); outBuf.destroy()
    }

    // --- dispatchWorkgroupsIndirect: workgroup count sourced from an INDIRECT buffer -------------
    run {
        val src = Float32Array(N)
        val sd = src.asDynamic()
        for (i in 0 until N) sd[i] = 10.0 + i
        val inDesc: dynamic = js("({})"); inDesc.size = NBYTES
        inDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val inBuf = device.createBuffer(inDesc)
        queue.writeBuffer(inBuf, 0, src)
        val outDesc: dynamic = js("({})"); outDesc.size = NBYTES
        outDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int)
        val outBuf = device.createBuffer(outDesc)
        val bgDesc: dynamic = js("({})"); bgDesc.layout = add1Bgl
        bgDesc.entries = arrayOf(bufEntry(0, inBuf), bufEntry(1, outBuf))
        val bgAdd = device.createBindGroup(bgDesc)
        val groups = Math.ceil(N.toDouble() / 256) as Int
        val indDesc: dynamic = js("({})"); indDesc.size = 12
        indDesc.usage = (GPUBufferUsage.INDIRECT as Int) or (GPUBufferUsage.COPY_DST as Int)
        val indirect = device.createBuffer(indDesc)
        queue.writeBuffer(indirect, 0, Uint32Array(arrayOf(groups, 1, 1)))
        val enc = device.createCommandEncoder()
        val pass = enc.beginComputePass()
        pass.setPipeline(add1Pipe); pass.setBindGroup(0, bgAdd)
        pass.dispatchWorkgroupsIndirect(indirect, 0)
        pass.end()
        queue.submit(arrayOf(enc.finish()))
        val got = readBackSync(::readBack, outBuf)
        val ref = DoubleArray(N) { i -> (10.0 + i) + 1.0 }
        ok(allEq(got, ref), "dispatchWorkgroupsIndirect c==a+1 (every element)")
        ok(ftpl(got, N - 1) == ref[N - 1], "indirect dispatch reached the last element")
        inBuf.destroy(); outBuf.destroy(); indirect.destroy()
    }

    // --- dynamic offsets: one storage buffer holding two windows, selected via setBindGroup offset -
    run {
        val halfN = N / 2
        val align = (device.limits.minStorageBufferOffsetAlignment as? Int) ?: 256
        val stride = (Math.ceil((halfN * 4).toDouble() / align) as Int) * align
        val packedDesc: dynamic = js("({})"); packedDesc.size = stride + halfN * 4
        packedDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val packed = device.createBuffer(packedDesc)
        val w0 = Float32Array(halfN); val w1 = Float32Array(halfN)
        val w0d = w0.asDynamic(); val w1d = w1.asDynamic()
        for (i in 0 until halfN) { w0d[i] = i; w1d[i] = 1000 + i }
        queue.writeBuffer(packed, 0, w0)
        queue.writeBuffer(packed, stride, w1)
        val outDesc: dynamic = js("({})"); outDesc.size = halfN * 4
        outDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int)
        val outBuf = device.createBuffer(outDesc)
        val dynBglDesc: dynamic = js("({})"); dynBglDesc.label = "dyn-bgl"
        val dynEntry0: dynamic = js("({})")
        dynEntry0.binding = 0; dynEntry0.visibility = GPUShaderStage.COMPUTE
        dynEntry0.buffer = js("({})")
        dynEntry0.buffer.type = "read-only-storage"; dynEntry0.buffer.hasDynamicOffset = true; dynEntry0.buffer.minBindingSize = halfN * 4
        dynBglDesc.entries = arrayOf(dynEntry0, storageEntry(1, false))
        val dynBgl = device.createBindGroupLayout(dynBglDesc)
        val dynPll = device.createPipelineLayout(pllDesc(null, arrayOf(dynBgl)))
        val dynPipeDesc: dynamic = js("({})"); dynPipeDesc.layout = dynPll
        dynPipeDesc.compute = computeStage(add1Mod, "main")
        val dynPipe = device.createComputePipeline(dynPipeDesc)
        val dynBgDesc: dynamic = js("({})"); dynBgDesc.layout = dynBgl
        val res0: dynamic = js("({})"); res0.buffer = packed; res0.offset = 0; res0.size = halfN * 4
        val e0: dynamic = js("({})"); e0.binding = 0; e0.resource = res0
        dynBgDesc.entries = arrayOf(e0, bufEntry(1, outBuf))
        val dynBg = device.createBindGroup(dynBgDesc)

        suspend fun runWindow(dynOff: Int, window: Float32Array): Boolean {
            val enc = device.createCommandEncoder()
            val pass = enc.beginComputePass()
            pass.setPipeline(dynPipe)
            pass.setBindGroup(0, dynBg, arrayOf(dynOff))
            pass.dispatchWorkgroups(Math.ceil(halfN.toDouble() / 256))
            pass.end()
            enc.copyBufferToBuffer(outBuf, 0, staging, 0, halfN * 4)
            queue.submit(arrayOf(enc.finish()))
            awaitDyn(staging.mapAsync(GPUMapMode.READ))
            val out = Float32Array(staging.getMappedRange(0, halfN * 4).slice(0))
            staging.unmap()
            var good = true
            val od = out.asDynamic(); val wd = window.asDynamic()
            for (i in 0 until halfN) { val ov: Double = od[i]; val wv: Double = wd[i]; good = good && (ov == wv + 1.0) }
            return good
        }
        ok(awaitBool { runWindow(0, w0) }, "dynamic offset window 0 c==a+1")
        ok(awaitBool { runWindow(stride, w1) }, "dynamic offset window 1 (offset) c==a+1")
        packed.destroy(); outBuf.destroy()
    }

    // --- timestamp querySet + resolveQuerySet (feature-gated; NON-COUNTING when unsupported) ------
    if (hasTimestamp) {
        val qsetDesc: dynamic = js("({})"); qsetDesc.type = "timestamp"; qsetDesc.count = 2
        val qset = device.createQuerySet(qsetDesc)
        ok((qset.count as Int) == 2, "createQuerySet(timestamp) count===2")
        ok(qset.type == "timestamp", "querySet type is timestamp")
        val resolveDesc: dynamic = js("({})"); resolveDesc.size = 16
        resolveDesc.usage = (GPUBufferUsage.QUERY_RESOLVE as Int) or (GPUBufferUsage.COPY_SRC as Int)
        val resolveBuf = device.createBuffer(resolveDesc)
        val tsStagingDesc: dynamic = js("({})"); tsStagingDesc.size = 16
        tsStagingDesc.usage = (GPUBufferUsage.COPY_DST as Int) or (GPUBufferUsage.MAP_READ as Int)
        val tsStaging = device.createBuffer(tsStagingDesc)
        device.pushErrorScope("validation")
        val enc = device.createCommandEncoder()
        val tw: dynamic = js("({})"); tw.querySet = qset; tw.beginningOfPassWriteIndex = 0; tw.endOfPassWriteIndex = 1
        val passDesc: dynamic = js("({})"); passDesc.timestampWrites = tw
        val pass = enc.beginComputePass(passDesc)
        pass.setPipeline(saxpyPipe); pass.setBindGroup(0, bind)
        pass.dispatchWorkgroups(GROUPS); pass.end()
        enc.resolveQuerySet(qset, 0, 2, resolveBuf, 0)
        enc.copyBufferToBuffer(resolveBuf, 0, tsStaging, 0, 16)
        queue.submit(arrayOf(enc.finish()))
        val tsErr = awaitDyn(device.popErrorScope())
        ok(tsErr == null, "timestampWrites + resolveQuerySet raise no validation error")
        awaitDyn(tsStaging.mapAsync(GPUMapMode.READ))
        val ts = BigUint64Array(tsStaging.getMappedRange().slice(0))
        tsStaging.unmap()
        val tsd = ts.asDynamic()
        val ordered = (ts.length == 2) && (js("(a,b)=>a>=b")(tsd[1], tsd[0]) as Boolean)
        ok(ordered, "resolved timestamps are ordered (end>=begin)")
        qset.destroy()
    } else {
        console.log("webgpu-kotlin NON-COUNTING: timestamp-query feature unavailable on this adapter, skipping querySet path")
    }

    // --- boundary: dispatchWorkgroups(0) is a no-op ----------------------------------------------
    run {
        val src = Float32Array(N)
        val sd = src.asDynamic()
        for (i in 0 until N) sd[i] = 42.0 + i
        val inDesc: dynamic = js("({})"); inDesc.size = NBYTES
        inDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val inBuf = device.createBuffer(inDesc)
        queue.writeBuffer(inBuf, 0, src)
        val outDesc: dynamic = js("({})"); outDesc.size = NBYTES
        outDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int) or (GPUBufferUsage.COPY_DST as Int)
        val outBuf = device.createBuffer(outDesc)
        val sentinel = Float32Array(N)
        run { val d = sentinel.asDynamic(); for (i in 0 until N) d[i] = -1.0 }
        queue.writeBuffer(outBuf, 0, sentinel)
        val bgDesc: dynamic = js("({})"); bgDesc.layout = add1Bgl
        bgDesc.entries = arrayOf(bufEntry(0, inBuf), bufEntry(1, outBuf))
        val bgAdd = device.createBindGroup(bgDesc)
        val enc = device.createCommandEncoder()
        val pass = enc.beginComputePass()
        pass.setPipeline(add1Pipe); pass.setBindGroup(0, bgAdd)
        pass.dispatchWorkgroups(0); pass.end()
        queue.submit(arrayOf(enc.finish()))
        val got = readBackSync(::readBack, outBuf)
        ok(allEqArr(got, sentinel), "dispatchWorkgroups(0) leaves output untouched (==sentinel)")
        inBuf.destroy(); outBuf.destroy()
    }

    // --- boundary: large N (>= 1<<20) multi-workgroup grid, every element verified ----------------
    run {
        val NL = 1 shl 20
        val src = Float32Array(NL)
        val sd = src.asDynamic()
        for (i in 0 until NL) sd[i] = (i % 4096) * 0.5
        val inDesc: dynamic = js("({})"); inDesc.size = NL * 4
        inDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val inBuf = device.createBuffer(inDesc)
        queue.writeBuffer(inBuf, 0, src)
        val outDesc: dynamic = js("({})"); outDesc.size = NL * 4
        outDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_SRC as Int)
        val outBuf = device.createBuffer(outDesc)
        val bgDesc: dynamic = js("({})"); bgDesc.layout = add1Bgl
        bgDesc.entries = arrayOf(bufEntry(0, inBuf), bufEntry(1, outBuf))
        val bgAdd = device.createBindGroup(bgDesc)
        val groups = Math.ceil(NL.toDouble() / 256) as Int
        ok(groups <= (device.limits.maxComputeWorkgroupsPerDimension as Int), "large-N grid within maxComputeWorkgroupsPerDimension")
        val enc = device.createCommandEncoder()
        val pass = enc.beginComputePass()
        pass.setPipeline(add1Pipe); pass.setBindGroup(0, bgAdd)
        pass.dispatchWorkgroups(groups); pass.end()
        val bigStagingDesc: dynamic = js("({})"); bigStagingDesc.size = NL * 4
        bigStagingDesc.usage = (GPUBufferUsage.COPY_DST as Int) or (GPUBufferUsage.MAP_READ as Int)
        val bigStaging = device.createBuffer(bigStagingDesc)
        enc.copyBufferToBuffer(outBuf, 0, bigStaging, 0, NL * 4)
        queue.submit(arrayOf(enc.finish()))
        awaitDyn(bigStaging.mapAsync(GPUMapMode.READ))
        val got = Float32Array(bigStaging.getMappedRange().slice(0))
        bigStaging.unmap()
        var mism = 0
        val gd = got.asDynamic()
        for (i in 0 until NL) { val gv: Double = gd[i]; if (gv != ((i % 4096) * 0.5) + 1.0) mism++ }
        ok(got.length == NL, "large-N readback length == 1<<20")
        ok(mism == 0, "large-N c==a+1 for all 1048576 elements")
        ok(ftpl(got, NL - 1) == ((NL - 1) % 4096) * 0.5 + 1.0, "large-N last element correct")
        inBuf.destroy(); outBuf.destroy(); bigStaging.destroy()
    }

    // --- boundary: zero-size buffer is a valid (empty) allocation --------------------------------
    run {
        val zbDesc: dynamic = js("({})"); zbDesc.size = 0; zbDesc.usage = GPUBufferUsage.STORAGE
        val zb = device.createBuffer(zbDesc)
        ok((zb.size as Int) == 0, "zero-size buffer reports size===0")
        zb.destroy()
    }

    // --- boundary/error: getMappedRange out of range throws --------------------------------------
    run {
        val mDesc: dynamic = js("({})"); mDesc.size = NBYTES
        mDesc.usage = (GPUBufferUsage.MAP_READ as Int) or (GPUBufferUsage.COPY_DST as Int)
        val m = device.createBuffer(mDesc)
        awaitDyn(m.mapAsync(GPUMapMode.READ))
        var threw = false
        try {
            m.getMappedRange(0, NBYTES * 4)
        } catch (e: dynamic) {
            threw = true
        }
        ok(threw, "getMappedRange with out-of-range size throws")
        m.unmap(); m.destroy()
    }

    // --- lifecycle: buffer.destroy then use-after-destroy surfaces a validation error -------------
    run {
        val victimDesc: dynamic = js("({})"); victimDesc.size = NBYTES
        victimDesc.usage = (GPUBufferUsage.STORAGE as Int) or (GPUBufferUsage.COPY_DST as Int)
        val victim = device.createBuffer(victimDesc)
        victim.destroy()
        device.pushErrorScope("validation")
        val enc = device.createCommandEncoder()
        enc.copyBufferToBuffer(bufC, 0, victim, 0, NBYTES)
        queue.submit(arrayOf(enc.finish()))
        val err = awaitDyn(device.popErrorScope())
        ok(err != null, "use-after-destroy (copy into destroyed buffer) reports validation error")
        ok(err != null && js("/destroy/i").test(err.message) as Boolean, "use-after-destroy error message mentions destroyed")
    }

    // drain and confirm the queue is idle
    awaitDyn(queue.onSubmittedWorkDone())

    // --- lifecycle teardown: destroy remaining buffers, then device.destroy + device.lost --------
    bufA.destroy(); bufB.destroy(); bufC.destroy(); pbuf.destroy(); mid.destroy(); staging.destroy()
    device.destroy()
    val lost = awaitDyn(device.lost)
    ok(lost != null, "device.lost resolves after device.destroy")
    ok(lost.reason == "destroyed", "device.lost reason is 'destroyed'")

    return finish()
}

// {binding, resource:{buffer}} entry helper.
fun bufEntry(binding: Int, buffer: dynamic): dynamic {
    val e: dynamic = js("({})")
    e.binding = binding
    e.resource = js("({})")
    e.resource.buffer = buffer
    return e
}

// {label?, code} shader-module descriptor.
fun shaderDesc(label: String, code: String): dynamic {
    val d: dynamic = js("({})")
    d.label = label
    d.code = code
    return d
}

// {label?, bindGroupLayouts:[..]} pipeline-layout descriptor.
fun pllDesc(label: String?, layouts: Array<dynamic>): dynamic {
    val d: dynamic = js("({})")
    if (label != null) d.label = label
    d.bindGroupLayouts = layouts
    return d
}

// {module, entryPoint} compute-stage descriptor.
fun computeStage(module: dynamic, entryPoint: String): dynamic {
    val d: dynamic = js("({})")
    d.module = module
    d.entryPoint = entryPoint
    return d
}

// jsTypeOf wrapper.
fun jsTypeOf(v: dynamic): String = js("typeof v")

// Bridge a `suspend (dynamic) -> Float32Array` readback inside a non-suspend `run {}` lambda scope.
// The whole run() body is one coroutine; these helpers just re-enter suspension by returning the
// awaited value directly - Kotlin allows calling suspend funcs from suspend context, and each `run {}`
// block inherits run()'s suspend context because `run` is inline.
suspend fun readBackSync(fn: suspend (dynamic) -> Float32Array, src: dynamic): Float32Array = fn(src)

suspend fun awaitBool(fn: suspend () -> Boolean): Boolean = fn()

fun main() {
    launch {
        val code = run()
        process.exit(code)
    }
}
