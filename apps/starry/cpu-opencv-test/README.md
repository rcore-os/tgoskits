# cpu-opencv-test - an industrial-grade OpenCV test carpet (pure CPU, C++ + Python)

A deterministic, per-API OpenCV test carpet for StarryOS. Each cell drives **real OpenCV** (`cv::Mat` /
`cvtColor` / `GaussianBlur` / `resize` / `threshold` / drawing / `Canny` / `imencode` / `VideoWriter` ...)
on **KNOWN, fixed inputs** and asserts the result against a **CLOSED-FORM / numpy golden** computed by hand:
BT.601 luma, Porter-Duff, the normalized Gaussian kernel, a Sobel gradient's constant derivative, bilinear
interpolation, an analytic drawn shape, a known step-edge column, byte-exact PNG/BMP/PPM/TIFF/WebP
round-trips, and a lossless FFV1 video round-trip. **"cv2 imported" is NOT a test** - every leg checks a
value predicted from first principles or calibrated numpy golden.

The carpet **links against OpenCV (it TESTS OpenCV); it does not reimplement any CV algorithm.** The only
self-written code is the three-gate marker + the closed-form / numpy reference helpers (`cpp/cv_common.h`,
`py/cv_common.py`) and the eight cells.

## Bindings (the two mature, apk-available OpenCV bindings, both TESTED)

- **C++** - Alpine `opencv` + `opencv-dev` (musl); each cell links `libopencv_core / libopencv_imgproc /
  libopencv_imgcodecs / libopencv_videoio / ...` via `pkg-config opencv4`.
- **Python** - Alpine `py3-opencv` (musl) `import cv2` + `numpy` from `py3-numpy`.

Every cell exists in **both** cpp and py. Rust (`opencv` crate) is a documented follow-up - see "Rust binding".

## Cells (three-gate `OPENCV_<CELL> OK <n>` marker)

Each cell prints `OPENCV_<CELL> OK <n>` only when `fail==0 && total==pass && total>0`; otherwise it prints
`OPENCV_<CELL> FAILED ...` and exits non-zero. `run_all.sh` gates on the `expected_cells` manifest
(`fail==0 && total==EXPECTED==pass`, `EXPECTED>=1` floor) and prints `TEST PASSED` / `TEST FAILED`. The
EXPECTED counts below are calibrated to the real host run (OpenCV 4.13.0, numpy 2.5.1).

| cell | cpp checks | py checks | what is asserted (closed form) |
|------|-----------:|----------:|--------------------------------|
| `opencv_mat`      | 16 | 16 | add/sub/mul/gemm/transpose/det/inv element-exact vs numpy; type/channels/ROI-as-view/reshape/countNonZero/minMax |
| `opencv_color`    | 14 | 14 | BGR<->RGB exact swap; BGR2GRAY == BT.601 fixed-point (76/150/29/128); YCrCb/HSV known primaries; I420 luma == studio-swing BT.601 |
| `opencv_filter`   | 14 | 12 | GaussianBlur(impulse) == outer(getGaussianKernel) (taps 0.054489/0.244201/0.40262); Sobel(10·x ramp)==80; box/blur(const)==const; medianBlur removes outlier; filter2D(identity)==input |
| `opencv_geometry` | 12 | 12 | resize NEAREST block replication; LINEAR bilinear==7.5; flip/transpose; warpAffine translation exact; getRotationMatrix2D(90); 90° rotation exact mapping; getAffineTransform |
| `opencv_morph`    | 12 | 12 | threshold BINARY/INV/TRUNC exact split; Otsu on bimodal; dilate(dot)==9-block, erode inverts; open removes speck; close fills hole; connectedComponents==4 |
| `opencv_draw`     | 23 | 21 | rectangle exact analytic mask (area 240); axis-aligned + diagonal lines exact; filled circle analytic πr² coverage sweep; ellipse axis samples; fillPoly/polylines triangle; putText ink-in-bbox |
| `opencv_feature`  | 10 | 10 | Canny step edge localized at the known column; cornerHarris/goodFeaturesToTrack snap to checkerboard intersections; HoughLinesP recovers the known horizontal line |
| `opencv_io`       | 17 | 17 | PNG/BMP/PPM/TIFF/WebP + PGM lossless byte-exact; JPEG q95 PSNR>35 (lossy); imwrite/imread round-trip; VideoWriter+VideoCapture FFV1 clip -> 5 frames + first-frame content; real-asset leg (honest-skip if no ASSET_DIR image) |

Total on the reference host: **cpp 118 + py 114 = 232 per-API assertions across 16 cells.**

## Determinism

`cv2.setNumThreads(1)` / `cv::setNumThreads(1)`; fixed inputs everywhere; a fixed RNG seed (`0x233`) wherever
a random path could appear (the `opencv_io` test image). All math is integer or IEEE-754 CPU, so pixels and
values are identical across arch. Anti-aliasing is off (`LINE_8`) in `opencv_draw` for exact pixels.

Two goldens carry a documented `±1` slack because OpenCV's exact fixed-point rounding differs by at most one
LSB from the textbook integer form: the I420 studio-swing luma and the YCrCb luma. Everything else is exact
(or, for the legitimately lossy JPEG and the sub-pixel-tolerant feature detectors, a stated PSNR / location
bound). `tools/gen_goldens.py` re-derives every pinned constant from numpy for review.

## Layout

```
cpu-opencv-test/
├── prebuild.sh                      # apk add opencv-dev + py3-opencv for target arch, HOST cross-compile the
│                                    # C++ cells, stage OpenCV runtime + CPython + cv2/numpy into the overlay
├── programs/
│   ├── run_all.sh                   # on-target runner; dispatches cpp/ ELFs and py/ scripts; three-gate marker
│   └── carpets/
│       ├── cpp/cv_common.h          # three-gate marker + closed-form helpers (BT.601, Porter-Duff, ...)
│       ├── cpp/opencv_*.cpp         # the 8 C++ cells (link libopencv_* via pkg-config opencv4)
│       ├── py/cv_common.py          # three-gate marker + numpy closed-form helpers
│       ├── py/opencv_*.py           # the 8 Python cells (import cv2 + numpy)
│       └── third_party/README.md    # links OpenCV, vendors nothing
├── tools/gen_goldens.py             # re-derives the closed-form constants (numpy) for review
├── build-x86_64-unknown-none.toml            # ax-driver/nvme (+ serial on la/rv)
├── build-aarch64-unknown-none-softfloat.toml
├── build-riscv64gc-unknown-none-elf.toml
├── build-loongarch64-unknown-none-softfloat.toml
└── qemu-{x86_64,aarch64,riscv64,loongarch64}.toml   # nvme rootfs, run_all.sh, TEST PASSED gate
```

## Running

```sh
cargo xtask starry app qemu -t cpu-opencv-test --arch x86_64
cargo xtask starry app qemu -t cpu-opencv-test --arch aarch64
cargo xtask starry app qemu -t cpu-opencv-test --arch riscv64
cargo xtask starry app qemu -t cpu-opencv-test --arch loongarch64
```

The app runner invokes `prebuild.sh` (per arch) then boots the matching `qemu-*.toml`, which runs
`sh /usr/bin/run_all.sh`. The runner walks `expected_cells` (one `cpp/<cell>` or `py/<cell>` per line),
executes each, and prints `TEST PASSED` only when every provisioned cell reports its `OPENCV_<CELL> OK <n>`.

### Assets (the `opencv_io` real-asset leg)

The closed-form legs need no external data. The `opencv_io` real-asset leg additionally decodes real rasters
from the per-app `assets` git submodule if present; `prebuild.sh` inits + LFS-pulls it automatically. To
provision the images explicitly:

```sh
git -C apps/starry/cpu-opencv-test submodule update --init assets
git -C apps/starry/cpu-opencv-test/assets lfs pull --include="images/*"
```

A missing submodule never fails the gate - the real-asset leg honest-skips on-target and every closed-form
leg still runs. `OPENCV_ASSET_SRC=<dir>` overrides the source directory.

## Host requirements

`prebuild.sh` provisions and cross-compiles entirely on the build host (only `apk` runs under qemu-user).
It needs, on the host: `qemu-user-static` + `e2fsprogs` (rootfs provisioning), a musl cross C++ toolchain
(`${triple}-g++`, e.g. from `/opt/<triple>-linux-musl-cross`) **and** a RELR-aware linker - any LLVM
`ld.lld` (the `lld` package, or `/usr/lib/llvm-*/bin/ld.lld`), or a `zig` on `PATH` - plus outbound network
to the Alpine `edge` CDN and PyPI-free apk resolution. No versions are pinned; whichever RELR-aware linker
is present is used.

Network/DNS: `prebuild.sh` writes the staging root's `resolv.conf` itself - it keeps the host's
**non-loopback** nameservers and appends public resolvers (`1.1.1.1`/`8.8.8.8`/`9.9.9.9`, overridable via
`STARRY_DNS`). It never copies a host loopback stub (`127.0.0.53` systemd-resolved / Docker) into the
qemu-user staging root, whose listener does not exist there and would make `apk` report `DNS: transient
error`. `pulseaudio-libs` (an optional highgui audio backend, unused by the offscreen cells) is installed
best-effort and skipped where Alpine does not package it.

## C++ cross-compile method (why HOST, not target g++)

The C++ cells are compiled on the **host** against the apk-staged OpenCV, not by the Alpine `g++` under
qemu-user. Alpine's `g++` spawns `cc1plus` via `posix_spawn`, which `qemu-user-static` cannot exec, so
running the staged compiler under qemu never compiles. `prebuild.sh` resolves a host C++ cross toolchain for
the target triple and builds each cell with `--sysroot` / OpenCV flags resolved by a **host** `pkgconf` run
against the staging root. Two hazards are handled. First, Alpine's OpenCV `.so` carry a `.relr.dyn` (SHT_RELR
`0x13`) section that the GNU ld 2.37 bundled in the musl-cross toolchains rejects (`unknown type [0x13]
section '.relr.dyn'`); the link therefore uses a RELR-aware linker. `prebuild.sh` probes `${triple}-g++`
against `libopencv_core`: it is used directly if its own `ld` links the RELR libs, else retried as
`${triple}-g++ -fuse-ld=lld` (native GNU C++ ABI, LLD reads RELR) when a standalone `ld.lld` is reachable,
and only otherwise falls back to `zig c++ -target ${triple}`. Second, Alpine OpenCV uses the libstdc++ GNU
C++ ABI (`std::__cxx11`); the `${triple}-g++` paths are that ABI by construction, and the `zig c++` fallback
compiles against the **staged** libstdc++ headers and links the staged `libstdc++.so.6` to match it exactly.

## Host validation

The x86_64 staging root was built the same way `prebuild.sh` does: an Alpine `edge` minirootfs with `apk add
opencv opencv-dev build-base pkgconf py3-opencv py3-numpy pulseaudio-libs` (OpenCV **4.13.0**, libstdc++
**15.2.0**). All 8 C++ cells were then host cross-compiled for `x86_64-linux-musl` via `zig c++` against the
staged libstdc++ headers and linked with the staged OpenCV `.so` + `libstdc++.so.6`. Each produced a valid
target ELF (`interpreter /lib/ld-musl-x86_64.so.1`, `NEEDED libopencv_core.so.413`, `libstdc++.so.6`), and
each ran on the staged OpenCV printing its marker: **8/8 C++ cells OK** (color 14, draw 23, feature 10,
filter 14, geometry 12, io 17, mat 16, morph 12 = 118 assertions).

This confirms the cross-toolchain ABI concern is resolved, not merely assumed: the naive `${triple}-g++`
fails to link Alpine's OpenCV (its binutils `ld` rejects `.relr.dyn`), and a plain `zig c++` fails the C++
ABI (its default libc++ `std::__1` mangling does not match OpenCV's `std::__cxx11`); the working path is
`zig c++` + staged libstdc++ headers/lib, which links and runs.

The assertions are mutation-tested: perturbing the pinned Gaussian center tap (`opencv_filter`, cpp+py) and
shifting a drawn shape while leaving its analytic golden fixed (`opencv_draw`, cpp+py) each flip the cell to
`FAILED` with a non-zero exit, proving the goldens actually constrain OpenCV's output.

## Rust binding (follow-up)

The Rust `opencv` crate binds the same `libopencv_*`. A bounded host build attempt against the staged
`opencv4` (with `OPENCV_INCLUDE_PATHS` / `OPENCV_LINK_*` pointed at the Alpine sysroot and host `libclang`)
did **not** complete within 30 minutes: the crate pulls a very large transitive dependency tree and runs
full-header `bindgen` over OpenCV's headers. It is not a clean quick link like C++/Python, so it is left as a
documented follow-up. C++ and Python - the two mature, apk-available bindings - are the priority and are both
complete and green.
