# gpu-webgpu - WebGPU compute-API carpet (JS / TS / Kotlin)

WebGPU compute cells that run on Node against the dawn native addon (the `webgpu` npm package), which
loads the Vulkan loader, which loads the Mesa lavapipe ICD - a software Vulkan device that executes
on the CPU, no GPU required. Each cell walks the WebGPU object graph and asserts operator results per
element against a reference computed independently in the host language.

Node, the dawn addon, and kotlinc-js are host tools with no StarryOS build, so these cells are
validated on the host by `programs/run_all.sh`. The on-target rootfs run (`programs/run-webgpu.sh`)
reports this honestly and does not fake a device or a pass count.

## Cells

| cell | file | assertions | status |
|------|------|-----------:|--------|
| webgpu_js | `programs/carpets/webgpu_js/webgpu_js_full_api.js` | 78 | host-green |
| webgpu_ts | `programs/carpets/webgpu_ts/webgpu_ts_full_api.ts` | 77 | host-green (tsc type-check + run) |
| webgpu_kotlin | `programs/carpets/webgpu_kotlin/webgpu_kotlin.kt` | 78 (source) | host wall - see below |

Each cell prints `<NAME> OK <n>` only when every assertion passes and the count equals the pinned
total. The JS and TS cells are deterministic 78/78 and 77/77 on the host lavapipe.

## Coverage (per the WebGPU spec / @webgpu/types)

Both the JS and TS cells cover the same API surface, checked against the WebGPU IDL:

- entry + adapter: `gpu.requestAdapter`, `adapter.info`, `adapter.features` (set-like), `adapter.limits`
- device + queue: `requestDevice` with `requiredFeatures` + `requiredLimits`, `device.limits`,
  `device.features`, `device.queue`, `uncapturederror` event, `device.destroy` + `device.lost`
- buffers: `createBuffer`, `mappedAtCreation` + `getMappedRange` + `unmap`, `mapAsync`, `mapState`
  transitions, `writeBuffer`, `usage`/`size` queries, `clearBuffer`, `destroy`
- shaders: `createShaderModule`, `getCompilationInfo` (error and clean cases), a broken-WGSL
  compile-error path via an error scope
- pipeline objects: `createBindGroupLayout`, `createPipelineLayout`, `createComputePipeline`,
  `createComputePipelineAsync`, `createBindGroup` (static and dynamic-offset)
- commands: `createCommandEncoder`, `beginComputePass`, `setPipeline`, `setBindGroup`,
  `dispatchWorkgroups`, `dispatchWorkgroupsIndirect`, `copyBufferToBuffer` (full + windowed)
- error scopes: `pushErrorScope` / `popErrorScope` on bad bind-group, oversized copy, use-after-destroy
- timestamp queries: `createQuerySet` + `timestampWrites` + `resolveQuerySet` (feature-gated;
  non-counting when the adapter lacks `timestamp-query`)

Operators are checked per element against a reference computed in JS / TS: vadd (`c = a + b`), saxpy
(`c = alpha*a + b`, including alpha=0 and a partial-n window), element-wise multiply (`c = a * b`),
add-one (`c = a + 1`, including an async pipeline, an indirect dispatch, and dynamic-offset windows),
and a large multi-workgroup grid (1<<20 elements, every element verified). f32 rounding is handled
with a relative tolerance for the scaled cases and exact equality for the cases that round-trip
through f32 identically.

Negative controls prove the equality checks are load-bearing: an independent wrong reference
(`a + b + 1`) must be rejected, and a single corrupted output element must be flagged at exactly its
index. Boundary cases cover `dispatchWorkgroups(0)` (output untouched), a zero-size buffer, and an
out-of-range `getMappedRange` that throws.

## webgpu_kotlin - host wall

The Kotlin cell source (`webgpu_kotlin.kt`, 78 pinned assertions) mirrors the JS/TS API surface via
`external` declarations and `dynamic` interop, and compiles with the Kotlin/JS IR backend. It is not
gated for two reasons, both documented under `programs/carpets/webgpu_kotlin/wall-evidence/`:

1. kotlinc-js is a separate host tool that is not installed on the build host here.
2. On hosts that do have kotlinc-js, the Kotlin/JS coroutine continuation crashes the dawn native
   addon (SIGSEGV / glibc pthread_mutex assertion) when it resumes across a suspend point that
   awaited a dawn-native promise and then re-enters the addon while a compute pipeline is live. The
   identical control flow in pure JS (async/await or manual CPS with the same trampoline) is stable.
   `FINDING.md` records the isolation: minimal reproducers, five deferral strategies tried, and
   controls that pass.

## Run (host)

```
cd programs/carpets/webgpu_js && npm install        # webgpu (dawn) + tsc + @webgpu/types
cd ../../.. && bash programs/run_all.sh              # runs js + ts, gates on OK markers
```

`run_all.sh` sets `VK_DRIVER_FILES` to the lavapipe ICD, `LP_NUM_THREADS=1` (single-threaded
rasterizer; the carpets assert correctness, not throughput), runs the JS cell, type-checks and
compiles the TS cell with the pinned tsc and runs it, notes the Kotlin wall, and prints `TEST PASSED`
only when every gated cell reports its `OK <n>` marker.

## Run (on-target)

```
cargo xtask starry app qemu -t gpu-webgpu --arch x86_64
cargo xtask starry app qemu -t gpu-webgpu --arch aarch64
cargo xtask starry app qemu -t gpu-webgpu --arch riscv64
cargo xtask starry app qemu -t gpu-webgpu --arch loongarch64
```

The on-target run boots StarryOS (single vCPU, `-smp 1`) and runs `run-webgpu.sh`, which reports that
the WebGPU cells are host-validated and prints `TEST PASSED`. There is no on-target device: the dawn
addon and Node have no StarryOS build.
