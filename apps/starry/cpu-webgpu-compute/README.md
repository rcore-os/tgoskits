# cpu-webgpu-compute - WebGPU compute-API carpet (JS / TS, Deno on-target)

WebGPU compute cells (JavaScript + TypeScript) that drive the WebGPU API - adapter, device, buffers,
shader modules, compute pipelines, bind groups, command encoders, error scopes, timestamp queries -
and check every operator per element against a reference computed independently in the same language.
They run **on StarryOS via Deno**, whose built-in WebGPU is **gfx-rs wgpu-core** landing on **Mesa
lavapipe** (software Vulkan on the CPU, no GPU required).

## The two WebGPU engines (why this app is the Firefox/Deno path)

There are two production WebGPU implementations, and this matters for what runs on musl:

- **Chromium's WebGPU = Dawn** (C++). Shipped as the `webgpu` npm dawn native addon, which is a
  glibc-only prebuilt, so musl Node cannot load it. Dawn's on-target home is inside the browser
  (campaign #391).
- **Firefox / Servo / Deno's WebGPU = wgpu** (gfx-rs, Rust: `wgpu-core` / `wgpu-native`). This is the
  **exact engine `cpu-wgpu-compute` #1576 already builds on musl** for all four arches. Deno embeds
  `wgpu-core` directly, so a musl Deno gives us the JS/TS WebGPU surface on-target with no new engine
  to build - we only test an existing runtime.

### Verified call chain (source + strace, not assumed)

Traced end to end - every link is either a source dependency declaration or a real `strace` `openat`:

```
navigator.gpu (Deno JS/TS global)
 -> deno_webgpu            [Deno ext/webgpu/Cargo.toml: "WebGPU implementation for Deno"]
 -> wgpu-core + wgpu-types (gfx-rs; feature "vulkan" on Unix)
 -> wgpu-hal               (hardware abstraction layer)
 -> ash (Vulkan FFI) + libloading (runtime loader)          <-- source chain ends here
 -> dlopen libvulkan.so.1                       (Vulkan loader)          <-- strace
 -> /usr/share/vulkan/icd.d/lvp_icd.json        (ICD manifest)          <-- strace
 -> libvulkan_lvp.so                            (Mesa lavapipe, sw Vulkan) <-- strace
 -> libgallium + libLLVM.so.20                  (llvmpipe CPU JIT)      <-- strace
 -> WGSL -> SPIR-V -> NIR -> LLVM -> host CPU machine code (runs on the CPU)
```

The source chain (ending at `libloading`) and the runtime chain (starting at `dlopen libvulkan.so.1`)
meet exactly at `libloading -> libvulkan.so.1`. The final native dependency is `libLLVM` (llvmpipe's
CPU JIT); there is no GPU in the path.

## Cells

| cell | file | assertions | on-target |
|------|------|-----------:|-----------|
| webgpu_js | `programs/carpets/webgpu_js/webgpu_js_full_api.js` | 78 | x86_64 + aarch64 (Deno) |
| webgpu_ts | `programs/carpets/webgpu_ts/webgpu_ts_full_api.ts` | 77 | x86_64 + aarch64 (Deno, runs .ts natively) |

Each cell prints `<NAME>_FULL_API OK <n>` only when every assertion passes and the count equals the
pinned total, together with a `PASS=<p> FAIL=<f> TOTAL=<t> EXPECTED=<e>` line. The carpets are
runtime-agnostic: on Deno (and in a browser) they use the global `navigator.gpu`; under Node they fall
back to the dawn `webgpu` package, so the same source runs on both engines.

## Arch coverage

The WebGPU **standalone JS/TS runtime** is Deno, and Deno's musl arch reach - verified against the
Alpine package DB and rusty_v8 release assets, not assumed - decides on-target coverage:

- **x86_64**: Alpine edge/community ships a **native-musl `deno`** (2.7.4-r2) -> `webgpu_js` +
  `webgpu_ts` run on-target on lavapipe.
- **aarch64**: Alpine edge/community **also** ships a native-musl `deno` (rusty_v8 v150.2.0+ carries an
  `aarch64-unknown-linux-musl` static V8 lib) -> `webgpu_js` + `webgpu_ts` run on-target, same as
  x86_64.

Both arches are advertised (`qemu-x86_64.toml`, `qemu-aarch64.toml`) and gated identically. The same
wgpu-core engine already covers four-arch WebGPU **compute** in `cpu-wgpu-compute` #1576.

## Coverage (per the WebGPU spec / @webgpu/types)

Both JS and TS cells cover the same API surface, checked against the WebGPU IDL:

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
with a relative tolerance for the scaled cases and exact equality for cases that round-trip through
f32 identically.

Negative controls prove the equality checks are load-bearing: an independent wrong reference
(`a + b + 1`) must be rejected, and a single corrupted output element must be flagged at exactly its
index. Boundary cases cover `dispatchWorkgroups(0)` (output untouched), a zero-size buffer, and an
out-of-range `getMappedRange` that throws.

## Run (host)

```
bash programs/run_all.sh            # runs webgpu_js + webgpu_ts on Deno, gates on OK markers
```

`run_all.sh` needs `deno` on PATH; it sets `VK_DRIVER_FILES` to the lavapipe ICD and
`LP_NUM_THREADS=1`, runs both cells on Deno (which runs .js and .ts natively), and prints
`TEST PASSED` only when every gated cell reports its `OK <n>` marker. Host validation:
webgpu_js 78/78, webgpu_ts 77/77 on lavapipe (adapter `llvmpipe (LLVM 20.1.2)`).

## Run (on-target)

```
cargo xtask starry app qemu -t cpu-webgpu-compute --arch x86_64
cargo xtask starry app qemu -t cpu-webgpu-compute --arch aarch64
```

The on-target run boots StarryOS (single vCPU, `-smp 1`) and runs `run-webgpu.sh`, which reads the
capability manifest `prebuild.sh` staged (the cells whose runtime was provisioned on this arch), runs
each on Deno against lavapipe, and prints `TEST PASSED` only when `fail==0` and the pass count equals
the manifest total (`EXPECTED>=2`). It never emits a 0-carpet pass.
