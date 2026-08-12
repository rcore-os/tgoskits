#!/bin/sh
# On-target runner for the cpu-image-test carpet - the "pyte for images". Each cell drives a real
# decoder/rasterizer (stb_image, stb_image_write, nanosvg+nanosvgrast) and asserts the output BYTE-EXACT
# (per-pixel SHA-256 / closed-form pixel regions) or PSNR-bounded (lossy) against goldens calibrated
# host-side with the same libraries. Prints "TEST PASSED" only when every provisioned cell reports its
# "IMAGE_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass).
#
# Cells:
#   image_raster     - decode the 6-format zoo (png/bmp/tga/ppm/pgm/jpg): the 4 lossless formats
#                      decode byte-exact to one shared RGB SHA + == reference; PGM gray SHA; JPEG PSNR;
#                      plus a synthetic in-memory pattern encoded via stb_image_write to png/bmp/tga and
#                      decoded back byte-exact (closed-form, no assets).
#   image_formats    - format matrix: png/bmp/tga byte-exact round-trip + magic + header; JPEG PSNR;
#                      hand-written ppm/pgm round-trip; prebuild-staged GIF byte-exact. WebP is not tested
#                      (stb has no WebP codec) and not claimed.
#   image_svg        - nanosvg rasterization: circle/rect/gradient/even-odd-vs-nonzero closed-form
#                      per-pixel; scale invariance 1x vs 2x; real 3DBenchy SVG vs calibrated golden SHA.
#   image_realassets - decode honkai3_base/wall PNGs: exact dims + channels + downscaled-signature golden.
#                      Hard-fails if the prebuild-staged rasters are absent (submodule staging failure).
set -u
BIN=/opt/cpu-image-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
# Asset dir: prebuild stages the format zoo + real rasters + benchy.svg + pal.gif here from the media
# submodule (prebuild hard-fails if they do not stage), so on-target the assets are present and the
# real-asset legs positively gate. ASSET_DIR may override the mount point.
export IMAGE_DIR="${IMAGE_DIR:-$BIN/assets}"
export ASSET_DIR="${ASSET_DIR:-$IMAGE_DIR}"
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-image-test: detected CPU count = $ncpu; IMAGE_DIR=$IMAGE_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-image-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E "OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-image-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-image-test: $pass/$total image carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-image-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
