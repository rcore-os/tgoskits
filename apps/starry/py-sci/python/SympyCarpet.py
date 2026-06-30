#!/usr/bin/env python3
# SympyCarpet.py — exact symbolic-algebra carpet for SymPy on musl-native CPython.
#
# Every assertion is an EXACT symbolic identity, exact rational arithmetic, or a fixed-prefix
# high-precision evaluation — all independent of library version and of float formatting.
# Self-contained ok/fail counters; prints SYMPY_RESULT then SYMPY_DONE only when fail == 0.
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


import sympy
from sympy import (Rational, Matrix, symbols, simplify, expand, factor, solve, diff,
                   integrate, limit, sin, cos, exp, sqrt, pi, oo, summation, Symbol)

chk("version", int(sympy.__version__.split(".")[0]) >= 1, "sympy=%s" % sympy.__version__)

x, y = symbols("x y")

# ---- simplify / trig identity ----
chk("pythagorean", simplify(sin(x) ** 2 + cos(x) ** 2) == 1)
chk("double_angle", simplify(sin(2 * x) - 2 * sin(x) * cos(x)) == 0)

# ---- expand / factor (exact polynomials) ----
chk("expand_square", expand((x + 1) ** 2) == x ** 2 + 2 * x + 1)
chk("expand_binomial", expand((x + y) ** 3) == x ** 3 + 3 * x ** 2 * y + 3 * x * y ** 2 + y ** 3)
chk("factor_diff_squares", factor(x ** 2 - 1) == (x - 1) * (x + 1))
chk("factor_quadratic", factor(x ** 2 - 5 * x + 6) == (x - 2) * (x - 3))

# ---- solve (exact roots) ----
chk("solve_quadratic", set(solve(x ** 2 - 5 * x + 6, x)) == {2, 3})
chk("solve_linear_system",
    solve([x + y - 3, x - y - 1], [x, y]) == {x: 2, y: 1})
roots = solve(x ** 2 + 1, x)
chk("solve_complex", set(roots) == {sympy.I, -sympy.I})

# ---- calculus: diff / integrate / limit (exact closed form) ----
chk("diff_power", diff(x ** 3, x) == 3 * x ** 2)
chk("diff_product", diff(x * sin(x), x) == sin(x) + x * cos(x))
chk("integrate_power", integrate(2 * x, x) == x ** 2)
chk("integrate_definite", integrate(x ** 2, (x, 0, 3)) == 9)
chk("integrate_gaussian", integrate(exp(-x ** 2), (x, -oo, oo)) == sqrt(pi))
chk("limit_sinc", limit(sin(x) / x, x, 0) == 1)
chk("limit_inf", limit((1 + 1 / x) ** x, x, oo) == sympy.E)

# ---- exact rational arithmetic ----
chk("rational_add", Rational(1, 3) + Rational(1, 6) == Rational(1, 2))
chk("rational_mul", Rational(2, 3) * Rational(3, 4) == Rational(1, 2))
chk("rational_no_float", Rational(1, 7) * 7 == 1)

# ---- Matrix: exact determinant / inverse (rationals) / eigenvalues ----
Mx = Matrix([[1, 2], [3, 4]])
chk("matrix_det", Mx.det() == -2)
chk("matrix_inv", Mx.inv() == Matrix([[Rational(-2), Rational(1)], [Rational(3, 2), Rational(-1, 2)]]))
chk("matrix_mul", (Mx * Mx) == Matrix([[7, 10], [15, 22]]))
chk("matrix_eigen", set(Matrix([[2, 0], [0, 3]]).eigenvals().keys()) == {2, 3})

# ---- summation (exact closed form) ----
k = Symbol("k")
chk("sum_arith", summation(k, (k, 1, 100)) == 5050)
chk("sum_squares", summation(k ** 2, (k, 1, 10)) == 385)

# ---- high-precision evaluation: pi to a fixed digit prefix (version-stable) ----
chk("pi_digits", str(pi.evalf(40)).startswith("3.141592653589793238462643383279502884"),
    "pi40=%s" % str(pi.evalf(40)))
chk("sqrt2_digits", str(sqrt(2).evalf(20)).startswith("1.4142135623730950488"))

print("SYMPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SYMPY_DONE")
    sys.exit(0)
sys.exit(1)
