#!/bin/sh
# On-target runner for the cpu-video-test carpet - the "pyte for video". Each cell decodes video to
# raw pixels with the `ffmpeg` CLI and asserts in the PIXEL domain (per-frame sha256 / 8x8 luma
# signature / binary white-ratio / PSNR / SSIM / PTS spacing) against analytically-known or golden
# references. Prints "TEST PASSED" only when every provisioned cell reports its
# "VIDEO_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor).
#
# Cells:
#   video_frames     - Bad Apple binary-frame leg (rgb24 sha + 8x8 luma sig + white-ratio vs golden;
#                      honest-skips if $ASSET_DIR absent) + a synthetic testsrc leg that always runs
#                      (smptebars closed-form bar colors, ffv1 lossless gradient/checkerboard round-trip).
#   video_codec      - codec x container matrix {ffv1,h264,hevc,vp9,mpeg2} x {valid containers}:
#                      encode -> decode first/mid frame -> lossless byte-exact (ffv1) / lossy PSNR+SSIM
#                      floors + flat-region structure. Fully synthetic source, no asset needed.
#   video_meta       - CFR timing: frame count == round(dur*fps), resolution / pix_fmt / SAR / fps
#                      exact, PTS strictly monotonic + evenly spaced dt == 1/fps.
#   video_realassets - decode the real transcodes + assert codec/geometry/firstframe-sha/t2-sha vs
#                      golden; honest-skip when $ASSET_DIR absent (the synthetic legs always gate).
set -u
BIN=/opt/cpu-video-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
# Real-asset dir: on-target the media submodule mounts here; default keeps the synthetic legs gating
# even if it is absent (video_frames / video_realassets honest-skip their real-asset legs).
export ASSET_DIR="${ASSET_DIR:-$BIN/assets}"
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-video-test: detected CPU count = $ncpu; ASSET_DIR=$ASSET_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-video-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -10
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-video-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-video-test: $pass/$total video carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-video-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
