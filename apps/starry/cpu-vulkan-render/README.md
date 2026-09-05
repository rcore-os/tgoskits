# cpu-vulkan-render - Vulkan RENDER carpet (C / C++ / Rust / Python)

The Vulkan rendering-track counterpart of `cpu-vulkan-compute` (#1575). A headless Vulkan device
(no surface/swapchain/window) builds an **offscreen render pass** into an `R8G8B8A8_UNORM` color image,
draws through **real graphics pipelines** (SPIR-V vertex+fragment shaders), copies the image to a
host-visible buffer with `vkCmdCopyImageToBuffer`, maps it, and checks **every pixel against a
closed-form reference**. The Vulkan implementation is **Mesa lavapipe** (CPU software Vulkan 1.3 over
the LLVM JIT) - no GPU, single vCPU (`-smp 1`).

## Cells (4 bindings)

| cell | file | binding | assertions (host) |
|------|------|---------|------:|
| vulkan_render_c | `programs/carpets/vulkan_render_c/` | C (libvulkan) | 68 |
| vulkan_render_cpp | `programs/carpets/vulkan_render_cpp/` | C++ (libvulkan) | 68 |
| vulkan_render_rust | `programs/carpets/vulkan_render_rust/` | Rust (ash 0.38, dynamic musl) | 68 |
| vulkan_render_py | `programs/carpets/vulkan_render_py/` | Python (`vulkan` cffi + numpy) | 68 |

Each cell prints `<NAME>_FULL_API OK <n>` only when every assertion passes and the count equals its
pinned `EXPECTED` (68), together with a `PASS/FAIL/TOTAL/EXPECTED` line. All four are 1:1 assertion
parity: the C cell is the ground truth; C++/Rust/Python mirror it API-for-API and reference-for-
reference. The C/C++ cells link `libvulkan`; the Rust cell loads it via `ash::Entry::load()` (dlopen,
so it is a dynamic-musl binary); the Python cell uses the `vulkan` cffi binding (which dlopens
`libvulkan` too). SPIR-V is committed alongside each cell (C `uint32` headers for C/C++, raw `.spv`
for Rust/Python), generated from the GLSL sources with `glslangValidator`.

## Render-scene cells (4 scenes x 4 bindings)

Four higher-level RENDER-scene scenarios mirror the GLES `cpu-gles-render` scenes, adapting the
closed-form GL scenes to explicit Vulkan. Each scenario has **all four bindings** (C, C++, Rust ash,
Python `vulkan` cffi) with **1:1 assertion parity** - the closed-form reference math (Porter-Duff over,
analytic rounded-rect, barycentric software rasterizer, cubic ease, BT.601 matrix, DCT-II/IDCT, RLE) is
behaviour-identical across all four; only the C / C++ / ash / cffi Vulkan binding syntax differs. Each
cell builds its own pipeline(s) and render pass into the offscreen target, reads pixels back, and
asserts against an **independent** closed-form reference (not derived from the Vulkan output). All four
bindings reuse the **same committed SPIR-V** (C `uint32` headers for C/C++, raw `.spv` for Rust/Python),
generated once from the GLSL sources with `glslangValidator`. `scene_3dmodel`'s depth vertex shader
carries `invariant gl_Position`.

| scenario | marker (`_C` / `_RUST` / `_PY` suffix per binding) | c | cpp | rust | py |
|----------|--------|--:|--:|--:|--:|
| scene_2dui | `SCENE_2DUI` | 39 | 39 | 39 | 39 |
| scene_3dmodel | `SCENE_3DMODEL` | 23 | 23 | 23 | 23 |
| scene_anim | `SCENE_ANIM` | 47 | 47 | 47 | 47 |
| scene_codec | `SCENE_CODEC` | 27 | 27 | 27 | 27 |

Cell directories: `programs/carpets/scene_<name>{,_c,_rust,_py}/` (the unsuffixed dir is the C++ cell).
The C/C++ cells link `libvulkan`; the Rust cells load it via `ash::Entry::load()` (dynamic musl); the
Python cells use the `vulkan` cffi binding (honest-skip per-arch where the binding is not provisioned).

- **scene_2dui**: filled rects, an analytic rounded-rect (fragment-shader discard vs the identical C++
  coverage test), a nine-patch border frame, an 8x8 bitmap-font glyph blit (VkImage + `VK_FILTER_NEAREST`
  combined image sampler, all 64 texels), a dynamic-scissor fill, and multi-layer Porter-Duff `over`
  compositing (explicit `SRC_ALPHA / ONE_MINUS_SRC_ALPHA` `VkPipelineColorBlendAttachmentState`).
- **scene_3dmodel**: an indexed cube through a depth-tested (`VK_COMPARE_OP_LESS`, `D32_SFLOAT`) Gouraud
  pipeline, checked against a barycentric software reference rasterizer. Adapted for **Vulkan NDC z in
  `[0,1]`**: the `perspective()` z-row uses the Vulkan mapping (near->0, far->1) and the reference window
  depth is `z_clip/w_clip` directly (not GL's `*0.5+0.5`), so the GPU depth buffer and the reference
  agree on occlusion (verified: disabling the GPU depth test makes the per-pixel/center asserts fail).
- **scene_anim**: N=4 keyframes of a transformed quad (rotation + scale + translate, cubic-ease scale),
  the per-frame model transform passed as push constants; the four `R*S*local+T` corners and the frame
  center are asserted at exact pixels.
- **scene_codec**: BT.601 full-range YUV->RGB from three `R8_UNORM` planes, chroma 4:2:0->4:4:4 NEAREST
  upsample, bilinear 2x downscale (`VK_FILTER_LINEAR` = 2x2 box average), and pure-CPU DCT-II/IDCT and
  RLE round-trip identities. GL sub-region `glViewport` becomes a Vulkan dynamic viewport.

**Vulkan vs GL orientation**: with a positive-height viewport `{0,0,W,H,0,1}` Vulkan clip-space Y is
down, so pixel-space `y=0` maps to NDC `y=-1` which lands on readback row 0 - the same row-0
correspondence as GL's bottom-origin `glReadPixels`. Both index the readback buffer by pixel-space `y`,
so the closed-form references index by `y` unchanged; only `scene_3dmodel`'s NDC-z mapping changes.
The scene assertion counts differ from the GLES counterparts (Vulkan-specific setup asserts + no
EGL/loader asserts); each is calibrated to its true green count.

## Render coverage (per cell, closed-form per pixel)

Because Vulkan bakes render state into the pipeline, each state is exercised by building a dedicated
`VkPipeline`. Every draw reads pixels back and checks them against a closed-form reference, plus a
negative control that proves the checker rejects a known-wrong value:

- **Base**: render-pass clear, a solid quad (push-constant color), an axis-aligned linear gradient (a
  triangle-strip quad interpolates per-triangle, so only an axis-aligned gradient matches a full-quad
  closed form), a `gl_FragCoord` checkerboard, a dynamic scissor, alpha blending
  (`SRC_ALPHA / ONE_MINUS_SRC_ALPHA` over all channels incl. alpha, a=191), a sub-rectangle readback.
- **Primitive topologies** (`VkPrimitiveTopology`): `TRIANGLE_LIST`, `TRIANGLE_FAN`, `LINE_LIST`,
  `LINE_STRIP`, `POINT_LIST` (points use a vertex shader that writes `gl_PointSize`).
- **Blend factor + op matrix** (`VkBlendFactor` / `VkBlendOp`): `ONE/ZERO`, `ONE/ONE` (ADD), `ZERO/ONE`,
  `DST_COLOR/ZERO`, `MAX`, `REVERSE_SUBTRACT` (alpha resolves to 0).
- **Depth-func matrix** (all 8 `VkCompareOp` against a `D32_SFLOAT` depth attachment): Vulkan NDC z is
  in `[0,1]`, so a `z=0.5` quad against clear-depth `0.75` draws under
  `{ALWAYS, LESS, LEQUAL, NOT_EQUAL}` and is rejected under `{NEVER, EQUAL, GREATER, GEQUAL}`.
- **Face culling + winding** (`VkCullModeFlags` NONE / FRONT_AND_BACK / BACK x `VkFrontFace` CCW-vs-CW).
- **Color write mask** (`VkColorComponentFlags`: R-only vs RGBA).
- **Format + device property queries** (`vkGetPhysicalDeviceFormatProperties` for
  `R8G8B8A8_UNORM` COLOR_ATTACHMENT and `D32_SFLOAT` DEPTH_STENCIL_ATTACHMENT;
  `vkGetPhysicalDeviceProperties` apiVersion / limits).
- **2x2 texture upload + NEAREST sampling** through a combined image sampler + descriptor set + staging
  buffer upload + image-layout barriers (corners: top-left red, top-right green, bottom-left blue,
  top-right white).

## Bring-up on StarryOS

`prebuild.sh` extracts the base Alpine rootfs and `apk add`s the Mesa software-Vulkan stack
(`mesa-vulkan-swrast` = lavapipe ICD, `vulkan-loader` = libvulkan, `vulkan-headers`) + `build-base` (the
staged `libstdc++.so.6` + its headers) + python3/numpy/py3-cffi under qemu-user (apk resolves the closure
for the TARGET arch). The `apk` step runs fine under qemu-user; the TARGET Alpine gcc/g++ does not (gcc
spawns `cc1`/`cc1plus` via `posix_spawn`, which qemu-user cannot exec), so the C and C++ cells are
compiled **on the host** with a musl cross toolchain (`<triple>-gcc`/`-g++` on `PATH` -> `/opt/<triple>-cross`
-> `zig cc`/`zig c++ -target <triple>`) against the staged sysroot. Alpine's `libvulkan.so.1` carries a
`.relr.dyn` (`SHT_RELR`) section that older musl-cross binutils `ld` rejects; the C/C++ compiler is
resolved by test-linking the staged `libvulkan`, falling back to zig's LLD (which reads `.relr.dyn`) when
GNU `ld` cannot. The C++ zig path uses the staged libstdc++ headers + links the staged `libstdc++.so.6`
positionally for the GNU (`std::__cxx11`) ABI. The Rust ash cells (`vulkan_render_rust` + `scene_*_rust`,
dynamic musl) cross-compile with `cargo --target` and dlopen `libvulkan`; the Python cells
(`vulkan_render_py` + `scene_*_py`) run on-target via the ld-musl `python3` and are wired where the
`vulkan` cffi package provisions (`pip install vulkan`). A capability manifest lists exactly the cells
provisioned on this arch; `run_all.sh` gates on that set (`fail==0 && total==EXPECTED==pass`,
`EXPECTED>=1` floor - the C/C++ cells are the guaranteed native gate) and never emits a 0-carpet pass.

## Run

```
cargo xtask starry app qemu -t cpu-vulkan-render --arch x86_64
cargo xtask starry app qemu -t cpu-vulkan-render --arch aarch64
cargo xtask starry app qemu -t cpu-vulkan-render --arch riscv64
cargo xtask starry app qemu -t cpu-vulkan-render --arch loongarch64
```

Host validation (Mesa lavapipe, `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`):
vulkan_render_c 68/68, vulkan_render_cpp 68/68, vulkan_render_rust 68/68, vulkan_render_py 68/68,
scene_2dui 39/39, scene_3dmodel 23/23, scene_anim 47/47, scene_codec 27/27.
