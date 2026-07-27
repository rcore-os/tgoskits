#!/bin/sh
# Bring-up probe: can StarryOS run the glibc Miniforge Python + numba @njit JIT?
# Prints CONDA_* markers so the qemu log shows exactly how far the glibc conda stack gets.
CP=/opt/miniconda/bin/python
if [ ! -x "$CP" ]; then
    echo "CONDA_SMOKE: /opt/miniconda absent (musl-only build)"
    exit 0
fi
export LD_LIBRARY_PATH="/opt/miniconda/lib:${LD_LIBRARY_PATH:-}"
export HOME=/root
export OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1 NUMBA_NUM_THREADS=1 MPLBACKEND=Agg
echo "CONDA_SMOKE_BEGIN"
"$CP" - <<'PY'
import sys
print("CONDA_PY", sys.version.split()[0])
try:
    import numpy as np
    print("CONDA_NUMPY", np.__version__)
    a = np.arange(1, 6, dtype=np.float64)
    print("CONDA_NUMPY_DOT", float(a @ a))   # 55.0
except Exception as e:
    print("CONDA_NUMPY_FAIL", type(e).__name__, e); sys.exit(0)
try:
    import scipy, scipy.linalg as la
    print("CONDA_SCIPY", scipy.__version__)
    print("CONDA_SCIPY_DET", round(float(la.det(np.array([[1.,2.],[3.,4.]]))), 6))  # -2.0
except Exception as e:
    print("CONDA_SCIPY_FAIL", type(e).__name__, e)
try:
    import numba
    from numba import njit
    print("CONDA_NUMBA", numba.__version__)
    @njit
    def sq(x):
        s = 0.0
        for i in range(x.shape[0]):
            s += x[i] * x[i]
        return s
    r = sq(np.arange(1, 6, dtype=np.float64))
    print("CONDA_NJIT", r)                     # 55.0 -> JIT MCJIT worked
    assert r == 55.0
    print("CONDA_SMOKE_OK")
except Exception as e:
    print("CONDA_NUMBA_FAIL", type(e).__name__, e)
PY
echo "CONDA_SMOKE_END"
