#!/usr/bin/env python3
# NumpyCarpet.py — exact-assertion carpet for NumPy on musl-native CPython.
#
# Every assertion is an EXACT integer, a closed-form linear-algebra identity, or a
# version-stable structural invariant — chosen so it holds identically on the host
# reference NumPy and on the (newer) on-target NumPy. Nothing depends on float repr,
# default-dtype width, or print formatting. Self-contained ok/fail counters; prints
# NUMPY_RESULT then NUMPY_DONE only when fail == 0.
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

# Lenient version floor (major), never an exact patch string.
chk("version", int(np.__version__.split(".")[0]) >= 1, "numpy=%s" % np.__version__)

# ---- PROVEN core (4-arch green): EXACT integer + closed-form linear algebra ----
A = np.arange(1, 10, dtype=np.int64).reshape(3, 3)
C = A.dot(np.eye(3, dtype=np.int64) * 2)  # matmul with 2*I
chk("matmul_sum", int(C.sum()) == 90)
chk("matmul_trace", int(np.trace(C)) == 30)
v = np.array([3, 1, 2, 5, 4], dtype=np.int64)
chk("sort", np.sort(v).tolist() == [1, 2, 3, 4, 5])
chk("cumsum", np.cumsum(v).tolist() == [3, 4, 6, 11, 15])
chk("dot", int(np.array([1, 2, 3]).dot(np.array([4, 5, 6]))) == 32)
chk("det_diag", round(float(np.linalg.det(np.diag([2.0, 3.0, 4.0])))) == 24)
xs = np.linalg.solve(np.array([[3.0, 0.0], [0.0, 5.0]]), np.array([9.0, 20.0]))
chk("solve", [round(float(z)) for z in xs] == [3, 4])
chk("fsum", float(np.array([0.5, 0.25, 0.125]).sum()) == 0.875)  # exact in binary

# ---- Broadcasting (row + column vector -> outer-sum matrix) ----
B = np.arange(3).reshape(3, 1) + np.arange(3).reshape(1, 3)
chk("broadcast", B.tolist() == [[0, 1, 2], [1, 2, 3], [2, 3, 4]])

# ---- Views / strides / reshape / transpose ----
a = np.arange(12, dtype=np.int64)
m = a.reshape(3, 4)
chk("reshape_index", int(m[1, 2]) == 6)
chk("transpose", m.T.tolist() == [[0, 4, 8], [1, 5, 9], [2, 6, 10], [3, 7, 11]])
chk("strided_view", a[::2].tolist() == [0, 2, 4, 6, 8, 10])
chk("ravel", m[:2, :2].ravel().tolist() == [0, 1, 4, 5])
# A slice is a view that shares memory with its base; mutate a fresh array to prove it.
fresh = np.arange(6, dtype=np.int64)
fview = fresh[::2]
fview[0] = 99
chk("view_aliases_base", int(fresh[0]) == 99)

# ---- dtype casting (truncation, bool, modular integer wrap) ----
# Array arithmetic wraps modularly in every NumPy version (defined C behaviour); avoid the
# scalar / out-of-bound-Python-int casts that NumPy 2.x turns into errors.
chk("cast_float_to_int", np.array([1.9, 2.1, -1.7]).astype(np.int64).tolist() == [1, 2, -1])
chk("cast_to_bool", np.array([0, 1, 2, 0]).astype(bool).tolist() == [False, True, True, False])
chk("uint8_wrap", (np.array([250, 200], dtype=np.uint8) + np.array([50, 100], dtype=np.uint8)).tolist() == [44, 44])
chk("int8_wrap", (np.array([127], dtype=np.int8) + np.array([1], dtype=np.int8)).tolist() == [-128])

# ---- Boolean masking / fancy indexing / where ----
g = np.arange(10)
chk("bool_mask", g[g % 2 == 0].tolist() == [0, 2, 4, 6, 8])
chk("fancy_index", g[[1, 3, 5, 7]].tolist() == [1, 3, 5, 7])
chk("where", np.where(np.array([True, False, True]), np.array([1, 2, 3]), np.array([4, 5, 6])).tolist() == [1, 5, 3])

# ---- Reductions over axes ----
M = np.arange(6, dtype=np.int64).reshape(2, 3)  # [[0,1,2],[3,4,5]]
chk("sum_axis0", M.sum(axis=0).tolist() == [3, 5, 7])
chk("sum_axis1", M.sum(axis=1).tolist() == [3, 12])
chk("prod_all", int(np.arange(1, 5).prod()) == 24)
chk("min_max", int(M.min()) == 0 and int(M.max()) == 5)
chk("argmin_argmax", int(M.argmin()) == 0 and int(M.argmax()) == 5)
chk("max_axis1", M.max(axis=1).tolist() == [2, 5])
chk("mean_axis0", M.mean(axis=0).tolist() == [1.5, 2.5, 3.5])  # exact in binary

# ---- Linear algebra (det / inv / eigvalsh / norm / matmul-operator) ----
chk("det_2x2", round(float(np.linalg.det(np.array([[1.0, 2.0], [3.0, 4.0]])))) == -2)
inv = np.linalg.inv(np.array([[2.0, 0.0], [0.0, 4.0]]))
chk("inv_diag", inv.tolist() == [[0.5, 0.0], [0.0, 0.25]])
ev = np.linalg.eigvalsh(np.diag([3.0, 1.0, 2.0]))  # symmetric -> sorted ascending
chk("eigvalsh", [round(float(z)) for z in ev] == [1, 2, 3])
chk("norm", float(np.linalg.norm(np.array([3.0, 4.0]))) == 5.0)
P = np.array([[1, 2], [3, 4]], dtype=np.int64)
Q = np.array([[5, 6], [7, 8]], dtype=np.int64)
chk("matmul_op", (P @ Q).tolist() == [[19, 22], [43, 50]])

# ---- FFT: DFT of [1,2,3,4] has exact integer bins (rfft -> [10, -2+2j, -2]) ----
sp = np.fft.rfft(np.array([1.0, 2.0, 3.0, 4.0]))
re = [round(float(z.real)) for z in sp]
im = [round(float(z.imag)) for z in sp]
chk("rfft_real", re == [10, -2, -2])
chk("rfft_imag", im == [0, 2, 0])

# ---- Random ----
# The PCG64 *bit-generator raw stream* is the algorithm's defining output and is frozen
# across versions (NEP 19 + frozen SeedSequence) -> exact uint64 golden is version-stable.
# (Generator.integers() is NOT stream-guaranteed across versions, so we assert only its
# reproducibility + range, not exact values.)
raw = np.random.PCG64(12345).random_raw(4).tolist()
chk("rng_raw_stream", raw == [4193609425186963869, 5843160025838961886,
                              14708796524633321433, 12474696839993944336], "raw=%s" % raw)
seq = np.random.Generator(np.random.PCG64(7)).integers(0, 1000, size=8)
seq2 = np.random.Generator(np.random.PCG64(7)).integers(0, 1000, size=8)
chk("rng_reproducible", seq.tolist() == seq2.tolist())
chk("rng_bounded", bool(((seq >= 0) & (seq < 1000)).all()) and len(seq) == 8)

# ---- Sorting helpers / searchsorted / concatenate / stack ----
chk("argsort", np.argsort(np.array([3, 1, 2])).tolist() == [1, 2, 0])
chk("searchsorted", int(np.searchsorted(np.array([1, 3, 5, 7]), 4)) == 2)
chk("concatenate", np.concatenate([np.array([1, 2]), np.array([3, 4])]).tolist() == [1, 2, 3, 4])
chk("stack", np.stack([np.array([1, 2]), np.array([3, 4])]).tolist() == [[1, 2], [3, 4]])
chk("unique", np.unique(np.array([3, 1, 2, 3, 1])).tolist() == [1, 2, 3])
chk("clip", np.clip(np.array([-5, 0, 5, 10]), 0, 7).tolist() == [0, 0, 5, 7])

print("NUMPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("NUMPY_DONE")
    sys.exit(0)
sys.exit(1)
