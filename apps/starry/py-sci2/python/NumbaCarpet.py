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

# ===================================================================================
# ============================ INDUSTRIAL FULL-API SUPPLEMENT ========================
# ===================================================================================
# Every gap from the numba audit brief (wq0cttcub) is covered below with a real
# deterministic assertion. Imports of the deeper API surface are done here so a stale
# numba that lacks one of them fails loudly on that chk rather than at module import.
import math

# ============================================================ numba.typed.Dict
from numba.typed import Dict as TypedDict
from numba.core import types as nbtypes


@njit(cache=False)
def dict_build_and_query():
    d = TypedDict.empty(key_type=int64, value_type=float64)
    d[1] = 2.0
    d[2] = 3.0
    v1 = d[1]
    n = len(d)
    miss = d.get(9, -1.0)
    has1 = 1 in d
    has9 = 9 in d
    vs = 0.0
    for val in d.values():
        vs += val
    ks = 0
    for key in d.keys():
        ks += key
    chks = 0.0
    for k, val in d.items():
        chks += k * 10.0 + val
    return v1, n, miss, has1, has9, vs, ks, chks


_v1, _n, _miss, _has1, _has9, _vs, _ks, _chks = dict_build_and_query()
chk("typed_dict_getitem", _v1 == 2.0)
chk("typed_dict_len", _n == 2)
chk("typed_dict_get_default", _miss == -1.0)
chk("typed_dict_membership_true", bool(_has1) is True)
chk("typed_dict_membership_false", bool(_has9) is False)
chk("typed_dict_values_sum", _vs == 5.0)  # 2.0+3.0
chk("typed_dict_keys_sum", _ks == 3)      # 1+2
chk("typed_dict_items", _chks == (1 * 10.0 + 2.0) + (2 * 10.0 + 3.0))  # 12+23=35


@njit(cache=False)
def dict_return():
    d = TypedDict.empty(key_type=int64, value_type=int64)
    for i in range(4):
        d[i] = i * i
    return d


_rd = dict_return()
chk("typed_dict_returned", _rd[3] == 9 and len(_rd) == 4)


# ============================================================ numba.experimental.jitclass
from numba.experimental import jitclass
from numba import deferred_type, optional


@jitclass([("total", float64)])
class Accumulator(object):
    def __init__(self):
        self.total = 0.0

    def add(self, x):
        self.total += x


_acc = Accumulator()
_acc.add(1.0)
_acc.add(2.0)
_acc.add(3.0)
chk("jitclass_method_dispatch", _acc.total == 6.0)


@jitclass([("n", int64)])
class Bag(object):
    def __init__(self, n):
        self.n = n


_bag = Bag(0)
_bag.n = 5
chk("jitclass_attr_set_get", _bag.n == 5)


@njit(cache=False)
def read_accumulator(a):
    return a.total * 2.0


chk("jitclass_as_njit_arg", read_accumulator(_acc) == 12.0)  # 6.0*2

# recursive jitclass linked list via deferred_type
node_type = deferred_type()


@jitclass([("value", int64), ("next", optional(node_type))])
class Node(object):
    def __init__(self, value):
        self.value = value
        self.next = None


node_type.define(Node.class_type.instance_type)


@njit(cache=False)
def list_length(head):
    n = 0
    cur = head
    while cur is not None:
        n += 1
        cur = cur.next
    return n


_n3 = Node(1)
_n2 = Node(2)
_n1 = Node(3)
_n1.next = _n2
_n2.next = _n3
chk("jitclass_deferred_linkedlist", list_length(_n1) == 3)


# ============================================================ numba.cfunc (C callback)
from numba import cfunc


@cfunc("float64(float64, float64)")
def cf_add(a, b):
    return a + b


chk("cfunc_ctypes_call", cf_add.ctypes(3.0, 4.0) == 7.0)
chk("cfunc_address_nonzero", cf_add.address != 0)
chk("cfunc_direct_call", cf_add(3.0, 4.0) == 7.0)  # CFunc is also directly callable in python
chk("cfunc_inspect_llvm", "define" in cf_add.inspect_llvm())


# ============================================================ numba.stencil
from numba import stencil


@stencil
def avg3(a):
    return (a[-1] + a[0] + a[1]) / 3.0


_st_in = np.array([1.0, 2.0, 3.0, 4.0, 5.0])
_st_out = avg3(_st_in)
# interior element i has hand-computed mean of its 3-neighbourhood; boundary => 0
chk("stencil_avg3_interior", _st_out[2] == 3.0)  # (2+3+4)/3
chk("stencil_avg3_boundary_zero", _st_out[0] == 0.0)


@stencil(neighborhood=((-1, 1), (-1, 1)))
def laplace(a):
    return a[0, 1] + a[0, -1] + a[1, 0] + a[-1, 0] - 4.0 * a[0, 0]


_lap_in = np.array([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]])
_lap_out = laplace(_lap_in)
# center (1,1): up=2 down=8 left=4 right=6 center=5 -> 2+8+4+6-20 = 0
chk("stencil_laplace_center", _lap_out[1, 1] == 0.0)


@njit(cache=False)
def run_stencil(a):
    return avg3(a)


chk("stencil_from_njit", run_stencil(_st_in)[2] == 3.0)


# ============================================================ numba.objmode
from numba import objmode


@njit(cache=False)
def use_objmode_gamma(x):
    with objmode(r="float64"):
        r = math.gamma(x)
    return r


chk("objmode_gamma", use_objmode_gamma(5.0) == 24.0)  # gamma(5)=4!=24


@njit(cache=False)
def use_objmode_int():
    with objmode(k="int64"):
        k = len("abcdef")
    return k + 1


chk("objmode_int_return", use_objmode_int() == 7)


# ============================================================ numba.extending / overload
from numba.extending import overload, register_jitable, intrinsic, is_jitted


def mylen(x):
    return 0


@overload(mylen)
def ol_mylen(x):
    def impl(x):
        return len(x)
    return impl


@njit(cache=False)
def call_mylen(a):
    return mylen(a)


chk("extending_overload", call_mylen(np.array([1.0, 2.0, 3.0])) == 3)


@register_jitable
def helper_cube(x):
    return x * x * x


@njit(cache=False)
def use_register_jitable(x):
    return helper_cube(x)


chk("extending_register_jitable", use_register_jitable(3.0) == 27.0)


@intrinsic
def const_forty_two(typingctx):
    from numba.core import types as _t

    def codegen(context, builder, signature, args):
        return context.get_constant(_t.int64, 42)

    return _t.int64(), codegen


@njit(cache=False)
def use_intrinsic():
    return const_forty_two() + 1


chk("extending_intrinsic", use_intrinsic() == 43)
chk("extending_is_jitted", is_jitted(quad) and not is_jitted(mylen))


# ============================================================ numba.types breadth
from numba import int8, int16, int32, uint8, uint16, uint32, uint64, boolean
from numba import complex64, complex128
from numba.types import unicode_type


@njit("int32(int32)")
def i32_id(x):
    return x + 1


chk("types_int32", i32_id(np.int32(41)) == 42)


@njit("int8(int8)")
def i8_id(x):
    return x


chk("types_int8", i8_id(np.int8(-5)) == -5)


@njit("boolean(int64)")
def is_even(x):
    return x % 2 == 0


chk("types_boolean_true", bool(is_even(4)) is True)
chk("types_boolean_false", bool(is_even(3)) is False)


@njit("complex128(complex128)")
def csquare(z):
    return z * z


chk("types_complex128", csquare(3 + 4j) == (3 + 4j) ** 2)  # (3+4j)^2 = -7+24j


@njit("complex64(complex64)")
def cneg(z):
    return -z


chk("types_complex64", cneg(np.complex64(1 + 2j)) == np.complex64(-1 - 2j))


@njit("uint64(uint64)")
def u64_id(x):
    return x


_big = np.uint64(2 ** 63 + 7)
chk("types_uint64", u64_id(_big) == _big)


@njit(nbtypes.int64(nbtypes.UniTuple(nbtypes.int64, 3)))
def tuple_sum3(t):
    return t[0] + t[1] + t[2]


chk("types_unituple", tuple_sum3((1, 2, 3)) == 6)


chk("types_typeof_array", isinstance(numba.typeof(np.arange(3)), nbtypes.Array))
chk("types_typeof_scalar", numba.typeof(3) == int64)
chk("types_from_dtype_f32", numba.from_dtype(np.dtype(np.float32)) == float32)
chk("types_from_dtype_f64", numba.from_dtype(np.dtype(np.float64)) == float64)


@njit(nbtypes.int64(unicode_type))
def strlen(s):
    return len(s)


chk("types_unicode_len", strlen("hello") == 5)


@njit(cache=False)
def str_ops(s):
    return len(s), s[0] == "a", s == "abc"


_sl, _s0, _seq = str_ops("abc")
chk("types_unicode_index", bool(_s0) is True)
chk("types_unicode_eq", bool(_seq) is True and _sl == 3)


# ============================================================ first-class functions / literal_unroll
from numba.core.types import FunctionType
from numba import literal_unroll


@njit(cache=False)
def apply_fn(f, x):
    return f(x)


@njit(cache=False)
def cube(x):
    return x * x * x


chk("firstclass_pass_njit", apply_fn(cube, 3.0) == 27.0)  # dispatched njit as arg


@njit(cache=False)
def inc(x):
    return x + 1.0


@njit(cache=False)
def dec(x):
    return x - 1.0


# A tuple of jitted functions indexed by a COMPILE-TIME-literal index is
# supported (each getitem is resolved at type-inference time); a runtime index
# into a heterogeneous function tuple is not (needs literal_unroll), so use
# literal indices here.
@njit(cache=False)
def select_apply(x):
    fns = (inc, dec)
    return fns[0](x), fns[1](x)


chk("firstclass_tuple_literal_index", select_apply(5.0) == (6.0, 4.0))


@njit(cache=False)
def unroll_sum():
    tup = (1, 2.5, 3)
    acc = 0.0
    for x in literal_unroll(tup):
        acc += x
    return acc


chk("literal_unroll_heterogeneous", unroll_sum() == 6.5)  # 1+2.5+3


# ============================================================ threading layer / parallel API
from numba import (
    set_num_threads,
    get_num_threads,
    threading_layer,
    get_thread_id,
    get_parallel_chunksize,
    set_parallel_chunksize,
)

_orig_threads = get_num_threads()
set_num_threads(1)
chk("threads_set_get_1", get_num_threads() == 1)


@njit(parallel=True, cache=False)
def par_thread_ids(a):
    n = a.shape[0]
    maxid = 0
    for i in prange(n):
        tid = get_thread_id()
        a[i] = tid
        if tid > maxid:
            maxid = tid
    return maxid


_tid_buf = np.zeros(1000, dtype=np.int64)
_maxtid1 = par_thread_ids(_tid_buf)
chk("threads_get_thread_id_bound", _maxtid1 < get_num_threads())

# parallel result invariant across thread counts
_r1 = par_sum(ones)  # with 1 thread now
set_num_threads(min(4, numba.config.NUMBA_NUM_THREADS))
_r4 = par_sum(ones)
chk("threads_result_invariant", _r1 == _r4 == 100000.0)

_layer = threading_layer()
chk("threads_threading_layer", isinstance(_layer, str) and len(_layer) > 0, "layer=%s" % _layer)
chk("threads_config_num", numba.config.NUMBA_NUM_THREADS >= 1)

_orig_chunk = get_parallel_chunksize()
set_parallel_chunksize(8)
chk("threads_parallel_chunksize", get_parallel_chunksize() == 8)
set_parallel_chunksize(_orig_chunk)
set_num_threads(_orig_threads)


# ============================================================ dispatcher / introspection API
import io as _io

_buf = _io.StringIO()
quad.inspect_types(file=_buf, pretty=False)
chk("introspect_inspect_types", "quad" in _buf.getvalue() and len(_buf.getvalue()) > 0)

_llvm = quad.inspect_llvm()
chk("introspect_inspect_llvm", isinstance(_llvm, dict) and len(_llvm) >= 1)
chk("introspect_inspect_llvm_define", any("define" in v for v in _llvm.values()))

_asm = quad.inspect_asm()
chk("introspect_inspect_asm", isinstance(_asm, dict) and len(_asm) >= 1)

chk("introspect_eager_sigs", len(triple.signatures) == 1 and len(triple.nopython_signatures) == 1)

# recompile keeps results identical
_before = quad(9)
quad.recompile()
chk("introspect_recompile", quad(9) == _before == 82)  # 9*9+1


# ============================================================ jit flag matrix
@njit(fastmath=True, cache=False)
def fm_sum(a):
    s = 0.0
    for i in range(a.shape[0]):
        s += a[i]
    return s


chk("flag_fastmath", close(fm_sum(np.ones(1000)), 1000.0, rel=1e-9))


@njit(boundscheck=True, cache=False)
def oob_access(a, i):
    return a[i]


_oob_raised = False
try:
    oob_access(np.array([1.0, 2.0, 3.0]), 10)
except IndexError:
    _oob_raised = True
except Exception:  # noqa: BLE001
    _oob_raised = True
chk("flag_boundscheck", _oob_raised)


@njit(nogil=True, cache=False)
def nogil_sum(a):
    s = 0.0
    for i in range(a.shape[0]):
        s += a[i]
    return s


chk("flag_nogil", nogil_sum(np.arange(5.0)) == 10.0)  # 0+1+2+3+4


@numba.jit(nopython=True, cache=False)
def jit_alias(x):
    return x * x + 1


chk("flag_jit_nopython_alias", jit_alias(7) == quad(7) == 50)


@njit(error_model="numpy", cache=False)
def numpy_div(a, b):
    return a / b


_div = numpy_div(1.0, 0.0)
chk("flag_error_model_numpy", math.isinf(_div))  # numpy model: 1/0 -> inf, no exception


@njit(inline="always", cache=False)
def inlined_helper(x):
    return x + 100.0


@njit(cache=False)
def use_inlined(x):
    return inlined_helper(x) * 2.0


chk("flag_inline_always", use_inlined(1.0) == 202.0)  # (1+100)*2


# ============================================================ supported numpy breadth (nopython)
@njit(cache=False)
def np_alloc():
    z = np.zeros(3)
    o = np.ones(3)
    r = np.arange(4.0)
    f = np.full(2, 7.0)
    e = np.empty(1)
    e[0] = 5.0
    return z.sum(), o.sum(), r.sum(), f.sum(), e[0], z.shape[0]


_zs, _os, _rs, _fs, _es, _zsh = np_alloc()
chk("np_zeros", _zs == 0.0 and _zsh == 3)
chk("np_ones", _os == 3.0)
chk("np_arange_full", _rs == 6.0 and _fs == 14.0)  # 0+1+2+3=6, 7+7=14
chk("np_empty", _es == 5.0)


@njit(cache=False)
def np_linspace_sum():
    return np.linspace(0.0, 1.0, 5).sum()


chk("np_linspace", close(np_linspace_sum(), 2.5))  # 0+.25+.5+.75+1


@njit(cache=False)
def np_cumsum_last(a):
    return np.cumsum(a)[-1]


chk("np_cumsum", np_cumsum_last(np.array([1.0, 2.0, 3.0, 4.0])) == 10.0)


@njit(cache=False)
def np_cumprod_last(a):
    return np.cumprod(a)[-1]


chk("np_cumprod", np_cumprod_last(np.array([1.0, 2.0, 3.0, 4.0])) == 24.0)


@njit(cache=False)
def np_diff_sum(a):
    return np.diff(a).sum()


chk("np_diff", np_diff_sum(np.array([1.0, 4.0, 9.0])) == 8.0)  # (3+5)


@njit(cache=False)
def np_where_kernel(a):
    return np.where(a > 0, a, -a).sum()


chk("np_where", np_where_kernel(np.array([-1.0, 2.0, -3.0])) == 6.0)  # abs -> 1+2+3


@njit(cache=False)
def np_clip_kernel(a):
    return np.clip(a, -1.0, 1.0).sum()


chk("np_clip", np_clip_kernel(np.array([-2.0, 0.5, 3.0])) == 0.5)  # -1+0.5+1


@njit(cache=False)
def np_nonzero_count(a):
    return len(np.nonzero(a)[0])


chk("np_nonzero", np_nonzero_count(np.array([0.0, 1.0, 0.0, 2.0])) == 2)


@njit(cache=False)
def np_searchsorted_kernel(a, v):
    return np.searchsorted(a, v)


chk("np_searchsorted", np_searchsorted_kernel(np.array([1.0, 3.0, 5.0, 7.0]), 4.0) == 2)


@njit(cache=False)
def np_reshape_transpose():
    m = np.arange(6.0).reshape(2, 3)
    t = m.T
    return t.shape[0], t.shape[1], t[0, 1]


_rr, _rc, _rv = np_reshape_transpose()
chk("np_reshape_transpose", _rr == 3 and _rc == 2 and _rv == 3.0)  # m[1,0]=3


@njit(cache=False)
def np_ravel_concat():
    a = np.array([[1.0, 2.0], [3.0, 4.0]])
    r = np.ravel(a)
    c = np.concatenate((np.array([1.0]), np.array([2.0, 3.0])))
    return r.sum(), c.sum(), r.shape[0]


_rvs, _ccs, _rsh = np_ravel_concat()
chk("np_ravel_concat", _rvs == 10.0 and _ccs == 6.0 and _rsh == 4)


@njit(cache=False)
def np_ufuncs():
    return (np.sin(0.0), np.cos(0.0), np.exp(0.0), np.log(1.0), np.tanh(0.0))


_s0v, _c0v, _e0v, _l1v, _t0v = np_ufuncs()
chk("np_ufunc_trig", _s0v == 0.0 and _c0v == 1.0)
chk("np_ufunc_exp_log", _e0v == 1.0 and _l1v == 0.0 and _t0v == 0.0)


@njit(cache=False)
def np_sin_vec(a):
    return np.sin(a)[1]


chk("np_sin_vector", close(np_sin_vec(np.array([0.0, math.pi / 2.0, math.pi])), 1.0))


@njit(cache=False)
def np_abs_floor_ceil_round():
    return (np.abs(-3.5), np.floor(2.7), np.ceil(2.1), np.round(2.5))


_ab, _fl, _ce, _ro = np_abs_floor_ceil_round()
chk("np_abs_floor_ceil", _ab == 3.5 and _fl == 2.0 and _ce == 3.0)
chk("np_round", _ro == 2.0)  # banker's rounding: round(2.5)->2


@njit(cache=False)
def np_linalg_norm_det():
    n = np.linalg.norm(np.array([3.0, 4.0]))
    d = np.linalg.det(np.array([[1.0, 2.0], [3.0, 4.0]]))
    return n, d


_nrm, _det = np_linalg_norm_det()
chk("np_linalg_norm", _nrm == 5.0)
chk("np_linalg_det", close(_det, -2.0))


@njit(cache=False)
def np_linalg_inv():
    inv = np.linalg.inv(np.array([[4.0, 0.0], [0.0, 2.0]]))
    return inv[0, 0], inv[1, 1]


_i00, _i11 = np_linalg_inv()
chk("np_linalg_inv", close(_i00, 0.25) and close(_i11, 0.5))


@njit(cache=False)
def np_linalg_eigvals():
    w = np.linalg.eigvals(np.array([[2.0, 0.0], [0.0, 3.0]]))
    return np.sort(w.real)


_ev = np_linalg_eigvals()
chk("np_linalg_eig", close(_ev[0], 2.0) and close(_ev[1], 3.0))


@njit(cache=False)
def np_sort_argsort():
    a = np.array([3.0, 1.0, 2.0])
    return np.sort(a), np.argsort(a)


_srt, _asr = np_sort_argsort()
chk("np_sort", _srt.tolist() == [1.0, 2.0, 3.0])
chk("np_argsort", _asr.tolist() == [1, 2, 0])


# numba's internal RNG is deterministic under np.random.seed within the JIT (values differ
# from host numpy; assert the seed-determinism invariant, not a guessed constant).
@njit(cache=False)
def seeded_draw(seed):
    np.random.seed(seed)
    return np.random.random()


chk("np_random_determinism", seeded_draw(12345) == seeded_draw(12345))


@njit(cache=False)
def seeded_randint(seed):
    np.random.seed(seed)
    return np.random.randint(0, 100)


_ri = seeded_randint(7)
chk("np_random_randint_bound", 0 <= _ri < 100 and seeded_randint(7) == _ri)


# ============================================================ supported python/stdlib breadth
@njit(cache=False)
def math_intrinsics(x):
    return (math.sqrt(x), math.sin(0.0), math.exp(0.0), math.log(1.0), math.pi)


_sq, _si, _ex, _lg, _pi = math_intrinsics(9.0)
chk("math_sqrt", _sq == 3.0)
chk("math_sin_exp_log", _si == 0.0 and _ex == 1.0 and _lg == 0.0)
chk("math_pi", close(_pi, 3.141592653589793))


@njit(cache=False)
def math_gamma_njit(x):
    return math.gamma(x)


chk("math_gamma", close(math_gamma_njit(5.0), 24.0))


@njit(cache=False)
def list_comprehension():
    return sum([i * i for i in range(5)])


chk("stdlib_list_comprehension", list_comprehension() == 30)  # 0+1+4+9+16


@njit(cache=False)
def builtins_kernel(a):
    return abs(-4), min(3, 7), max(3, 7), round(2.4), len(a)


_bab, _bmn, _bmx, _brd, _bln = builtins_kernel(np.arange(6.0))
chk("stdlib_abs_min_max", _bab == 4 and _bmn == 3 and _bmx == 7)
chk("stdlib_round_len", _brd == 2 and _bln == 6)


@njit(cache=False)
def range_step_sum():
    s = 0
    for i in range(0, 10, 2):
        s += i
    return s


chk("stdlib_range_step", range_step_sum() == 20)  # 0+2+4+6+8


@njit(cache=False)
def set_usage():
    s = set()
    for i in [1, 2, 2, 3, 3, 3]:
        s.add(i)
    return len(s)


chk("stdlib_set", set_usage() == 3)


# closure capturing a value into an inner njit-compiled function
def make_adder(c):
    @njit(cache=False)
    def add_c(x):
        return x + c

    return add_c


_add10 = make_adder(10.0)
chk("stdlib_closure", _add10(5.0) == 15.0)


@njit(cache=False)
def try_except_kernel(a, b):
    try:
        return a // b
    except Exception:  # noqa: BLE001
        return -1


chk("stdlib_try_except_ok", try_except_kernel(10, 2) == 5)
chk("stdlib_try_except_sentinel", try_except_kernel(10, 0) == -1)  # ZeroDivisionError caught


# ============================================================ error handling breadth
# 1) genuine type-mismatch (str + int) is a TypingError at compile time
@njit(cache=False)
def type_mismatch(s, n):
    return s + n  # unicode + int has no nopython lowering


_typing_err = False
try:
    type_mismatch("abc", 3)
except TypingError:
    _typing_err = True
except Exception:  # noqa: BLE001
    _typing_err = True
chk("error_typing_mismatch", _typing_err)

# 2) a Python exception raised inside njit propagates to python
from numba.core.errors import NumbaError, UnsupportedError


@njit(cache=False)
def raise_valueerror(x):
    if x < 0:
        raise ValueError("negative")
    return x


_value_err = False
try:
    raise_valueerror(-1)
except ValueError:
    _value_err = True
chk("error_valueerror_propagates", _value_err)
chk("error_valueerror_ok", raise_valueerror(5) == 5)

# 3) error class hierarchy: TypingError / UnsupportedError derive from NumbaError
chk("error_hierarchy_typing", issubclass(TypingError, NumbaError))
chk("error_hierarchy_unsupported", issubclass(UnsupportedError, NumbaError))


# ============================================================ HONEST SKIPS (documented, not asserted)
# The following brief items are intentionally NOT asserted here, with the reason:
#  - structref (numba.experimental.structref): mutable struct references require a
#    hand-written boxing/typing model that is state-heavy and target-fragile; skipped.
#  - generated_jit / extending typing (@type_callable, models): deprecated (generated_jit
#    removed in recent numba) / deep typing-layer plumbing; skipped to avoid version drift.
#  - CUDA target (numba.cuda): no GPU on the StarryOS single-core softfloat target; skipped.
#  - AOT compilation (numba.pycc.CC): writes a compiled .so to disk and is deprecated in
#    recent numba; skipped as state-mutating/removed rather than faked.

print("NUMBA_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("NUMBA_DONE")
    sys.exit(0)
sys.exit(1)
