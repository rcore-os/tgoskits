#!/usr/bin/env python3
# ScipyCarpet.py — exact/closed-form-assertion carpet for SciPy on musl-native CPython.
#
# Floating-point results are checked within a tolerance against closed-form analytic values
# (det, Cholesky/LU reconstruction, convex-quadratic argmin, polynomial roots, Gaussian
# cdf/pdf, perfect correlation); integer/structural results are checked exactly. Nothing
# depends on print formatting. Self-contained ok/fail counters; prints SCIPY_RESULT then
# SCIPY_DONE only when fail == 0.
import math
import sys

ok = 0
fail = 0


def chk(name, cond, info=""):
    global ok, fail
    if cond:
        ok += 1
        print("  ok %s%s" % (name, (" " + info) if info else ""))
    else:
        fail += 1
        print("  FAIL %s%s" % (name, (" " + info) if info else ""))


import numpy as np
import scipy

chk("version", int(scipy.__version__.split(".")[0]) >= 1, "scipy=%s" % scipy.__version__)

# ---- scipy.linalg: LU / Cholesky / solve / det (closed-form reconstructions) ----
from scipy import linalg

A = np.array([[4.0, 3.0], [6.0, 3.0]])
P, L, U = linalg.lu(A)
chk("lu_reconstruct", np.allclose(P @ L @ U, A))
chk("lu_L_unit_lower", np.allclose(np.tril(L), L) and np.allclose(np.diag(L), [1.0, 1.0]))
chk("lu_U_upper", np.allclose(np.triu(U), U))

S = np.array([[4.0, 2.0], [2.0, 3.0]])  # symmetric positive-definite
Lc = linalg.cholesky(S, lower=True)
chk("cholesky_reconstruct", np.allclose(Lc @ Lc.T, S))
chk("cholesky_lower", np.allclose(np.tril(Lc), Lc))

x = linalg.solve(np.array([[3.0, 0.0], [0.0, 5.0]]), np.array([9.0, 20.0]))
chk("solve", np.allclose(x, [3.0, 4.0]))
chk("det", abs(linalg.det(np.array([[1.0, 2.0], [3.0, 4.0]])) - (-2.0)) < 1e-9)
chk("inv", np.allclose(linalg.inv(np.array([[2.0, 0.0], [0.0, 4.0]])), [[0.5, 0.0], [0.0, 0.25]]))

# ---- scipy.sparse: build a CSR matrix and check exact matrix-vector product ----
from scipy.sparse import csr_matrix

data = np.array([1.0, 2.0, 3.0])
rows = np.array([0, 1, 2])
cols = np.array([0, 1, 2])
sp = csr_matrix((data, (rows, cols)), shape=(3, 3))  # diag(1,2,3)
chk("sparse_nnz", int(sp.nnz) == 3)
chk("sparse_matvec", sp.dot(np.array([1.0, 1.0, 1.0])).tolist() == [1.0, 2.0, 3.0])
dense = sp.toarray()
chk("sparse_toarray", dense.tolist() == [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]])
# CSR @ CSR product is still diagonal: diag(1,2,3)^2 = diag(1,4,9).
chk("sparse_matmul", (sp @ sp).toarray().tolist() == [[1.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 9.0]])

# ---- scipy.optimize: minimize a convex quadratic + bracketed polynomial root ----
from scipy import optimize


def quad(p):
    return (p[0] - 3.0) ** 2 + (p[1] + 1.0) ** 2  # unique min at (3, -1)


res = optimize.minimize(quad, np.array([0.0, 0.0]), method="BFGS")
chk("minimize_argmin", np.allclose(res.x, [3.0, -1.0], atol=1e-4), "x=%s" % res.x.tolist())
chk("minimize_fmin", abs(float(res.fun)) < 1e-8)
root = optimize.brentq(lambda t: t * t - 2.0, 0.0, 2.0)  # sqrt(2)
chk("brentq_sqrt2", abs(root - math.sqrt(2.0)) < 1e-10, "root=%.12f" % root)
root2 = optimize.brentq(lambda t: t ** 3 - t - 2.0, 1.0, 2.0)
chk("brentq_cubic", abs(root2 ** 3 - root2 - 2.0) < 1e-10)

# ---- scipy.signal: discrete convolution of known integer sequences (exact) ----
from scipy import signal

chk("convolve_full", signal.convolve(np.array([1, 2, 3]), np.array([1, 1])).tolist() == [1, 3, 5, 3])
chk("convolve_kernel",
    signal.convolve(np.array([0, 1, 0, 0]), np.array([1, 2, 3])).tolist() == [0, 1, 2, 3, 0, 0])
chk("correlate",
    signal.correlate(np.array([1, 2, 3]), np.array([1, 1]), mode="valid").tolist() == [3, 5])

# ---- scipy.stats: Gaussian cdf/pdf (closed-form) + perfect-correlation pearsonr ----
from scipy import stats

chk("norm_cdf_0", abs(stats.norm.cdf(0.0) - 0.5) < 1e-12)
chk("norm_pdf_0", abs(stats.norm.pdf(0.0) - (1.0 / math.sqrt(2.0 * math.pi))) < 1e-12)
chk("norm_cdf_196", abs(stats.norm.cdf(1.959963984540054) - 0.975) < 1e-9)
chk("norm_symmetry", abs((stats.norm.cdf(1.0) + stats.norm.cdf(-1.0)) - 1.0) < 1e-12)
r = stats.pearsonr(np.array([1.0, 2.0, 3.0, 4.0, 5.0]), np.array([2.0, 4.0, 6.0, 8.0, 10.0]))[0]
chk("pearsonr_perfect", abs(r - 1.0) < 1e-12, "r=%.12f" % r)
ra = stats.pearsonr(np.array([1.0, 2.0, 3.0]), np.array([3.0, 2.0, 1.0]))[0]
chk("pearsonr_anti", abs(ra - (-1.0)) < 1e-12)

print("SCIPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SCIPY_DONE")
    sys.exit(0)
sys.exit(1)
