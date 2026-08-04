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

# ================================================================ full-API supplement
# Every assertion below is closed-form or a documented invariant verified against a real
# scipy build; nothing depends on print formatting or default float repr.

# ---------------------------------------------------------------- scipy.cluster
from scipy.cluster import vq as _vq, hierarchy as _hier

_blobs = np.array([[0.0, 0.0], [0.1, 0.0], [10.0, 10.0], [10.1, 10.0]])
chk("vq_whiten_shape",
    _vq.whiten(np.array([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]])).shape == (4, 2))
# vq assigns each point to the nearest of two well-separated codebook entries.
_codes, _d = _vq.vq(_blobs, np.array([[0.0, 0.0], [10.0, 10.0]]))
chk("vq_assign", _codes.tolist() == [0, 0, 1, 1])
# kmeans2 with fixed seed recovers two clusters: labels split the blobs into {0,1}.
_cent2, _lab2 = _vq.kmeans2(_blobs, np.array([[0.05, 0.0], [10.05, 10.0]]), seed=1)
chk("kmeans2_split", set(_lab2.tolist()) == {0, 1} and _cent2.shape == (2, 2))
_centk, _dist = _vq.kmeans(_blobs, 2, seed=1)
chk("kmeans_shape", _centk.shape == (2, 2) and float(_dist) >= 0.0)
_Z = _hier.linkage(_blobs, method="single")
chk("linkage_shape", _Z.shape == (3, 4))
chk("fcluster_count", len(set(_hier.fcluster(_Z, t=2, criterion="maxclust").tolist())) == 2)
_dn = _hier.dendrogram(_Z, no_plot=True)
chk("dendrogram", "leaves" in _dn and len(_dn["leaves"]) == 4)

# ---------------------------------------------------------------- scipy.constants
from scipy import constants

chk("const_pi", constants.pi == math.pi)
chk("const_c", constants.c == 299792458.0)                            # exact SI definition
chk("const_speed_of_light", constants.speed_of_light == 299792458.0)
chk("const_golden", abs(constants.golden - 1.618033988749895) < 1e-9)
chk("const_avogadro", constants.Avogadro == 6.02214076e23)            # exact 2019 SI
chk("const_boltzmann", constants.Boltzmann == 1.380649e-23)           # exact 2019 SI
chk("const_g", constants.g == 9.80665)                                # standard gravity
chk("const_convert_temperature",
    abs(constants.convert_temperature(0.0, "Celsius", "Kelvin") - 273.15) < 1e-9)
chk("const_physical_constants",
    constants.physical_constants["speed of light in vacuum"][0] == 299792458.0)
chk("const_value", constants.value("elementary charge") == 1.602176634e-19)  # exact 2019 SI
chk("const_unit", constants.unit("elementary charge") == "C")
chk("const_precision", constants.precision("speed of light in vacuum") == 0.0)  # exact -> 0

# ---------------------------------------------------------------- scipy.fftpack (legacy)
from scipy import fftpack

_fx = np.array([1.0, 2.0, 3.0, 4.0])
chk("fftpack_fft_delta", np.allclose(fftpack.fft(np.array([1.0, 0.0, 0.0, 0.0])), [1, 1, 1, 1]))
chk("fftpack_ifft_roundtrip", np.allclose(fftpack.ifft(fftpack.fft(_fx)), _fx))
chk("fftpack_dct_idct",
    np.allclose(fftpack.idct(fftpack.dct(_fx, norm="ortho"), norm="ortho"), _fx))
chk("fftpack_fftshift", fftpack.fftshift(np.array([0, 1, 2, 3])).tolist() == [2, 3, 0, 1])
chk("fftpack_fftfreq", np.allclose(fftpack.fftfreq(4, 1.0), [0.0, 0.25, -0.5, -0.25]))

# ---------------------------------------------------------------- scipy.integrate (more)
chk("tplquad_unit_cube",
    abs(integrate.tplquad(lambda z, y, x: 1.0, 0, 1, 0, 1, 0, 1)[0] - 1.0) < 1e-9)
chk("nquad_unit_square", abs(integrate.nquad(lambda y, x: 1.0, [[0, 1], [0, 1]])[0] - 1.0) < 1e-9)
chk("fixed_quad_x2", abs(integrate.fixed_quad(lambda x: x * x, 0.0, 1.0, n=5)[0] - 1.0 / 3.0) < 1e-9)
# romberg() was removed in scipy 1.15; the documented replacement romb() on 2^k+1 samples of
# sin over [0, pi] integrates to 2, and newton_cotes(4) weights sum to the interval count.
_rx = np.linspace(0.0, math.pi, 2 ** 6 + 1)
chk("romb_sin", abs(integrate.romb(np.sin(_rx), dx=math.pi / (2 ** 6)) - 2.0) < 1e-6)
_ncw, _ = integrate.newton_cotes(4)
chk("newton_cotes_weights", abs(float(np.sum(_ncw)) - 4.0) < 1e-9)
# odeint and the ode class integrator both solve y'=y, y(0)=1 -> y(1)=e.
_od = integrate.odeint(lambda y, t: y, 1.0, np.array([0.0, 1.0]))
chk("odeint_exp", abs(float(_od[-1, 0]) - math.e) < 1e-6)
_ode = integrate.ode(lambda t, y: y).set_integrator("dopri5")
_ode.set_initial_value(1.0, 0.0)
_yv = _ode.integrate(1.0)
chk("ode_class_dopri5", _ode.successful() and abs(float(_yv[0]) - math.e) < 1e-5)

# ---------------------------------------------------------------- scipy.interpolate (more)
# interpn on the 2x2 grid of f(x,y)=x+y evaluated at (0.5, 0.5) == 1.0.
_gx = np.array([0.0, 1.0])
_gv = np.array([[0.0, 1.0], [1.0, 2.0]])
chk("interpn_bilinear",
    abs(float(interpolate.interpn((_gx, _gx), _gv, np.array([[0.5, 0.5]]))[0]) - 1.0) < 1e-9)
_akx = np.array([0.0, 1.0, 2.0, 3.0])
_ak = interpolate.Akima1DInterpolator(_akx, _akx ** 2)
chk("akima_nodes", np.allclose(_ak(_akx), _akx ** 2))
# BSpline built from an splrep tck evaluates identically to splev on the same tck.
_bx = np.linspace(-2.0, 2.0, 9)
_btck = interpolate.splrep(_bx, np.sin(_bx), s=0)
_bsp = interpolate.BSpline(_btck[0], _btck[1], _btck[2])
chk("bspline_matches_splev",
    abs(float(_bsp(0.5)) - float(interpolate.splev(0.5, _btck))) < 1e-12)
_rn = np.array([[0.0], [1.0], [2.0]])
_rv = np.array([0.0, 1.0, 4.0])
chk("rbfinterpolator_nodes", np.allclose(interpolate.RBFInterpolator(_rn, _rv)(_rn), _rv, atol=1e-6))
_rbf = interpolate.Rbf(np.array([0.0, 1.0, 2.0]), np.array([0.0, 1.0, 4.0]))
chk("rbf_nodes", np.allclose(_rbf(np.array([0.0, 1.0, 2.0])), [0.0, 1.0, 4.0], atol=1e-6))

# ---------------------------------------------------------------- scipy.io (filesystem)
import tempfile as _tempfile
import os as _os
import shutil as _shutil
from scipy import io as sio

_Mio = np.array([[1.0, 2.0], [3.0, 4.0]])
_iodir = _tempfile.mkdtemp()
try:
    _matp = _os.path.join(_iodir, "carpet.mat")
    sio.savemat(_matp, {"M": _Mio})
    _ld = sio.loadmat(_matp)
    chk("savemat_loadmat", np.allclose(_ld["M"], _Mio) and _ld["M"].shape == (2, 2))
    _mtxp = _os.path.join(_iodir, "carpet.mtx")
    sio.mmwrite(_mtxp, sparse.csr_matrix(_Mio))
    chk("mmwrite_mmread", np.allclose(sio.mmread(_mtxp).toarray(), _Mio))
    from scipy.io import wavfile as _wavfile
    _samp = np.array([0, 1000, -1000, 32767, -32768], dtype=np.int16)
    _wavp = _os.path.join(_iodir, "carpet.wav")
    _wavfile.write(_wavp, 8000, _samp)
    _rate, _rd = _wavfile.read(_wavp)
    chk("wavfile_roundtrip", _rate == 8000 and np.array_equal(_rd, _samp))
finally:
    _shutil.rmtree(_iodir, ignore_errors=True)

# ---------------------------------------------------------------- scipy.linalg (more)
# eigvals of the 90-degree rotation matrix are +-i; eig/eigh of diagonal/SPD are known.
chk("eigvals_imag_pair",
    np.allclose(sorted(linalg.eigvals(np.array([[0.0, -1.0], [1.0, 0.0]])).imag), [-1.0, 1.0]))
_ew, _ = linalg.eig(np.diag([2.0, 3.0]))
chk("eig_diag", np.allclose(sorted(_ew.real), [2.0, 3.0]))
_eh = linalg.eigh(np.array([[4.0, 2.0], [2.0, 3.0]]), eigvals_only=True)
chk("eigh_ascending", _eh[0] <= _eh[1] and np.allclose(np.sort(_eh), _eh))
_Asch = np.array([[2.0, 1.0], [1.0, 3.0]])
_T, _Zsch = linalg.schur(_Asch)
chk("schur_reconstruct", np.allclose(_Zsch @ _T @ _Zsch.T, _Asch))
_Utri = np.array([[2.0, 1.0], [0.0, 3.0]])
chk("solve_triangular",
    np.allclose(_Utri @ linalg.solve_triangular(_Utri, np.array([5.0, 9.0])), [5.0, 9.0]))
chk("cond_identity", abs(np.linalg.cond(np.eye(3)) - 1.0) < 1e-9)  # scipy.linalg exposes no cond
_nsp = linalg.null_space(np.array([[1.0, 1.0], [1.0, 1.0]]))
chk("null_space",
    _nsp.shape[1] == 1 and np.allclose(np.array([[1.0, 1.0], [1.0, 1.0]]) @ _nsp, 0, atol=1e-9))
_ob = linalg.orth(np.array([[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]))
chk("orth_orthonormal", np.allclose(_ob.T @ _ob, np.eye(_ob.shape[1]), atol=1e-9))
chk("block_diag",
    linalg.block_diag(np.array([[1.0]]), np.array([[2.0, 0.0], [0.0, 2.0]])).tolist()
    == [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]])
chk("circulant", linalg.circulant([1.0, 2.0, 3.0])[:, 0].tolist() == [1.0, 2.0, 3.0])
chk("toeplitz",
    linalg.toeplitz([1.0, 2.0, 3.0]).tolist() == [[1.0, 2.0, 3.0], [2.0, 1.0, 2.0], [3.0, 2.0, 1.0]])
chk("hadamard", linalg.hadamard(2).tolist() == [[1, 1], [1, -1]])
chk("dft_matches_fft", np.allclose(linalg.dft(4) @ _fx, np.fft.fft(_fx)))
_Mlog = np.array([[2.0, 0.0], [0.0, 3.0]])
chk("logm_inverse_expm", np.allclose(linalg.logm(linalg.expm(_Mlog)), _Mlog, atol=1e-9))
chk("sqrtm_diag", np.allclose(linalg.sqrtm(np.diag([4.0, 9.0])), np.diag([2.0, 3.0])))

# ---------------------------------------------------------------- scipy.ndimage
from scipy import ndimage

_const_img = np.full((5, 5), 3.0)
chk("gaussian_filter_const", np.allclose(ndimage.gaussian_filter(_const_img, sigma=1.0), 3.0))
chk("uniform_filter_const", np.allclose(ndimage.uniform_filter(_const_img, size=3), 3.0))
_spike = np.zeros((5, 5))
_spike[2, 2] = 100.0
chk("median_filter_despike", ndimage.median_filter(_spike, size=3)[2, 2] == 0.0)
_ramp = np.tile(np.arange(5.0), (5, 1))
_sob = ndimage.sobel(_ramp, axis=1)
chk("sobel_constant_interior", np.allclose(_sob[:, 2], _sob[0, 2]))
chk("shift_integer",
    np.allclose(ndimage.shift(np.array([[1.0, 2.0, 3.0, 4.0, 5.0]]), [0, 1], order=0, cval=0.0),
                [[0.0, 1.0, 2.0, 3.0, 4.0]]))
chk("zoom_doubles_shape", ndimage.zoom(np.array([[1.0, 2.0], [3.0, 4.0]]), 2, order=1).shape == (4, 4))
_lab, _ncomp = ndimage.label(np.array([[1, 0, 1], [0, 0, 0], [1, 0, 0]]))
chk("label_count", _ncomp == 3)
chk("center_of_mass", np.allclose(ndimage.center_of_mass(np.array([[1.0, 1.0], [1.0, 1.0]])), [0.5, 0.5]))
_delta = np.zeros((3, 3))
_delta[1, 1] = 1.0
_img33 = np.arange(1, 10, dtype=float).reshape(3, 3)
chk("ndimage_convolve_delta", np.allclose(ndimage.convolve(_img33, _delta), _img33))
_ero = ndimage.binary_erosion(np.ones((3, 3), dtype=bool))
chk("binary_erosion", bool(_ero[1, 1]) and not bool(_ero[0, 0]))
chk("binary_dilation", int(ndimage.binary_dilation(_delta.astype(bool)).sum()) == 5)
chk("maximum_filter", ndimage.maximum_filter(_spike, size=3)[2, 2] == 100.0)
chk("rotate_shape", ndimage.rotate(np.eye(4), 90, reshape=False, order=0).shape == (4, 4))

# ---------------------------------------------------------------- scipy.odr
from scipy import odr

_oxd = np.array([0.0, 1.0, 2.0, 3.0])
_oyd = 2.0 * _oxd + 1.0
_odata = odr.Data(_oxd, _oyd)
_omodel = odr.Model(lambda B, x: B[0] * x + B[1])
_ofit = odr.ODR(_odata, _omodel, beta0=[1.0, 1.0]).run()
chk("odr_linear", np.allclose(_ofit.beta, [2.0, 1.0], atol=1e-6))
_ofit2 = odr.ODR(odr.RealData(_oxd, _oyd), _omodel, beta0=[1.0, 1.0]).run()
chk("odr_realdata_shape", _ofit2.beta.shape == (2,))
_opoly = odr.ODR(_odata, odr.polynomial(1)).run()
chk("odr_polynomial", np.allclose(sorted(_opoly.beta), [1.0, 2.0], atol=1e-5))

# ---------------------------------------------------------------- scipy.optimize (more)
def _paraboloid(p):
    return (p[0] - 3.0) ** 2 + (p[1] + 1.0) ** 2  # unique min at (3, -1)


chk("minimize_scalar", abs(optimize.minimize_scalar(lambda x: (x - 3.0) ** 2).x - 3.0) < 1e-5)
chk("bisect_sqrt2", abs(optimize.bisect(lambda t: t * t - 2.0, 0.0, 2.0) - math.sqrt(2.0)) < 1e-10)
chk("brenth_sqrt2", abs(optimize.brenth(lambda t: t * t - 2.0, 0.0, 2.0) - math.sqrt(2.0)) < 1e-10)
chk("ridder_sqrt2", abs(optimize.ridder(lambda t: t * t - 2.0, 0.0, 2.0) - math.sqrt(2.0)) < 1e-10)
_rt = optimize.root(lambda t: [t[0] * t[0] - 2.0], [1.5])
chk("root_hybr", _rt.success and abs(_rt.x[0] - math.sqrt(2.0)) < 1e-8)
# linear_sum_assignment: the min-cost assignment of this 3x3 matrix has total cost 5.
_cost = np.array([[4.0, 1.0, 3.0], [2.0, 0.0, 5.0], [3.0, 2.0, 2.0]])
_ri, _ci = optimize.linear_sum_assignment(_cost)
chk("linear_sum_assignment", float(_cost[_ri, _ci].sum()) == 5.0)
_xnn, _rnn = optimize.nnls(np.array([[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]), np.array([1.0, 2.0, 3.0]))
chk("nnls", np.all(_xnn >= 0.0) and np.allclose(_xnn, [1.0, 2.0], atol=1e-6))
# Global optimizers with a fixed seed land on the paraboloid minimum (3, -1).
chk("differential_evolution",
    np.allclose(optimize.differential_evolution(_paraboloid, [(-5, 5), (-5, 5)], seed=1, tol=1e-7).x,
                [3.0, -1.0], atol=1e-3))
chk("dual_annealing",
    np.allclose(optimize.dual_annealing(_paraboloid, [(-5, 5), (-5, 5)], seed=1, maxiter=200).x,
                [3.0, -1.0], atol=1e-2))
chk("basinhopping",
    np.allclose(optimize.basinhopping(_paraboloid, [0.0, 0.0], seed=1, niter=5).x,
                [3.0, -1.0], atol=1e-3))
for _meth in ["Nelder-Mead", "Powell", "CG", "L-BFGS-B", "SLSQP", "TNC", "COBYLA"]:
    chk("minimize_" + _meth,
        np.allclose(optimize.minimize(_paraboloid, [0.0, 0.0], method=_meth).x, [3.0, -1.0], atol=1e-2))

# ---------------------------------------------------------------- scipy.signal (more)
_bb, _aa = signal.butter(4, 0.2)
chk("butter_order", len(_bb) == 5 and len(_aa) == 5)                  # order+1 coefficients
_bc, _ac = signal.cheby1(4, 1, 0.2)
chk("cheby1_order", len(_bc) == 5 and len(_ac) == 5)
_imp = np.zeros(5)
_imp[0] = 1.0
chk("lfilter_identity", np.allclose(signal.lfilter([1.0], [1.0], _imp), _imp))
chk("filtfilt_const",
    np.allclose(signal.filtfilt(*signal.butter(2, 0.3), np.full(50, 4.0)), 4.0, atol=1e-6))
# Moving-average filter has unity DC gain.
_wf, _hf = signal.freqz([0.5, 0.5], worN=8)
chk("freqz_dc_gain", abs(abs(_hf[0]) - 1.0) < 1e-9)
_stime = np.linspace(0.0, 1.0, 500, endpoint=False)
_sine = np.sin(2 * np.pi * 5 * _stime)                                # 5 full periods -> 5 peaks
chk("find_peaks", len(signal.find_peaks(_sine)[0]) == 5)
_fw, _pw = signal.welch(_sine, nperseg=64)
chk("welch_freq_range", _fw[0] >= 0.0 and _fw[-1] <= 0.5 + 1e-9)
_fsp, _tsp, _sxx = signal.spectrogram(_sine, nperseg=64)
chk("spectrogram_shapes", _sxx.shape[0] == _fsp.shape[0] and _sxx.shape[1] == _tsp.shape[0])
# Analytic-signal envelope of a pure cosine is ~1 away from the edges.
_env = np.abs(signal.hilbert(np.cos(2 * np.pi * 5 * _stime)))
chk("hilbert_envelope", np.allclose(_env[50:450], 1.0, atol=1e-2))
chk("resample_roundtrip", np.allclose(signal.resample(_sine, len(_sine)), _sine, atol=1e-6))
_hann = signal.windows.hann(5)
chk("windows_hann", _hann[0] == 0.0 and _hann[-1] == 0.0 and np.allclose(_hann, _hann[::-1]))
_ham = signal.windows.hamming(5)
chk("windows_hamming", np.allclose(_ham, _ham[::-1]))
_blk = signal.windows.blackman(5)
chk("windows_blackman", np.allclose(_blk, _blk[::-1]) and _blk[0] < 1e-10)

# ---------------------------------------------------------------- scipy.sparse (more)
_lil = sparse.lil_matrix((3, 3))
_lil[0, 0] = 5.0
_lil[2, 1] = 7.0
chk("lil_matrix", _lil.toarray()[0, 0] == 5.0 and _lil.toarray()[2, 1] == 7.0)
chk("dia_matrix", np.allclose(sparse.diags([1.0, 2.0, 3.0]).todia().toarray(), np.diag([1.0, 2.0, 3.0])))
_bsr = sparse.bsr_matrix(sparse.csr_matrix(np.array([[1.0, 0.0], [0.0, 2.0]])))
chk("bsr_matrix", np.allclose(_bsr.toarray(), [[1.0, 0.0], [0.0, 2.0]]))
chk("sparse_hstack", sparse.hstack([sparse.eye(3), sparse.eye(3)]).shape == (3, 6))
chk("sparse_vstack", sparse.vstack([sparse.eye(3), sparse.eye(3)]).shape == (6, 3))
from scipy.sparse.csgraph import shortest_path as _shortest_path
from scipy.sparse.csgraph import connected_components as _cc
from scipy.sparse.csgraph import dijkstra as _dijkstra

# directed weighted path 0->1 (w=1) ->2 (w=2) so d(0,2)=3.
_graph = sparse.csr_matrix(np.array([[0.0, 1.0, 0.0], [0.0, 0.0, 2.0], [0.0, 0.0, 0.0]]))
chk("csgraph_shortest_path", _shortest_path(_graph, method="D")[0, 2] == 3.0)
chk("csgraph_dijkstra", _dijkstra(_graph, indices=0)[2] == 3.0)
chk("csgraph_connected_components",
    _cc(sparse.csr_matrix(np.array([[0, 1, 0], [1, 0, 0], [0, 0, 0]])), directed=False)[0] == 2)
# Largest-magnitude eigenvalue of diag(1,2,3) is 3.
chk("splinalg_eigs",
    abs(abs(splinalg.eigs(sparse.diags([1.0, 2.0, 3.0]).tocsc(), k=1, which="LM")[0][0]) - 3.0) < 1e-6)

# ---------------------------------------------------------------- scipy.spatial
from scipy import spatial
from scipy.spatial import distance as _distance

chk("distance_euclidean", abs(_distance.euclidean([0, 0], [3, 4]) - 5.0) < 1e-12)
chk("distance_cityblock", _distance.cityblock([0, 0], [3, 4]) == 7)
chk("distance_cosine_parallel", abs(_distance.cosine([1, 0], [2, 0])) < 1e-12)
chk("distance_hamming", abs(_distance.hamming([1, 0, 1, 1], [1, 1, 1, 0]) - 0.5) < 1e-12)
_pd = _distance.pdist(np.array([[0.0, 0.0], [3.0, 4.0]]))
chk("pdist", _pd.shape == (1,) and abs(_pd[0] - 5.0) < 1e-12)
chk("cdist",
    np.allclose(_distance.cdist(np.array([[0.0, 0.0]]), np.array([[3.0, 4.0], [0.0, 0.0]])), [[5.0, 0.0]]))
_kd, _ki = spatial.KDTree(np.array([[0.0, 0.0], [10.0, 10.0]])).query([0.1, 0.1])
chk("kdtree_query", _ki == 0 and abs(_kd - math.hypot(0.1, 0.1)) < 1e-9)
_square = np.array([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
_hull = spatial.ConvexHull(_square)
chk("convex_hull", len(_hull.vertices) == 4 and abs(_hull.volume - 1.0) < 1e-9)  # 2D area
chk("delaunay", spatial.Delaunay(_square).simplices.shape[0] == 2)
_vor = spatial.Voronoi(np.array([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]))
chk("voronoi", _vor.vertices.shape[0] == 1 and np.allclose(_vor.vertices[0], [0.5, 0.5]))
chk("procrustes", abs(spatial.procrustes(_square, _square.copy())[2]) < 1e-12)

# ---------------------------------------------------------------- scipy.special (more)
chk("bessel_jn0", abs(special.jn(0, 0.0) - 1.0) < 1e-12)              # J0(0) = 1
chk("bessel_jv0", abs(special.jv(0, 0.0) - 1.0) < 1e-12)
chk("bessel_yn_neg", special.yn(1, 1.0) < 0.0)                        # Y1(1) is negative
chk("special_binom", abs(special.binom(5, 2) - 10.0) < 1e-12)
chk("logit_half", abs(special.logit(0.5)) < 1e-12)                    # logit(0.5) = 0
chk("softmax", np.allclose(special.softmax([0.0, 0.0]), [0.5, 0.5]))
chk("zeta_2", abs(special.zeta(2.0) - math.pi ** 2 / 6.0) < 1e-9)     # Basel problem
chk("digamma_1", abs(special.digamma(1.0) + 0.5772156649015329) < 1e-9)  # psi(1) = -gamma_E
chk("psi_1", abs(special.psi(1.0) + 0.5772156649015329) < 1e-9)
_P2 = special.legendre(2)                                            # P2(x) = (3x^2 - 1)/2
chk("legendre_P2", abs(_P2(1.0) - 1.0) < 1e-12 and abs(_P2(0.0) + 0.5) < 1e-12)
chk("erfinv_roundtrip", abs(special.erfinv(special.erf(0.5)) - 0.5) < 1e-9)

# ---------------------------------------------------------------- scipy.stats (more)
chk("uniform_cdf", abs(stats.uniform.cdf(0.5) - 0.5) < 1e-12)
chk("gamma_mean", abs(stats.gamma(a=1).mean() - 1.0) < 1e-12)         # Gamma(1) == Exp(1)
chk("beta_cdf", abs(stats.beta(1, 1).cdf(0.5) - 0.5) < 1e-12)         # Beta(1,1) == Uniform
chk("t_cdf", abs(stats.t(df=1e6).cdf(0.0) - 0.5) < 1e-9)
chk("chi2_mean", abs(stats.chi2(df=2).mean() - 2.0) < 1e-12)          # mean of chi2 == df
chk("f_median_positive", stats.f(5, 10).median() > 0.0)
_desc = stats.describe(np.array([1.0, 2.0, 3.0, 4.0, 5.0]))
chk("describe",
    _desc.nobs == 5 and abs(_desc.mean - 3.0) < 1e-12 and abs(_desc.variance - 2.5) < 1e-12)
chk("kendalltau",
    abs(stats.kendalltau(np.array([1.0, 2.0, 3.0, 4.0]), np.array([1.0, 2.0, 3.0, 4.0])).correlation
        - 1.0) < 1e-12)
chk("ttest_ind",
    abs(float(stats.ttest_ind(np.array([1.0, 2.0, 3.0]), np.array([1.0, 2.0, 3.0])).statistic)) < 1e-12)
_trel = float(stats.ttest_rel(np.array([1.0, 2.0, 3.0]), np.array([1.0, 2.0, 3.0])).statistic)
chk("ttest_rel", abs(_trel) < 1e-12 or math.isnan(_trel))
_fone = float(stats.f_oneway(np.array([1.0, 2.0, 3.0]), np.array([1.0, 2.0, 3.0])).statistic)
chk("f_oneway", abs(_fone) < 1e-9 or math.isnan(_fone))
chk("kruskal",
    abs(float(stats.kruskal(np.array([1.0, 2.0, 3.0]), np.array([1.0, 2.0, 3.0])).statistic)) < 1e-9)
chk("mannwhitneyu",
    stats.mannwhitneyu(np.array([1.0, 2.0, 3.0]), np.array([100.0, 200.0, 300.0])).pvalue < 0.2)
chk("wilcoxon",
    stats.wilcoxon(np.array([1.0, 2.0, 3.0]), np.array([1.5, 2.5, 3.5])).statistic >= 0.0)
_shap = stats.shapiro(np.array([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]))
chk("shapiro", 0.0 <= float(_shap.statistic) <= 1.0)
_ntest = stats.normaltest(np.array([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]))
chk("normaltest", hasattr(_ntest, "statistic") and hasattr(_ntest, "pvalue"))
chk("ks_2samp",
    abs(float(stats.ks_2samp(np.array([1.0, 2.0, 3.0]), np.array([1.0, 2.0, 3.0])).statistic)) < 1e-12)
# Perfectly proportional contingency table -> chi2 statistic == 0.
chk("chi2_contingency", abs(float(stats.chi2_contingency(np.array([[10, 20], [20, 40]])).statistic)) < 1e-9)
_zs = stats.zscore(np.array([1.0, 2.0, 3.0]))
chk("zscore", abs(float(_zs.mean())) < 1e-12 and abs(float(np.std(_zs)) - 1.0) < 1e-9)
chk("sem_constant", abs(float(stats.sem(np.array([2.0, 2.0, 2.0, 2.0])))) < 1e-12)
chk("mode", int(np.atleast_1d(stats.mode(np.array([1, 2, 2, 3, 2])).mode)[0]) == 2)
chk("skew_symmetric", abs(float(stats.skew(np.array([1.0, 2.0, 3.0, 4.0, 5.0])))) < 1e-12)
chk("kurtosis_fisher", abs(float(stats.kurtosis(np.array([1.0, 2.0, 3.0, 4.0, 5.0]))) - (-1.3)) < 0.01)
chk("entropy_uniform", abs(float(stats.entropy([0.5, 0.5])) - math.log(2.0)) < 1e-12)  # == ln 2

# HONEST-SKIP: scipy.datasets (ascent/face/electrocardiogram) requires a network fetch via
# pooch to download the sample data; StarryOS has no network, so it is documented, not asserted.

print("SCIPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SCIPY_DONE")
    sys.exit(0)
sys.exit(1)
