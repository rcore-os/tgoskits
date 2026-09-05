# cpu-tcltk-gui-test - a "pyte for GUI widgets" (Tcl/Tk, headless Xvfb)

An industrial-grade GUI-framework test carpet for StarryOS built on **Tcl/Tk driven headlessly through
Xvfb**. Tk widgets, canvas items, and photo images render against a virtual-framebuffer X server
(`Xvfb`, no physical display); `$photo get x y` hands pixels back for per-pixel assertions, the canvas
`coords`/`bbox` report Tk's own item geometry, the geometry managers (`pack`/`grid`/`place`) realize exact
child geometry, and `event generate` injects real mouse/key events for interaction testing. Fully
deterministic, pure CPU. Tcl/Tk is the fifth GUI framework in the goal (alongside Qt and egui).

The carpet **runs Tcl/Tk (it TESTS Tk); it does not reimplement a widget toolkit.** The only self-written
code is the three-gate marker + closed-form helpers (`gui_common.tcl`) and the four cells, which assert Tk's
output against goldens computed from first principles - "widget created" alone is **not** a test.

## Why photo pixels + canvas geometry (the offscreen readback method)

Base Tk 8.6 hands back exact pixels only from a **`photo` image** (Tk's real image surface: after
`$img put color -to x0 y0 x1 y1` fills a half-open span and `$img copy` composites, `$img get x y` reads the
stored RGB). Grabbing a live **canvas** to pixels needs the `Img`/tkimg `window` photo format or a
Ghostscript rasterizer for `canvas postscript`, neither of which base Tk ships. So the render leg asserts
**photo pixels** (Tk's `Tk_PhotoImage` engine) for exact color/compositing and asserts **canvas item
geometry** (`coords`/`bbox`/`find`/`move` - Tk's canvas layout engine) separately. Together they cover
"closed-form pixels" and "closed-form geometry"; both are genuine Tk rendering paths, integer-exact and
identical across arch.

## Cells (three-gate `GUI_<CELL> OK <n>` marker)

Each cell prints `GUI_<CELL> OK <n>` only when `fail==0 && total==pass && total>0`; otherwise it prints
`GUI_<CELL> FAILED ...` and exits non-zero. `run_all.sh` gates on the `expected_cells` manifest
(`fail==0 && total==EXPECTED==pass`, `EXPECTED>=1` floor) and prints `TEST PASSED` / `TEST FAILED`.

### `gui_render` - photo pixels + canvas item geometry (38 checks)
- **photo fillRect**: a 40x30 red rect at `(20,15)` in a 100x80 photo -> every interior pixel exactly red,
  the four background bands untouched dark gray, exact edge pixels (inside vs one-past the half-open span),
  exact covered-pixel count `= 40*30 = 1200`.
- **photo copy**: an 8x8 red image copied to `(6,6)` over a 30x30 blue photo -> the overlay region is exactly
  red, the surrounding blue is untouched, exact counts (`red=64`, `blue=836`).
- **canvas geometry**: rectangle/oval/line/polygon/arc items with known coords -> Tk's `coords`, `bbox`,
  `type`, `itemcget -fill/-start/-extent`, `find overlapping`, and `move` all report the exact closed form.
- **canvas text**: one `'A'` item -> `bbox` is non-degenerate and inside a bounded band (font-agnostic; bbox
  extent, never glyph-exact pixels).

### `gui_layout` - deterministic geometry (36 checks)
- **place**: a child placed at `-x 37 -y 52` sits at exactly `winfo x/y = 37/52` with its fixed size.
- **pack** (vertical stack): child `i` top edge `= PAD + i*(CH + 2*PAD)` -> `[6,48,90]` (pack `-pady` adds
  PAD above and below each child); each child's realized width/height equals the requested size.
- **grid**: 2x2 fixed cells -> `grid info` reports exact row/column/span, `grid bbox` gives exact cell
  rectangles at the origin and past the first row/col, `grid size == 2 2`.
- **labelframe**: `-padx 12 -pady 8` -> the frame's `reqwidth >= inner reqwidth + 2*padx` and
  `reqheight >= inner reqheight + 2*pady`; the inner label is parented to the labelframe.
- **reqsize**: a fixed-size frame's `reqwidth`/`reqheight` equal the requested pixels.

### `gui_interact` - inject events, assert state (25 checks)
- **button**: a synthesized `<Enter>`+`<ButtonPress-1>`+`<ButtonRelease-1>` fires the `-command` exactly
  once, twice on a second click (real event routing through Tk's binding tables); a **disabled** button's
  `invoke` does **not** fire (negative control).
- **checkbutton**: `invoke` toggles the linked variable `0 -> 1 -> 0`; a focused `<space>` key also toggles.
- **entry**: `<KeyPress>` keysym events produce `"hello"`; `<BackSpace>` -> `"hell"`; exact `insert`/`delete`/
  `index`/`icursor` ops (`"world"`, `end==5`, `"orld"`, `"orXYld"`).
- **scale**: `set 42`, `<Right>` -> 43, `<Left>` -> 42 (resolution 1), clamp `set 200 -> 100`, `set -50 -> 0`.
- **listbox**: `insert`/`size`/`get`/`get 0 end`/`selection set`/`curselection`/`selection includes`/`delete`
  are all exact (`50 -> ...` style deterministic sequence).

### `gui_realassets` - real font family metrics (10 checks, or 1 honest-skip)
- Picks a resolvable real font family (a staged `.ttf`'s derived family, else a known monospace family, else
  the Tk logical `Courier` fixed alias). Asserts a **fixed-pitch** family measures N chars as exactly
  `N * one-char` width, `font metrics -fixed == 1`, `linespace >= ascent + descent`, and a larger pixel size
  yields a strictly wider/taller canvas text bbox.
- **Honest-skips** (still passes, `total>0`) when no real font family is resolvable, so the synthetic legs
  always gate. (Base Tk has no `addApplicationFont` file-load API - fonts come from the X server /
  fontconfig, so the "asset" is a resolvable family, not a raw file load.)

## Determinism

Headless `Xvfb` X server (virtual framebuffer, no display), Tk photo/canvas/font engines (pure CPU). Fixed
widget sizes, named colors, fixed-pixel fonts, and a fixed seed (`0x233`) anywhere a random path could
appear. Photo pixels, canvas coords/bbox, and geometry-manager arithmetic are integer-exact and identical
across arch.

## Layout

```
cpu-tcltk-gui-test/
├── prebuild.sh                      # apk add tcl/tk + xvfb + font-dejavu for target arch, stage .tcl cells
│                                    # + a font asset into the overlay, write expected_cells
├── programs/
│   ├── run_all.sh                   # on-target runner; starts Xvfb on :99, runs each .tcl under wish;
│   │                                # three-gate marker
│   └── carpets/
│       ├── gui_common.tcl           # three-gate marker + photo-pixel/bbox/ink closed-form helpers
│       ├── gui_render.tcl
│       ├── gui_layout.tcl
│       ├── gui_interact.tcl
│       ├── gui_realassets.tcl
│       └── third_party/README.md    # runs Tk, vendors nothing
├── tools/gen_goldens.py             # re-derives the closed-form constants for review
├── build-x86_64-unknown-none.toml            # ax-driver/nvme (+ serial on la/rv)
├── build-aarch64-unknown-none-softfloat.toml
├── build-loongarch64-unknown-none-softfloat.toml
├── build-riscv64gc-unknown-none-elf.toml
├── qemu-x86_64.toml                 # headless Xvfb, -smp 1, 1024M
├── qemu-aarch64.toml
├── qemu-loongarch64.toml            # dynamic platform (uefi=false, to_bin=true)
└── qemu-riscv64.toml
```

## Host validation (real output)

Tcl/Tk 8.6 and Xvfb (Debian/Ubuntu host) were used to run each cell under `wish` against a headless Xvfb
display. Full gate (runner starts its own Xvfb on `:99`, or run under `xvfb-run -a`):

```
cpu-tcltk-gui-test: detected CPU count = 12; DISPLAY=:99; wish=/usr/bin/wish; ASSET_DIR=.../assets
GUI_RENDER OK 38
GUI_LAYOUT OK 36
GUI_INTERACT OK 25
GUI_REALASSETS OK 10
cpu-tcltk-gui-test: 4/4 GUI carpets OK on x86_64 (expected 4: gui_render gui_layout gui_interact gui_realassets )
TEST PASSED
```

With no resolvable font family, `GUI_REALASSETS OK 1` (honest-skip) and the gate still passes 4/4.

**Mutation tests** (both correctly FAIL):
- `gui_render`: expect the fillRect interior to be green instead of red -> `GUI_RENDER FAILED pass=37
  total=38 fail=1`, exit 1, and `run_all.sh` reports `TEST FAILED`.
- `gui_interact`: assert the checkbutton variable is still 0 after `invoke` -> `GUI_INTERACT FAILED pass=24
  total=25 fail=1`, exit 1.

Tk version: **8.6**. Display backend: **Xvfb** (virtual framebuffer, no GPU, no physical display).

## On-target run

```
cargo xtask starry app qemu -t cpu-tcltk-gui-test
```

`prebuild.sh` grows the rootfs, `apk add`s `tcl tk xvfb font-dejavu fontconfig` for the target arch via
qemu-user, stages the four `.tcl` cells + a DejaVu font into the overlay, and writes `expected_cells`.
`run_all.sh` then brings up **Xvfb on `:99`** and runs each cell under `wish`, gating on the manifest.

### Honest on-target scoping (display backend)

Tk requires an X display. This carpet supplies it **in userspace via `Xvfb`** - a self-contained
virtual-framebuffer X server that renders into RAM with **no GPU and no physical display device** - so it
does not depend on StarryOS's kernel display/framebuffer bring-up. `run_all.sh` starts `Xvfb :99` and points
`DISPLAY` at it; if Xvfb starts (the Alpine `xvfb` package is a pure-CPU X server), the full carpet runs
headless on-target exactly as on the host.

If `Xvfb` cannot start on a given StarryOS target (e.g. an X-server syscall/driver gap that the display
bring-up work under **#392** would close), `run_all.sh` prints `GATE BLOCKED (display backend gated on #392)`
and fails the gate honestly - it does **not** fall back to a host-only fake pass. So: **works headless
wherever the Alpine `xvfb` X server runs; otherwise the on-target run is gated on #392 display bring-up.**
The host validation above is real and the packaging is arch-portable; whether Xvfb itself boots under
StarryOS per-arch is the remaining on-target question this carpet surfaces cleanly.
