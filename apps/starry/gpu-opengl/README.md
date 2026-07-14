# gpu-opengl

Per-binding desktop-OpenGL compute carpet on StarryOS. OpenGL runs as a CPU software implementation:
Mesa llvmpipe provides a real GL 4.5 core context whose GL 4.3 compute pipeline (`glDispatchCompute`)
executes on the LLVM CPU JIT, so no host GPU is required. The on-target StarryOS gate builds and runs
the native surfaceless-EGL desktop-GL carpet (`opengl_c_egl`); the OSMesa C/C++, PyOpenGL, moderngl
and glow cells are exercised in the host reference layer. Each cell enumerates the compute-relevant GL
API surface against the real `GL/gl.h` / `GL/glcorearb.h` headers (or the binding's documented API),
dispatches GLSL 430 compute shaders and checks every result element against a numpy or closed-form
reference, and drives the error paths against real `GL_INVALID_*` enums. A cell prints `<name> OK <n>`
only when its failure count is zero and the assertion total equals a pinned `EXPECTED` constant.

## Cells and assertions

| Cell | Binding | Context | Assertions | Runs |
|:--|:--|:--|--:|:--|
| `opengl_c_egl` | GL C API + `eglGetProcAddress` loader | EGL surfaceless | 88 | on-target (all arches) + host |
| `opengl_c` | GL C API + `OSMesaGetProcAddress` loader | OSMesa off-screen | 78 | host reference |
| `opengl_cpp` | GL C++ + DSA (`glMapNamedBufferRange`, program-uniform) | OSMesa off-screen | 119 | host reference |
| `opengl_py` | PyOpenGL + numpy | OSMesa off-screen | 90 | host reference |
| `opengl_moderngl` | moderngl + numpy | standalone (llvmpipe) | 48 | host reference |
| `opengl_rust` | glow + khronos-egl | EGL surfaceless | 78 | host reference |

Total: 501 assertions.

Each cell covers the compute API end to end: surfaceless / off-screen context creation
(OSMesa / EGL) - make-current - GL version/renderer introspection - compute work-group limit queries -
GLSL 430 compute-shader compile (plus a compile-error path asserting `GL_COMPILE_STATUS == GL_FALSE`
with a non-empty info log) - program link (plus a link-error path) - SSBO create / `glBufferData` /
`glBufferStorage` / `glBindBufferBase` / `glBindBufferRange` - uniform set + read-back -
`glDispatchCompute` + `glMemoryBarrier` - `glDispatchComputeIndirect` - fence sync
(`glFenceSync` / `glClientWaitSync` / `glGetSynciv`) - timer query (`GL_TIME_ELAPSED` /
`glQueryCounter`) - map read / map write + explicit flush - `glGetBufferSubData` readback -
`glCopyBufferSubData` / `glClearBufferData` - program-resource reflection
(`glGetProgramResourceIndex` / `glGetProgramInterfaceiv` / `glGetProgramResourceiv` /
`glGetProgramResourceName`). The operators (vector-add, saxpy including `alpha=0`, element-multiply and
a shared-memory tree reduction) are dispatched as real GLSL compute shaders and every output element is
compared to the closed-form / numpy reference with a relative tolerance. Boundary cases (zero-size
dispatch left as a no-op, a non-divisible tail guard, oversubscription with an `i>=n` guard, and a
`1<<20`-element grid verified element-wise) and error paths are asserted directly against the real GL
enum (`GL_INVALID_VALUE` / `GL_INVALID_OPERATION` / `GL_INVALID_ENUM`), and each operator carries a
negative control proving the checker rejects a wrong reference.

## Backend and runtime

Provisioned from Alpine edge (main + community) as musl packages: `mesa-gl` (libGL), `mesa-egl`
(libEGL), `mesa-gles` and `mesa-dri-gallium` (the llvmpipe gallium DRI driver), plus the `llvm-libs`
closure llvmpipe links against. Alpine edge builds these for all four target architectures (x86_64,
aarch64, riscv64, loongarch64), so the surfaceless-EGL desktop-GL carpet runs on-target on every arch.
`prebuild.sh` cross-compiles `opengl_c_egl` against the provisioned musl headers/libraries under
qemu-user (the GL/glcorearb.h, EGL and KHR headers are vendored under `programs/headers`, since
Alpine's `mesa-dev` is the only package carrying `glcorearb.h` and it pulls a large clang closure the
runtime does not need), and stages the binary plus the mesa closure into the per-arch rootfs.
`programs/run_all.sh` runs the native carpet and prints `TEST PASSED` when it reports `OK` and none
fails.

Runtime environment on target:

- `EGL_PLATFORM=surfaceless` creates a desktop-GL 4.3 context with no window-system surface.
- `LIBGL_ALWAYS_SOFTWARE=1` + `GALLIUM_DRIVER=llvmpipe` pin the gallium DRI driver to the llvmpipe CPU
  software rasterizer.
- `XDG_RUNTIME_DIR` points at a writable directory.
- `LP_NUM_THREADS=1` pins the mesa thread pool to one thread, matching StarryOS's single vCPU.

## Host reference layer

Alpine ships no `mesa-osmesa` package on any arch, so `libOSMesa` is absent on-target: the OSMesa
carpets (`opengl_c`, `opengl_cpp`, `opengl_py`) run in the host reference layer only. `opengl_c_egl`
reaches the identical GL 4.3 compute surface (compile / link / SSBO / dispatch / barrier / readback /
reflection / error paths) through EGL-surfaceless instead of OSMesa, so the on-target gate covers the
desktop-GL compute path on every arch.

The moderngl and glow (Rust) cells run in the host reference layer because their language runtimes
(CPython + moderngl, rustc/cargo) are not part of the musl on-target provisioning. On the host all six
cells run against the same Mesa llvmpipe CPU driver the on-target `opengl_c_egl` cell uses:

- `opengl_py` (PyOpenGL) and `opengl_moderngl` (moderngl) bind the GL 4.3 compute API through the
  OSMesa / standalone-context loaders.
- `opengl_rust` (glow + khronos-egl) requests a GL 4.5 core context over EGL-surfaceless and drives
  the compute lifecycle through glow's safe wrappers.

## Single-core execution

StarryOS runs on one vCPU (SMP is off by default), so llvmpipe's LLVM JIT executes every workgroup on a
single thread. `run_all.sh` pins the mesa thread pool with `LP_NUM_THREADS=1` and prints the detected
CPU count, so the single-core reality is explicit in the output. The carpets assert numerical
correctness and API ordering semantics, not throughput; the results are independent of thread count.

## Run

```
cargo xtask starry app qemu -t gpu-opengl --arch x86_64
cargo xtask starry app qemu -t gpu-opengl --arch aarch64
cargo xtask starry app qemu -t gpu-opengl --arch riscv64
cargo xtask starry app qemu -t gpu-opengl --arch loongarch64
```
