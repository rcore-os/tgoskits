'use strict';
const awaitVia = require('./await.js').awaitVia;
const { create, globals } = require('webgpu');
Object.assign(globalThis, globals);
function mk(s){ process.stderr.write("J: "+s+"\n"); }
const SAXPY="struct Params{alpha:f32,n:u32};@group(0)@binding(0)var<storage,read>a:array<f32>;@group(0)@binding(1)var<storage,read>b:array<f32>;@group(0)@binding(2)var<storage,read_write>c:array<f32>;@group(0)@binding(3)var<uniform>p:Params;@compute @workgroup_size(64)fn main(@builtin(global_invocation_id)g:vec3<u32>){let i=g.x;if(i<p.n){c[i]=p.alpha*a[i]+b[i];}}";
const gpu=create([]);
awaitVia(gpu.requestAdapter({powerPreference:'low-power'}),(adapter)=>{ mk("adapter");
 awaitVia(adapter.requestDevice({requiredLimits:{maxComputeInvocationsPerWorkgroup:64}}),(device)=>{ mk("device");
  device.pushErrorScope("validation");
  const mod=device.createShaderModule({code:SAXPY});
  const bgl=device.createBindGroupLayout({entries:[{binding:0,visibility:4,buffer:{type:'read-only-storage'}},{binding:1,visibility:4,buffer:{type:'read-only-storage'}},{binding:2,visibility:4,buffer:{type:'storage'}},{binding:3,visibility:4,buffer:{type:'uniform'}}]});
  const pll=device.createPipelineLayout({bindGroupLayouts:[bgl]});
  const pipe=device.createComputePipeline({layout:pll,compute:{module:mod,entryPoint:'main'}}); mk("pipe");
  awaitVia(device.popErrorScope(),(err)=>{ mk("pop err="+err); mk("JSCPS ALL OK"); process.exit(0); },(e)=>{mk("ERR "+e);process.exit(1);});
 },(e)=>{mk("ERR "+e);process.exit(1);});
},(e)=>{mk("ERR "+e);process.exit(1);});
