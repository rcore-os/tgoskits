#!/bin/sh
# On-target runner: set up the software WebGPU render runtime and run the wgpu render carpet cells
# under BOTH software backends - wgpu-on-Vulkan (Mesa lavapipe) and wgpu-on-GL (Mesa llvmpipe). Each
# cell renders offscreen into an RGBA8 texture, copies it to a MAP_READ buffer and checks pixels back
# against a closed-form reference. Prints "TEST PASSED" only when every (cell x backend) run reports
# its "<name> OK <n>" marker and exits 0.
set -u
BIN=/opt/cpu-wgpu-render
mkdir -p /tmp/vkrt
export XDG_RUNTIME_DIR=/tmp/vkrt
export LD_LIBRARY_PATH=/usr/lib
# lavapipe (software Vulkan) ICD for the wgpu Vulkan backend.
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
export VK_DRIVER_FILES="$ICD"
export VK_ICD_FILENAMES="$ICD"
# llvmpipe (software GL) via surfaceless EGL for the wgpu GL backend.
export EGL_PLATFORM=surfaceless
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
# the C/C++ (libwgpu_native.so) and Python (wgpu-py) cells dlopen the native lib; point wgpu-py at it.
export WGPU_LIB_PATH=/usr/lib/libwgpu_native.so
# StarryOS runs one vCPU; pin the LLVM JIT thread pool to 1. The carpets assert pixel correctness, not
# throughput, so thread count does not affect results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-wgpu-render: detected CPU count = $ncpu; lavapipe/llvmpipe pinned single-threaded; ICD=$ICD"

pass=0; total=0; fail=0
# run <name> <binary> <backend>. A pass requires BOTH a clean exit (rc==0) AND the exact "<name> OK <n>"
# marker: a cell that prints its marker then aborts in teardown must fail, not pass.
run() {
    name="$1"; prog="$2"; backend="$3"
    [ -x "$prog" ] || { echo "cpu-wgpu-render: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && WGPU_BACKEND="$backend" "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1 | sed "s/^/[$backend] /"
        pass=$((pass + 1))
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name [$backend] (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
# Capability manifest: prebuild lists the cells it provisioned on this arch. wgpu_render_rust (wgpu
# crate, on-target every arch) and wgpu_render_c / wgpu_render_cpp (linked against the libwgpu_native.so
# the prebuild builds from source for musl) are the native cells; wgpu_render_py (wgpu-py + WGPU_LIB_PATH)
# appends where provisioned. The four render-scenario scene cells (scene_2dui / scene_3dmodel /
# scene_anim / scene_codec) are wgpu-crate cargo crates cross-compiled by prebuild the same way as
# wgpu_render_rust; each prints its own "SCENE_<NAME> OK <n>" marker. Each build hard-fails in prebuild,
# so a listed cell genuinely built. Every cell is run under both software backends (vulkan=lavapipe,
# gl=llvmpipe); both must pass. The run() gate below accepts any "<name> OK <n>" marker (three-gate:
# rc==0 AND marker present AND counted into total/pass), so no per-cell change is needed.
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-wgpu-render: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
# EXPECTED_CELLS is the hard-coded full cell set (4 render + 16 scene bindings); a manifest with fewer than
# these 20 means prebuild dropped a cell, so the gate fails rather than shrinking EXPECTED to a partial run.
EXPECTED_CELLS=20
NCELL=$(grep -c . "$MANIFEST")
EXPECTED=$((EXPECTED_CELLS * 2))
for backend in vulkan gl; do
    while IFS= read -r cell; do
        [ -n "$cell" ] || continue
        run "$cell" "$BIN/$cell" "$backend"
    done < "$MANIFEST"
done

echo "cpu-wgpu-render: $pass/$total (cell x backend) runs OK on $(uname -m) (cells $NCELL x 2 backends = $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$NCELL" -eq "$EXPECTED_CELLS" ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-wgpu-render: GATE FAILED - need all $EXPECTED_CELLS cells x 2 backends = $EXPECTED runs to pass; got cells=$NCELL fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
