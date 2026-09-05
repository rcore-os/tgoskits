#!/bin/sh
# On-target runner for the cpu-gui-qt-test carpet - a "pyte for GUI widgets". Each cell drives real Qt
# Widgets / QPainter rendering on the CPU RASTER paint engine with the offscreen QPA platform plugin
# (QT_QPA_PLATFORM=offscreen) - NO GPU, NO display server - and asserts CLOSED-FORM goldens: exact per-pixel
# colors from known fillRect/drawLine/drawEllipse geometry, Porter-Duff "over" alpha compositing computed by
# hand, exact layout geometry() from the QVBoxLayout/QHBoxLayout/QGridLayout math, and post-event widget
# state from injected QTest events. Prints "TEST PASSED" only when every provisioned cell reports its
# "GUI_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass).
#
# Cells:
#   gui_render      - per-pixel widget rendering vs closed form: fillRect exact pixels + untouched background,
#                     drawLine axis-aligned coverage, drawEllipse analytic pi*r^2 coverage, Porter-Duff over
#                     alpha compositing per pixel, a grabbed QLabel's palette bg + centered ink, one text glyph
#                     ink-in-bbox (font-agnostic).
#   gui_layout      - deterministic geometry: QVBox/QHBox/QGrid child geometry() == closed-form layout math,
#                     resize -> stretch children re-layout, sizeHint/minimumSizeHint composition.
#   gui_interact    - per-interaction: QTest::mouseClick fires a real handler (and a disabled button does not),
#                     QCheckBox toggle changes state AND indicator pixels, QLineEdit keyClicks/backspace/select,
#                     QSlider arrow/page/home/end move value() by the exact step.
#   gui_realassets  - load a real font from ASSET_DIR into Qt and render a label; assert family + ink-in-bbox.
#                     Honest-skips (still passes with total>0) if no font is staged.
set -u
BIN=/opt/cpu-gui-qt-test
export PATH="/usr/bin:/usr/local/bin:$PATH"

# Qt runtime: offscreen QPA (pure CPU raster, no display server). Point the loader at the staged Qt libs and
# the platform plugin dir; a writable HOME/XDG dir keeps QStandardPaths quiet.
export QT_QPA_PLATFORM=offscreen
export QT_QPA_PLATFORM_PLUGIN_PATH=/usr/lib/qt6/plugins/platforms
export LD_LIBRARY_PATH="/usr/lib:/lib:${LD_LIBRARY_PATH:-}"
export HOME="${HOME:-/tmp}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}"
# Asset dir for the real-font leg; defaults to the staged assets next to the binaries.
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"

ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-gui-qt-test: detected CPU count = $ncpu; QT_QPA_PLATFORM=$QT_QPA_PLATFORM; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-gui-qt-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -12
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-gui-qt-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-gui-qt-test: $pass/$total GUI carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-gui-qt-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
