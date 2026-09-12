# cpu-opencl-compute

On-target test of the OpenCL compute API on StarryOS, delivered through two CPU software
implementations: `mesa-rusticl` (OpenCL over llvmpipe, available from Alpine edge on x64 and aa)
and `pocl` (portable CPU OpenCL over LLVM, available via `POCL_PREBUILT` on arches without a
`mesa-rusticl` package).

Every carpet enumerates the backend's OpenCL API surface, dispatches compute operators
(vector-add, saxpy, element multiply, local-memory reduction, sub-buffer, image + sampler,
separate compile+link, event wait chains, out-of-order queue, SVM, spec-constants) and checks
operator results against a closed-form reference. A carpet prints `<name> OK <n>` only when its
failure count is zero and the assertion total equals a pinned `EXPECTED` constant.

## Runtime availability by arch

| arch | runtime | on-target OpenCL |
| --- | --- | --- |
| x64 | mesa-rusticl (Alpine edge) | yes - pocl also available via POCL_PREBUILT |
| aa | mesa-rusticl (Alpine edge) | yes |
| rv | no Alpine rusticl; pocl via POCL_PREBUILT | gated: fails until pocl is folded |
| la | no Alpine rusticl; pocl via POCL_PREBUILT | gated: fails until pocl is folded |

Alpine ships `mesa-rusticl` for x64 and aa but not for rv/la (as of edge 2026-07). On rv/la an
OpenCL runtime is provisioned by folding a `pocl` staging tree via `POCL_PREBUILT`. When no runtime
is provisioned the carpet binaries are absent, so `run_all.sh` sees `total < EXPECTED` and prints
`TEST FAILED` - it does NOT emit a vacuous `total=0` `TEST PASSED`. Reaching four-arch green requires
the pocl provisioning on rv/la, not a relaxed gate.

## What the gate builds and runs

`prebuild.sh` + `run_all.sh` build and run the OpenCL cells listed below - the native C and C++, the
opencl3 (Rust) cell cross-compiled to a dynamic musl binary, and the PyOpenCL (Python) cell - each
against the Alpine musl `opencl-icd-loader`/`rusticl` or `pocl` `libOpenCL`:

| cell | assertions | on-target |
| --- | --- | --- |
| `opencl_c` | 168 | x64, aa (present and gated) |
| `opencl_cpp` | 54 | x64, aa (present and gated) |
| `opencl_rust` | 102 | opencl3, dynamic musl (present and gated wherever libOpenCL is provisioned) |
| `opencl_py` | 83 | PyOpenCL: `apk py3-opencl` native `_cl` extension + python3 + numpy (gated wherever the extension provisions) |
| `clvk_c` | 67 | OpenCL-over-Vulkan through clvk on lavapipe (x64; no-compiler clvk + host-precompiled SPIR-V, see below) |

The C++/Python binding availability legitimately varies by arch, so the expected cell count is a
per-arch **capability manifest** (`expected_cells`) written by `prebuild.sh` listing exactly the
cells it provisioned - not a hard-coded constant. Every cell build hard-fails in `prebuild.sh`, so a
listed cell is one that genuinely built (the manifest cannot silently under-count). `run_all.sh`
gates on that exact set: `fail==0 && total==EXPECTED && pass==EXPECTED` with an `EXPECTED>=2` floor
(the two native cells are the minimum any arch with an OpenCL runtime provisions), so an unprovisioned
runtime (rv/la without pocl) fails the gate - never a vacuous `total=0` pass.

## clvk_c: OpenCL-over-Vulkan on-target (no online compiler + precompiled SPIR-V)

`programs/carpets/clvk_c/` runs the OpenCL C API through clvk (OpenCL-over-Vulkan) on Mesa lavapipe.
clvk's online-compiler path needs clspv (top-of-tree LLVM/Clang + libclc), which has no Alpine
package and does not cross-build for musl in bounded time. The cheap path avoids LLVM entirely:

- clvk is built with `CLVK_COMPILER_AVAILABLE=OFF` - a ~3 MiB Vulkan-only OpenCL ICD that links
  `libvulkan` + `libstdc++` + SPIRV-Tools and **no LLVM/clspv**. It cross-builds cleanly for musl
  (a one-line CMake guard skips the clspv/LLVM subtree in no-compiler mode).
- the four kernels are compiled to a clvk-native SPIR-V executable binary **at build time on the
  host** (host clspv + host clvk's `clGetProgramInfo(CL_PROGRAM_BINARIES)`) and shipped in the image.
- the cell loads that binary via `clCreateProgramWithBinary` + `clBuildProgram` (a no-op "build"
  that just materialises the Vulkan pipeline from the SPIR-V), then dispatches the kernels.

It HARD-ASSERTS `CL_DEVICE_COMPILER_AVAILABLE == CL_FALSE` (the compiler is genuinely absent, so the
kernels really came from the precompiled binary), `CL_PROGRAM_BINARY_TYPE == EXECUTABLE`, and that
the platform is `"clvk"` (rejecting pocl/rusticl) so the CL calls route through Vulkan. It then runs
vector-add / element-multiply / saxpy / local-memory reduction, each checked element-by-element vs a
host reference, with tail-guard, oversubscription, a zero-dispatch no-op and negative controls.

| cell | assertions | binding | on-target |
| --- | --- | --- | --- |
| `clvk_c` | 67 | CL/cl.h via clvk (no-compiler) | x64 (via `CLVK_PREBUILT`); other arches once the no-compiler musl clvk cross-builds there |

Provisioning is gated on `CLVK_PREBUILT` pointing at a per-arch no-compiler musl clvk staging tree
(`$CLVK_PREBUILT/<arch>/usr/lib/libOpenCL.so*`) plus the host-precompiled `clvk_c_kernels.clvkbin`.
Where the musl clvk was cross-built (x64) the `clvk_c` cell is added to the manifest; elsewhere it is
skipped honestly (rusticl/pocl already cover OpenCL on-target on the other arches, so clvk_c is an
additive OpenCL-over-Vulkan cell, not a gate dependency). clvk itself is a Vulkan client, so it needs
only lavapipe + `libvulkan.so.1` at runtime (from the mesa closure `mesa-rusticl` already pulls in).

## Run

```
cargo xtask starry app qemu -t cpu-opencl-compute --arch x86_64
cargo xtask starry app qemu -t cpu-opencl-compute --arch aarch64
cargo xtask starry app qemu -t cpu-opencl-compute --arch riscv64
cargo xtask starry app qemu -t cpu-opencl-compute --arch loongarch64
```
