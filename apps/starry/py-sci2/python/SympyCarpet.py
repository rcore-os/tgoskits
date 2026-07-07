#!/usr/bin/env python3
# SympyCarpet.py - deep exact symbolic-algebra carpet for SymPy on musl-native CPython.
#
# Every assertion is an EXACT symbolic identity, exact rational / integer arithmetic, a closed-
# form transform, or a fixed-prefix high-precision evaluation - all independent of library
# version and of float formatting. Covers simplify / trig, expand / factor / apart / together /
# cancel, solve (poly / system / complex / nonlinear), calculus (diff / partial / integrate /
# limit / series), Rational arithmetic, Matrix (det / inv / mul / eigenvals / eigenvects / rref /
# nullspace / rank / LU), summation & product closed forms, dsolve (1st / 2nd order ODE), number
# theory (isprime / primerange / factorint / gcd / lcm / nextprime / totient / prime), sets &
# logic, nsimplify, lambdify (numeric bridge), roots-with-multiplicity and Poly operations.
#
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
from sympy import (Rational, Matrix, symbols, simplify, trigsimp, expand, factor, apart,
                   together, cancel, solve, diff, integrate, limit, series, summation, product,
                   sin, cos, tan, exp, sqrt, pi, oo, E, GoldenRatio, Symbol, Function, Eq, dsolve,
                   isprime, primerange, factorint, gcd, lcm, nextprime, prevprime, totient, prime,
                   Interval, FiniteSet, Union, satisfiable, roots, Poly, lambdify, nsimplify, I)

chk("version", int(sympy.__version__.split(".")[0]) >= 1, "sympy=%s" % sympy.__version__)

x, y, n = symbols("x y n")

# ---- simplify / trig ----
chk("pythagorean", simplify(sin(x) ** 2 + cos(x) ** 2) == 1)
chk("double_angle", simplify(sin(2 * x) - 2 * sin(x) * cos(x)) == 0)
chk("trigsimp_tan", trigsimp(sin(x) / cos(x)) == tan(x))

# ---- expand / factor / apart / together / cancel ----
chk("expand_square", expand((x + 1) ** 2) == x ** 2 + 2 * x + 1)
chk("expand_binomial", expand((x + y) ** 3) == x ** 3 + 3 * x ** 2 * y + 3 * x * y ** 2 + y ** 3)
chk("factor_diff_squares", factor(x ** 2 - 1) == (x - 1) * (x + 1))
chk("factor_quadratic", factor(x ** 2 - 5 * x + 6) == (x - 2) * (x - 3))
chk("apart", simplify(apart(1 / (x * (x + 1))) - (1 / x - 1 / (x + 1))) == 0)
chk("together", simplify(together(1 / x + 1 / (x + 1)) - (2 * x + 1) / (x * (x + 1))) == 0)
chk("cancel", cancel((x ** 2 - 1) / (x - 1)) == x + 1)

# ---- solve (exact roots / systems / complex) ----
chk("solve_quadratic", set(solve(x ** 2 - 5 * x + 6, x)) == {2, 3})
chk("solve_linear_system", solve([x + y - 3, x - y - 1], [x, y]) == {x: 2, y: 1})
chk("solve_complex", set(solve(x ** 2 + 1, x)) == {I, -I})
chk("solve_nonlinear", set(solve(x ** 3 - x, x)) == {-1, 0, 1})

# ---- calculus: diff / partial / integrate / limit / series ----
chk("diff_power", diff(x ** 3, x) == 3 * x ** 2)
chk("diff_product", diff(x * sin(x), x) == sin(x) + x * cos(x))
chk("diff_partial", diff(x ** 2 * y, x) == 2 * x * y)
chk("diff_higher", diff(x ** 4, x, 2) == 12 * x ** 2)
chk("integrate_power", integrate(2 * x, x) == x ** 2)
chk("integrate_definite", integrate(x ** 2, (x, 0, 3)) == 9)
chk("integrate_gaussian", integrate(exp(-x ** 2), (x, -oo, oo)) == sqrt(pi))
chk("limit_sinc", limit(sin(x) / x, x, 0) == 1)
chk("limit_e", limit((1 + 1 / x) ** x, x, oo) == E)
chk("series_exp",
    series(exp(x), x, 0, 4).removeO() == 1 + x + x ** 2 / 2 + x ** 3 / 6)
chk("series_sin",
    series(sin(x), x, 0, 6).removeO() == x - x ** 3 / 6 + x ** 5 / 120)

# ---- exact rational arithmetic ----
chk("rational_add", Rational(1, 3) + Rational(1, 6) == Rational(1, 2))
chk("rational_mul", Rational(2, 3) * Rational(3, 4) == Rational(1, 2))
chk("rational_no_float", Rational(1, 7) * 7 == 1)

# ---- Matrix: det / inv / mul / eigenvals / eigenvects / rref / nullspace / rank / LU ----
Mx = Matrix([[1, 2], [3, 4]])
chk("matrix_det", Mx.det() == -2)
chk("matrix_inv", Mx.inv() == Matrix([[Rational(-2), Rational(1)], [Rational(3, 2), Rational(-1, 2)]]))
chk("matrix_mul", (Mx * Mx) == Matrix([[7, 10], [15, 22]]))
chk("matrix_eigenvals", Matrix([[2, 0], [0, 3]]).eigenvals() == {2: 1, 3: 1})
_ev = Matrix([[2, 0], [0, 3]]).eigenvects()
chk("matrix_eigenvects", {val for (val, mult, vecs) in _ev} == {2, 3})
Rank = Matrix([[1, 2, 3], [2, 4, 6], [1, 0, 1]])
rref_mat, pivots = Rank.rref()
chk("matrix_rref_pivots", pivots == (0, 1))
chk("matrix_rank", Rank.rank() == 2)
chk("matrix_nullspace_dim", len(Rank.nullspace()) == 1)
# This matrix needs no row pivoting (leading entry non-zero), so L*U reconstructs it directly.
L_, U_, perm = Mx.LUdecomposition()
chk("matrix_lu", perm == [] and L_ * U_ == Mx and L_.is_lower and U_.is_upper)

# ---- summation & product closed forms ----
k = Symbol("k")
chk("sum_arith", summation(k, (k, 1, 100)) == 5050)
chk("sum_squares", summation(k ** 2, (k, 1, 10)) == 385)
chk("sum_symbolic", simplify(summation(k, (k, 1, n)) - n * (n + 1) / 2) == 0)
chk("product_factorial", product(k, (k, 1, 5)) == 120)

# ---- dsolve: 1st and 2nd order ODEs (verified by back-substitution) ----
f = Function("f")
sol1 = dsolve(Eq(f(x).diff(x), f(x)), f(x))
chk("dsolve_first_order", simplify(sol1.rhs.diff(x) - sol1.rhs) == 0)
sol2 = dsolve(Eq(f(x).diff(x, 2) + f(x), 0), f(x))
chk("dsolve_second_order", simplify(sol2.rhs.diff(x, 2) + sol2.rhs) == 0)

# ---- number theory ----
chk("isprime", isprime(97) and not isprime(91))                      # 91 = 7 * 13
chk("primerange", list(primerange(10, 20)) == [11, 13, 17, 19])
chk("factorint", factorint(360) == {2: 3, 3: 2, 5: 1})               # 360 = 2^3 * 3^2 * 5
chk("gcd_int", gcd(12, 18) == 6)
chk("gcd_poly", gcd(x ** 2 - 1, x - 1) == x - 1)
chk("lcm_int", lcm(4, 6) == 12)
chk("nextprevprime", nextprime(10) == 11 and prevprime(10) == 7)
chk("totient", totient(10) == 4)                                     # coprime {1,3,7,9}
chk("prime_nth", prime(5) == 11)                                     # 2,3,5,7,11

# ---- sets & logic ----
chk("interval_intersect", Interval(0, 2).intersect(Interval(1, 3)) == Interval(1, 2))
chk("finiteset_intersect", FiniteSet(1, 2, 3).intersect(FiniteSet(2, 3, 4)) == FiniteSet(2, 3))
chk("union", Union(Interval(0, 1), Interval(2, 3)).measure == 2)
a, b = symbols("a b")
chk("logic_unsat", satisfiable(a & ~a) is False)
chk("logic_sat", satisfiable(a | b) != False)

# ---- roots with multiplicity / Poly ----
chk("roots_multiplicity", roots(x ** 2 - 2 * x + 1, x) == {1: 2})
chk("poly_coeffs", Poly(x ** 2 - 1, x).all_coeffs() == [1, 0, -1])
chk("poly_degree", Poly(x ** 3 + x, x).degree() == 3)

# ---- nsimplify / lambdify ----
chk("nsimplify_half", nsimplify(0.5) == Rational(1, 2))
chk("nsimplify_quarter", nsimplify(0.25) == Rational(1, 4))
g = lambdify(x, x ** 2 + 1, "math")
chk("lambdify", g(3) == 10 and g(0) == 1)

# ---- high-precision evaluation: fixed digit prefixes (version-stable) ----
chk("pi_digits", str(pi.evalf(45)).startswith("3.14159265358979323846264338327950288419716"))
chk("e_digits", str(E.evalf(30)).startswith("2.71828182845904523536028747135"))
chk("sqrt2_digits", str(sqrt(2).evalf(20)).startswith("1.4142135623730950488"))
chk("golden_digits", str(GoldenRatio.evalf(20)).startswith("1.6180339887498948482"))

print("SYMPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SYMPY_DONE")
    sys.exit(0)
sys.exit(1)
