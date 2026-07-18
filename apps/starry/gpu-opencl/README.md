# gpu-opencl

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
| rv | none in Alpine edge 2026-07 | no - TEST PASSED vacuously (no binary built) |
| la | none in Alpine edge 2026-07 | no - TEST PASSED vacuously (no binary built) |

On arches where `libOpenCL` is absent at build time, `prebuild.sh` does not produce `opencl_c` or
`opencl_cpp` binaries. `run_all.sh` skips absent binaries and exits 0 with `TEST PASSED`; the
prebuild log documents the absence explicitly.

## What the gate builds and runs

`prebuild.sh` + `run_all.sh` build and run two native C and C++ cells that link against the Alpine
musl `opencl-icd-loader` or `pocl` `libOpenCL`:

| cell | assertions | on-target |
| --- | --- | --- |
| `opencl_c` | 168 | x64, aa (present and gated) |
| `opencl_cpp` | 54 | x64, aa (present and gated) |

Any cell that is present (binary built) must exit 0 and print its `<name> OK <n>` marker;
otherwise `TEST FAILED`. Absent cells (binary not built, la/rv) are skipped silently.

## Other binding sources in this tree

`programs/carpets/` also carries Python, Rust, and clvk sources. Their language runtimes and
native toolchains (pocl from `POCL_PREBUILT` only, clvk's clspv+LLVM) are not part of the musl
on-target provisioning, so `prebuild.sh` and `run_all.sh` do not build or run them on-target.
They are exercised host-side during development:

| cell | assertions | binding | note |
| --- | --- | --- | --- |
| `opencl_py_full_api.py` | 83 | pyopencl | host-only |
| `opencl_rust/` | 102 | opencl3 crate | host-only |
| `clvk_c/` | 65 | CL/cl.h via clvk | host-only; routes OpenCL through Vulkan lavapipe via clspv |

`clvk_c/` asserts `CL_PLATFORM_NAME`/`CL_PLATFORM_VENDOR` == `"clvk"` (rejecting pocl) to prove
the CL calls route through Vulkan, then runs vector-add / element-multiply / saxpy / local-memory
reduction with tail-guard, oversubscription and negative controls. Its clspv+LLVM toolchain is not
provisioned on-target.

## Run

```
cargo xtask starry app qemu -t gpu-opencl --arch x86_64
cargo xtask starry app qemu -t gpu-opencl --arch aarch64
cargo xtask starry app qemu -t gpu-opencl --arch riscv64
cargo xtask starry app qemu -t gpu-opencl --arch loongarch64
```
