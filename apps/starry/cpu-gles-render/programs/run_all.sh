#!/bin/sh
# On-target runner: set up the software OpenGL ES RENDER runtime and run the render carpet cells.
# Each cell renders into an off-screen FBO and checks pixels back against a closed-form reference.
# Prints "TEST PASSED" only when every provisioned cell reports its "<name> OK <n>" marker.
set -u
BIN=/opt/cpu-gles-render
mkdir -p /tmp/glrt
export XDG_RUNTIME_DIR=/tmp/glrt
export LD_LIBRARY_PATH=/usr/lib
# surfaceless EGL desktop-GL over the gallium llvmpipe DRI driver (CPU software rendering, no GPU).
export EGL_PLATFORM=surfaceless
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
# StarryOS runs one vCPU; pin the mesa thread pool to 1. The carpets assert pixel correctness against
# closed-form references, not throughput, so thread count does not affect results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-gles-render: detected CPU count = $ncpu; llvmpipe pinned single-threaded (LP_NUM_THREADS=1)"

# Each cell's expected green marker. The base bindings print "<NAME>_FULL_API OK <n>"; the four
# real-scenario C++ cells print their scene marker: scene_2dui -> "SCENE_2DUI OK", scene_3dmodel ->
# "SCENE_3DMODEL OK", scene_anim -> "SCENE_ANIM OK", scene_codec -> "SCENE_CODEC OK". run() asserts
# the exact per-cell marker (not just any "OK <n>") so a cell emitting the wrong marker fails the gate.
marker_for() {
    case "$1" in
        gles_render_cpp)  echo "GLES_RENDER_CPP_FULL_API OK" ;;
        gles_render_rust) echo "GLES_RENDER_RUST_FULL_API OK" ;;
        gles_render_py)   echo "GLES_RENDER_PY_FULL_API OK" ;;
        scene_2dui)       echo "SCENE_2DUI OK" ;;
        scene_3dmodel)    echo "SCENE_3DMODEL OK" ;;
        scene_anim)       echo "SCENE_ANIM OK" ;;
        scene_codec)      echo "SCENE_CODEC OK" ;;
        scene_2dui_rust)    echo "SCENE_2DUI_RUST OK" ;;
        scene_3dmodel_rust) echo "SCENE_3DMODEL_RUST OK" ;;
        scene_anim_rust)    echo "SCENE_ANIM_RUST OK" ;;
        scene_codec_rust)   echo "SCENE_CODEC_RUST OK" ;;
        scene_2dui_py)      echo "SCENE_2DUI_PY OK" ;;
        scene_3dmodel_py)   echo "SCENE_3DMODEL_PY OK" ;;
        scene_anim_py)      echo "SCENE_ANIM_PY OK" ;;
        scene_codec_py)     echo "SCENE_CODEC_PY OK" ;;
        *)                echo "OK" ;;
    esac
}

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"; marker="$(marker_for "$name")"
    [ -x "$prog" ] || { echo "cpu-gles-render: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "${marker} [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -6
        echo "CARPET FAILED: $name (exit $rc, expected marker '${marker} <n>')"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
# Capability manifest: prebuild lists the render cells it provisioned on this arch. gles_render_cpp
# (surfaceless-EGL C++, every arch), the four real-scenario C++ cells (scene_2dui / scene_3dmodel /
# scene_anim / scene_codec, every arch), gles_render_rust and the four scene_*_rust cells (glow,
# dynamic musl, every arch) are the guaranteed native cells; gles_render_py and the four scene_*_py
# cells (PyOpenGL) append where provisioned. The scene cells thus cover all three bindings (cpp / rust /
# py). Each cell renders clear/solid/gradient/checker/viewport/scissor/depth/blend (and, for the scene
# cells, UI compositing / a projected 3D model / a keyframed animation / a streaming-codec block
# pipeline) into an FBO and checks pixels back against a closed-form reference, plus a negative control.
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-gles-render: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done < "$MANIFEST"

echo "cpu-gles-render: $pass/$total render carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 1 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-gles-render: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=1 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
