# cpu-gui-qt-test - a "pyte for GUI widgets" (Qt offscreen/raster, pure CPU)

An industrial-grade GUI-framework test carpet for StarryOS built on **Qt6 in offscreen/raster mode**. Qt
Widgets render through Qt's **raster paint engine on the CPU** - no GPU. With the `offscreen` QPA platform
plugin (`QT_QPA_PLATFORM=offscreen`) a `QWidget`/`QImage` renders with **no display server**, and
`QWidget::grab()` / `QImage` hand the pixels back for per-pixel assertions while `QTest::mouseClick` /
`keyClicks` inject real events for interaction testing. Fully deterministic, pure CPU. This is also the front
of the browser GUI stack.

The carpet **links against Qt (it TESTS Qt); it does not reimplement a widget toolkit.** The only
self-written code is the three-gate marker + closed-form helpers (`gui_common.h`) and the four cells, which
assert Qt's output against goldens computed from first principles - "widget created" alone is **not** a test.

## Cells (three-gate `GUI_<CELL> OK <n>` marker)

Each cell prints `GUI_<CELL> OK <n>` only when `fail==0 && total==pass && total>0`; otherwise it prints
`GUI_<CELL> FAILED ...` and exits non-zero. `run_all.sh` gates on the `expected_cells` manifest
(`fail==0 && total==EXPECTED==pass`, `EXPECTED>=1` floor) and prints `TEST PASSED` / `TEST FAILED`.

### `gui_render` - per-pixel widget rendering vs closed form (41 checks)
- **fillRect(20,15,40,30, red)**: every pixel inside `[20,60)x[15,45)` is exactly red; the four background
  bands around it are untouched; exact edge pixels; exact covered-pixel count `= 40*30 = 1200`.
- **drawLine** (axis-aligned): horizontal row `y=30` and vertical column `x=45` carry the pen color at the
  exact interior coverage (36 / 46 hits); pixels off the line stay background.
- **drawEllipse** (filled circle, r=50): center + `r=30` samples are fill; bbox corners and outside-bbox are
  background; total filled area within 6% of `pi*r^2 = 7854`; every pixel inside `r-2` is fill and every
  pixel outside `r+2` is background (analytic coverage sweep).
- **alpha compositing**: red@alpha=128 over opaque green -> **Porter-Duff "over" per pixel**, closed form
  `RGBA(128,127,0,255)`; the green-only ring and outside-both regions verified too.
- **QLabel grab**: a fixed-size `QLabel` with a known palette Window color grabbed to a `QImage` -> corners
  carry the palette background, glyph ink is present and horizontally centered (alignment), edges clean.
- **text glyph**: one `'A'` at a fixed pixel size -> ink lands inside the expected bbox, nothing in the far
  corners. Font-agnostic (bbox + coverage, never glyph-exact pixels).

### `gui_layout` - deterministic geometry (24 checks)
- **QVBoxLayout / QHBoxLayout / QGridLayout** with fixed margins/spacing and fixed-size children: each
  child's `geometry()` equals the closed-form layout math (e.g. vbox children at `y = M + i*(CH+S)`
  -> `[9,46,83]`; hbox at `x = M + i*(CW+S)` -> `[5,56,107,158]`; grid cell origins). `sizeHint()` /
  `minimumSizeHint()` equal the composed extents.
- **resize + stretch**: two `Expanding` children split the usable height; after `resize(200x300)` and again
  `resize(200x500)` they re-layout to the new closed-form positions/heights.

### `gui_interact` - per-interaction: inject events, assert state + re-render (16 checks)
- **QPushButton**: `QTest::mouseClick` fires the `clicked` handler exactly once, twice on a second click; a
  **disabled** button click does **not** fire (negative control).
- **QCheckBox**: click toggles `isChecked()` **and** the grabbed indicator pixels change between checked and
  unchecked; a second click toggles back.
- **QLineEdit**: `keyClicks("hello")` -> `text()=="hello"`; backspace -> `"hell"`; `Ctrl-A` + type ->
  `"world"`.
- **QSlider**: arrow keys move `value()` by exactly `singleStep`, page keys by `pageStep`, Home/End to the
  bounds (50 -> 55 -> 50 -> 70 -> 0 -> 100).

### `gui_realassets` - optional real-font leg (8 checks, or 1 honest-skip)
- Loads a real `.ttf` from `ASSET_DIR` via `QFontDatabase::addApplicationFont`, asserts a family registered,
  renders a string with it, and asserts ink lands in the expected bbox with a clean background outside.
- **Honest-skips** (still passes, `total>0`) when no font is staged, so the synthetic legs always gate.

## Determinism

`QT_QPA_PLATFORM=offscreen`, no display server, Qt raster engine (pure CPU). Fixed widget sizes, `ARGB32`
images (a pixel is a plain `0xAARRGGBB`), fixed pixel-size fonts, and a fixed seed (`0x233`) anywhere a
random path could appear. Pixels and geometry are identical across arch.

## Layout

```
cpu-gui-qt-test/
├── prebuild.sh                      # apk add qt6-qtbase-dev/qt6-qtbase for target arch (qemu-user), compile
│                                    # cells on the HOST cross C++ toolchain, stage Qt6 runtime + plugin + font
├── programs/
│   ├── run_all.sh                   # on-target runner; sets QT_QPA_PLATFORM=offscreen; three-gate marker
│   └── carpets/
│       ├── gui_common.h             # three-gate marker + pixel/Porter-Duff/pi*r^2 closed-form helpers
│       ├── gui_render.cpp
│       ├── gui_layout.cpp
│       ├── gui_interact.cpp
│       ├── gui_realassets.cpp
│       └── third_party/README.md    # links Qt, vendors nothing
├── tools/gen_goldens.py             # re-derives the closed-form constants for review
├── build-x86_64-unknown-none.toml            # ax-driver/nvme (+ serial on la/rv)
├── build-aarch64-unknown-none-softfloat.toml
├── build-loongarch64-unknown-none-softfloat.toml
├── build-riscv64gc-unknown-none-elf.toml
├── qemu-x86_64.toml                 # offscreen/raster, -smp 1, 1024M
├── qemu-aarch64.toml
├── qemu-loongarch64.toml            # dynamic platform (uefi=false, to_bin=true)
└── qemu-riscv64.toml
```

## Compile model - HOST cross C++ toolchain, not qemu-user

The Qt6 stack is provisioned into the staging Alpine rootfs with `apk add` under `qemu-user-static` (apk only
forks/reads/writes, so it runs fine emulated). The cells are then compiled and linked **on the host** with a
native cross C++ toolchain targeting the staging root as a sysroot. The in-guest Alpine `g++` is not used: it
spawns `cc1plus` via `posix_spawn`, which `qemu-user-static` cannot exec, so every emulated C++ compile fails
on `cc1plus`.

`prebuild.sh` resolves the host compiler for `<triple>` in order `${triple}-g++` on `PATH` ->
`/opt/${triple}-cross/bin/${triple}-g++` -> `zig c++ -target ${triple}` -> host `g++` (x86_64 only). Alpine's
Qt6 shared libraries carry `SHT_RELR` (`.relr.dyn`) relocations, so the **linker must be RELR-aware**. An
older cross-binutils (GCC 11 era) rejects the Qt6 `.so` with `unknown type [0x13] section .relr.dyn`; the
prebuild detects that link failure on the first cell and falls through to `zig c++`, whose bundled LLD accepts
the RELR `.so`. zig also statically satisfies the C++ runtime (libc++ / compiler-rt), so the produced binary
carries **no external libstdc++/GLIBCXX/CXXABI dependency** and there is no GCC-11-vs-GCC-14 libstdc++ ABI
skew against the Alpine Qt6 build.

Verified on this host (x86_64): a 6.11.1 Qt6 stack (315 packages) was `apk add`ed into an x86_64 staging root;
the prebuild's own resolver tried `/opt/x86_64-linux-musl-cross` g++ (rejected the RELR `.so`), fell through
to `zig c++`, and linked all four cells. Each is a valid x86_64 musl ELF whose `NEEDED` list carries
`libQt6Widgets.so.6` / `libQt6Gui.so.6` / `libQt6Core.so.6` (+ `libQt6Test.so.6` for `gui_interact`), and all
52 mangled Qt/C++ undefined symbols in `gui_render` resolve against the staged Qt6 libraries (0 unresolved).

## Host validation (real output)

Qt6 (Alpine `qt6-qtbase` 6.11.1, musl) was installed into an Alpine x86_64 staging root and the cells
compiled with the host cross C++ toolchain and run natively with `QT_QPA_PLATFORM=offscreen`. Full gate:

```
cpu-gui-qt-test: detected CPU count = 12; QT_QPA_PLATFORM=offscreen; ASSET_DIR=/opt/cpu-gui-qt-test/assets
GUI_RENDER OK 41
GUI_LAYOUT OK 24
GUI_INTERACT OK 16
GUI_REALASSETS OK 8
cpu-gui-qt-test: 4/4 GUI carpets OK on x86_64 (expected 4: gui_render gui_layout gui_interact gui_realassets )
TEST PASSED
```

(The offscreen QPA prints a benign `This plugin does not support propagateSizeHints()` notice during
`gui_layout`; geometry is driven explicitly via `resize()` + `show()` + `processEvents()`, so it is
irrelevant.) With no font staged, `GUI_REALASSETS OK 1` (honest-skip) and the gate still passes 4/4.

**Mutation tests** (both correctly FAIL):
- `gui_render`: expect the fillRect interior to be green instead of red -> `GUI_RENDER FAILED pass=40
  total=41 fail=1`, exit 1, and `run_all.sh` reports `TEST FAILED`.
- `gui_interact`: assert the checkbox is still unchecked after a click -> `GUI_INTERACT FAILED pass=15
  total=16 fail=1`, exit 1.

Qt version: **6.11.1**. QPA platform: **offscreen**. Paint engine: **raster (CPU)**.

## On-target run

```
cargo xtask starry app qemu -t cpu-gui-qt-test --arch x86_64
cargo xtask starry app qemu -t cpu-gui-qt-test --arch aarch64
cargo xtask starry app qemu -t cpu-gui-qt-test --arch riscv64
cargo xtask starry app qemu -t cpu-gui-qt-test --arch loongarch64
```

The runner exports `STARRY_ROOTFS` / `STARRY_STAGING_ROOT` / `STARRY_OVERLAY_DIR` and calls `prebuild.sh`,
which grows the rootfs, `apk add`s `qt6-qtbase-dev qt6-qtbase font-dejavu fontconfig` for the target arch via
qemu-user, compiles the four cells on the **host cross C++ toolchain** (`${triple}-g++` ->
`/opt/${triple}-cross` -> `zig c++ -target ${triple}` -> host `g++`) linking Qt6 Widgets/Test against the
staging root, stages the Qt6 runtime libs + the `offscreen` QPA plugin + a DejaVu font into the overlay, and
writes `expected_cells`. `run_all.sh` then runs each cell under `QT_QPA_PLATFORM=offscreen` and gates on the
manifest. The host must provide a RELR-aware cross linker for the target - `zig` on `PATH` satisfies every
arch (Alpine's Qt6 `.so` use `.relr.dyn`, which older cross-binutils reject).
