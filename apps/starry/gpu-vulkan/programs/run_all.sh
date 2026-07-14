#!/bin/sh
# On-target runner: set up the software Vulkan runtime and run the native Vulkan compute carpets.
# Prints "TEST PASSED" only when every built carpet reports its "<name> OK <n>" marker.
set -u
BIN=/opt/gpu-vulkan
mkdir -p /tmp/vkrt
export XDG_RUNTIME_DIR=/tmp/vkrt
export LD_LIBRARY_PATH=/usr/lib
# the lavapipe ICD JSON carries an absolute library_path that resolves against the rootfs root
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd.*.json 2>/dev/null | head -1)
export VK_DRIVER_FILES="$ICD"
export VK_ICD_FILENAMES="$ICD"
# StarryOS runs one vCPU (SMP off by default), so lavapipe's llvmpipe JIT executes every workgroup on
# one thread. Pin the mesa thread pool to 1 to make that explicit. The carpets assert numerical
# correctness against numpy/closed-form references, not throughput, so thread count does not affect
# results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "gpu-vulkan: detected CPU count = $ncpu; lavapipe pinned single-threaded (LP_NUM_THREADS=1); ICD=$ICD"

pass=0; total=0; fail=0
# run <name> <binary> - a carpet whose binary is absent (did not build on this arch) is skipped.
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "gpu-vulkan: $name absent (not built this arch) - skipped"; return 0; }
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
# The native C and C++ Vulkan compute carpets over lavapipe (instance / physical-device / device /
# queue / buffer / memory / shader-module / descriptor / pipeline / command-buffer / fence /
# semaphore / event / query-pool / dispatch / indirect-dispatch / push-constant / transfer plus the
# core-1.1 *2 queries). Each dispatches real GLSL compute shaders (vadd / saxpy / element-multiply)
# and checks every result element against a closed-form reference.
run vulkan_c   "$BIN/vulkan_c"
run vulkan_cpp "$BIN/vulkan_cpp"

echo "gpu-vulkan: $pass/$total carpets OK on $(uname -m)"
if [ "$fail" -eq 0 ] && [ "$pass" -ge 2 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
