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
val SAXPY = "struct Params{alpha:f32,n:u32};@group(0)@binding(0)var<storage,read>a:array<f32>;@group(0)@binding(1)var<storage,read>b:array<f32>;@group(0)@binding(2)var<storage,read_write>c:array<f32>;@group(0)@binding(3)var<uniform>p:Params;@compute @workgroup_size(64)fn main(@builtin(global_invocation_id)g:vec3<u32>){let i=g.x;if(i<p.n){c[i]=p.alpha*a[i]+b[i];}}"
suspend fun run() {
    val webgpu = require("webgpu")
    js("Object").assign(globalThis, webgpu.globals)
    val gpu = webgpu.create(js("[]"))
    val adapterReq: dynamic = js("({})"); adapterReq.powerPreference = "low-power"
    val adapter = awaitDyn(gpu.requestAdapter(adapterReq)); mk("adapter")
    val device = awaitDyn(adapter.requestDevice(js("({requiredLimits:{maxComputeInvocationsPerWorkgroup:64}})"))); mk("device")
    device.pushErrorScope("validation"); mk("push")
    val md: dynamic = js("({})"); md.code = SAXPY
    val mod = device.createShaderModule(md); mk("mod")
    val bgl = device.createBindGroupLayout(js("({entries:[{binding:0,visibility:4,buffer:{type:'read-only-storage'}},{binding:1,visibility:4,buffer:{type:'read-only-storage'}},{binding:2,visibility:4,buffer:{type:'storage'}},{binding:3,visibility:4,buffer:{type:'uniform'}}]})"))
    val plld: dynamic = js("({})"); plld.bindGroupLayouts = arrayOf(bgl)
    val pll = device.createPipelineLayout(plld)
    val cs: dynamic = js("({})"); cs.module = mod; cs.entryPoint = "main"
    val pd: dynamic = js("({})"); pd.layout = pll; pd.compute = cs
    val pipe = device.createComputePipeline(pd); mk("pipe")
    val err = awaitDyn(device.popErrorScope()); mk("pop err=" + err)
    mk("KPOP ALL OK")
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
