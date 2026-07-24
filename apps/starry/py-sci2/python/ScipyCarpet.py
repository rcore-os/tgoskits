#!/usr/bin/env python3
# ScipyCarpet.py - deep closed-form-assertion carpet for SciPy on musl-native CPython.
#
# Extends the surface of the py-sci scipy carpet to the full submodule set: linalg (LU /
# Cholesky / SVD / QR / eig / expm / pinv / lstsq / norm), optimize (minimize / brentq /
# newton / fsolve / least_squares / curve_fit / linprog), integrate (quad / dblquad / simpson /
# trapezoid / cumulative_trapezoid / solve_ivp), interpolate (interp1d / CubicSpline / splrep+
# splev / PchipInterpolator / barycentric), fft (fft / ifft / rfft / fftfreq / dct + Parseval),
# signal (convolve / correlate / fftconvolve), sparse (csr / csc / coo / eye / diags / kron /
# spsolve), stats (norm / binom / poisson / linregress / spearmanr / ttest) and special
# (gamma / gammaln / erf / comb / factorial / beta / expit).
#
# Floating results are compared to closed-form analytic values within a tolerance; integer and
# structural results are compared exactly. No assertion depends on print formatting, default
# dtype width or float repr, so the host reference and a newer target build agree byte-for-byte.
# Self-contained ok/fail counters; prints SCIPY_RESULT then SCIPY_DONE only when fail == 0.
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

# ---------------------------------------------------------------- scipy.linalg
from scipy import linalg

A = np.array([[4.0, 3.0], [6.0, 3.0]])
P, L, U = linalg.lu(A)
chk("lu_reconstruct", np.allclose(P @ L @ U, A))
chk("lu_L_unit_lower", np.allclose(np.tril(L), L) and np.allclose(np.diag(L), [1.0, 1.0]))

S = np.array([[4.0, 2.0], [2.0, 3.0]])  # symmetric positive-definite
Lc = linalg.cholesky(S, lower=True)
chk("cholesky_reconstruct", np.allclose(Lc @ Lc.T, S) and np.allclose(np.tril(Lc), Lc))

chk("solve", np.allclose(linalg.solve(np.array([[3.0, 0.0], [0.0, 5.0]]), np.array([9.0, 20.0])),
                         [3.0, 4.0]))
chk("det", abs(linalg.det(np.array([[1.0, 2.0], [3.0, 4.0]])) - (-2.0)) < 1e-9)
chk("inv", np.allclose(linalg.inv(np.array([[2.0, 0.0], [0.0, 4.0]])), [[0.5, 0.0], [0.0, 0.25]]))

# SVD: A = U diag(s) Vt; the symmetric B has eigenvalues 4 and 2, so its singular values are 4, 2.
B = np.array([[3.0, 1.0], [1.0, 3.0]])
Us, s, Vt = linalg.svd(B)
chk("svd_reconstruct", np.allclose(Us @ np.diag(s) @ Vt, B))
chk("svd_singvals", np.allclose(sorted(s, reverse=True), [4.0, 2.0]))

# QR: A = Q R, Q orthonormal, R upper-triangular.
Q, R = linalg.qr(A)
chk("qr_reconstruct", np.allclose(Q @ R, A))
chk("qr_orthonormal", np.allclose(Q.T @ Q, np.eye(2)))
chk("qr_upper", np.allclose(np.triu(R), R))

# Eigenvalues of a symmetric matrix (ascending, real).
chk("eigvalsh", np.allclose(linalg.eigvalsh(np.array([[2.0, 0.0], [0.0, 3.0]])), [2.0, 3.0]))

# Matrix exponential: expm(0) = I, expm(diag) = diag(exp).
chk("expm_zero", np.allclose(linalg.expm(np.zeros((2, 2))), np.eye(2)))
chk("expm_diag", np.allclose(linalg.expm(np.diag([0.0, 1.0])), np.diag([1.0, math.e])))

# Pseudo-inverse of an invertible matrix equals its inverse; least squares of an exact fit.
chk("pinv", np.allclose(linalg.pinv(np.array([[2.0, 0.0], [0.0, 4.0]])), [[0.5, 0.0], [0.0, 0.25]]))
Aov = np.array([[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]])
xls = linalg.lstsq(Aov, np.array([1.0, 2.0, 3.0]))[0]
chk("lstsq", np.allclose(xls, [1.0, 2.0]), "x=%s" % xls.tolist())
chk("norm_fro", abs(linalg.norm(np.array([[3.0, 4.0]])) - 5.0) < 1e-12)

# ---------------------------------------------------------------- scipy.optimize
from scipy import optimize


def paraboloid(p):
    return (p[0] - 3.0) ** 2 + (p[1] + 1.0) ** 2  # unique min at (3, -1)


res = optimize.minimize(paraboloid, np.array([0.0, 0.0]), method="BFGS")
chk("minimize_argmin", np.allclose(res.x, [3.0, -1.0], atol=1e-4))
chk("minimize_fmin", abs(float(res.fun)) < 1e-8)
chk("brentq_sqrt2", abs(optimize.brentq(lambda t: t * t - 2.0, 0.0, 2.0) - math.sqrt(2.0)) < 1e-10)
chk("newton_sqrt2", abs(optimize.newton(lambda t: t * t - 2.0, 1.5) - math.sqrt(2.0)) < 1e-10)
chk("fsolve_sqrt2",
    abs(float(optimize.fsolve(lambda t: t * t - 2.0, 1.5)[0]) - math.sqrt(2.0)) < 1e-10)
lsq = optimize.least_squares(lambda p: [p[0] - 3.0, p[1] + 1.0], [0.0, 0.0])
chk("least_squares", np.allclose(lsq.x, [3.0, -1.0], atol=1e-6))
# curve_fit recovers exact linear coefficients from noise-free data (y = 2x + 1).
popt = optimize.curve_fit(lambda x, a, b: a * x + b,
                          np.array([0.0, 1.0, 2.0, 3.0]), np.array([1.0, 3.0, 5.0, 7.0]))[0]
chk("curve_fit", np.allclose(popt, [2.0, 1.0], atol=1e-6), "a,b=%s" % popt.tolist())
# linprog: maximise x+y over the simplex x+y<=1, x,y>=0 -> optimum value 1 (minimise -(x+y)).
lp = optimize.linprog(c=[-1.0, -1.0], A_ub=[[1.0, 1.0]], b_ub=[1.0], bounds=[(0, None), (0, None)])
chk("linprog", lp.success and abs(lp.fun - (-1.0)) < 1e-9, "fun=%.6f" % lp.fun)

# ---------------------------------------------------------------- scipy.integrate
from scipy import integrate

chk("quad_sin", abs(integrate.quad(math.sin, 0.0, math.pi)[0] - 2.0) < 1e-9)
chk("quad_x2", abs(integrate.quad(lambda x: x * x, 0.0, 1.0)[0] - 1.0 / 3.0) < 1e-12)
chk("quad_gaussian",
    abs(integrate.quad(lambda x: math.exp(-x * x), -np.inf, np.inf)[0] - math.sqrt(math.pi)) < 1e-9)
chk("dblquad_unit", abs(integrate.dblquad(lambda y, x: 1.0, 0, 1, 0, 1)[0] - 1.0) < 1e-12)
xs = np.linspace(0.0, 1.0, 101)
chk("simpson_x2", abs(integrate.simpson(xs ** 2, x=xs) - 1.0 / 3.0) < 1e-6)
chk("trapezoid",
    abs(integrate.trapezoid(np.array([0.0, 1.0, 2.0]), x=np.array([0.0, 1.0, 2.0])) - 2.0) < 1e-12)
ct = integrate.cumulative_trapezoid(np.array([1.0, 1.0, 1.0]), dx=1.0, initial=0.0)
chk("cumulative_trapezoid", np.allclose(ct, [0.0, 1.0, 2.0]))
# solve_ivp: y' = y, y(0)=1 -> y(1)=e.
iv = integrate.solve_ivp(lambda t, y: y, [0.0, 1.0], [1.0], rtol=1e-10, atol=1e-12)
chk("solve_ivp_exp", abs(float(iv.y[0, -1]) - math.e) < 1e-6, "y(1)=%.9f" % iv.y[0, -1])

# ---------------------------------------------------------------- scipy.interpolate
from scipy import interpolate

f1 = interpolate.interp1d(np.array([0.0, 1.0, 2.0]), np.array([0.0, 2.0, 4.0]))
chk("interp1d_linear", abs(float(f1(0.5)) - 1.0) < 1e-12 and abs(float(f1(1.5)) - 3.0) < 1e-12)
xk = np.linspace(-2.0, 2.0, 9)
cs = interpolate.CubicSpline(xk, xk ** 3)
chk("cubic_spline", abs(float(cs(1.0)) - 1.0) < 1e-9 and np.allclose(cs(xk), xk ** 3))
tck = interpolate.splrep(xk, np.sin(xk), s=0)
chk("splrep_splev", abs(float(interpolate.splev(0.0, tck)) - 0.0) < 1e-9)
pch = interpolate.PchipInterpolator(np.array([0.0, 1.0, 2.0]), np.array([0.0, 1.0, 4.0]))
chk("pchip_nodes", np.allclose(pch([0.0, 1.0, 2.0]), [0.0, 1.0, 4.0]))
bc = interpolate.BarycentricInterpolator(np.array([0.0, 1.0, 2.0]), np.array([1.0, 2.0, 5.0]))
chk("barycentric", np.allclose(bc([0.0, 1.0, 2.0]), [1.0, 2.0, 5.0]))

# ---------------------------------------------------------------- scipy.fft
from scipy import fft

chk("fft_delta", np.allclose(fft.fft(np.array([1.0, 0.0, 0.0, 0.0])), [1.0, 1.0, 1.0, 1.0]))
chk("fft_dc", np.allclose(fft.fft(np.ones(4)), [4.0, 0.0, 0.0, 0.0]))
xr = np.array([1.0, 2.0, 3.0, 4.0])
chk("ifft_roundtrip", np.allclose(fft.ifft(fft.fft(xr)), xr))
chk("rfft_len", fft.rfft(xr).shape[0] == 3 and np.allclose(fft.irfft(fft.rfft(xr), n=4), xr))
chk("fftfreq", np.allclose(fft.fftfreq(4, d=1.0), [0.0, 0.25, -0.5, -0.25]))
# Parseval: sum|x|^2 == sum|X|^2 / N.
X = fft.fft(xr)
chk("parseval", abs(np.sum(xr ** 2) - np.sum(np.abs(X) ** 2) / len(xr)) < 1e-9)
chk("dct_idct", np.allclose(fft.idct(fft.dct(xr, norm="ortho"), norm="ortho"), xr))

# ---------------------------------------------------------------- scipy.signal
from scipy import signal

chk("convolve_full", signal.convolve(np.array([1, 2, 3]), np.array([1, 1])).tolist() == [1, 3, 5, 3])
chk("correlate_valid",
    signal.correlate(np.array([1, 2, 3]), np.array([1, 1]), mode="valid").tolist() == [3, 5])
chk("fftconvolve",
    np.allclose(signal.fftconvolve(np.array([1.0, 2.0, 3.0]), np.array([1.0, 1.0])),
                [1.0, 3.0, 5.0, 3.0]))

# ---------------------------------------------------------------- scipy.sparse
from scipy import sparse
from scipy.sparse import linalg as splinalg

diag = sparse.csr_matrix((np.array([1.0, 2.0, 3.0]),
                          (np.array([0, 1, 2]), np.array([0, 1, 2]))), shape=(3, 3))
chk("sparse_nnz", int(diag.nnz) == 3)
chk("sparse_matvec", diag.dot(np.ones(3)).tolist() == [1.0, 2.0, 3.0])
chk("sparse_matmul", (diag @ diag).toarray().tolist() ==
    [[1.0, 0.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 9.0]])
chk("sparse_csc", sparse.csc_matrix(diag).toarray().tolist() == diag.toarray().tolist())
chk("sparse_coo", sparse.coo_matrix(diag).toarray().tolist() == diag.toarray().tolist())
chk("sparse_eye", sparse.eye(3).toarray().tolist() == np.eye(3).tolist())
chk("sparse_diags", sparse.diags([1.0, 2.0, 3.0]).toarray().tolist() == diag.toarray().tolist())
chk("sparse_kron",
    sparse.kron(sparse.eye(2), sparse.eye(2)).toarray().tolist() == np.eye(4).tolist())
xsp = splinalg.spsolve(diag.tocsc(), np.array([1.0, 4.0, 9.0]))
chk("spsolve", np.allclose(xsp, [1.0, 2.0, 3.0]))

# ---------------------------------------------------------------- scipy.stats
from scipy import stats

chk("norm_cdf_0", abs(stats.norm.cdf(0.0) - 0.5) < 1e-12)
chk("norm_pdf_0", abs(stats.norm.pdf(0.0) - (1.0 / math.sqrt(2.0 * math.pi))) < 1e-12)
chk("norm_cdf_196", abs(stats.norm.cdf(1.959963984540054) - 0.975) < 1e-9)
chk("binom_pmf", abs(stats.binom.pmf(2, 4, 0.5) - 0.375) < 1e-12)     # C(4,2) * 0.5^4 = 6/16
chk("poisson_pmf", abs(stats.poisson.pmf(0, 2.0) - math.exp(-2.0)) < 1e-12)
chk("expon_cdf", abs(stats.expon.cdf(1.0) - (1.0 - math.exp(-1.0))) < 1e-12)
lr = stats.linregress(np.array([0.0, 1.0, 2.0, 3.0]), np.array([1.0, 3.0, 5.0, 7.0]))
chk("linregress",
    abs(lr.slope - 2.0) < 1e-12 and abs(lr.intercept - 1.0) < 1e-12 and abs(lr.rvalue - 1.0) < 1e-12)
chk("pearsonr",
    abs(stats.pearsonr(np.array([1.0, 2.0, 3.0]), np.array([2.0, 4.0, 6.0]))[0] - 1.0) < 1e-12)
chk("spearmanr",
    abs(stats.spearmanr(np.array([1.0, 2.0, 3.0, 4.0]),
                        np.array([1.0, 4.0, 9.0, 16.0]))[0] - 1.0) < 1e-12)
chk("ttest_1samp", abs(float(stats.ttest_1samp(np.array([2.0, 2.0, 2.0, 2.0]), 2.0).statistic)) < 1e-9
    or math.isnan(float(stats.ttest_1samp(np.array([2.0, 2.0, 2.0, 2.0]), 2.0).statistic)))

# ---------------------------------------------------------------- scipy.special
from scipy import special

chk("gamma", abs(special.gamma(5.0) - 24.0) < 1e-9)                   # (5-1)! = 24
chk("gammaln", abs(special.gammaln(6.0) - math.log(120.0)) < 1e-9)   # ln(5!) = ln 120
chk("erf", abs(special.erf(0.0)) < 1e-15 and abs(special.erf(np.inf) - 1.0) < 1e-15)
chk("erfc", abs(special.erfc(0.0) - 1.0) < 1e-15)
chk("comb", int(special.comb(5, 2, exact=True)) == 10)
chk("factorial", int(special.factorial(5, exact=True)) == 120)
chk("beta", abs(special.beta(2.0, 3.0) - (1.0 / 12.0)) < 1e-12)      # B(2,3)=1!*2!/4!=1/12
chk("expit", abs(special.expit(0.0) - 0.5) < 1e-15)

print("SCIPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SCIPY_DONE")
    sys.exit(0)
sys.exit(1)
