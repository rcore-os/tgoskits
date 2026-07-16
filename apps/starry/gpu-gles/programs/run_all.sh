#!/bin/sh
# On-target runner: set up the software GLES runtime (Mesa llvmpipe over EGL-surfaceless) and run the
# native GLES 3.1 compute carpets. Prints "TEST PASSED" only when every built carpet reports its
# "<name> OK <n>" marker.
set -u
BIN=/opt/gpu-gles
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
echo "gpu-gles: detected CPU count = $ncpu; llvmpipe pinned single-threaded (LP_NUM_THREADS=1); EGL_PLATFORM=surfaceless"

pass=0; total=0; fail=0
# run <name> <binary> - a carpet whose binary is absent (did not build on this arch) is skipped.
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "gpu-gles: $name absent (not built this arch) - skipped"; return 0; }
    total=$((total + 1))
    # Capture the carpet's exit status *immediately*: a teardown crash, an
    # assert-then-exit, or a signal (rc > 128) after the "OK <n>" marker was
    # printed must count as a failure. Gating on the marker alone would report
    # such a run as passed. Require both a zero exit and the marker.
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -6
        if [ "$rc" -ne 0 ]; then
            echo "CARPET FAILED: $name (exit status $rc)"
        else
            echo "CARPET FAILED: $name (missing OK marker)"
        fi
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
# The native C and C++ GLES 3.1 compute carpets over llvmpipe (EGL surfaceless display / config /
# context / make-current, compute shader compile+link, SSBO + buffer-base binding, uniform, dispatch,
# indirect dispatch, memory barrier, map-range readback, mapped writes, fence sync, query objects,
# image load/store, limits and resource/uniform introspection). Each dispatches real GLSL ES compute
# shaders (vadd / saxpy / element-multiply / 2D index math) and checks every result element against a
# closed-form reference, plus boundary and error-enum paths.
run gles_c   "$BIN/gles_c"
run gles_cpp "$BIN/gles_cpp"

echo "gpu-gles: $pass/$total carpets OK on $(uname -m)"
if [ "$fail" -eq 0 ] && [ "$pass" -ge 2 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
