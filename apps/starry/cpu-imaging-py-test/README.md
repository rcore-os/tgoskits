# cpu-imaging-py-test - an industrial-grade Python imaging test carpet (Pillow + imageio + scikit-image)

A deterministic, per-API Python imaging test carpet for StarryOS. Each cell drives a **real imaging
library** - Pillow (`PIL`), `imageio` (v3), scikit-image (`skimage`) - on **KNOWN, fixed inputs** and
asserts every result against a **CLOSED-FORM / numpy golden** computed by hand: PIL's L24 ITU-R 601-2 luma,
bilinear interpolation at derived source coordinates, analytic drawn masks, impulse-response kernels,
byte-exact PNG/BMP/PPM/TIFF/GIF round-trips + a JPEG PSNR floor, scikit-image's BT.709 luma, a Sobel ramp's
constant gradient, Otsu on a bimodal field, morphology on a known pattern, `regionprops` on known blobs, and
cross-library decode agreement. **"import PIL" / "imread succeeded" is NOT a test** - every leg checks a
value predicted from first principles or a calibrated numpy golden.

The carpet **tests the libraries; it does not reimplement any imaging algorithm.** The only self-written
code is the three-gate marker and the closed-form / numpy reference helpers (`py/img_common.py`) and the
four cells.

## Libraries (reused, not reinvented - apk + pip over HTTP)

- **Pillow (PIL)** - Alpine `py3-pillow` (musl). `from PIL import Image, ImageDraw, ImageFilter`.
- **numpy** - Alpine `py3-numpy`. The **golden reference** lib; the comparison and every closed form is ours.
- **scipy** - Alpine `py3-scipy`. A scikit-image runtime dependency.
- **imageio** - pip wheel (`py3-imageio` is not reliably on Alpine). Fetched with `pip download` over HTTP to
  PyPI and installed offline (`pip install --no-index --find-links`). Only still-image codecs are exercised;
  the mp4/ffmpeg video leg is excluded because the `imageio_ffmpeg` binary is not available for all four
  arches (no riscv64/loongarch64), so it could not really run everywhere and is dropped, not skip-passed.
- **scikit-image** - pip wheel (musllinux per arch), same offline-install path; pulls its pure-python deps
  (`lazy-loader`, `networkx`, `tifffile`, `imageio`, `packaging`).

Network model follows the repo owner's constraint (apt broken behind a proxy, SSH blocked): everything is
fetched over **HTTP** - apk for the musl Pillow/numpy/scipy wheels, `pip download` for the imageio +
scikit-image wheels, then an **offline** `pip install`.

## Cells (three-gate `IMAGING_<CELL> OK <n>` marker)

Each cell prints `IMAGING_<CELL> OK <n>` only when `fail==0 && total==pass && total>0`; otherwise
`IMAGING_<CELL> FAILED ...` and a non-zero exit. `run_all.sh` gates on the FIXED `expected_cells` manifest -
all four cells always, so `EXPECTED` is constant (4) across arches - and prints `TEST PASSED` only when
`fail==0 && total==EXPECTED==pass`. The per-cell assertion counts below are calibrated to the real host run
(versions in the table footer).

| cell | checks | what is asserted (closed form / numpy golden) |
|------|-------:|-----------------------------------------------|
| `imaging_pil`        | 45 | new/getpixel/putdata == flat-index raster; NEAREST 2x == block replication; BILINEAR == derived linear-interp closed form; ROTATE_90/180 + FLIP + TRANSPOSE == `np.rot90`/reverses; RGB->L == PIL L24 601-2 (`76,150,29` primaries) + `<=1 LSB` of real 601-2; RGB<->RGBA; ImageDraw rectangle/line/diagonal/ellipse(πr²)/polygon/text analytic masks; ImageFilter BLUR/GaussianBlur impulse+constant-field, FIND_EDGES on step/solid; point/eval inverse & halve ramps; PNG/BMP/PPM/TIFF/GIF byte-exact + JPEG q95 PSNR>30; getbbox/histogram/split/merge/ImageChops |
| `imaging_imageio`    | 21 | imwrite/imread PNG/BMP/TIFF/PPM byte-exact (shape+dtype); grayscale PNG/PGM byte-exact; in-memory `<bytes>` imencode/imdecode == source; JPEG PSNR>28 on a smooth gradient; 4-frame GIF stack frame-count + per-frame constant field; volumetric TIFF stack byte-exact; `improps` shape/dtype (mp4/ffmpeg video leg dropped - `imageio_ffmpeg` binary unavailable on all four arches) |
| `imaging_skimage`    | 37 | `color.rgb2gray` == `0.2125R+0.7154G+0.0721B` (BT.709, pins green=0.7154); rgb2hsv/lab round-trip + HSV(red)=(0,1,1); `filters.gaussian` impulse unit-sum + constant fixed point; `filters.sobel` ramp == `sqrt(2)`, `sobel_v`==2, constant==0; `threshold_otsu` separates a bimodal field; `transform.resize`/`rescale`/`rotate`/`warp` block-replication, `np.rot90`, identity, exact pixel shift; `morphology.dilation`/`erosion`/`opening`/`closing` 9-block/dot/speck/hole + grayscale ordering bound; `feature.canny` step edge localized to cols {9,10,11}; `corner_harris`/`corner_peaks` checkerboard; `measure.label`/`regionprops` two blobs -> area/centroid/bbox `(9,(2,2),(1,1,4,4))` & `(6,(6.5,7))`; `exposure.rescale_intensity`/`histogram`; `util.img_as_float`/`img_as_ubyte` exact endpoints |
| `imaging_realassets` | corpus-dependent | PIL + imageio + skimage decode the **same** staged image and agree byte-for-byte on shape/dtype/pixels, share one SHA-256 of the decoded buffer, and compute the same quantized dominant color (red for the pinned sample). The pinned `programs/sample_red.png` always runs (a real content assertion); a decode failure of any staged corpus image is a **hard failure**, not a silent drop. Assertion count scales with the staged corpus (6 per corpus image + the pinned red golden) |

Total on the reference host: **45 (pil) + 21 (imageio) + 37 (skimage) + realassets (corpus-scaled) per-API
assertions across the 4 fixed cells.**

### PIL 601-2 luma vs scikit-image BT.709 luma (asserted against DIFFERENT documented formulas)

The two libraries use **different, documented** RGB->gray weightings, and each cell asserts against **its
own** formula, not a shared one:

- **PIL** `convert("L")` = ITU-R **601-2**: `L = R*299/1000 + G*587/1000 + B*114/1000`, computed as the L24
  fixed point `(R*19595 + G*38470 + B*7471 + 0x8000) >> 16` (byte-exact, verified 0 mismatches over the
  probe grid). Pure primaries -> `(76, 150, 29)`.
- **scikit-image** `color.rgb2gray` = **BT.709**: `0.2125 R + 0.7154 G + 0.0721 B` on float `[0,1]` images.
  Pure green -> `0.7154`.

Green weighs `0.587` (PIL) vs `0.7154` (skimage) - so a green pixel's luma differs by design. Conflating
the two would be a bug; the carpet keeps them separate.

## Determinism

`np.random.seed(0x233)` wherever any RNG path appears (the round-trip test rasters); every other input is a
fixed literal. All math is IEEE-754 CPU or integer, so pixels and values are identical across arch. Drawing
uses non-antialiased primitives for exact masks. Two legs carry a documented tolerance because the operation
is legitimately lossy or sub-pixel: the JPEG legs use a stated **PSNR floor**; the BILINEAR closed form uses
PIL's **round-half-away-from-zero** at `.5` boundaries. Everything else is exact.
`tools/gen_goldens.py` re-derives every pinned constant from numpy/Pillow/skimage for review.

## Mutation tests (the assertions bite)

Verified host-side that the goldens actually fail on a wrong result:

- **skimage**: changing the `rgb2gray` expected coefficient `0.7154 -> 0.7000` in `img_common.py` makes
  `imaging_skimage` report `FAILED pass=35 total=37 fail=2` (exit 1); restoring it returns `OK 37`.
- **PIL**: shifting the drawn rectangle by one column (`[5,5,14,12] -> [6,5,15,12]`) while keeping the
  analytic mask makes `imaging_pil` report `FAILED pass=44 total=45 fail=1` (exit 1); restoring returns
  `OK 45`.

## Layout

```
cpu-imaging-py-test/
├── prebuild.sh                      # apk add py3-pillow/numpy/scipy for the target arch, pip-download
│                                    # imageio + scikit-image wheels (HTTP), install offline into
│                                    # site-packages, stage CPython + site-packages into the overlay,
│                                    # write the FIXED expected_cells manifest (all four cells); HARD-FAILS
│                                    # if an imageio/scikit-image wheel cannot be provisioned for an arch
├── programs/
│   ├── run_all.sh                   # on-target runner: runs each manifest cell with python3, gates on
│   │                                # IMAGING_<CELL> OK <n> + the manifest, prints TEST PASSED/FAILED
│   └── carpets/py/
│       ├── img_common.py            # Gate (three-gate marker) + closed-form helpers (601-2 luma, BT.709
│       │                            # luma, gaussian taps)
│       ├── imaging_pil.py           # Pillow cell
│       ├── imaging_imageio.py       # imageio v3 cell
│       ├── imaging_skimage.py       # scikit-image cell
│       └── imaging_realassets.py    # cross-library decode-consistency cell (pinned sample always runs)
├── tools/gen_goldens.py             # re-derive every pinned constant from numpy/Pillow/skimage
├── programs/sample_red.png          # pinned deterministic red-dominant image for the real-asset leg (committed)
├── build-{x86_64,aarch64,riscv64,loongarch64}-*.toml   # per-arch kernel build (ax-driver/nvme)
├── qemu-{x86_64,aarch64,riscv64,loongarch64}.toml      # per-arch QEMU boot + run_all.sh
└── README.md
```

## Running the carpet

On StarryOS (via the app runner) the four QEMU targets each boot the Alpine rootfs, run
`sh /usr/bin/run_all.sh`, and pass on `TEST PASSED`:

```
cargo xtask starry app qemu -t cpu-imaging-py-test
```

Host validation (what was actually run to calibrate the per-cell assertion counts): stage the libraries into
a venv or the Alpine site-packages, put the cells on `PYTHONPATH`, and run each cell / `run_all.sh`. On the
reference host all four cells go green: `IMAGING_PIL OK 45`, `IMAGING_IMAGEIO OK 21`, `IMAGING_SKIMAGE OK 37`,
`IMAGING_REALASSETS OK <n>` (n scales with the staged corpus) -> `TEST PASSED`.

## Fixed manifest (no self-shrinking)

`expected_cells` is FIXED - all four cells (`imaging_pil`, `imaging_imageio`, `imaging_skimage`,
`imaging_realassets`) are always listed, so `EXPECTED` is constant across arches and the three-gate cannot be
satisfied by a shrunk manifest. `imaging_pil` needs PIL + numpy from apk; `imaging_imageio` /
`imaging_skimage` / `imaging_realassets` need the imageio / scikit-image pip wheels, which must run on all
four arches per the four-dimension bar. If a wheel cannot be resolved / staged / imported for an arch,
`prebuild.sh` HARD-FAILS (a "不支持" to surface) rather than dropping the cell. The mp4/ffmpeg video leg is
excluded entirely (no all-arch `imageio_ffmpeg` binary) - it is dropped, never counted as a skip-as-pass.

Reference host versions: Pillow 12.3.0, numpy 1.26.4, imageio 2.37.4, scikit-image 0.26.0, scipy 1.17.1,
CPython 3.12 (the on-target Alpine versions differ; the closed-form goldens are version-independent, and the
assertions are written to the documented library formulas, not a pinned build).
