#!/bin/sh
# On-target runner: set up the OpenCL runtime environment and run all OpenCL carpets.
# Prints "TEST PASSED" when every carpet that is present reports its "<name> OK <n>" marker
# and exits 0. Carpets whose binary was not built on this arch are silently skipped.
set -u
BIN=/opt/gpu-opencl
export LD_LIBRARY_PATH=/usr/lib:/usr/lib/pocl
export POCL_DEVICES=basic
export RUSTICL_ENABLE=llvmpipe
export OCL_ICD_VENDORS=/etc/OpenCL/vendors
export LP_NUM_THREADS=1

pass=0; total=0; fail=0

run() {
    local name="$1"; prog="$2"
    [ -x "$prog" ] || return 0
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
# OpenCL C and C++ carpets. Binaries are present only when libOpenCL was available at build time:
# rusticl from Alpine edge on x64/aa, pocl from POCL_PREBUILT on arches without Alpine rusticl.
# On arches with no OpenCL runtime (la/rv in the Alpine edge package set as of 2026-07) both
# binaries are absent; total=0 and TEST PASSED is printed vacuously - documented in README.
run opencl_c   "$BIN/opencl_c"
run opencl_cpp "$BIN/opencl_cpp"

echo "gpu-opencl: $pass/$total carpets OK on $(uname -m)"
if [ "$fail" -eq 0 ]; then echo "TEST PASSED"; else echo "TEST FAILED"; fi
