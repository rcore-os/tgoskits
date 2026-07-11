import kotlin.coroutines.*
external fun require(module: String): dynamic
external val globalThis: dynamic
external val process: dynamic
val awaitHelper: dynamic = require("./await.js")
suspend fun awaitDyn(p: dynamic): dynamic = suspendCoroutine { cont ->
    awaitHelper.awaitVia(p, { v: dynamic -> cont.resume(v) },
        { e: dynamic -> cont.resumeWithException((e as? Throwable) ?: RuntimeException("$e")) })
}
fun mk(s: String) { process.stderr.write("K: $s\n") }
val SAXPY = "@group(0)@binding(0)var<storage,read_write>c:array<f32>;@compute @workgroup_size(64)fn main(@builtin(global_invocation_id)g:vec3<u32>){c[g.x]=1.0;}"
suspend fun run() {
    val webgpu = require("webgpu")
    js("Object").assign(globalThis, webgpu.globals)
    val gpu = webgpu.create(js("[]"))
    val adapter = awaitDyn(gpu.requestAdapter(js("({powerPreference:'low-power'})"))); mk("adapter")
    val device = awaitDyn(adapter.requestDevice(js("({})"))); mk("device")
    val md: dynamic = js("({})"); md.code = SAXPY
    val mod = device.createShaderModule(md); mk("mod")
    val bgl = device.createBindGroupLayout(js("({entries:[{binding:0,visibility:4,buffer:{type:'storage'}}]})"))
    val plld: dynamic = js("({})"); plld.bindGroupLayouts = arrayOf(bgl)
    val cs: dynamic = js("({})"); cs.module = mod; cs.entryPoint = "main"
    val pd: dynamic = js("({})"); pd.layout = device.createPipelineLayout(plld); pd.compute = cs
    val pipe = device.createComputePipeline(pd); mk("pipe")
    // await a TRIVIAL promise unrelated to Dawn (Promise.resolve(42))
    val v = awaitDyn(js("Promise.resolve(42)")); mk("resolved v=" + v)
    mk("KRES ALL OK")
}
fun main() {
    val block: suspend () -> Unit = ::run
    block.startCoroutine(object : Continuation<Unit> {
        override val context = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) {
            result.onFailure { e -> process.stderr.write("FATAL: " + (e.asDynamic().stack ?: e.message) + "\n"); process.exit(1) }
        }
    })
}
