# gpu-wgpu - wgpu (WebGPU) compute carpet (Rust on-target + Python / C / C++ host-reference)

Runs **wgpu** (the Rust WebGPU implementation, wgpu-core / wgpu-native) on StarryOS and covers its language bindings:

- **Rust** - the `wgpu` crate (bundles wgpu-core / naga) - the on-target gate on all four arches
- **Python** - `wgpu-py` - host-reference
- **C** - wgpu-native C API (`webgpu.h` + `wgpu.h`, linked against `libwgpu_native.so`) - host-reference
- **C++** - wgpu-native C API via C++17 - host-reference

All four bindings drive wgpu's **Vulkan backend**, which lands on **lavapipe** (Mesa's software Vulkan driver, llvmpipe LLVM CPU JIT) - a WebGPU **COMPUTE** device that runs entirely on the CPU, no GPU required. Each operator is a WGSL compute shader (compiled at runtime by wgpu-core's naga), executed through the full WebGPU pipeline (bind group + compute pipeline + dispatch + buffer readback) on lavapipe. Results are checked per element against a closed-form / numpy reference with tolerance (not import-only, not happy-path-only).

> Terminology: **wgpu** is the py / rs / c / cpp side (this app); **webgpu** usually refers to the node / js / ts WebGPU bindings, delivered by a separate app (`gpu-webgpu`).

## Coverage (four bindings, 271 assertions)

| Binding | Assertions | On-target | Coverage |
|:--|:--|:--|:--|
| Rust (wgpu crate) | 60 | gate (all 4 arches) | full WebGPU object graph via `wgpu` + `pollster` + `bytemuck`, per-element checks with negative controls, validation error paths (bad bind group / malformed WGSL / destroyed buffer), indirect dispatch, clear-buffer, copy chains, mapped_at_creation, on_submitted_work_done, zero-workgroup + >=1M-element + non-divisible-tail boundaries, timestamp monotonicity note |
| Python (wgpu-py) | 95 | host-reference | full WebGPU object graph, explicit map_read/map_write, mapped_at_creation, compilation-info + malformed-WGSL error, layout='auto' reflection, async pipeline, dynamic bind-group offsets, timestamp query set, indirect dispatch, >=1M-element boundary, zero-size / zero-workgroup boundary, validation-error paths, negative control, teardown |
| C (wgpu-native C API) | 58 | host-reference | full object graph over the raw C API + WGPUFuture async callbacks, synchronous device poll, per-element numeric checks vs CPU reference, boundary and validation error paths |
| C++ (wgpu-native via C++17) | 58 | host-reference | same object graph with RAII wrappers, same per-element checks, boundary and error paths |

Operators covered per binding include vadd, saxpy (alpha=1/k/0, partial-n), elementwise multiply, buffer copy chains, windowed readback, clear-buffer, and (per binding) the extended paths listed above. Each assertion compares a computed result against a numpy or closed-form reference, a queried property against a known value, or a genuine validation error surfaced by wgpu-core / wgpu-native. Every cell prints `<NAME>_FULL_API OK <n>` together with a `PASS=<p> FAIL=<f> TOTAL=<t> EXPECTED=<e>` line, and passes only when `FAIL=0` and the count equals the pinned `EXPECTED` total.

## Bring-up on StarryOS

The on-target gate is the **Rust cell over musl lavapipe** - the same software Vulkan stack the merged `gpu-vulkan` app runs on-target on all four arches. `prebuild.sh`:

- `apk add` **mesa-vulkan-swrast** (lavapipe) + the **Vulkan loader** from Alpine edge for the target arch via qemu-user-static. Alpine builds mesa-vulkan-swrast for x86_64 / aarch64 / riscv64 / loongarch64, so lavapipe runs on every arch.
- cross-compiles the wgpu Rust carpet to `<arch>-unknown-linux-musl`. **Dynamic musl** (`-C target-feature=-crt-static`) is required: the musl default is a fully static binary whose `dlopen` is a NULL stub, so ash's runtime `dlopen("libvulkan.so.1")` returns nothing and wgpu reports "no adapter"; a dynamic-musl PIE links the real musl loader so `dlopen` resolves the staged Vulkan loader -> lavapipe. The wgpu crate builds its own wgpu-core / naga against musl, so the only runtime dependency is the Vulkan loader + lavapipe. The crate builds from a scratch copy under a fresh `CARGO_HOME` (immune to a host global cargo mirror, reproducible on a clean host) with the committed `Cargo.lock` (`--locked`).
- stages the `wgpu_rust` binary + the mesa lavapipe closure + the lavapipe ICD metadata into the overlay; `run_all.sh` sets `VK_DRIVER_FILES` to the ICD, `WGPU_BACKEND=vulkan`, `LP_NUM_THREADS=1`, runs the carpet and prints `TEST PASSED` only when it reports `WGPU_RUST_FULL_API OK <n>` and exits 0.

The **Python / C / C++ bindings are host-reference** (not staged on-target): `wgpu-py` is a conda-forge glibc build for x86_64 / aarch64 only, and gfx-rs ships `libwgpu_native.so` only as prebuilt linux-x86_64 / linux-aarch64 **glibc** (no musl, no riscv64 / loongarch64), so the C / C++ / py cells cannot cross to the musl / rv / la targets without building `libwgpu_native` from source. They are validated on the build host (see below); the on-target proof of the wgpu compute stack on StarryOS is the Rust cell.

## Run

```
cargo xtask starry app qemu -t gpu-wgpu --arch x86_64        # wgpu Rust on lavapipe
cargo xtask starry app qemu -t gpu-wgpu --arch aarch64       # wgpu Rust on lavapipe
cargo xtask starry app qemu -t gpu-wgpu --arch riscv64       # wgpu Rust on lavapipe
cargo xtask starry app qemu -t gpu-wgpu --arch loongarch64   # wgpu Rust on lavapipe
```

## Notes

- The carpet runs as a CPU software Vulkan implementation (lavapipe), single-core (`-smp 1`); this is CPU-side software-GPU testing with no GPU dependency.
- Host validation (build host, lavapipe llvmpipe): wgpu-rust 60/60, wgpu-py 95/95, wgpu-c 58/58, wgpu-cpp 58/58 (device `llvmpipe`).
- x86_64: `-cpu Haswell` (AVX2 + XSAVE for the llvmpipe LLVM JIT), `-m 4096M`. aarch64: `-cpu max`. riscv64: `-cpu rv64`. loongarch64: `-machine virt -cpu la464` (dynamic platform). All `-m 4096M`.
