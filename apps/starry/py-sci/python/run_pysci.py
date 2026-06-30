#!/usr/bin/env python3
# run_pysci.py — on-target gate for the StarryOS python scientific-computing carpet.
#
# Runs each library carpet (NumPy / OpenCV / pyarrow / SciPy / SymPy) as a subprocess; each
# carpet self-checks (XXX_RESULT ok=N fail=0 then XXX_DONE only when its fail count is zero).
# A carpet is OK only when its DONE marker is present, it exited 0, and no FAIL line appears.
# Prints PY_SCI_OK=<P>/<T> and TEST PASSED only when ALL five carpets pass, else TEST FAILED.
#
# numba is a documented deferred wall: Alpine ships no py3-numba musl apk and llvmlite / the
# LLVM JIT it needs has no musl distribution. It is reported as an informational SKIP and is
# NOT counted toward PASS / TOTAL.
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
os.chdir(HERE)  # so each carpet's imports resolve from the staged carpet dir

# Deterministic, single-threaded BLAS / OpenMP: reproducible results and we do not stress the
# kernel's threading before correctness is established. Inherited by the carpet subprocesses.
for _k in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS",
           "NUMEXPR_NUM_THREADS", "OPENCV_NUM_THREADS"):
    os.environ[_k] = "1"
os.environ.setdefault("PYTHONDONTWRITEBYTECODE", "1")
os.environ.setdefault("PYTHONUNBUFFERED", "1")

PY = sys.executable
MODULES = [
    ("numpy", "NumpyCarpet.py", "NUMPY_DONE"),
    ("opencv", "OpencvCarpet.py", "OPENCV_DONE"),
    ("pyarrow", "PyarrowCarpet.py", "PYARROW_DONE"),
    ("scipy", "ScipyCarpet.py", "SCIPY_DONE"),
    ("sympy", "SympyCarpet.py", "SYMPY_DONE"),
]

print("=== python3 %s ===" % sys.version.split()[0])
print("=== py-sci: scientific-computing carpets (numpy | opencv | pyarrow | scipy | sympy) ===")
print("  SKIP numba (no musl py3-numba apk / no musl LLVM JIT for llvmlite; documented defer)")

passed = 0
for name, fn, marker in MODULES:
    r = subprocess.run([PY, os.path.join(HERE, fn)], capture_output=True, text=True)
    out = (r.stdout or "") + (r.stderr or "")
    res = [ln for ln in out.splitlines() if "_RESULT ok=" in ln]
    if marker in out and r.returncode == 0 and "  FAIL " not in out:
        print("  OK   %s (%s)" % (name, res[0].strip() if res else ""))
        passed += 1
    else:
        print("  FAIL %s (%s) rc=%s" % (name, marker, r.returncode))
        bad = [ln for ln in out.splitlines()
               if any(t in ln for t in ("FAIL", "Error", "Traceback", "Exception"))]
        print("\n".join(bad[-12:]))

total = len(MODULES)
print("PY_SCI_RESULT PASS=%d TOTAL=%d" % (passed, total))
print("PY_SCI_OK=%d/%d" % (passed, total))
if passed == total and total > 0:
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
