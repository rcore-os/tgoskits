#!/usr/bin/env python3
"""opencv_mat - cv2/numpy Mat arithmetic & structure vs element-exact closed form.

add/subtract/multiply/matmul/transpose on KNOWN small matrices, asserted element-exact vs a numpy golden;
Mat dtype/channels/ROI/reshape semantics. cv2 Mats ARE numpy arrays, so numpy is the closed form. No RNG.
"""
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_MAT")

A = np.array([[1., 2., 3.], [4., 5., 6.]])
B = np.array([[6., 5., 4.], [3., 2., 1.]])

# element-wise add via cv2.add == numpy A+B == all 7
S = cv2.add(A, B)
g.check(np.array_equal(S, A + B) and np.all(S == 7.0), "cv2.add != A+B / not all 7")

# element-wise subtract via cv2.subtract
D = cv2.subtract(A, B)
g.check(np.array_equal(D, np.array([[-5., -3., -1.], [1., 3., 5.]])), "cv2.subtract mismatch")

# element-wise multiply via cv2.multiply (Hadamard)
M = cv2.multiply(A, B)
g.check(np.array_equal(M, np.array([[6., 10., 12.], [12., 10., 6.]])), "cv2.multiply mismatch")

# scalar scale
g.check(np.array_equal(cv2.multiply(A, 2.0), 2.0 * A), "scalar multiply mismatch")

# matmul (cv2.gemm): A(2x3) * A^T(3x2) = [[14,32],[32,77]]
G = cv2.gemm(A, A.T, 1.0, None, 0.0)
g.check(np.array_equal(G, np.array([[14., 32.], [32., 77.]])), "cv2.gemm mismatch")
# and numpy matmul agrees
g.check(np.array_equal(G, A @ A.T), "gemm != numpy @")

# transpose
T = cv2.transpose(A)
g.check(T.shape == (3, 2) and np.array_equal(T, A.T), "transpose mismatch")

# determinant of known 2x2
K = np.array([[1., 2.], [3., 4.]])
g.check(abs(cv2.determinant(K) - (-2.0)) < 1e-9, "det != -2")

# inverse: K @ K^-1 == I
ok, Ki = cv2.invert(K)
g.check(ok != 0.0 and np.allclose(K @ Ki, np.eye(2), atol=1e-9), "K*K^-1 != I")

# dtype / channels: 3-channel 8U image
C8 = np.zeros((4, 5, 3), dtype=np.uint8)
g.check(C8.dtype == np.uint8, "dtype != uint8")
g.check(C8.shape == (4, 5, 3), "shape != (4,5,3)")
g.check(C8.ndim == 3 and C8.shape[2] == 3, "channels != 3")

# ROI is a view: mutate parent at mapped location only
P = np.zeros((6, 6), dtype=np.uint8)
P[2:4, 1:4] = 200  # y=2..4, x=1..4
expect = np.zeros((6, 6), dtype=np.uint8)
expect[2:4, 1:4] = 200
g.check(np.array_equal(P, expect), "ROI write mapping wrong")

# reshape row-major
lin = np.arange(12, dtype=np.int32).reshape(1, 12)
r34 = lin.reshape(3, 4)
g.check(r34.shape == (3, 4) and np.array_equal(r34, np.arange(12).reshape(3, 4)), "reshape order wrong")

# countNonZero / minMaxLoc
mm = np.array([[0, 5, 0], [9, 0, 3]], dtype=np.uint8)
g.check(cv2.countNonZero(mm) == 3, "countNonZero != 3")
mn, mx, _, _ = cv2.minMaxLoc(mm)
g.check(mn == 0.0 and mx == 9.0, "minMax != (0,9)")

raise SystemExit(g.finish())
