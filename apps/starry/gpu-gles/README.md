# gpu-gles

Per-binding OpenGL ES 3.1 compute carpet on StarryOS. GLES runs as a CPU software implementation:
Mesa llvmpipe (the LLVM CPU rasterizer/JIT) provides the GLES 3.1 compute pipeline, and EGL creates a
headless context on the surfaceless platform (`EGL_MESA_platform_surfaceless`, selected with
`EGL_PLATFORM=surfaceless`), so no host GPU or display server is required. The on-target StarryOS gate
builds and runs the native C and C++ carpets; the Python (moderngl) and Rust (glow + khronos-egl)
cells are exercised in the host reference layer. Each cell enumerates the GLES compute API surface
against the real Khronos headers (`GLES3/gl31.h`, `EGL/egl.h`), dispatches GLSL ES compute shaders and
checks every result element against a numpy or closed-form reference, and drives the error paths
against real `glGetError` / `eglGetError` enums. A cell prints `<name> OK <n>` only when its failure
count is zero and the assertion total equals a pinned `EXPECTED` constant.

## Cells and assertions

| Cell | Binding | Assertions | Runs |
|:--|:--|--:|:--|
| `gles_c` | EGL + GLESv2/GLES3 C API (`EGL/egl.h`, `GLES3/gl31.h`) | 104 | on-target (all arches) + host |
| `gles_cpp` | same C API, C++17 driver | 108 | on-target (all arches) + host |
| `gles_py` | moderngl (EGL standalone) + numpy | 60 | host reference |
| `gles_rust` | glow 0.13 + khronos-egl 6 | 68 | host reference |

Total: 340 assertions.

Each cell covers the compute path end to end: EGL surfaceless display (`eglGetDisplay` /
`eglInitialize` / `eglQueryString`), config selection (`eglChooseConfig` /
`eglGetConfigAttrib` with `EGL_RENDERABLE_TYPE = EGL_OPENGL_ES3_BIT`), API binding (`eglBindAPI` /
`eglQueryAPI`), context creation (`eglCreateContext` with `EGL_CONTEXT_MAJOR/MINOR_VERSION = 3.1`),
`eglMakeCurrent` surfaceless, compute-shader compile + program link (with the compile-error and
link-error negative paths), SSBO allocation and `glBindBufferBase` / `glBindBufferRange` binding,
uniforms, `glDispatchCompute` and `glDispatchComputeIndirect`, `glMemoryBarrier` /
`glMemoryBarrierByRegion`, `glMapBufferRange` readback (full range, partial range, mapped writes with
`glFlushMappedBufferRange`), `glCopyBufferSubData`, fence sync (`glFenceSync` / `glClientWaitSync` /
`glGetSynciv`), query objects, image load/store (`glBindImageTexture` + `glReadPixels`), compute
limits (`glGetIntegeri_v` for work-group count/size, `glGetIntegerv` for invocations and storage
blocks) and program/resource introspection (`glGetProgramResourceIndex` / `glGetProgramInterfaceiv` /
`glGetProgramResourceiv` / `glGetProgramResourceName` / `glGetActiveUniform`). GLES 3.1 has no
`glGetBufferSubData`, so all readback is via `glMapBufferRange`.

The operators (vector-add, saxpy with a uniform alpha, element-multiply, a `i*scale` UBO kernel and a
2D-index kernel) are dispatched as real GLSL ES compute shaders and every output element is compared
to the closed-form / numpy reference with a relative tolerance, followed by a negative control that
corrupts one real device-output element and asserts the same checker rejects it. Boundary cases
(zero-group dispatch, tail guards on non-multiple-of-64 sizes, `>= 1M`-element dispatches verified
element-wise, zero-size buffers) and error paths are asserted against the real enum: bad buffer
target `-> GL_INVALID_ENUM`, negative size `-> GL_INVALID_VALUE`, oversubscribed group count
`-> GL_INVALID_VALUE`, `glShaderStorageBlockBinding` on the software ES path `-> GL_INVALID_OPERATION`
(that entry point is core GLES 3.1 but absent from the `GLES3` client header, so `gles_cpp` resolves
it via `eglGetProcAddress`).

## Backend and runtime

Provisioned from Alpine edge (main + community) as musl packages: `mesa-gles` (the GLES client
library), `mesa-egl` (EGL, including the surfaceless platform), `mesa-dri-gallium` (the llvmpipe CPU
driver) and the build toolchain, plus the `llvm-libs` closure llvmpipe links against. Alpine edge
builds these for all four target architectures (x86_64, aarch64, riscv64, loongarch64), so the C and
C++ carpets run on-target on every arch. `prebuild.sh` cross-compiles them against the provisioned
musl libraries and the vendored EGL/GLES2/GLES3/KHR client headers under qemu-user, and stages the
binaries plus the mesa closure into the per-arch rootfs. `programs/run_all.sh` runs the native carpets
and prints `TEST PASSED` when every built carpet reports `OK` and none fails.

The EGL/GLES2/GLES3/KHR client headers are vendored under `programs/headers` because Alpine carries
them only in `mesa-dev`, which would pull the ~200 MiB clang-libs closure the runtime does not need.
The vendored `GLES3/gl31.h` and `EGL/egl.h` are the unmodified Khronos headers (byte-identical to the
system `/usr/include` copies used for the host build).

Runtime environment on target:

- `EGL_PLATFORM=surfaceless` selects the surfaceless EGL platform (no window-system surface).
- `GALLIUM_DRIVER=llvmpipe` / `MESA_LOADER_DRIVER_OVERRIDE=llvmpipe` select the CPU driver.
- `LP_NUM_THREADS=1` pins the mesa thread pool to one thread, matching StarryOS's single vCPU.

## Host reference layer

The Python and Rust cells run in the host reference layer only: their language runtimes (CPython +
moderngl/numpy, rustc/cargo + glow) are not part of the musl on-target provisioning. On the host they
run against the same llvmpipe device the on-target C/C++ cells use:

- `gles_py` uses moderngl's standalone context (EGL backend), which drives the same GLES 3.1 compute
  path (compute-shader compile, SSBO/UBO, single- and multi-dimensional dispatch, `run_indirect`,
  memory barrier, buffer map/read/write/clear/copy) and verifies every result element against numpy.
- `gles_rust` uses glow 0.13 over khronos-egl 6, mirroring `gles_c` (EGL surfaceless
  display/init/config/context/make-current, compute SSBOs, dispatch + barrier, indirect dispatch,
  fence sync, mapped/sub-data readback, error-injection, boundary sizes) with per-element correctness
  and negative controls.

The raw GLES-over-EGL path they exercise is covered on every arch on-target by `gles_c` / `gles_cpp`.

## Single-core execution

StarryOS runs on one vCPU (SMP is off by default), so llvmpipe's LLVM JIT executes every workgroup on
a single thread. `run_all.sh` pins the mesa thread pool with `LP_NUM_THREADS=1` and prints the
detected CPU count, so the single-core reality is explicit in the output. The carpets assert numerical
correctness and API ordering semantics, not throughput; the results are independent of thread count.

## Run

```
cargo xtask starry app qemu -t gpu-gles --arch x86_64
cargo xtask starry app qemu -t gpu-gles --arch aarch64
cargo xtask starry app qemu -t gpu-gles --arch riscv64
cargo xtask starry app qemu -t gpu-gles --arch loongarch64
```
