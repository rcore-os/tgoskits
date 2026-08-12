# cpu-image-test - the "pyte for images"

An industrial-grade image test carpet for StarryOS covering **both raster (bitmap) and vector (SVG)**.
Where `pyte` gives a headless terminal you can assert against cell-by-cell, this gives a headless image
decode + rasterize pipeline you can assert against **per pixel**: every cell drives a real decoder or
rasterizer and checks the output BYTE-EXACT against a golden - a per-pixel SHA-256 of the decoded buffer,
a closed-form pixel region (inside/outside a circle, a sharp rect edge, a linear gradient), or a
PSNR-bounded comparison for lossy formats - never a smoke test. "Image loaded" alone is not a test here.

## Libraries (reused, not reinvented)

The cells bundle four pinned single-header libraries under `programs/carpets/third_party/`. These headers
are gitignored (repo-root `.gitignore` excludes `third_party/`), so `prebuild.sh` fetches them from their
pinned upstream commits and verifies each against a SHA-256 pinned to the exact bytes the goldens were
calibrated against - a mismatch or fetch failure is a hard error:

- **stb_image.h** (v2.30, `nothings/stb`) - decode PNG / BMP / TGA / PPM / PGM / JPEG / GIF.
- **stb_image_write.h** (v1.16, `nothings/stb`) - encode PNG / BMP / TGA / JPEG.
- **nanosvg.h** + **nanosvgrast.h** (`memononen/nanosvg`) - parse + rasterize SVG.

```
stb     @ 2c980bb59875b0d32144a71867fbdebb2f77cd20
  https://raw.githubusercontent.com/nothings/stb/2c980bb59875b0d32144a71867fbdebb2f77cd20/stb_image.h
  https://raw.githubusercontent.com/nothings/stb/2c980bb59875b0d32144a71867fbdebb2f77cd20/stb_image_write.h
nanosvg @ 239e102ec2c691f2902e20ace2ed36ee4a35cfe6
  https://raw.githubusercontent.com/memononen/nanosvg/239e102ec2c691f2902e20ace2ed36ee4a35cfe6/src/nanosvg.h
  https://raw.githubusercontent.com/memononen/nanosvg/239e102ec2c691f2902e20ace2ed36ee4a35cfe6/src/nanosvgrast.h
```

GIF (palette-quantized) is staged host-side during prebuild: a palette GIF is written deterministically
(4-colour pattern, no dither - lossless) so the GIF leg gates without a codec regression. Only the
comparison logic - per-pixel diff, PSNR, SHA-256 - and the golden constants are self-written in the
cells. No PNG/JPEG/SVG codec is reimplemented; the point is to
TEST stb/nanosvg. The cells link statically, so on target there is **no runtime image-library dependency**
- only libc + libm.

## Assets

Staged into the image under `/opt/cpu-image-test/assets` from `render-assets/images` +
`render-assets/models` (on target the media submodule may mount at `ASSET_DIR`):

- **Format zoo**: `fmt_ref.png` + `fmt.{bmp,tga,ppm,pgm,jpg}` - the same 640x360 image encoded 6 ways.
- **Real rasters**: `honkai3_base.png` (1920x1080), `honkai3_wall_home.png` (1024x1024).
- **Vector**: `benchy.svg` (3DBenchy outline) + a prebuild-generated palette GIF (`pal.gif`).

The assets are a git submodule that prebuild stages onto the target rootfs, so on-target they are always
present. Prebuild hard-fails if zero assets stage (submodule/LFS failure), and `image_realassets`
hard-fails on-target if the required rasters are absent - the real-image legs positively gate, they do not
pass vacuously. The synthetic (closed-form, in-memory) legs run regardless.

## Cells

Each cell prints `IMAGE_<CELL> OK <n>` only when `fail==0 && total==pass==<n>` (three-gate). `run_all.sh`
gates on the capability manifest: `fail==0 && total==EXPECTED==pass`, EXPECTED>=1 floor. Assertion counts
below are the real host green run with all assets present.

### `image_raster` - bitmap format decode -> pixels - 41 assertions (16 without assets)

- **Format zoo**: decode each of the 6 files with stb_image and assert:
  - the four LOSSLESS formats (PNG/BMP/TGA/PPM) decode **byte-exact to one identical RGB buffer** - a
    single shared SHA-256 (`0f4ff65a...`) and each equals the reference byte-for-byte. Four independent
    format decoders converging on the same pixels bit-for-bit is the strongest possible assertion.
  - PGM (grayscale) decodes to its calibrated gray SHA (`efc07b74...`) at exact dims.
  - JPEG decodes within a PSNR bound (>35 dB, real 37.9 dB) of the reference RGB - lossy, so no SHA.
  - exact dimensions (640x360) and native channel counts (PNG/TGA=4, BMP/PPM=3, PGM=1).
- **Synthetic round-trip (no assets)**: generate a known checkerboard+gradient RGB pattern in memory
  (generator SHA pinned), encode via stb_image_write to PNG/BMP/TGA, decode back, assert **byte-exact
  round-trip** (closed-form, asset-independent), plus known-position pixel probes.

### `image_formats` - format matrix / round-trip + magic + header - 37 assertions

Drive one synthetic pattern through the mainstream raster set:

- **PNG / BMP / TGA**: stb_image_write encode -> stb_image decode, **byte-exact**; magic-byte detection
  (`\x89PNG`, `BM`); `stbi_info` header w/h exact.
- **JPEG**: stb baseline encode -> decode, PSNR bound; SOI magic `FF D8`.
- **PPM (P6) / PGM (P5)**: the cell hand-writes the trivial NETPBM header (encoding only - no decoder
  reinvented) -> stb decode, byte-exact; `P6`/`P5` magic.
- **GIF**: prebuild-staged palette pattern (lossless), stb decode **byte-exact** vs the regenerated
  4-colour pattern; `GIF8` magic; dims. `pal.gif` is staged unconditionally by prebuild.

### `image_svg` - vector rasterization -> per-pixel closed form - 27 assertions (23 without benchy)

nanosvg parse + rasterize, asserting analytically-known output:

- **`<circle r=30>`**: every pixel with dist<28 of center is solid red (`a==255, R>200, G/B<40`); every
  pixel with dist>32 is fully transparent - closed-form inside/outside, exact interior pixel count.
- **`<rect>`**: solid interior, transparent exterior, and a **sharp** left edge (x=21 opaque, x=18
  transparent), exact interior pixel count.
- **two-stop linear `<gradient>`**: each pixel `R==G==B` and equals the closed-form `x*255/100` within
  +/-4, monotonic non-decreasing, endpoints ~0 and ~255.
- **fill-rule even-odd vs nonzero** on nested same-direction rects: even-odd punches the inner hole
  (center transparent), nonzero fills it solid - a genuine discriminator.
- **scale invariance**: the circle at 1x vs 2x (scale=2) inks ~4x as many pixels (area scales scale^2)
  within 3% (measured 4.005x).
- **real 3DBenchy SVG**: rasterize at fixed 512-wide and assert the nanosvg output SHA-256
  (`a43cc8b9...`) + inked pixel count vs the calibrated golden. Honest-skips if `benchy.svg` is absent.

### `image_realassets` - real raster decode, dimension + signature golden - 11 assertions

Decode `honkai3_base.png` (1920x1080 RGBA) and `honkai3_wall_home.png` (1024x1024 RGB) with stb_image and
assert exact width/height, native channel count, a downscaled 8x8-block **signature SHA-256** vs the
calibrated golden, and that the buffer is non-trivial. The assets are a prebuild-staged submodule, so this
cell hard-fails on-target if they are absent (a staging failure), never passing vacuously.

## Determinism of the goldens

stb_image's lossless decoders (PNG/BMP/TGA/PPM/PGM/GIF) are exact integer pipelines and nanosvg's
rasterizer is a deterministic fixed-point coverage rasterizer, so every SHA and closed-form region is
reproducible across arches. The lossy leg (JPEG) is asserted by PSNR bound, never SHA. All goldens
were calibrated once host-side against the exact single-header libraries pinned in `third_party/`; a codec
regression or a decode divergence flips a SHA / breaks a closed-form region and the cell FAILs loudly.

## Coverage

- **Raster formats**: PNG, BMP, TGA, PPM, PGM, JPEG (decode+encode via stb), GIF (decode via stb, palette
  GIF staged by prebuild). 7 mainstream formats.
- **Vector**: SVG `<circle>` / `<rect>` / `<linearGradient>` / filled path (even-odd + nonzero), scale
  invariance, real-model rasterization.
- **Unavailable (documented, honest)**: WebP (stb has no WebP codec, so WebP is not tested and not claimed);
  GIF encode in stb (the palette GIF is staged by prebuild). No format is silently skipped.

## Running

```
cargo xtask starry app qemu -t cpu-image-test --arch x86_64
cargo xtask starry app qemu -t cpu-image-test --arch aarch64
cargo xtask starry app qemu -t cpu-image-test --arch riscv64
cargo xtask starry app qemu -t cpu-image-test --arch loongarch64
```

Each invocation runs `prebuild.sh` (fetch + SHA-verify the vendored headers, host cross-compile the four
cells with a musl-cross toolchain, stage the assets into the overlay) and then boots QEMU running
`run_all.sh`; the run passes when the three-gate prints `TEST PASSED`.

The real image assets live in the per-app `assets` git submodule (rasters are Git LFS objects). On a
fresh checkout, materialize them before running:

```
git submodule update --init apps/starry/cpu-image-test/assets
git -C apps/starry/cpu-image-test/assets lfs pull --include="images/*,models/*,golden/*"
```

`prebuild.sh` also runs this init + sparse LFS pull automatically when `assets/images/fmt_ref.png` is
missing, and hard-fails if any required asset does not stage. Cross-compilation needs a musl-cross
toolchain for the target arch on the host (`<triple>-gcc` on `PATH`, `/opt/<triple>-cross`, `zig cc`, or
`musl-gcc` for a native x86_64 build); no target compiler is ever run under qemu-user.

## Layout

```
cpu-image-test/
  prebuild.sh                       # fetch+verify headers, host cross-compile cells, stage assets into overlay
  build-<arch>.toml x4              # ArceOS build features per arch
  qemu-<arch>.toml x4               # QEMU boot + run_all.sh + success/fail regex per arch
  programs/
    run_all.sh                      # on-target three-gate runner
    carpets/
      image_common.h                # SHA-256 + PSNR + gate + signature primitives (self-written)
      image_raster.c                # cell 1
      image_formats.c               # cell 2
      image_svg.c                   # cell 3
      image_realassets.c            # cell 4
      third_party/                  # pinned stb_image / stb_image_write / nanosvg / nanosvgrast
```
