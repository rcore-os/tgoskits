#!/bin/sh
# On-target runner: set up the OpenCL runtime environment and run the OpenCL carpets listed in the
# per-arch capability manifest (expected_cells, written by prebuild.sh). Prints "TEST PASSED" only
# when every manifest carpet runs, reports its "<name> OK <n>" marker and exits 0. The manifest lists
# exactly the cells the prebuild provisioned on this arch, and every cell build hard-fails in prebuild,
# so a listed cell is one that genuinely built - the manifest cannot silently under-count. A missing
# manifest carpet, a regression, or an empty/too-small manifest fails the gate: a vacuous total=0
# "TEST PASSED" is never emitted.
set -u
BIN=/opt/cpu-opencl-compute
export LD_LIBRARY_PATH=/usr/lib:/usr/lib/pocl
export POCL_DEVICES=basic
export RUSTICL_ENABLE=llvmpipe
export OCL_ICD_VENDORS=/etc/OpenCL/vendors
export LP_NUM_THREADS=1
export OPENBLAS_NUM_THREADS=1

pass=0; total=0; fail=0

run() {
    local name="$1"; prog="$2"
    [ -x "$prog" ] || { echo "cpu-opencl-compute: $name in manifest but binary absent at runtime"; total=$((total + 1)); fail=$((fail + 1)); return 0; }
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
# Capability manifest: prebuild lists the cells it provisioned on this arch - opencl_c/opencl_cpp
# wherever libOpenCL is present (rusticl on x64/aa, pocl on rv/la); opencl_rust (opencl3) where cargo
# cross-built it; opencl_py (PyOpenCL) where py3-opencl's native _cl extension provisioned. The C++/py
# binding availability legitimately varies by arch, so the expected count is data (the manifest), not
# a hard-coded constant - but every listed cell must run and pass.
MANIFEST="$BIN/expected_cells"
[ -f "$MANIFEST" ] || { echo "cpu-opencl-compute: expected_cells manifest missing - prebuild provisioned no carpet"; echo "TEST FAILED"; exit 1; }
EXPECTED=$(grep -c . "$MANIFEST")
while IFS= read -r cell; do
    [ -n "$cell" ] || continue
    run "$cell" "$BIN/$cell"
done < "$MANIFEST"

# Floor: opencl_c + opencl_cpp are the minimum any arch with an OpenCL runtime provisions, so
# EXPECTED < 2 means no runtime was provisioned (rv/la without pocl) - that is a FAIL, never a vacuous
# pass. Above the floor the gate is the canonical strict triple-check against the manifest count.
echo "cpu-opencl-compute: $pass/$total carpets OK on $(uname -m) (expected $EXPECTED: $(tr '\n' ' ' < "$MANIFEST"))"
if [ "$EXPECTED" -ge 2 ] && [ "$fail" -eq 0 ] && [ "$total" -eq "$EXPECTED" ] && [ "$pass" -eq "$EXPECTED" ]; then
    echo "TEST PASSED"; exit 0
else
    echo "cpu-opencl-compute: GATE FAILED - need all $EXPECTED manifest carpets to pass (>=2 floor); got fail=$fail total=$total pass=$pass; provision the OpenCL runtime (rusticl/pocl) + py3-opencl for this arch"
    echo "TEST FAILED"; exit 1
fi
