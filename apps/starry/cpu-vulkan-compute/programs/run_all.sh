#!/bin/sh
# On-target runner: set up the software Vulkan runtime and run the native Vulkan compute carpets.
# Prints "TEST PASSED" only when every built carpet reports its "<name> OK <n>" marker.
set -u
BIN=/opt/cpu-vulkan-compute
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
echo "cpu-vulkan-compute: detected CPU count = $ncpu; lavapipe pinned single-threaded (LP_NUM_THREADS=1); ICD=$ICD"

pass=0; total=0; fail=0
# run <name> <binary> - a carpet whose binary is absent (did not build on this arch) is skipped.
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-vulkan-compute: $name absent (not built this arch) - skipped"; return 0; }
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
# dlopen diagnostic (not a carpet, not gated): confirms the runtime dlopen path the Python (pyVulkan/
# cffi) binding needs works on this dynamic binary. Static musl binaries stub dlopen; a dynamic one
# routes through the real ld-musl.
if [ -x "$BIN/dlopen_probe" ]; then "$BIN/dlopen_probe" 2>&1 || echo "cpu-vulkan-compute: dlopen_probe reported failure"; fi
# The native C and C++ Vulkan compute carpets over lavapipe (instance / physical-device / device /
# queue / buffer / memory / shader-module / descriptor / pipeline / command-buffer / fence /
# semaphore / event / query-pool / dispatch / indirect-dispatch / push-constant / transfer plus the
# core-1.1 *2 queries). Each dispatches real GLSL compute shaders (vadd / saxpy / element-multiply)
# and checks every result element against a closed-form reference.
run vulkan_c    "$BIN/vulkan_c"
run vulkan_cpp  "$BIN/vulkan_cpp"
# The Rust (ash) carpet, cross-compiled to a dynamically linked musl binary and injected like the C/C++ cells.
run vulkan_rust "$BIN/vulkan_rust"
# The Python (pyVulkan / cffi) carpet - a shell wrapper that execs python3 on the vendored cell.
run vulkan_py   "$BIN/vulkan_py"
# The Kompute (libkompute C++) carpet, scoped to x86_64 / aarch64. It is a dynamically linked musl
# binary that drives libkompute over Vulkan-Hpp dynamic dispatch (dlopen libvulkan). On arches where
# prebuild does not inject it (riscv64 / loongarch64) the binary is absent and run's absence-skip
# handles it - the EXPECTED count below is set per arch by prebuild so an absent binary there is not
# counted against the gate, while on x86_64 / aarch64 it must run and PASS.
run kompute_cpp "$BIN/kompute_cpp"

# EXPECTED is the number of language-binding carpets prebuild.sh injected for THIS arch, written to
# expected_cells (4 Vulkan cells everywhere + kompute_cpp on x86_64 / aarch64). Every injected binary
# must run and PASS - an absent expected binary is a prebuild failure (compile_*/provision_* hard-fail),
# never a silent skip, so total must equal EXPECTED and pass must equal EXPECTED with zero failures.
EXPECTED=$(cat "$BIN/expected_cells" 2>/dev/null || echo 4)
echo "cpu-vulkan-compute: $pass/$total carpets OK on $(uname -m) (expected $EXPECTED)"
if [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "TEST FAILED"; exit 1
fi
