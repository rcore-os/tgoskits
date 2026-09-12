# cpu-opengl-render - desktop-OpenGL RENDER carpet (C++ / Python / Rust)

The rendering-track counterpart of `cpu-opengl-compute` (#1609). Where the compute app dispatches
compute shaders, this app drives the **graphics pipeline**: it creates a headless desktop-GL context
(surfaceless EGL, no window), renders into an **off-screen framebuffer** (RGBA color texture + depth
renderbuffer) and reads the pixels back with `glReadPixels`, checking **every pixel against a
closed-form reference**. The GL implementation is **Mesa llvmpipe** (the CPU software rasterizer over
the LLVM JIT) - no GPU, single vCPU (`-smp 1`).

## Cells (3 bindings)

| cell | file | binding | assertions (host) |
|------|------|---------|------:|
| opengl_render_cpp | `programs/carpets/opengl_render_cpp/` | C++ (eglGetProcAddress) | 45 |
| opengl_render_py | `programs/carpets/opengl_render_py/` | Python (moderngl + numpy) | 33 |
| opengl_render_rust | `programs/carpets/opengl_render_rust/` | Rust (glow 0.13 + khronos-egl, dynamic musl) | 40 |

Each cell prints `<NAME>_FULL_API OK <n>` only when every assertion passes and the count equals its
pinned `EXPECTED`, together with a `PASS=<p> FAIL=<f> TOTAL=<t> EXPECTED=<e>` line. The C++ cell
resolves every GL entry point (including GL 1.0 core) via `eglGetProcAddress` and links only libEGL;
the Rust cell is a dynamic-musl binary (khronos-egl `dynamic` dlopens libEGL, GL via the loader); the
Python cell uses a moderngl standalone context.

## Real-scenario cells (4 scenarios x 3 bindings)

Four additional render scenarios exercise realistic GPU workloads end to end, each in all three
bindings (C++, Rust/glow, Python/PyOpenGL). The C++ cells use the same surfaceless-EGL / GL-4.5-core /
`eglGetProcAddress` path as `opengl_render_cpp` (they include the same `gl_render_loader.h` and link
libEGL only); the Rust cells drive the same glow + khronos-egl (dynamic musl) API over a desktop-GL 4.5
core context; the Python cells use PyOpenGL's `OpenGL.EGL` + `OpenGL.GL` desktop-GL bindings. Each cell
computes an **independent closed-form software reference** (never derived from the GL output) and asserts
the `glReadPixels` readback against it per pixel, closing with a negative control. They print
`SCENE_<NAME>[_RUST|_PY] OK <n>` only when `FAIL==0 && TOTAL==EXPECTED==PASS`. The C++ and Rust scene
cells build unconditionally on every arch (always in the manifest); the Python scene cells append where
`py3-opengl` resolves (honest per-arch skip, same gate as `opengl_render_py`). They are mirrored from the
GLES track (`cpu-gles-render`), adapting GLES 3.1 -> desktop GL 4.5 core; the closed-form reference math
is behaviour-identical, only the GL flavor (context, `#version 450 core` shaders, entry points via the
loader) differs.

| scenario | C++ | Rust (glow) | Python (PyOpenGL) | assertions (host) |
|----------|-----|-------------|-------------------|------:|
| 2D UI compositing | `scene_2dui/` | `scene_2dui_rust/` | `scene_2dui_py/` | 38 |
| 3D indexed-mesh model | `scene_3dmodel/` | `scene_3dmodel_rust/` | `scene_3dmodel_py/` | 20 |
| keyframe animation | `scene_anim/` | `scene_anim_rust/` | `scene_anim_py/` | 46 |
| streaming / codec math | `scene_codec/` | `scene_codec_rust/` | `scene_codec_py/` | 24 |

- **scene_2dui** (`SCENE_2DUI OK 38`): orthographic pixel-space 2D UI - filled rectangles, an analytic
  rounded-rect (inside / corner-arc / outside coverage), a nine-patch scaled border frame, an 8x8
  bitmap-font glyph blit (every lit/unlit texel), a scissor-clipped fill, and multi-layer Porter-Duff
  over compositing of 3 stacked semi-transparent layers `Co = Cs + Cd*(1-As)` matched channel-by-channel
  including alpha.
- **scene_3dmodel** (`SCENE_3DMODEL OK 20`): an indexed cube mesh with a hand-computed perspective MVP,
  depth-buffered occlusion (`GL_LESS`) and Gouraud shading. The reference is a full software rasterizer
  in C++ (same MVP -> clip -> NDC -> viewport, per-pixel barycentric + perspective-correct depth test in
  a private z-buffer + interpolated vertex colors), compared to the GL readback per pixel.
- **scene_anim** (`SCENE_ANIM OK 46`): N=4 keyframes of a transformed unit quad (rotation about the FBO
  center composed with translate + uniform scale, interpolated over `t in {0,0.25,0.5,0.75}`). Each
  frame the four transformed corners are computed closed-form in C++ (`R(theta)*S*local + T`) and the
  readback is asserted at those exact corner pixels plus a just-outside background point; a cubic ease
  `eased(t)=3t^2-2t^3` drives the scale and is asserted at each `t`.
- **scene_codec** (`SCENE_CODEC OK 24`): media-pipeline math, each vs a numpy-equivalent C++ reference -
  BT.601 full-range YUV->RGB in a three-plane fragment shader; chroma 4:2:0 -> 4:4:4 NEAREST upsample;
  bilinear 2x downscale (`GL_LINEAR` vs a 2x2 box average); and CPU-path round-trips (8-sample DCT-II
  forward/inverse identity, plus an RLE encode/decode identity).

The desktop-GL scene counts are each one higher than the GLES counterparts (37/18/45/23) because the
desktop cells add a `glr_load()` entry-point-resolution assertion that the GLES cells (which link
libGLESv2 directly, no loader) do not need; `scene_3dmodel` adds a second (resolving `glUniformMatrix4fv`
for the MVP upload, which is not in the shared render loader). The Rust and Python cells hit the same
counts (38/20/46/24) via the equivalent context assertions: glow / PyOpenGL resolve entry points
implicitly (`glow::Context::from_loader_function` / `OpenGL.GL`), so each `_rust`/`_py` scene asserts the
non-empty desktop GL VERSION string in place of `glr_load()`, and `scene_3dmodel_{rust,py}` add a second
(the RENDERER string) in place of the C++ `glUniformMatrix4fv`-via-`eglGetProcAddress` assert. No render
assertion or closed-form reference changed.

## Render coverage (per cell, closed-form per pixel)

The same 10 render primitives are exercised in every cell and each is checked per pixel against a
closed-form / numpy reference, plus a negative control that proves the pixel checker rejects a
known-wrong value:

- **clear-color** - `glClearColor` + `glClear`, whole-buffer readback equals the 8-bit color
- **solid quad** - a compiled+linked program draws a fullscreen quad, every pixel equals the uniform
- **gradient** - an axis-aligned linear (horizontal red->blue) per-vertex color; a triangle-strip quad
  interpolates per-triangle, so only an axis-aligned gradient matches a full-quad closed form
- **checkerboard** - a procedural `gl_FragCoord` pattern, per-pixel `(x/8 + y/8)` parity
- **viewport** - a restricted `glViewport` leaves the rest at the clear color
- **scissor** - a `glScissor` box confines a clear
- **depth test** - two overlapping quads at different z, `GL_LESS` occlusion, nearer color wins
- **alpha blend** - `SRC_ALPHA / ONE_MINUS_SRC_ALPHA` over all channels incl. alpha, closed-form blend
- **1x1 FBO** + **sub-rectangle readback** - boundary framebuffer sizes and partial `glReadPixels`

## Bring-up on StarryOS

`prebuild.sh` extracts the base Alpine rootfs, `apk add`s the Mesa desktop-GL stack (mesa-gl / mesa-egl
/ mesa-gles / mesa-dri-gallium = llvmpipe) + build-base (for the staged libstdc++) + python3/numpy via
qemu-user (apk resolves for the target arch), then per cell: cross-compiles the C++ cell **on the host**
with a musl cross `${triple}-g++` (`--sysroot` at the staged rootfs), cross-compiles the Rust cell to
dynamic musl, and wires the moderngl/PyOpenGL Python cell where the binding resolves. The staging g++ is
not run under qemu-user (gcc spawns `cc1plus` via `posix_spawn`, which qemu-user cannot exec). Alpine
edge's mesa `libEGL` carries `DT_RELR` (`.relr.dyn`, section type `0x13`) relocations that GNU ld 2.37
in the musl-cross toolchains rejects (`unknown type [0x13] section '.relr.dyn'`); the C++ link therefore
uses a RELR-aware linker - `${triple}-g++ -fuse-ld=lld` when a standalone `ld.lld` is reachable (native
GNU C++ ABI, LLD reads RELR), else `zig c++ -target ${triple}` with the staged libstdc++ (zig bundles its
own LLD). Any LLVM `ld.lld` (Debian/Ubuntu `lld` package) or a `zig` on PATH satisfies this. A capability
manifest lists exactly the cells provisioned on this arch; `run_all.sh` gates on that set (`fail==0 &&
total==EXPECTED==pass`, `EXPECTED>=1` floor - the C++ cell is the guaranteed native gate) and never emits
a 0-carpet pass.

## Run

```
cargo xtask starry app qemu -t cpu-opengl-render --arch x86_64
cargo xtask starry app qemu -t cpu-opengl-render --arch aarch64
cargo xtask starry app qemu -t cpu-opengl-render --arch riscv64
cargo xtask starry app qemu -t cpu-opengl-render --arch loongarch64
```

Host validation (build host, Mesa llvmpipe, `EGL_PLATFORM=surfaceless GALLIUM_DRIVER=llvmpipe
LP_NUM_THREADS=1`): opengl_render_cpp 45/45, opengl_render_py 33/33, opengl_render_rust 40/40;
scene_2dui 38/38, scene_3dmodel 20/20, scene_anim 46/46, scene_codec 24/24 - identical counts for the
`_rust` (glow) and `_py` (PyOpenGL) scene cells (38/20/46/24 each).
