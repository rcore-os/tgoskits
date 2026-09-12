# cpu-wgpu-render - WebGPU (wgpu) RENDER carpet (Rust / C / C++ / Python)

The WebGPU rendering-track counterpart of `cpu-wgpu-compute` (#1576). Each cell renders **offscreen**
into a 64x64 `RGBA8Unorm` texture through real WebGPU render pipelines, copies the texture to a
`MAP_READ` buffer (256-byte `bytesPerRow` aligned) and checks **every pixel against a closed-form
reference**. It runs on Mesa software adapters under **both** wgpu backends - wgpu-on-Vulkan
(**lavapipe**) and wgpu-on-GL (**llvmpipe**) - no GPU, single vCPU (`-smp 1`).

## Cells (4 bindings), each run under both backends

| cell | file | binding | assertions (host, per backend) |
|------|------|---------|------:|
| wgpu_render_rust | `programs/carpets/wgpu_render_rust/` | Rust (`wgpu` 22, dynamic musl) | 56 |
| wgpu_render_c | `programs/carpets/wgpu_render_c/` | C (webgpu.h / wgpu.h over libwgpu_native v22) | 56 |
| wgpu_render_cpp | `programs/carpets/wgpu_render_cpp/` | C++ (webgpu.h over libwgpu_native v22) | 56 |
| wgpu_render_py | `programs/carpets/wgpu_render_py/` | Python (wgpu-py + numpy) | 56 |

Each cell prints `<NAME>_FULL_API OK <n>` only when every assertion passes and the count equals its
pinned `EXPECTED` (56); `run_all.sh` runs every cell under `WGPU_BACKEND=vulkan` and `WGPU_BACKEND=gl`,
requiring both to pass (gate = cells x 2). The Rust cell carries its own wgpu-core; the C/C++ cells link
`libwgpu_native.so` (gfx-rs wgpu-native v22, built from source for musl by `prebuild.sh` - gfx-rs ships
only glibc x86_64/aarch64 prebuilts); the Python cell is pure-python wgpu-py pointed at that same musl
lib via `WGPU_LIB_PATH`. WGSL shaders are inline.

## Render-scenario scene cells (4 scenarios x 4 bindings)

Four render scenarios mirror the GLES/Vulkan `scene_*` carpets, each provided in **all four bindings** -
Rust (`wgpu` 22), C and C++ (`webgpu.h`/`wgpu.h` over `libwgpu_native` v22), and Python (wgpu-py). Every
binding of a scenario renders the **same WGSL shaders** and asserts against the **same independent
closed-form reference** (Porter-Duff over / barycentric software rasterizer with 1/w perspective-correct
Gouraud / cubic ease `3t^2-2t^3` / BT.601 / DCT-II+IDCT & RLE), plus a negative control; only the binding
syntax differs. Each renders offscreen into a 64x64 `Rgba8Unorm` texture (`Depth32Float` depth for the 3D
scene), copies to a readback buffer (256-byte `bytesPerRow` padding) and checks **every pixel**. The Rust
cells are cargo crates cross-compiled to dynamic musl (`--release --locked`); the C/C++ cells link the
same `libwgpu_native.so` (built from source for musl by `prebuild.sh`) and `#include` its fetched ffi
headers; the Python cells are the wgpu-py sdist pointed at that lib via `WGPU_LIB_PATH`. Each is staged as
its own binary and run under both backends.

| scenario | markers (`<SCENE> [_C\|_CPP\|_PY]`) | assertions per binding | closed-form reference |
|----------|-------------------------------------|----------------------:|-----------------------|
| scene_2dui    | `SCENE_2DUI OK 28` / `_C` / `_CPP` / `_PY` | 28 | filled rects, analytic rounded-rect, nine-patch frame, 8x8 glyph blit, scissor fill, multi-layer Porter-Duff over |
| scene_3dmodel | `SCENE_3DMODEL OK 14` / `_C` / `_CPP` / `_PY` | 14 | indexed-cube MVP + Depth32Float `CompareFunction::Less` occlusion, barycentric software rasterizer with 1/w perspective-correct Gouraud |
| scene_anim    | `SCENE_ANIM OK 38` / `_C` / `_CPP` / `_PY` | 38 | 4 keyframes, cubic ease `3t^2-2t^3`, closed-form `R(theta)*S*local+T` corner positions |
| scene_codec   | `SCENE_CODEC OK 15` / `_C` / `_CPP` / `_PY` | 15 | BT.601 YUV->RGB, 4:2:0->4:4:4 NEAREST upsample, bilinear 2x box-average downscale, 8-point DCT-II/IDCT + RLE round-trip |

The four bindings of a scenario print the same assertion count (`28 / 14 / 38 / 15`) - the closed-form
reference is identical, so a divergence in any binding is a real per-binding bug, not a tolerance drift.

WebGPU adaptations vs the GL sources (closed-form math kept behavior-identical):

- **NDC z in `[0,1]`** (like Vulkan): scene_3dmodel uses the Vulkan `perspective()` z-row
  (`m[2][2]=zf/(zn-zf)`, `m[3][2]=zf*zn/(zn-zf)`) and window depth `sz = ndcz` directly, with a
  `Depth32Float` attachment and `CompareFunction::Less`. The vertex shader carries `@invariant` on
  `@builtin(position)` so rasterized depth is bit-exact and occlusion is deterministic.
- **Top-origin framebuffer**: WebGPU's `copy_texture_to_buffer` readback row 0 is the top (NDC y=+1),
  the reverse of the GL/Vulkan bottom-origin `glReadPixels` / positive-height viewport. Each pixel-space
  vertex shader flips NDC y so pixel-row `y` lands at readback row `y` and the closed-form arithmetic
  (rects, corners, `@builtin(position)`-based analytic tests) is unchanged; the codec UV quads carry
  `v=0` at the top vertices to the same effect.
- **Porter-Duff over** -> `BlendState` SrcAlpha/OneMinusSrcAlpha; scissor via `set_scissor_rect`;
  viewport via `set_viewport`; textures via `Texture` + `Sampler` (Nearest/Linear).

Assertion-count deltas vs the C++ sources (2dui 37, 3dmodel 23, anim 45, codec 23): the GL/Vulkan cells
count EGL/GL bring-up asserts (eglGetDisplay/Initialize/ChooseConfig/BindAPI/CreateContext/MakeCurrent,
FBO-complete, per-shader compile/link + uniform-location) and Vulkan `VKOK()` object-creation wrappers
(vkCreateInstance/Device/Image/RenderPass/Framebuffer/CommandPool/Pipeline) that have **no wgpu
counterpart** - wgpu validates via `on_uncaptured_error` and its `create_*` calls do not return a
per-call `Result` to assert on. Those are replaced by the two `request_adapter` / `request_device`
asserts. Every SCENE closed-form assertion is preserved 1:1, which lands the counts at 28 / 14 / 38 / 15.

## Render coverage (per cell, closed-form per pixel)

Every draw reads pixels back and checks against a closed-form reference, plus a negative control:

- **Base**: clear (`LoadOp::Clear`), solid quad (uniform buffer + bind group - WebGPU has no push
  constants), axis-aligned linear gradient (a triangle-strip quad interpolates per-triangle, so only an
  axis-aligned gradient matches a full-quad closed form), a `@builtin(position)` checkerboard, dynamic
  scissor (`set_scissor_rect`), viewport (`set_viewport`), alpha blend (`SrcAlpha/OneMinusSrcAlpha`,
  a=191), sub-rectangle readback.
- **Primitive topologies** (all 5 in WebGPU - there is no triangle-fan): `PointList` (1px points; WebGPU
  has no PointSize), `LineList`, `LineStrip`, `TriangleList`, `TriangleStrip`.
- **Blend factor + op matrix**: `One/Zero`, `One/One` (Add), `Zero/One`, `Dst/Zero`, `One/One` Max,
  `One/One` ReverseSubtract (alpha resolves to 0).
- **Depth-func matrix** (all 8 `CompareFunction` against a `Depth32Float` attachment): WebGPU NDC z is
  in `[0,1]`, so a `z=0.5` quad against clear-depth `0.75` draws under `{Always, Less, LessEqual,
  NotEqual}` and is rejected under `{Never, Equal, Greater, GreaterEqual}`. The depth-test vertex
  shaders carry `@invariant` on `@builtin(position)` so z is bit-exact across pipelines and the exact
  Equal/NotEqual compares are deterministic.
- **Face culling + winding** (`Face::None` / `Back` x `FrontFace::Ccw`-vs-`Cw`).
- **Color write mask** (`ColorWrites::RED` vs `::ALL`).
- **Format + limit queries** (renderable-format checks, device limits).
- **2x2 texture upload + NEAREST sampling** through a sampler + bind group (corners: top-left red,
  top-right green, bottom-left blue, top-right white).

## Bring-up on StarryOS

`prebuild.sh` extracts the base Alpine rootfs and `apk add`s the software Vulkan stack
(`mesa-vulkan-swrast` = lavapipe, `vulkan-loader`) + the GL stack (`mesa-gl`/`mesa-egl`/
`mesa-dri-gallium` = llvmpipe) + python3/numpy/cffi into the staging root via qemu-user (only apk runs
under qemu-user; it works).

All native code is cross-compiled on the HOST, never under qemu-user - the target Alpine gcc/g++ cannot
run under qemu-user (cc1/cc1plus fails to spawn). The C compiler / C++ compiler for each arch are resolved
from the musl-cross toolchain: `${triple}-gcc`/`${triple}-g++` on PATH, then the conventional
`/opt/${triple}-cross` (or `/usr/local/${triple}-cross`) install prefix, then `zig cc`/`zig c++
-target ${triple}` as a portable fallback. The Rust cells (and `libwgpu_native.so`, built from gfx-rs
wgpu-native v22 source) are cross-compiled by cargo with `--target <arch>-unknown-linux-musl` (dynamic
musl); the C and C++ cells are compiled on the host against the fetched wgpu ffi headers and linked to the
staged `libwgpu_native.so` with `-Wl,-rpath-link` so the host linker resolves its transitive musl deps out
of the staging root. The pure-python wgpu-py sdist is pointed at the same musl lib via `WGPU_LIB_PATH`.

A capability manifest lists the cells provisioned on this arch; `run_all.sh` gates on that set under both
backends (`fail==0 && total==2*cells==pass`, `>=1` cell floor) and never emits a 0-carpet pass.

## Run

```
cargo xtask starry app qemu -t cpu-wgpu-render --arch x86_64
cargo xtask starry app qemu -t cpu-wgpu-render --arch aarch64
cargo xtask starry app qemu -t cpu-wgpu-render --arch riscv64
cargo xtask starry app qemu -t cpu-wgpu-render --arch loongarch64
```

Host validation (Mesa lavapipe + llvmpipe): wgpu_render_rust / wgpu_render_c / wgpu_render_cpp 56/56 on
both `WGPU_BACKEND=vulkan` and `WGPU_BACKEND=gl`; wgpu_render_py 56/56 (wgpu-py). The scene cells pass
their pinned counts on both backends in every binding - Rust and C/C++/Python each report
`28 / 14 / 38 / 15` for `scene_2dui / scene_3dmodel / scene_anim / scene_codec` (C/C++ linked against a
host-built `libwgpu_native` v22.1.0.5, Python via wgpu-py 0.20.0 pointed at the same v22 lib).
