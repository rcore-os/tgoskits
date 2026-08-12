#!/bin/sh
# On-target runner for the cpu-opencv-test carpet - an industrial-grade OpenCV test carpet for StarryOS.
# Each cell drives real OpenCV (cv::Mat / cvtColor / GaussianBlur / resize / threshold / drawing / Canny /
# imencode / VideoWriter ...) on KNOWN, fixed inputs and asserts the result against a CLOSED-FORM / numpy
# golden computed by hand (BT.601 luma, Porter-Duff, the normalized Gaussian kernel, a Sobel gradient's
# constant derivative, bilinear interpolation, an analytic drawn shape, a known step-edge column, a
# byte-exact PNG/BMP round-trip). "cv2 imported" is NOT a test - every leg checks a predicted value.
#
# Two bindings, run side by side:
#   cpp/opencv_*  - the C++ cells (link libopencv_* via pkg-config opencv4), cross-compiled by prebuild.
#   py/opencv_*   - the Python cells (import cv2 + numpy from Alpine py3-opencv / py3-numpy).
#
# Cells (each in BOTH cpp and py): mat, color, filter, geometry, morph, draw, feature, io.
# Prints "TEST PASSED" only when every provisioned cell reports its "OPENCV_<CELL> OK <n>" marker
# (three-gate: fail==0 && total==pass && total>0) and the run matches the expected_cells manifest.
set -u
BIN=/opt/cpu-opencv-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
export LD_LIBRARY_PATH="/usr/lib:/lib:${LD_LIBRARY_PATH:-}"
export HOME="${HOME:-/tmp}"
export TMPDIR="${TMPDIR:-/tmp}"
# Real-asset leg (opencv_io) reads images from ASSET_DIR; defaults to the staged assets next to the cells.
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"
# make the staged cv2 importable without a system-wide install
if [ -d "$BIN/py" ]; then export PYTHONPATH="$BIN/py:${PYTHONPATH:-}"; fi

ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-opencv-test: detected CPU count = $ncpu; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
# run one manifest entry. An entry is "cpp/<cell>" (an ELF to exec) or "py/<cell>" (a .py to run with
# python3). Both must print "OPENCV_<CELL> OK <n>" and exit 0.
run() {
    entry="$1"
    kind=${entry%%/*}; cell=${entry#*/}
    case "$kind" in
        cpp) prog="$BIN/cpp/$cell"
             [ -x "$prog" ] || { echo "cpu-opencv-test: $entry in manifest but binary absent"; total=$((total+1)); fail=$((fail+1)); return 0; }
             total=$((total+1))
             out="$(cd "$BIN" && "$prog" 2>&1 </dev/null)"; rc=$? ;;
        py)  prog="$BIN/py/$cell.py"
             [ -f "$prog" ] || { echo "cpu-opencv-test: $entry in manifest but script absent"; total=$((total+1)); fail=$((fail+1)); return 0; }
             total=$((total+1))
             out="$(cd "$BIN" && python3 "$prog" 2>&1 </dev/null)"; rc=$? ;;
        *)   echo "cpu-opencv-test: bad manifest entry '$entry'"; total=$((total+1)); fail=$((fail+1)); return 0 ;;
    esac
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        printf '  [%s] ' "$entry"; echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass+1))
    else
        echo "$out" | tail -12
        echo "CARPET FAILED: $entry (exit $rc)"
        fail=$((fail+1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-opencv-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell"
done 3< "$MANIFEST"

echo "cpu-opencv-test: $pass/$total OpenCV carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-opencv-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
