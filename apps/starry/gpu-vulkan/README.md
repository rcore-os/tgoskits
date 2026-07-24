# gpu-vulkan

Per-binding Vulkan compute carpet on StarryOS. Vulkan runs as a CPU software implementation: Mesa
lavapipe (the `vulkan-swrast` driver) provides a real Vulkan compute queue over llvmpipe's LLVM CPU
JIT, so no host GPU is required. The on-target StarryOS gate builds and runs the native C and C++
carpets; the Rust (ash) and Python (pyvulkan / kompute) cells are exercised in the host reference
layer. Each cell enumerates the Vulkan compute API surface against the real `vulkan_core.h` / Vulkan
spec, dispatches GLSL compute shaders and checks every result element against a numpy or closed-form
reference, and drives the error paths against real `VkResult` enums. A cell prints `<name> OK <n>`
only when its failure count is zero and the assertion total equals a pinned `EXPECTED` constant.

## Cells and assertions

| Cell | Binding | Assertions | Runs |
|:--|:--|--:|:--|
| `vulkan_c` | Vulkan C API (`vulkan/vulkan.h`) | 114 | on-target (all arches) + host |
| `vulkan_cpp` | Vulkan-Hpp (`vulkan/vulkan.hpp`) | 54 | on-target (all arches) + host |
| `vulkan_rust` | ash 0.38 | 115 | host reference |
| `vulkan_py` | pyvulkan + numpy | 191 | host reference |
| `kompute_py` | Kompute (`kp`) + numpy | 72 | host reference (x86_64 / aarch64) |

Total: 546 assertions.

Each cell covers the compute API end to end: instance / physical-device / device / queue / buffer /
device-memory (map / flush / invalidate) / shader-module / descriptor-set-layout / pipeline-layout /
compute-pipeline / descriptor-pool / command-buffer / fence / semaphore / event / query-pool /
push-constant / dispatch / indirect-dispatch / timestamp / transfer commands, plus the core-1.1 `*2`
queries. The operators (vector-add, saxpy, element-multiply, local-memory reduction and the derived
kernels) are dispatched as real GLSL compute shaders and every output element is compared to the
closed-form / numpy reference with a relative tolerance. Boundary cases (tail guards,
oversubscription, corrupt SPIR-V, bad memory-type indices) and error paths are asserted directly;
where lavapipe has no validation layer and permits a case the assertion records it as PERMITTED or a
non-counting skip rather than faking a rejection.

## Backend and runtime

Provisioned from Alpine edge (main + community) as musl packages: `mesa-vulkan-swrast` (lavapipe),
`vulkan-loader`, `vulkan-headers` and `glslang` / `shaderc` (GLSL to SPIR-V), plus the `llvm-libs`
closure lavapipe links against. Alpine edge builds `mesa-vulkan-swrast` for all four target
architectures (x86_64, aarch64, riscv64, loongarch64), so the C and C++ carpets run on-target on
every arch. `prebuild.sh` cross-compiles them against the provisioned musl headers/libraries under
qemu-user, compiles the GLSL shaders to SPIR-V, and stages the binaries plus the mesa closure into
the per-arch rootfs. `programs/run_all.sh` runs the native carpets and prints `TEST PASSED` when
every built carpet reports `OK` and none fails.

Runtime environment on target:

- `XDG_RUNTIME_DIR` must point at a writable directory; lavapipe maps host-visible memory through a
  file under it.
- `VK_DRIVER_FILES` selects the lavapipe ICD; the ICD JSON's absolute `library_path` resolves
  against the rootfs root.
- `LP_NUM_THREADS=1` pins the mesa thread pool to one thread, matching StarryOS's single vCPU.

## Host reference layer

The Rust, Python and kompute cells run in the host reference layer only: their language runtimes
(rustc/cargo, CPython + pyvulkan/kompute) are not part of the musl on-target provisioning. On the
host they run against the same lavapipe device the on-target C/C++ cells use:

- `vulkan_rust` (ash 0.38) and `vulkan_py` (pyvulkan) load the lavapipe ICD directly.
- `kompute_py` uses the Kompute Python binding. Kompute's prebuilt binaries are glibc x86_64 /
  aarch64 only (conda-forge builds no `linux-riscv64` / `linux-loongarch64` kompute, and its sdist
  needs a full Vulkan SDK + CMake to build). Its runtime coverage is therefore host-side on x86_64 /
  aarch64; the same Vulkan compute path is covered on every arch on-target by `vulkan_c` /
  `vulkan_cpp`, which drive the raw Vulkan API rather than the kompute wrapper.

## Single-core execution

StarryOS runs on one vCPU (SMP is off by default), so lavapipe's llvmpipe JIT executes every
workgroup on a single thread. `run_all.sh` pins the mesa thread pool with `LP_NUM_THREADS=1` and
prints the detected CPU count, so the single-core reality is explicit in the output. The carpets
assert numerical correctness and API ordering semantics, not throughput; the results are independent
of thread count. "Multi-queue" on lavapipe (`queueCount == 1`) is exercised as asynchronous
multi-submit rather than hardware-parallel queues, and is asserted as such.

## Run

```
cargo xtask starry app qemu -t gpu-vulkan --arch x86_64
cargo xtask starry app qemu -t gpu-vulkan --arch aarch64
cargo xtask starry app qemu -t gpu-vulkan --arch riscv64
cargo xtask starry app qemu -t gpu-vulkan --arch loongarch64
```
