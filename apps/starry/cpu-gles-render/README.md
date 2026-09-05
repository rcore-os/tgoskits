# cpu-gles-render - OpenGL ES RENDER carpet (C++ / Python / Rust)

The GLES rendering-track counterpart of `cpu-gles-compute` (#1610) and the ES sibling of
`cpu-opengl-render` (#1816). A headless surfaceless-EGL **OpenGL ES 3.1** context renders into an
**off-screen framebuffer** (RGBA8 color texture + depth renderbuffer) and reads the pixels back with
`glReadPixels`, checking **every pixel against a closed-form reference**. The GL implementation is
**Mesa llvmpipe** (CPU software rasterizer over the LLVM JIT) - no GPU, single vCPU (`-smp 1`).

## Cells (3 bindings)

| cell | file | binding | assertions (host) |
|------|------|---------|------:|
| gles_render_cpp | `programs/carpets/gles_render_cpp/` | C++ (GLES 3.1, direct libGLESv2) | 77 |
| gles_render_py | `programs/carpets/gles_render_py/` | Python (PyOpenGL: OpenGL.EGL + OpenGL.GLES3 + numpy) | 70 |
| gles_render_rust | `programs/carpets/gles_render_rust/` | Rust (glow 0.13 + khronos-egl, ES context, dynamic musl) | 71 |

Each cell prints `<NAME>_FULL_API OK <n>` only when every assertion passes and the count equals its
pinned `EXPECTED`, together with a `PASS/FAIL/TOTAL/EXPECTED` line. The context is bound with
`eglBindAPI(EGL_OPENGL_ES_API)` + an `EGL_OPENGL_ES3_BIT` config + an ES 3.1 context; shaders are
`#version 310 es`. The C++ cell calls GLES entry points directly (libGLESv2 exports them, no loader);
the Rust cell drives the same glow API over an ES context; the Python cell uses PyOpenGL's ES bindings
(moderngl is desktop-GL-only, so it is not used here).

## Real-scenario cells (4 scenarios x 3 bindings = 12)

Four real-scenario render cells exercise realistic GPU workloads end to end, on the same
surfaceless-EGL / GLES 3.1 / llvmpipe path as the base bindings. Each is now provided in **all three
bindings** (C++ / Rust / Python), mirroring the per-API binding coverage: the C++ cells use direct
libGLESv2, the Rust cells use glow + khronos-egl (dynamic musl), the Python cells use PyOpenGL
(`OpenGL.EGL` + `OpenGL.GLES3` + numpy). Every cell computes an **independent closed-form software
reference in its own language** (never derived from the GL output) - Porter-Duff over compositing, a
barycentric + perspective-correct (1/w) software rasterizer, a cubic ease, BT.601 YUV->RGB, DCT-II /
IDCT and RLE round-trips - and asserts the `glReadPixels` readback against it per pixel, closing with a
negative control. They print `SCENE_<NAME>[_RUST|_PY] OK <n>` only when `FAIL==0 && TOTAL==EXPECTED==PASS`.
The C++ and Rust scene cells build unconditionally on every arch (always in the manifest); the Python
scene cells append where `py3-opengl` resolves (honest per-arch skip, same gate as `gles_render_py`).

| scenario | C++ | Rust (glow) | Python (PyOpenGL) | assertions (host) |
|----------|-----|-------------|-------------------|------:|
| 2D UI compositing | `scene_2dui/` | `scene_2dui_rust/` | `scene_2dui_py/` | 37 |
| 3D indexed-mesh model | `scene_3dmodel/` | `scene_3dmodel_rust/` | `scene_3dmodel_py/` | 18 |
| keyframe animation | `scene_anim/` | `scene_anim_rust/` | `scene_anim_py/` | 45 |
| streaming / codec math | `scene_codec/` | `scene_codec_rust/` | `scene_codec_py/` | 23 |

All three bindings of a given scenario share the same pinned `EXPECTED` (37 / 18 / 45 / 23) - the
closed-form reference math is behaviour-identical; only the glow-Rust / PyOpenGL binding syntax differs.
Markers: `SCENE_2DUI OK` / `SCENE_2DUI_RUST OK` / `SCENE_2DUI_PY OK` (and likewise for the other three).

- **scene_2dui** (`SCENE_2DUI OK 37`): orthographic pixel-space 2D UI - filled rectangles, an analytic
  rounded-rect (inside / corner-arc / outside coverage), a nine-patch scaled border frame, an 8x8
  bitmap-font glyph blit (every lit/unlit texel), a scissor-clipped fill, and multi-layer Porter-Duff
  over compositing of 3 stacked semi-transparent layers `Co = Cs + Cd*(1-As)` matched channel-by-channel
  including alpha.
- **scene_3dmodel** (`SCENE_3DMODEL OK 18`): an indexed cube mesh with a hand-computed perspective MVP,
  depth-buffered occlusion (`GL_LESS`) and Gouraud shading. The reference is a full software rasterizer
  in C++ (same MVP -> clip -> NDC -> viewport, per-pixel barycentric + perspective-correct depth test in
  a private z-buffer + interpolated vertex colors), compared to the GL readback per pixel.
- **scene_anim** (`SCENE_ANIM OK 45`): N=4 keyframes of a transformed unit quad (rotation about the FBO
  center composed with translate + uniform scale, interpolated over `t in {0,0.25,0.5,0.75}`). Each
  frame the four transformed corners are computed closed-form in C++ (`R(theta)*S*local + T`) and the
  readback is asserted at those exact corner pixels plus a just-outside background point; a cubic ease
  `eased(t)=3t^2-2t^3` drives the scale and is asserted at each `t`.
- **scene_codec** (`SCENE_CODEC OK 23`): media-pipeline math, each vs a numpy-equivalent C++ reference -
  BT.601 full-range YUV->RGB in a three-plane fragment shader; chroma 4:2:0 -> 4:4:4 NEAREST upsample;
  bilinear 2x downscale (`GL_LINEAR` vs a 2x2 box average); and CPU-path round-trips (8-sample DCT-II
  forward/inverse identity, plus an RLE encode/decode identity).

## Render coverage (per cell, closed-form per pixel)

Every cell is checked per pixel against a closed-form / numpy reference, plus a negative control that
proves the pixel checker rejects a known-wrong value. Base primitives: clear-color, a solid quad
through a compiled+linked program, an axis-aligned linear gradient (a triangle-strip quad interpolates
per-triangle, so only an axis-aligned gradient matches a full-quad closed form), a `gl_FragCoord`
checkerboard, viewport restriction, a scissor box, the depth test (`GL_LESS` occlusion), alpha blending
(`SRC_ALPHA / ONE_MINUS_SRC_ALPHA` over all channels incl. alpha), a 1x1 FBO, and a sub-rectangle
readback. Exhaustive per-API coverage: primitive topologies (indexed `GL_TRIANGLES` / `GL_TRIANGLE_FAN`
/ `GL_LINES` / `GL_POINTS` with `gl_PointSize`), a blend factor+equation matrix (`ONE/ZERO`, `ONE/ONE`,
`ZERO/ONE`, `DST_COLOR`, `GL_MAX` and `GL_FUNC_REVERSE_SUBTRACT` - GLES3 core), the full depth-func
matrix (all 8 comparisons at window depth 0.75), face culling + winding (`FRONT_AND_BACK` / `BACK` with
CCW-vs-CW), a 2x2 texture upload + NEAREST sampling, and state queries
(`glGetIntegerv`/`glIsEnabled`/`glGetBooleanv`/`glGetError`).

## Bring-up on StarryOS

`prebuild.sh` extracts the base Alpine rootfs and `apk add`s the Mesa GLES stack (mesa-gles / mesa-egl /
mesa-dri-gallium = llvmpipe) + mesa-dev (glesv2.pc / egl.pc) + build-base (staged libstdc++) +
python3/numpy/py3-opengl into a per-arch staging root via qemu-user. The cells are then built on the
HOST, not under qemu-user: the target Alpine gcc cannot spawn cc1/cc1plus through qemu-user, so the C++
cells cross-compile with a host musl toolchain (`${triple}-g++`, or `zig c++ -target ${triple}` when the
GNU cross ld cannot read Mesa's `.relr.dyn` sections) against `--sysroot=<staging>` with the glesv2/egl
link flags from a host `pkgconf` run; the Rust cells cross-compile with cargo `--target` using a host
cross linker; the PyOpenGL cells are copied and run on-target. Only x86_64 needs `-machine q35` (PVH boot);
aa/rv/loong use raw-binary boot. A capability manifest lists exactly the cells provisioned on this arch;
`run_all.sh` gates on that set (`fail==0 && total==EXPECTED==pass`, `EXPECTED>=1` floor - the C++ cell is
the guaranteed native gate) and never emits a 0-carpet pass.

## Run

```
cargo xtask starry app qemu -t cpu-gles-render --arch x86_64
cargo xtask starry app qemu -t cpu-gles-render --arch aarch64
cargo xtask starry app qemu -t cpu-gles-render --arch riscv64
cargo xtask starry app qemu -t cpu-gles-render --arch loongarch64
```

Host validation (Mesa llvmpipe, `EGL_PLATFORM=surfaceless GALLIUM_DRIVER=llvmpipe LP_NUM_THREADS=1`;
Python cells add `PYOPENGL_PLATFORM=egl`): gles_render_cpp 77/77, gles_render_py 70/70 (OpenGL ES 3.2
Mesa), gles_render_rust 71/71. Scene cells (all three bindings, same closed-form references): scene_2dui
37/37, scene_3dmodel 18/18, scene_anim 45/45, scene_codec 23/23 - identical counts for the `_rust` and
`_py` variants.
