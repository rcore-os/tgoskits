# cpu-vulkan-compute

Per-binding Vulkan compute carpet on StarryOS. Vulkan runs as a CPU software implementation: Mesa
lavapipe (the `vulkan-swrast` driver) provides a real Vulkan compute queue over llvmpipe's LLVM CPU
JIT, so no host GPU is required. The on-target StarryOS gate builds and runs the C, C++, Rust (ash)
and Python (pyVulkan) carpets on every arch, plus the Kompute (libkompute C++) carpet on x86_64 and
aarch64. Each cell enumerates the Vulkan compute API surface against the real `vulkan_core.h` / Vulkan
spec, dispatches GLSL compute shaders and checks every result element against a numpy or closed-form
reference, and drives the error paths against real `VkResult` enums. A cell prints `<name> OK <n>`
only when its failure count is zero and the assertion total equals a pinned `EXPECTED` constant.

## Cells and assertions

| Cell | Binding | Assertions | Runs |
|:--|:--|--:|:--|
| `vulkan_c` | Vulkan C API (`vulkan/vulkan.h`) | 114 | on-target (all arches) + host |
| `vulkan_cpp` | Vulkan-Hpp (`vulkan/vulkan.hpp`) | 54 | on-target (all arches) + host |
| `vulkan_rust` | ash 0.38 | 115 | on-target (all arches) + host |
| `vulkan_py` | pyvulkan + numpy | 191 | on-target (all arches) + host |
| `kompute_cpp` | Kompute (libkompute C++ `v0.9.0`) | 69 | on-target (x86_64 / aarch64) + host |

Total: 543 assertions.

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
`vulkan-loader`, `vulkan-headers`, `glslang` / `shaderc` (GLSL to SPIR-V) and `fmt` / `fmt-dev` (for
Kompute), plus the `llvm-libs` closure lavapipe links against. Alpine edge builds `mesa-vulkan-swrast`
for all four target architectures (x86_64, aarch64, riscv64, loongarch64), so the four Vulkan language
cells run on-target on every arch. `apk` resolves the TARGET package closure under qemu-user, but the
staged Alpine gcc cannot run there (it spawns cc1/cc1plus via posix_spawn, which qemu-user cannot
exec), so `prebuild.sh` builds the C/C++ carpets HOST-side with a musl cross toolchain against the
staged sysroot. Alpine's `libvulkan.so.1` carries a `.relr.dyn` (SHT_RELR) section the older
musl-cross binutils ld rejects; the toolchain resolver probes `<triple>-gcc`/`g++` by test-linking the
staged libvulkan and falls back to `zig cc` / `zig c++ -target <triple>` (whose bundled LLD reads
`.relr.dyn`) where GNU ld fails. The Rust (ash, `linked` feature) cell is cross-compiled with cargo and
linked by the same host cross linker (`zig cc` where the GNU path would fail on `.relr.dyn` or a missing
musl `libgcc_s.so.1`) to a dynamically linked musl binary; a musl `python3` + numpy + cffi + the
vendored pyVulkan wheel provision the Python cell; the GLSL shaders are compiled to SPIR-V by the staged
`glslc` (a self-contained ELF that still runs under qemu-user); and the binaries plus the mesa closure
are staged into the per-arch rootfs. On x86_64 / aarch64 `prebuild.sh` also fetches `libkompute` (pinned
`v0.9.0`), builds its core C++ sources into a static `libkompute.a` with the same host cross C++
toolchain, and cross-compiles the `kompute_cpp` cell against it. `programs/run_all.sh` runs the injected carpets and prints
`TEST PASSED` only when all cells report `OK` and none fails (a strict `fail==0 && total==EXPECTED &&
pass==EXPECTED` gate, where `EXPECTED` is written per arch by `prebuild.sh`: 4 on riscv64 / loongarch64,
5 on x86_64 / aarch64).

Runtime environment on target:

- `XDG_RUNTIME_DIR` must point at a writable directory; lavapipe maps host-visible memory through a
  file under it.
- `VK_DRIVER_FILES` selects the lavapipe ICD; the ICD JSON's absolute `library_path` resolves
  against the rootfs root.
- `LP_NUM_THREADS=1` pins the mesa thread pool to one thread, matching StarryOS's single vCPU.

## Kompute (`kompute_cpp`)

`kompute_cpp` drives the real Kompute C++ API (`kp::Manager` / `kp::Tensor` / `kp::TensorT` /
`kp::Algorithm` / `kp::Sequence` and the `OpTensorSyncDevice` / `OpAlgoDispatch` (with push-constant
override) / `OpTensorSyncLocal` / `OpTensorCopy` operations) over lavapipe, running SAXPY /
element-wise multiply / spec-constant scale / shared-memory reduction and checking every read-back
`Tensor` element-wise against a closed-form reference (seed `0x233`), plus the resource lifecycle
transitions (`isInit` / `destroy` / `clear` / `rerecord`) and the timestamp query path. It follows the
same three-gate contract as the other cells (`fail==0 && total==EXPECTED==pass`) and includes a
negative control plus a corrupted-element check so the comparator is non-vacuous.

`prebuild.sh` builds only Kompute's core library (`KOMPUTE_OPT_LOG_LEVEL_DISABLED`, no spdlog, no
Python / gtest / benchmark) from the pinned `v0.9.0` sources; `fmt` is linked because Kompute formats
its exception strings through `fmt::format`. Kompute drives Vulkan through Vulkan-Hpp's dynamic
dispatch (`vk::DynamicLoader` dlopen()s `libvulkan.so.1` at runtime), so `kompute_cpp` is a
dynamically linked musl binary - the same dynamic-link / dlopen model as the Rust and Python cells,
which StarryOS supports (only fully static musl binaries stub dlopen). SPIR-V is precompiled by `glslc`
and passed to `kp::Manager::algorithm(...)` directly, so no shaderc / glslang is linked into the cell.

Kompute is scoped to x86_64 and aarch64. The same Vulkan compute path is exercised on every arch by
`vulkan_c` / `vulkan_cpp` / `vulkan_rust` / `vulkan_py`, which drive the Vulkan API directly.

## Host reference layer

The `vulkan_rust` (ash 0.38), `vulkan_py` (pyVulkan) and `kompute_cpp` (libkompute) cells build and run
on-target (see the tables above); on the host they run against the same lavapipe device for
cross-checking.

## Single-core execution

StarryOS runs on one vCPU (SMP is off by default), so lavapipe's llvmpipe JIT executes every
workgroup on a single thread. `run_all.sh` pins the mesa thread pool with `LP_NUM_THREADS=1` and
prints the detected CPU count, so the single-core reality is explicit in the output. The carpets
assert numerical correctness and API ordering semantics, not throughput; the results are independent
of thread count. "Multi-queue" on lavapipe (`queueCount == 1`) is exercised as asynchronous
multi-submit rather than hardware-parallel queues, and is asserted as such.

## Run

```
cargo xtask starry app qemu -t cpu-vulkan-compute --arch x86_64
cargo xtask starry app qemu -t cpu-vulkan-compute --arch aarch64
cargo xtask starry app qemu -t cpu-vulkan-compute --arch riscv64
cargo xtask starry app qemu -t cpu-vulkan-compute --arch loongarch64
```
