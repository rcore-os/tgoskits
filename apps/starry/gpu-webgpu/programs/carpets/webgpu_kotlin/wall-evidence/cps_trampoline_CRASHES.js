'use strict';
const { create, globals } = require('webgpu');
Object.assign(globalThis, globals);
function mk(s){ process.stderr.write("C: "+s+"\n"); }
// same trampoline as the kotlin bridge
function awaitVia(promise, onOk, onErr){ (async()=>{ try{ onOk(await promise);}catch(e){onErr(e);} })(); }
// manual CPS chain mimicking a coroutine state machine
function go(done){
  const gpu = create([]);
  awaitVia(gpu.requestAdapter({powerPreference:'low-power'}), (adapter)=>{
    mk("adapter");
    awaitVia(adapter.requestDevice({requiredLimits:{maxComputeInvocationsPerWorkgroup:64}}), (device)=>{
      mk("device");
      const queue = device.queue;
      const seed=(data,extra)=>{ const b=device.createBuffer({size:8192,usage:GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST|GPUBufferUsage.COPY_SRC|extra,mappedAtCreation:true}); new Float32Array(b.getMappedRange()).set(data); b.unmap(); return b; };
      const a=new Float32Array(2048).map((_,i)=>i*0.5);
      const bufA=seed(a,GPUBufferUsage.COPY_SRC); mk("bufA");
      const mod=device.createShaderModule({code:"@group(0) @binding(0) var<storage, read> a: array<f32>;\n@group(0) @binding(1) var<storage, read_write> c: array<f32>;\n@compute @workgroup_size(64)\nfn main(@builtin(global_invocation_id) g: vec3<u32>){let i=g.x; if(i<arrayLength(&a)){c[i]=a[i]+1.0;}}"});
      const bgl=device.createBindGroupLayout({entries:[{binding:0,visibility:GPUShaderStage.COMPUTE,buffer:{type:'read-only-storage'}},{binding:1,visibility:GPUShaderStage.COMPUTE,buffer:{type:'storage'}}]});
      const pipe=device.createComputePipeline({layout:device.createPipelineLayout({bindGroupLayouts:[bgl]}),compute:{module:mod,entryPoint:'main'}});
      const bufC=device.createBuffer({size:8192,usage:GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_SRC});
      const bg=device.createBindGroup({layout:bgl,entries:[{binding:0,resource:{buffer:bufA}},{binding:1,resource:{buffer:bufC}}]});
      const staging=device.createBuffer({size:8192,usage:GPUBufferUsage.COPY_DST|GPUBufferUsage.MAP_READ});
      const enc=device.createCommandEncoder(); const pass=enc.beginComputePass();
      pass.setPipeline(pipe); pass.setBindGroup(0,bg); pass.dispatchWorkgroups(32); pass.end();
      enc.copyBufferToBuffer(bufC,0,staging,0,8192);
      queue.submit([enc.finish()]); mk("submit");
      awaitVia(staging.mapAsync(GPUMapMode.READ), ()=>{
        mk("mapAsync done");
        const out=new Float32Array(staging.getMappedRange());
        let good=true; for(let i=0;i<2048;i++){ if(out[i]!==a[i]+1){good=false;break;} }
        staging.unmap(); mk("readback correct="+good);
        awaitVia(queue.onSubmittedWorkDone(), ()=>{ mk("drained"); device.destroy(); awaitVia(device.lost,(l)=>{ mk("lost="+l.reason); done(); }, ()=>done()); }, ()=>done());
      }, ()=>done());
    }, ()=>done());
  }, ()=>done());
}
go(()=>{ mk("ALL OK"); process.exit(0); });
