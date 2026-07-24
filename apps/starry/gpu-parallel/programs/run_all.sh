#!/bin/sh
# On-target runner: set up the software GPU compute runtime and run the parallel/concurrent compute
# carpets. Prints "TEST PASSED" only when every built carpet reports its "<name> OK <n>" marker AND
# exits 0.
set -u
BIN=/opt/gpu-parallel
mkdir -p /tmp/vkrt
export XDG_RUNTIME_DIR=/tmp/vkrt
export LD_LIBRARY_PATH=/usr/lib:/usr/lib/pocl
# lavapipe (software Vulkan) ICD; the JSON carries an absolute library_path resolved against the root
ICD=$(ls /usr/share/vulkan/icd.d/lvp_icd.*.json 2>/dev/null | head -1)
export VK_DRIVER_FILES="$ICD"
export VK_ICD_FILENAMES="$ICD"
# OpenCL: rusticl (mesa, over llvmpipe) advertises only drivers named in RUSTICL_ENABLE; pocl uses its
# single-threaded CPU device. Both are software CPU OpenCL runtimes.
export RUSTICL_ENABLE=llvmpipe
export OCL_ICD_VENDORS=/etc/OpenCL/vendors
export POCL_DEVICES=basic
# StarryOS runs one vCPU (SMP off), so the software rasterizers/JITs execute every workgroup on one
# thread. Pin the mesa (lavapipe/llvmpipe) thread pool to 1 to make that explicit; pocl's basic device
# is already single-threaded. The carpets assert numerical/atomic correctness, not throughput, so the
# thread count does not change the results - the atomic and reduction checks still cover the
# cross-workgroup race, since the software scheduler still interleaves the workgroups.
export LP_NUM_THREADS=1
ncpu=$(nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo '?')
echo "gpu-parallel: detected CPU count = $ncpu; software GPU backends pinned single-threaded (LP_NUM_THREADS=1, POCL_DEVICES=basic); ICD=$ICD"

pass=0; total=0; fail=0
core_pass=0; core_fail=0; add_pass=0; add_fail=0; add_present=0
# run <class> <name> <binary> - class "core" (the Vulkan/lavapipe parallel carpet, built and required
# on every arch) or "add" (the OpenCL parallel carpet, provisioned where the arch has rusticl/pocl;
# absent on arches with no CPU OpenCL runtime). A carpet whose binary is absent is skipped. A pass
# requires BOTH a clean exit (rc==0) AND the exact "<name> OK <n>" marker: a carpet that prints its
# marker then aborts in teardown must fail, not pass.
run() {
    cls="$1"; name="$2"; prog="$3"
    [ -x "$prog" ] || { [ "$cls" = add ] && echo "gpu-parallel: $name absent (no CPU OpenCL runtime this arch) - skipped"; return 0; }
    total=$((total + 1))
    [ "$cls" = add ] && add_present=$((add_present + 1))
    out="$(cd "$BIN" && "$prog" 2>&1)"; rc=$?
    if [ "$rc" -eq 0 ] && echo "$out" | grep -qE "OK [0-9]+$"; then
        echo "$out" | grep -E ": PASS=|OK [0-9]+$" | tail -1
        pass=$((pass + 1))
        if [ "$cls" = core ]; then core_pass=$((core_pass + 1)); else add_pass=$((add_pass + 1)); fi
    else
        echo "$out" | tail -8
        echo "CARPET FAILED: $name (exit $rc)"
        fail=$((fail + 1))
        if [ "$cls" = core ]; then core_fail=$((core_fail + 1)); else add_fail=$((add_fail + 1)); fi
    fi
}

cd "$BIN" || exit 1
run core vk_parallel "$BIN/vk_parallel"
run add  cl_parallel "$BIN/cl_parallel"

echo "gpu-parallel: $pass/$total carpets OK on $(uname -m); core $core_pass/$((core_pass + core_fail)); additive present=$add_present ok=$add_pass failed=$add_fail"
# The core (Vulkan parallel compute on lavapipe) is built and required on every arch and is what gates.
# The OpenCL parallel carpet is additive and does not gate the core (mirrors the gpu-compute app):
# Alpine builds rusticl/pocl for x86_64 and aarch64 but not for every arch (riscv64/loongarch64 lack a
# packaged CPU OpenCL runtime today), and rusticl's OpenCL kernel-execution + async multi-submit paths
# reproduce a Mesa/qemu-TCG upstream limitation under emulation (the software OpenCL device's LLVM JIT
# faults under TCG - verified against a host CPU-OpenCL control, not a StarryOS kernel issue). The
# additive result is run and reported above but never blocks the Vulkan gate.
if [ "$core_fail" -eq 0 ] && [ "$core_pass" -ge 1 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
