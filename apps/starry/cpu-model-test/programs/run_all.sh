#!/bin/sh
# On-target runner for the cpu-model-test carpet - a "pyte for 3D models". Each cell drives a real geometry
# pipeline the carpet implements itself (OBJ/STL/PLY parsing, mesh-plane slicing, a barycentric+z-buffer
# software rasterizer, point-cloud stats) and asserts the output against a CLOSED-FORM property (unit-cube
# slice = square perimeter 4 / area 1; sphere cloud centroid at origin, all points at radius r) or a value
# calibrated host-side with this exact code (bunny count/bbox/centroid/spatial-hash sig; suzanne render
# signature; per-layer slice goldens). Prints "TEST PASSED" only when every provisioned cell reports its
# "MODEL_<CELL> OK <n>" marker (three-gate: fail==0 && total==EXPECTED==pass).
#
# Cells:
#   model_parse      - OBJ/STL(bin+ascii)/PLY(ascii+bin) loaders; KNOWN cube parses to identical geometry
#                      across 5 readers (8 verts / 12 tris / bbox / corner set); suzanne+benchy counts/bbox
#                      vs golden; glb via vendored cgltf.
#   model_slice      - mesh-plane intersection: unit cube -> square (perim 4.0 / area 1.0 exact); tessellated
#                      cylinder -> circle within discretization bound; suzanne/benchy per-layer perim/area/
#                      segment-count vs slice_golden.json.
#   model_render     - software rasterizer: cube silhouette solid+square+centered, front-face depth uniform,
#                      farther cube strictly deeper (occlusion); 2-triangle nearest-wins z-buffer; suzanne
#                      coverage + depth signature vs golden.
#   model_pointcloud - PLY cloud: synthetic sphere centroid==origin, all points at radius r (closed form);
#                      Stanford bunny 35947 verts + bbox + centroid + 16^3 spatial-hash signature.
#   model_realassets - iterate every shipped model, assert parse + counts/bbox vs golden. Honest-skip if
#                      MODEL_DIR is absent.
set -u
BIN=/opt/cpu-model-test
export PATH="/usr/bin:/usr/local/bin:$PATH"
# Asset dir: the carpet stages the real models (suzanne/benchy/bunny) + derived closed-form assets (cube in
# 5 formats, sphere point cloud) here; on-target the media submodule may mount at ASSET_DIR. Default keeps
# the synthetic/closed-form legs gating even if the submodule is absent (asset-dependent legs honest-skip).
export MODEL_DIR="${MODEL_DIR:-$BIN/assets}"
export ASSET_DIR="${ASSET_DIR:-$MODEL_DIR}"
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-model-test: detected CPU count = $ncpu; MODEL_DIR=$MODEL_DIR"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-model-test: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
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
[ -f "$MANIFEST" ] || { echo "cpu-model-test: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell <&3; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done 3< "$MANIFEST"

echo "cpu-model-test: $pass/$total model carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-model-test: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
