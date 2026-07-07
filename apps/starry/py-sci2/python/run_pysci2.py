#!/usr/bin/env python3
# run_pysci2.py - on-target gate for the StarryOS python scientific-computing + ML carpet, batch 2.
#
# Two independently-provisioned stacks, both gated:
#   - musl (Alpine py3-scipy / py3-sympy): scipy + sympy on all four target arches.
#   - glibc conda (Miniforge + conda-forge, x86_64 / aarch64 - the arches conda ships): numba
#     (@njit MCJIT, which breaks the musl numba wall), pandas, scikit-learn, matplotlib, networkx,
#     statsmodels. StarryOS runs the glibc Miniforge Python via its staged libc6 closure.
# Every carpet self-checks (XXX_RESULT ok=N fail=0 then XXX_DONE only when fail == 0). The gate
# prints PYSCI2_OK=<P>/<T> and TEST PASSED only when every present carpet passes; the conda stack is
# absent (and its carpets are not counted) on the two arches conda has no distribution for.
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
os.chdir(HERE)

# Deterministic, single-threaded BLAS / OpenMP / numba, headless Agg. Inherited by subprocesses.
for _k in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS",
           "NUMEXPR_NUM_THREADS", "NUMBA_NUM_THREADS"):
    os.environ[_k] = "1"
os.environ["MPLBACKEND"] = "Agg"
os.environ.setdefault("PYTHONDONTWRITEBYTECODE", "1")
os.environ.setdefault("PYTHONUNBUFFERED", "1")

MUSL_PY = sys.executable
CONDA_PY = "/opt/miniconda/bin/python"

MUSL = [
    ("scipy", "ScipyCarpet.py", "SCIPY_DONE"),
    ("sympy", "SympyCarpet.py", "SYMPY_DONE"),
]
CONDA = [
    ("numba", "NumbaCarpet.py", "NUMBA_DONE"),
    ("pandas", "PandasCarpet.py", "PANDAS_DONE"),
    ("scikit-learn", "SklearnCarpet.py", "SKLEARN_DONE"),
    ("matplotlib", "MatplotlibCarpet.py", "MATPLOTLIB_DONE"),
    ("networkx", "NetworkxCarpet.py", "NETWORKX_DONE"),
    ("statsmodels", "StatsmodelsCarpet.py", "STATSMODELS_DONE"),
    ("conda-cli", "CondaCliCarpet.py", "CONDACLI_DONE"),
]


def run(py, fn):
    env = dict(os.environ)
    if py == CONDA_PY:
        env["LD_LIBRARY_PATH"] = "/opt/miniconda/lib:" + env.get("LD_LIBRARY_PATH", "")
    r = subprocess.run([py, os.path.join(HERE, fn)], capture_output=True, text=True, env=env)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def gate(py, carpets):
    n = 0
    for name, fn, marker in carpets:
        rc, out = run(py, fn)
        res = [ln for ln in out.splitlines() if "_RESULT ok=" in ln]
        if marker in out and rc == 0 and "  FAIL " not in out:
            print("  OK   %s (%s)" % (name, res[0].strip() if res else ""))
            n += 1
        else:
            print("  FAIL %s (%s) rc=%s" % (name, marker, rc))
            print("\n".join(out.splitlines()[-25:]))
    return n


print("=== py-sci2: scientific-computing + ML carpets ===")
print("=== musl python %s ===" % sys.version.split()[0])
passed = gate(MUSL_PY, MUSL)
total = len(MUSL)

if os.path.exists(CONDA_PY):
    print("=== glibc conda stack: numba(MCJIT) pandas scikit-learn matplotlib networkx statsmodels ===")
    passed += gate(CONDA_PY, CONDA)
    total += len(CONDA)
else:
    print("  NOTE conda stack absent (conda ships x86_64/aarch64 only; scipy/sympy cover this arch)")

print("PYSCI2_RESULT PASS=%d TOTAL=%d" % (passed, total))
print("PYSCI2_OK=%d/%d" % (passed, total))
if passed == total and total > 0:
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
