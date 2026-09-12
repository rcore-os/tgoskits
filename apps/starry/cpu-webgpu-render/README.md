# cpu-webgpu-render - WebGPU render-API carpet (JS / TS, Deno on-target)

WebGPU render cells (JavaScript + TypeScript) that drive the WebGPU rendering pipeline - adapter,
device, render pipelines, vertex/fragment shaders (WGSL), render passes, textures, depth/stencil,
blend, scissor/viewport, all draw topologies, `copyTextureToBuffer` readback - render offscreen to an
RGBA8 texture, read the pixels back, and assert every pixel against a closed-form reference computed
independently in the same language. They run **on StarryOS via Deno**, whose built-in WebGPU is
**gfx-rs wgpu-core** landing on **Mesa lavapipe** (software Vulkan on the CPU, no GPU required). This
is the render-track counterpart of the WebGPU compute carpet `cpu-webgpu-compute`.

## The two WebGPU engines (why this app is the Firefox/Deno path)

There are two production WebGPU implementations, and this matters for what runs on musl:

- **Chromium's WebGPU = Dawn** (C++). Shipped as the `webgpu` npm dawn native addon, a glibc-only
  prebuilt, so musl Node cannot load it. Dawn's on-target home is inside the browser (campaign #391).
- **Firefox / Servo / Deno's WebGPU = wgpu** (gfx-rs, Rust: `wgpu-core` / `wgpu-native`). This is the
  **exact engine `cpu-wgpu-render` #1820 builds on musl** for all four arches. Deno embeds `wgpu-core`
  directly, so a musl Deno gives us the JS/TS WebGPU surface on-target with no new engine to build - we
  only test an existing runtime.

### Verified call chain (source + strace, not assumed)

```
navigator.gpu (Deno JS/TS global)
 -> deno_webgpu            [Deno ext/webgpu: "WebGPU implementation for Deno"]
 -> wgpu-core + wgpu-types (gfx-rs; feature "vulkan" on Unix)
 -> wgpu-hal               (hardware abstraction layer)
 -> ash (Vulkan FFI) + libloading (runtime loader)          <-- source chain ends here
 -> dlopen libvulkan.so.1                       (Vulkan loader)          <-- strace
 -> /usr/share/vulkan/icd.d/lvp_icd.json        (ICD manifest)          <-- strace
 -> libvulkan_lvp.so                            (Mesa lavapipe, sw Vulkan) <-- strace
 -> libgallium + libLLVM                        (llvmpipe CPU JIT)      <-- strace
 -> WGSL -> SPIR-V -> NIR -> LLVM -> host CPU machine code (runs on the CPU)
```

The render pipeline rasterizes on the CPU (llvmpipe); there is no GPU in the path.

## Cells

| cell | file | assertions | on-target |
|------|------|-----------:|-----------|
| webgpu_js | `programs/carpets/webgpu_js/webgpu_render_js_full_api.js` | 56 | x86_64 + aarch64 (Deno) |
| webgpu_ts | `programs/carpets/webgpu_ts/webgpu_render_ts_full_api.ts` | 56 | x86_64 + aarch64 (Deno, runs .ts natively) |
| scene_2dui_js | `programs/carpets/scene_2dui_js/scene_2dui_js.js` | 28 | x86_64 + aarch64 (Deno) |
| scene_2dui_ts | `programs/carpets/scene_2dui_ts/scene_2dui_ts.ts` | 28 | x86_64 + aarch64 (Deno) |
| scene_3dmodel_js | `programs/carpets/scene_3dmodel_js/scene_3dmodel_js.js` | 14 | x86_64 + aarch64 (Deno) |
| scene_3dmodel_ts | `programs/carpets/scene_3dmodel_ts/scene_3dmodel_ts.ts` | 14 | x86_64 + aarch64 (Deno) |
| scene_anim_js | `programs/carpets/scene_anim_js/scene_anim_js.js` | 38 | x86_64 + aarch64 (Deno) |
| scene_anim_ts | `programs/carpets/scene_anim_ts/scene_anim_ts.ts` | 38 | x86_64 + aarch64 (Deno) |
| scene_codec_js | `programs/carpets/scene_codec_js/scene_codec_js.js` | 15 | x86_64 + aarch64 (Deno) |
| scene_codec_ts | `programs/carpets/scene_codec_ts/scene_codec_ts.ts` | 15 | x86_64 + aarch64 (Deno) |

Each cell prints `<MARKER> OK <n>` only when every assertion passes and the count equals the pinned total,
together with a `PASS=<p> FAIL=<f> TOTAL=<t> EXPECTED=<e>` line. The full-API cells
(`WEBGPU_RENDER_<LANG>_FULL_API`) and the four render-scene cells (`SCENE_<NAME>_<LANG>`) each have a JS and
a TS variant that mirror one-for-one (WGSL shaders, geometry, per-pixel assertions) and match the Rust
parity cells in `cpu-wgpu-render` (#1820): full-API EXPECTED=56, scene_2dui=28, scene_3dmodel=14,
scene_anim=38, scene_codec=15.

## Render scenes (WebGPU port of the wgpu Rust render-scene cells)

The four scenarios each render offscreen and assert every pixel against an INDEPENDENT closed-form
reference computed in the same language (not read back from the GPU). WebGPU is top-origin (readback row 0 =
top, NDC y=+1); every pixel-space vertex shader flips NDC y so pixel-row == readback-row while the
closed-form arithmetic stays byte-identical to the wgpu Rust reference.

- **scene_2dui**: filled rects, an analytic rounded-rect (corner-arc discard in the fragment shader), a
  nine-patch scaled border frame, an 8x8 bitmap-font glyph blit (`setScissorRect` + nearest texture
  sampling), and MULTI-LAYER Porter-Duff `over` compositing of 3 semi-transparent layers
  (src-alpha / one-minus-src-alpha blend).
- **scene_3dmodel**: an indexed cube with a hand-built perspective MVP (WebGPU NDC z in [0,1]), depth32float
  + `CompareFunction 'less'` occlusion and Gouraud shading, checked against an independent software
  rasterizer (barycentric coverage + 1/w perspective-correct color interpolation + private z-buffer). The
  vertex shader carries `@invariant` on `@builtin(position)` so the rasterized depth is bit-exact.
- **scene_anim**: 4 keyframes of a unit quad transformed by `R(angle(t))*S(scale(ease(t)))+T(center(t))`,
  with a cubic ease `3t^2-2t^3`; the four transformed corners and the eased scale are asserted per frame
  against the closed form.
- **scene_codec**: BT.601 YUV->RGB (three planes sampled as textures, 4:2:0 nearest chroma), chroma
  4:2:0->4:4:4 nearest upsample, bilinear 2x downscale = 2x2 box average, plus an 8-point DCT-II/IDCT
  round-trip and an RLE round-trip identity (the DCT/RLE math is pure JS/TS; the conversions run on
  textures + fragment shaders).

## What each pixel proves

64x64 rgba8unorm `RENDER_ATTACHMENT | COPY_SRC` texture, `copyTextureToBuffer` with 256-byte
`bytesPerRow` padding then unpad on readback, per-pixel hard assert against a closed form:

- clear (`loadOp:'clear'`), uniform-buffer solid quad, axis-aligned gradient, `@builtin(position)`
  checkerboard, `setScissorRect`, `setViewport`, alpha blend, sub-rect readback
- all 5 draw topologies (point / line-list / line-strip / triangle-list / triangle-strip; WebGPU has
  no triangle-fan, and points are always 1px)
- blend factor + op matrix (one/zero, additive, zero/one, dst/zero, max, reverse-subtract)
- all 8 depth compare functions on depth32float with `@invariant` on the vertex `@builtin(position)`
  (quad z=0.5 vs clear 0.75 -> only always / less / less-equal / not-equal draw)
- cull + winding (none / back x ccw-vs-cw / front), color write mask (RED vs ALL), limit/format
  queries, and a 2x2 nearest-sampled texture (TL red / TR green / BL blue / BR white)

## Arch coverage

The WebGPU standalone JS/TS runtime is Deno, and Deno's musl arch reach - verified against the Alpine
package DB and rusty_v8 release assets, not assumed - decides on-target coverage:

- **x86_64**: Alpine edge/community ships a native-musl `deno` -> `webgpu_js` + `webgpu_ts` run
  on-target.
- **aarch64**: Alpine edge/community also ships a native-musl `deno` (rusty_v8 carries an
  `aarch64-unknown-linux-musl` static V8) -> both cells run on-target, same as x86_64.
- **riscv64 / loongarch64**: no Alpine `deno` yet (rusty_v8 ships only `riscv64-gnu`, no loong). These
  arches are a follow-up (riscv64 via the community gnu prebuilt / from-source, loong via the
  V8-loong64 backend port into rusty_v8) and are not advertised here. `prebuild.sh` writes an empty
  manifest and `run-webgpu-render.sh` refuses to emit a 0-carpet pass on those arches.

## Run

```
cargo xtask starry app qemu -t cpu-webgpu-render --arch x86_64
cargo xtask starry app qemu -t cpu-webgpu-render --arch aarch64
```

Host smoke: `programs/run_all.sh` (needs a host Deno + Mesa lavapipe).
