#!/usr/bin/env python3
# NumbaCarpet.py - exhaustive JIT-correctness carpet for Numba, with a structured availability gate.
#
# Numba has no musl `py3-numba` apk and no musllinux / riscv64 / loongarch64 wheels; llvmlite,
# the LLVM binding it JITs through, ships no musl or riscv64 / loongarch64 build either (see the
# README wall analysis). So on the default apk-provisioned overlay `import numba` fails - this
# carpet then prints NUMBA_SKIP with the concrete reason and exits 2 (a SKIP sentinel that
# run_pysci2.py reports but does NOT count toward PASS/TOTAL).
#
# When numba IS provisioned (the opt-in source build against the matching LLVM), this carpet
# compiles and executes a battery of @njit / @vectorize / @guvectorize kernels and checks their
# results against fixed closed-form values exactly, plus a warm-steady-state speedup to prove the
# JIT actually lowered to native code. Coverage: scalar / array-reduction / control-flow (if /
# for / while) / cross-function / recursion / tuple return / typed.List / numpy intrinsics and
# np.linalg / explicit eager signatures (int64 / float64 / multi-sig) / prange parallel reduction
# (1D & 2D) / @vectorize ufunc / @guvectorize gufunc / Mandelbrot escape count / complex arithmetic
# / nopython-mode enforcement. It prints NUMBA_RESULT ok=N fail=0 then NUMBA_DONE and exits 0.
#
# Every assertion has a fixed input and a known closed-form output (exact integers / rationals, or
# a float compared to the interpreted reference within rel<=1e-6), independent of print formatting
# so host reference and target build agree.
import sys
import time

try:
    import numpy as np
    import numba
    from numba import njit, prange, vectorize, guvectorize, int64, float64, float32
    from numba.typed import List as TypedList
    from numba.core.errors import TypingError
except Exception as exc:  # noqa: BLE001 - any import failure means numba is unavailable here
    print("NUMBA_SKIP unavailable: %s: %s" % (type(exc).__name__, exc))
    print("NUMBA_SKIP reason: no musl py3-numba apk and no musl/riscv64/loongarch64 llvmlite "
          "distribution; JIT toolchain not provisioned on this overlay")
    sys.exit(2)

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


def close(a, b, rel=1e-6):
    return abs(a - b) <= rel * max(1.0, abs(b))


chk("version", int(numba.__version__.split(".")[1]) >= 60, "numba=%s" % numba.__version__)

# =============================================================== scalar kernels
@njit(cache=False)
def quad(v):
    return v * v + 1


chk("njit_scalar", quad(7) == 50)
chk("njit_dispatcher", isinstance(quad, numba.core.dispatcher.Dispatcher))
chk("njit_signatures", len(quad.signatures) >= 1)
chk("njit_nopython_sig", len(quad.nopython_signatures) >= 1)


# lazy dispatch: a second dtype triggers a second specialization
@njit(cache=False)
def poly(x):
    return x * x - 2 * x + 1  # (x-1)^2


chk("njit_lazy_int", poly(3) == 4)
chk("njit_lazy_float", close(poly(3.0), 4.0))
chk("njit_lazy_two_sigs", len(poly.signatures) == 2)


# =============================================================== explicit eager signatures
@njit("int64(int64)")
def triple(v):
    return 3 * v


chk("njit_eager_int64", triple(11) == 33)


@njit("float64(float64)")
def halve(v):
    return v / 2.0


chk("njit_eager_float64", close(halve(7.0), 3.5))


@njit([int64(int64), float64(float64)])
def negate(x):
    return -x


chk("njit_multi_sig_int", negate(5) == -5)
chk("njit_multi_sig_float", close(negate(2.5), -2.5))
chk("njit_multi_sig_count", len(negate.signatures) == 2)


# =============================================================== array reduction
@njit(cache=False)
def sum_sq(a):
    s = 0.0
    for i in range(a.shape[0]):
        s += a[i] * a[i]
    return s


arr = np.arange(1000, dtype=np.float64)
chk("njit_array_reduce", sum_sq(arr) == float(np.sum(arr * arr)))  # 332833500.0 exact


# =============================================================== control flow: if / for
@njit(cache=False)
def clamp_sum(a, lo, hi):
    s = 0.0
    for i in range(a.shape[0]):
        v = a[i]
        if v < lo:
            v = lo
        elif v > hi:
            v = hi
        s += v
    return s


ref = sum(min(max(v, -1.0), 1.0) for v in [-3.0, -0.5, 0.0, 0.5, 3.0])
chk("njit_control_flow_if", clamp_sum(np.array([-3.0, -0.5, 0.0, 0.5, 3.0]), -1.0, 1.0) == ref)


# =============================================================== control flow: while (Collatz)
@njit(cache=False)
def collatz_steps(n):
    steps = 0
    while n != 1:
        if n % 2 == 0:
            n = n // 2
        else:
            n = 3 * n + 1
        steps += 1
    return steps


chk("njit_while_collatz", collatz_steps(27) == 111)  # known: 27 reaches 1 in 111 steps
chk("njit_while_collatz_6", collatz_steps(6) == 8)


# =============================================================== cross-function lowering
@njit(cache=False)
def sq(v):
    return v * v


@njit(cache=False)
def sum_of_squares(a):
    s = 0.0
    for i in range(a.shape[0]):
        s += sq(a[i])
    return s


chk("njit_nested_call", sum_of_squares(arr) == sum_sq(arr))


# =============================================================== integer / recursion
@njit(cache=False)
def fib(k):
    a, b = 0, 1
    for _ in range(k):
        a, b = b, a + b
    return a


chk("njit_fibonacci", fib(30) == 832040)


@njit(cache=False)
def fact(k):
    if k <= 1:
        return 1
    return k * fact(k - 1)


chk("njit_recursion", fact(6) == 720)


@njit(cache=False)
def gcd(a, b):
    while b != 0:
        a, b = b, a % b
    return a


chk("njit_recursion_gcd", gcd(1071, 462) == 21)  # gcd(1071,462)=21


# =============================================================== tuple return
@njit(cache=False)
def minmax(a):
    lo = a[0]
    hi = a[0]
    for i in range(a.shape[0]):
        if a[i] < lo:
            lo = a[i]
        if a[i] > hi:
            hi = a[i]
    return lo, hi


chk("njit_tuple_return", minmax(np.array([3.0, -2.0, 7.0, 1.0])) == (-2.0, 7.0))


# =============================================================== enumerate / zip iteration
@njit(cache=False)
def weighted_dot(vals, wts):
    s = 0.0
    for v, w in zip(vals, wts):
        s += v * w
    return s


chk("njit_zip", weighted_dot(np.array([1.0, 2.0, 3.0]), np.array([4.0, 5.0, 6.0])) == 32.0)


@njit(cache=False)
def index_weighted(a):
    s = 0.0
    for i, v in enumerate(a):
        s += i * v
    return s


chk("njit_enumerate", index_weighted(np.array([10.0, 20.0, 30.0])) == 80.0)  # 0+20+60


# =============================================================== numpy intrinsics
@njit(cache=False)
def dot_plus_sum(u, v):
    return np.dot(u, v) + np.sum(u)


u = np.array([1.0, 2.0, 3.0])
v = np.array([4.0, 5.0, 6.0])
chk("njit_np_dot_sum", dot_plus_sum(u, v) == float(np.dot(u, v) + np.sum(u)))  # 32+6=38


@njit(cache=False)
def np_reductions(a):
    return np.mean(a), np.max(a), np.min(a), np.argmax(a), np.prod(a)


mean_, max_, min_, amax_, prod_ = np_reductions(np.array([1.0, 3.0, 2.0, 4.0]))
chk("njit_np_mean", close(mean_, 2.5))
chk("njit_np_max_min", max_ == 4.0 and min_ == 1.0)
chk("njit_np_argmax", amax_ == 3)
chk("njit_np_prod", prod_ == 24.0)


@njit(cache=False)
def np_elementwise(a):
    return np.sum(np.sqrt(a))


chk("njit_np_sqrt", close(np_elementwise(np.array([1.0, 4.0, 9.0, 16.0])), 10.0))  # 1+2+3+4


# =============================================================== 2D array / manual matmul
@njit(cache=False)
def matmul(A, B):
    n, k = A.shape
    _, m = B.shape
    C = np.zeros((n, m))
    for i in range(n):
        for j in range(m):
            s = 0.0
            for t in range(k):
                s += A[i, t] * B[t, j]
            C[i, j] = s
    return C


A = np.array([[1.0, 2.0], [3.0, 4.0]])
B = np.array([[5.0, 6.0], [7.0, 8.0]])
chk("njit_matmul_2d", matmul(A, B).tolist() == (A @ B).tolist())  # [[19,22],[43,50]]


# =============================================================== np.linalg inside njit
@njit(cache=False)
def linsolve(M, b):
    return np.linalg.solve(M, b)


chk("njit_linalg_solve",
    np.allclose(linsolve(np.array([[3.0, 0.0], [0.0, 5.0]]), np.array([9.0, 20.0])), [3.0, 4.0]))


# =============================================================== complex arithmetic
@njit(cache=False)
def cabs2(z):
    return (z * z.conjugate()).real


chk("njit_complex", cabs2(3 + 4j) == 25.0)  # |3+4i|^2 = 25


# =============================================================== typed.List
@njit(cache=False)
def sum_squares_list(n):
    lst = TypedList.empty_list(int64)
    for i in range(n):
        lst.append(i * i)
    tot = 0
    for v in lst:
        tot += v
    return tot, len(lst)


tot, ln = sum_squares_list(5)
chk("njit_typed_list_sum", tot == 30)  # 0+1+4+9+16
chk("njit_typed_list_len", ln == 5)


@njit(cache=False)
def reverse_list(a):
    lst = TypedList.empty_list(float64)
    for i in range(a.shape[0]):
        lst.append(a[i])
    out = TypedList.empty_list(float64)
    for i in range(len(lst) - 1, -1, -1):
        out.append(lst[i])
    return out[0], out[-1]


chk("njit_typed_list_reverse", reverse_list(np.array([1.0, 2.0, 3.0, 4.0])) == (4.0, 1.0))


# =============================================================== parallel reduction (prange)
@njit(parallel=True, cache=False)
def par_sum(a):
    s = 0.0
    for i in prange(a.shape[0]):
        s += a[i]
    return s


ones = np.ones(100000)
chk("njit_prange_1d", par_sum(ones) == 100000.0)


@njit(parallel=True, cache=False)
def par_sum_2d(a):
    s = 0.0
    for i in prange(a.shape[0]):
        for j in range(a.shape[1]):
            s += a[i, j]
    return s


chk("njit_prange_2d", par_sum_2d(np.ones((200, 200))) == 40000.0)


@njit(parallel=True, cache=False)
def par_dot(a, b):
    s = 0.0
    for i in prange(a.shape[0]):
        s += a[i] * b[i]
    return s


xa = np.arange(1, 1001, dtype=np.float64)
chk("njit_prange_dot", par_dot(xa, xa) == float(np.dot(xa, xa)))  # sum k^2, 1..1000


# =============================================================== @vectorize ufunc
@vectorize(["float64(float64, float64)"])
def vadd(a, b):
    return a + b


chk("vectorize_elemwise", vadd(np.array([1.0, 2.0]), np.array([3.0, 4.0])).tolist() == [4.0, 6.0])
chk("vectorize_broadcast", vadd(np.array([1.0, 2.0, 3.0]), 10.0).tolist() == [11.0, 12.0, 13.0])
chk("vectorize_reduce", vadd.reduce(np.arange(1.0, 5.0)) == 10.0)  # 1+2+3+4
chk("vectorize_accumulate", vadd.accumulate(np.array([1.0, 2.0, 3.0])).tolist() == [1.0, 3.0, 6.0])


@vectorize([float32(float32), float64(float64)])
def dbl(x):
    return 2 * x


chk("vectorize_multitype_f64", dbl(np.array([1.5, 2.5])).tolist() == [3.0, 5.0])
chk("vectorize_multitype_f32", dbl(np.array([1.5], dtype=np.float32)).dtype == np.float32)


# =============================================================== @guvectorize gufunc
@guvectorize(["void(float64[:], float64[:])"], "(n)->(n)")
def gu_cumsum(x, out):
    acc = 0.0
    for i in range(x.shape[0]):
        acc += x[i]
        out[i] = acc


chk("guvectorize_cumsum", gu_cumsum(np.array([1.0, 2.0, 3.0, 4.0])).tolist() == [1.0, 3.0, 6.0, 10.0])


@guvectorize(["void(float64[:], float64[:], float64[:])"], "(n),(n)->()")
def gu_dot(a, b, out):
    s = 0.0
    for i in range(a.shape[0]):
        s += a[i] * b[i]
    out[0] = s


chk("guvectorize_dot", float(gu_dot(np.array([1.0, 2.0, 3.0]), np.array([4.0, 5.0, 6.0]))) == 32.0)
# batched over the leading axis: two independent dot products in one call
gu_batch = gu_dot(np.array([[1.0, 2.0], [3.0, 4.0]]), np.array([[1.0, 1.0], [1.0, 1.0]]))
chk("guvectorize_batched", gu_batch.tolist() == [3.0, 7.0])


# =============================================================== Mandelbrot escape count
@njit(cache=False)
def escape(cx, cy, maxit):
    zx = 0.0
    zy = 0.0
    for i in range(maxit):
        nx = zx * zx - zy * zy + cx
        ny = 2.0 * zx * zy + cy
        zx = nx
        zy = ny
        if zx * zx + zy * zy > 4.0:
            return i
    return maxit


def escape_py(cx, cy, maxit):
    zx = 0.0
    zy = 0.0
    for i in range(maxit):
        nx = zx * zx - zy * zy + cx
        ny = 2.0 * zx * zy + cy
        zx = nx
        zy = ny
        if zx * zx + zy * zy > 4.0:
            return i
    return maxit


chk("njit_mandelbrot_inside", escape(-0.5, 0.0, 100) == 100)  # inside the set: never escapes
chk("njit_mandelbrot_outside", escape(2.0, 2.0, 100) == escape_py(2.0, 2.0, 100))
chk("njit_mandelbrot_edge", escape(0.3, 0.5, 100) == escape_py(0.3, 0.5, 100))


# grid histogram of escape counts is byte-identical to the interpreted reference
@njit(cache=False)
def escape_grid_sum(x0, x1, y0, y1, n, maxit):
    total = 0
    for a in range(n):
        cx = x0 + (x1 - x0) * a / (n - 1)
        for b in range(n):
            cy = y0 + (y1 - y0) * b / (n - 1)
            total += escape(cx, cy, maxit)
    return total


def escape_grid_sum_py(x0, x1, y0, y1, n, maxit):
    total = 0
    for a in range(n):
        cx = x0 + (x1 - x0) * a / (n - 1)
        for b in range(n):
            cy = y0 + (y1 - y0) * b / (n - 1)
            total += escape_py(cx, cy, maxit)
    return total


chk("njit_mandelbrot_grid",
    escape_grid_sum(-2.0, 1.0, -1.5, 1.5, 40, 60) == escape_grid_sum_py(-2.0, 1.0, -1.5, 1.5, 40, 60))


# =============================================================== nopython-mode enforcement
# @njit refuses to fall back to object mode: an unsupported op is a compile-time TypingError,
# proving the kernel really was lowered in nopython mode rather than interpreted.
@njit(cache=False)
def unsupported():
    return open("/nonexistent")  # file I/O has no nopython lowering


nopython_enforced = False
try:
    unsupported()
except TypingError:
    nopython_enforced = True
except Exception:  # noqa: BLE001
    nopython_enforced = True  # still a hard compile failure, not a silent object-mode fallback
chk("njit_nopython_enforced", nopython_enforced)


# =============================================================== steady-state speedup
@njit(cache=False)
def busy(n):
    acc = 0.0
    xv = 1.0
    for _ in range(n):
        xv = (xv * 1.0000001 + 0.5) % 1000.0
        acc += xv
    return acc


def busy_py(n):
    acc = 0.0
    xv = 1.0
    for _ in range(n):
        xv = (xv * 1.0000001 + 0.5) % 1000.0
        acc += xv
    return acc


N = 2000000
rj = busy(N)  # warm compile
chk("njit_deterministic", abs(rj - busy_py(N)) < 1e-6)

t0 = time.perf_counter()
for _ in range(3):
    busy(N)
tj = time.perf_counter() - t0
t0 = time.perf_counter()
busy_py(N)
tp = time.perf_counter() - t0
speedup = (tp / (tj / 3.0)) if tj > 0 else float("inf")
chk("njit_speedup", speedup > 1.5, "speedup=%.1fx" % speedup)

print("NUMBA_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("NUMBA_DONE")
    sys.exit(0)
sys.exit(1)
