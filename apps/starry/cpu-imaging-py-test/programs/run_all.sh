#!/bin/sh
# On-target runner for the cpu-imaging-py-test carpet - an industrial-grade Python imaging test carpet for
# StarryOS covering Pillow (PIL) + imageio + scikit-image. Each cell drives a REAL imaging library on KNOWN,
# fixed inputs and asserts every result against a CLOSED-FORM / numpy golden computed by hand (PIL's L24
# 601-2 luma, bilinear interpolation at derived source coordinates, analytic drawn masks, impulse-response
# kernels, byte-exact format round-trips, skimage's BT.709 luma, a Sobel ramp's constant gradient, Otsu on a
# bimodal field, morphology on a known pattern, regionprops on known blobs, cross-library decode agreement).
# "import PIL" / "imread succeeded" is NOT a test - every leg checks a predicted value.
#
# Cells (each prints "IMAGING_<CELL> OK <n>", three-gate: fail==0 && total==pass && total>0):
#   py/imaging_pil        - Pillow
#   py/imaging_imageio    - imageio v3
#   py/imaging_skimage    - scikit-image
#   py/imaging_realassets - cross-library decode consistency (pinned sample_red.png always runs)
#
# Prints "TEST PASSED" only when every cell reports OK and the run matches the FIXED expected_cells manifest
# (all four cells: fail==0 && total==EXPECTED==pass, EXPECTED constant across arches).
set -u
BIN=/opt/cpu-imaging-py-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
export LD_LIBRARY_PATH="/usr/lib:/lib:${LD_LIBRARY_PATH:-}"
export HOME="${HOME:-/tmp}"
export TMPDIR="${TMPDIR:-/tmp}"
export MPLBACKEND=Agg
# Real-asset leg reads images from ASSET_DIR; defaults to the staged assets next to the cells.
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"
# make the staged cells importable (img_common) without a system-wide install
if [ -d "$BIN/py" ]; then export PYTHONPATH="$BIN/py:${PYTHONPATH:-}"; fi

ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-imaging-py-test: detected CPU count = $ncpu; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
# run one manifest entry "py/<cell>": a .py to run with python3 that must print "IMAGING_<CELL> OK <n>".
run() {
    entry="$1"
    cell=${entry#*/}
    prog="$BIN/py/$cell.py"
    [ -f "$prog" ] || { echo "cpu-imaging-py-test: $entry in manifest but script absent"; total=$((total+1)); fail=$((fail+1)); return 0; }
    total=$((total+1))
    out="$(cd "$BIN" && python3 "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        printf '  [%s] ' "$entry"; echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass+1))
    else
        echo "$out" | tail -14
        echo "CARPET FAILED: $entry (exit $rc)"
        fail=$((fail+1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-imaging-py-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell"
done 3< "$MANIFEST"

echo "cpu-imaging-py-test: $pass/$total imaging carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-imaging-py-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
