#!/bin/sh
# On-target runner: set up the software GLES runtime (Mesa llvmpipe over EGL-surfaceless) and run the
# native GLES 3.1 compute carpets. Prints "TEST PASSED" only when every built carpet reports its
# "<name> OK <n>" marker.
set -u
BIN=/opt/cpu-gles-compute
export LD_LIBRARY_PATH=/usr/lib
# EGL surfaceless platform: create a headless context with no window-system surface
# (EGL_MESA_platform_surfaceless / EGL_PLATFORM=surfaceless).
export EGL_PLATFORM=surfaceless
# select the llvmpipe CPU rasterizer/JIT explicitly (no host GPU present).
export GALLIUM_DRIVER=llvmpipe
export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
# StarryOS runs one vCPU (SMP off by default), so llvmpipe's LLVM JIT executes every workgroup on one
# thread. Pin the mesa thread pool to 1 to make that explicit. The carpets assert numerical
# correctness against closed-form references, not throughput, so thread count does not affect results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "cpu-gles-compute: detected CPU count = $ncpu; llvmpipe pinned single-threaded (LP_NUM_THREADS=1); EGL_PLATFORM=surfaceless"

pass=0; total=0; fail=0
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-gles-compute: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
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
# Capability manifest: prebuild lists the cells it provisioned on this arch - gles_c/gles_cpp (native,
# every arch) and gles_rust (glow, dynamic musl, every arch); gles_py (moderngl) appends once its
# runtime provisions. Each cell exercises GLES 3.1 compute over EGL-surfaceless llvmpipe: context /
# compute shader compile+link / SSBO+buffer-base / uniform / dispatch / indirect dispatch / memory
# barrier / map-range readback / fence sync / query objects / image load-store / introspection, with
# every result element checked against a closed-form reference plus boundary and error-enum paths.
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-gles-compute: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done < "$MANIFEST"

# Floor: gles_c + gles_cpp are the minimum every arch provisions (mesa GLES is 4-arch), so EXPECTED<2
# means a broken provision - a FAIL, never a vacuous pass. Above the floor the gate is the canonical
# strict triple-check against the manifest count.
echo "cpu-gles-compute: $pass/$total carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 2 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-gles-compute: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=2 floor); got fail=$fail total=$total pass=$pass"
    echo "TEST FAILED"; exit 1
fi
