#!/bin/sh
# run-pysci.sh — on-target launcher for the python scientific-computing carpet.
#
# Sets the musl dynamic-loader search path (so the loader finds the OpenBLAS / Arrow C++ /
# OpenCV / Fortran .so injected under /lib + /usr/lib), pins BLAS/OpenMP to a single thread for
# determinism, then hands off to python3 run_pysci.py. The PASS/FAIL gate (the TEST PASSED /
# TEST FAILED anchor) lives entirely in run_pysci.py — this wrapper never prints it, so the
# success regex cannot self-match on the launch command.
#
# The musl loader reads only /etc/ld-musl-<this-arch>.path; writing all four names is harmless
# and keeps the launcher arch-agnostic.
for a in x86_64 aarch64 riscv64 loongarch64; do
    printf '/lib\n/usr/lib\n' > "/etc/ld-musl-$a.path"
done

export PATH=/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
export OPENBLAS_NUM_THREADS=1
export OMP_NUM_THREADS=1
export MKL_NUM_THREADS=1
export NUMEXPR_NUM_THREADS=1
export OPENCV_NUM_THREADS=1

cd /root/pysci || exit 1
exec python3 run_pysci.py
