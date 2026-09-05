#!/bin/sh
# On-target runner: set up the software Vulkan RENDER runtime (Mesa lavapipe) and run the render carpet
# cells. Each cell builds an offscreen render pass into an R8G8B8A8_UNORM image, draws through real
# graphics pipelines, copies the image to a host-visible buffer and checks pixels back against a
# closed-form reference. Prints "TEST PASSED" only when every provisioned cell reports its
# "<name> OK <n>" marker.
set -u
BIN=/opt/cpu-vulkan-render
mkdir -p /tmp/vkrt
export XDG_RUNTIME_DIR=/tmp/vkrt
export LD_LIBRARY_PATH=/usr/lib
# lavapipe: Mesa's software Vulkan device over the LLVM JIT (CPU, no GPU, no surface/swapchain). The
# Vulkan loader discovers it through the lvp ICD manifest; force it so no other ICD is picked up. Mesa
# names the manifest per-arch (lvp_icd.aarch64.json etc), so glob rather than hard-code the name.
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
export VK_ICD_FILENAMES="$ICD"
export VK_DRIVER_FILES="$ICD"
# StarryOS runs one vCPU; pin the lavapipe thread pool to 1. The carpets assert pixel correctness
# against closed-form references, not throughput, so thread count does not affect results.
export LP_NUM_THREADS=1
export NODE_PATH=/opt/cpu-vulkan-render/jsdeps/node_modules
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-vulkan-render: detected CPU count = $ncpu; lavapipe pinned single-threaded (LP_NUM_THREADS=1)"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-vulkan-render: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -6
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
# Capability manifest: prebuild lists the render cells it provisioned on this arch. vulkan_render_c and
# vulkan_render_cpp (native libvulkan, every arch Mesa lavapipe builds for) plus the four render-scene
# cells scene_2dui / scene_3dmodel / scene_anim / scene_codec (C++/libvulkan) and their C ports
# scene_*_c (also native libvulkan) are the guaranteed cells; vulkan_render_rust + scene_*_rust (ash,
# dynamic musl) and vulkan_render_py + scene_*_py (python `vulkan` cffi + numpy) append where
# provisioned. Each render scene has all four bindings (c/cpp/rust/py) with 1:1 assertion parity.
# vulkan_render_* render clear/solid/gradient/checker/scissor/blend plus the exhaustive per-API matrix
# (topologies, blend factor+op, depth-func, cull, colorWriteMask, format queries, texture); the scene_*
# cells build their own pipeline(s)/pass into the offscreen target and assert a 2D-UI composite / a
# depth-buffered Gouraud cube (Vulkan NDC z in [0,1]) / keyframe-animated quads / codec math (YUV->RGB,
# chroma upsample, bilinear downscale, DCT/RLE) - printing SCENE_<NAME>[_C|_RUST|_PY] OK <n>. Every
# cell checks pixels back against a closed-form reference (three-gate: fail==0 && total==EXPECTED==pass).
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-vulkan-render: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done < "$MANIFEST"

echo "cpu-vulkan-render: $pass/$total render carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-vulkan-render: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
