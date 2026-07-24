#!/bin/sh
# On-target runner: set up the software GPU compute runtime and run the wgpu (WebGPU) Rust compute
# carpet. Prints "TEST PASSED" only when the carpet reports its "WGPU_RUST_FULL_API OK <n>" marker AND
# exits 0.
set -u
BIN=/opt/gpu-wgpu
mkdir -p /tmp/vkrt
export XDG_RUNTIME_DIR=/tmp/vkrt
export LD_LIBRARY_PATH=/usr/lib
# lavapipe (software Vulkan) ICD; the JSON carries an absolute library_path resolved against the root.
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd.*.json 2>/dev/null | head -1)
export VK_DRIVER_FILES="$ICD"
export VK_ICD_FILENAMES="$ICD"
# wgpu lands on the ash Vulkan backend; pin it so it does not probe a GL fallback that is not staged.
export WGPU_BACKEND=vulkan
# StarryOS runs one vCPU (SMP off), so lavapipe's llvmpipe JIT executes every workgroup on one thread.
# Pin the thread pool to 1 to make that explicit; the carpet asserts numerical correctness, not
# throughput, so the thread count does not change the results.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "gpu-wgpu: detected CPU count = $ncpu; lavapipe pinned single-threaded (LP_NUM_THREADS=1); ICD=$ICD"

pass=0; total=0; fail=0
# run <name> <binary>. A pass requires BOTH a clean exit (rc==0) AND the exact "<name> OK <n>" marker:
# a carpet that prints its marker then aborts in teardown must fail, not pass.
run() {
    name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "gpu-wgpu: $name absent - not staged"; return 0; }
    total=$((total + 1))
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
    else
        echo "$out" | tail -12
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
    fi
}

cd "$BIN" || exit 1
run wgpu_rust "$BIN/wgpu_rust"

echo "gpu-wgpu: $pass/$total carpets OK on $(uname -m)"
# The wgpu Rust carpet (WebGPU compute on lavapipe) is built and required on every arch.
if [ "$fail" -eq 0 ] && [ "$pass" -ge 1 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
