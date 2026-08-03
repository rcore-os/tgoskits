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

# ============================================================================
# INDUSTRIAL FULL-API SUPPLEMENT (per gap brief wq0cttcub)
# Every assertion below is an exact symbolic identity / documented closed form,
# verified against SymPy 1.14.0. Values are version-stable (exact rationals,
# structural equalities, or roundtrip/invariant checks).
# ============================================================================
from sympy import (Add, Mul, Pow, Integer, Float, collect, N, solveset, linsolve,
                   nonlinsolve, Sum, Product, log, Abs, gamma, beta, factorial,
                   binomial, fibonacci, bernoulli, legendre, besselj, atan, asin,
                   acos, cosh, sinh, tanh, floor, ceiling, re, im, conjugate, Min,
                   Max, radsimp, powsimp, logcombine, expand_log, expand_trig,
                   ratsimp, combsimp, Q, ask, refine, S, Intersection, Complement,
                   SymmetricDifference, ImageSet, Lambda, And, Or, Not, Xor, Implies,
                   Equivalent, simplify_logic, to_cnf, to_dnf, divisors, primefactors,
                   divisor_count, mobius, primepi, n_order, jacobi_symbol,
                   nthroot_mod, continued_fraction, Point, Line, Circle, Triangle,
                   Segment, latex, ccode, sstr, pretty, python, mathematica_code,
                   octave_code, Integral, factor_list, div, rem, resultant, groebner,
                   discriminant, eye, zeros, ones, diag, fourier_series, fps,
                   true, false)

# ---- core: subs / collect / Integer / Float / Add-Mul-Pow / free_symbols / coeff / N ----
chk("subs_single", (x ** 2 + 1).subs(x, 3) == 10)
chk("subs_multiple", (x + y).subs([(x, 1), (y, 2)]) == 3)
chk("collect_coeff", collect(x * y + x - 3 + 2 * x ** 2 - y * x ** 2, x).coeff(x, 2) == 2 - y)
chk("integer_exact", Integer(7) / Integer(2) == Rational(7, 2))
chk("float_close", abs(float(Float("0.1", 10)) - 0.1) < 1e-9)
chk("expr_coeff", (3 * x ** 2 + 2 * x).coeff(x, 2) == 3)
chk("free_symbols", (x + y).free_symbols == {x, y})
chk("Add_ctor", Add(x, y) == x + y)
chk("Mul_ctor", Mul(x, y) == x * y)
chk("Pow_ctor", Pow(x, 2) == x ** 2)
chk("N_pi", str(N(pi, 10)).startswith("3.14159265"))

# ---- solvers: solveset / linsolve / nonlinsolve / dsolve-with-ics ----
chk("solveset_real", solveset(x ** 2 - 1, x, domain=S.Reals) == FiniteSet(-1, 1))
chk("solveset_complex", solveset(x ** 2 - 1, x) == FiniteSet(-1, 1))
chk("linsolve", linsolve([x + y - 3, x - y - 1], [x, y]) == FiniteSet((2, 1)))
chk("nonlinsolve", (sqrt(2) / 2, sqrt(2) / 2) in nonlinsolve([x ** 2 + y ** 2 - 1, x - y], [x, y]))
_sol_ic = dsolve(Eq(f(x).diff(x), f(x)), f(x), ics={f(0): 1})
chk("dsolve_ics", _sol_ic.rhs == exp(x))

# ---- calculus: Sum / Product objects / directional limit / nested integrate ----
chk("Sum_doit_symbolic", simplify(Sum(k, (k, 1, n)).doit() - n * (n + 1) / 2) == 0)
chk("Sum_basel", Sum(1 / k ** 2, (k, 1, oo)).doit() == pi ** 2 / 6)
chk("Product_doit", Product(k, (k, 1, 5)).doit() == 120)
chk("limit_dir_plus", limit(1 / x, x, 0, "+") == oo)
chk("limit_dir_minus", limit(1 / x, x, 0, "-") == -oo)
chk("integrate_double", integrate(integrate(x * y, (x, 0, 1)), (y, 0, 1)) == Rational(1, 4))
chk("integrate_multiple", integrate(x * y, x, y) == x ** 2 * y ** 2 / 4)   # iterated over x then y
chk("diff_trig_chain", diff(sin(exp(x)), x) == exp(x) * cos(exp(x)))
chk("diff_exp_chain", diff(exp(sin(x)), x) == exp(sin(x)) * cos(x))

# ---- matrices: constructors / QR / charpoly / trace / T / norm / adjugate / cofactor / joins / symmetric ----
chk("eye3", eye(3) == Matrix([[1, 0, 0], [0, 1, 0], [0, 0, 1]]))
chk("zeros22", zeros(2, 2) == Matrix([[0, 0], [0, 0]]))
chk("ones22", ones(2, 2) == Matrix([[1, 1], [1, 1]]))
chk("diag123", diag(1, 2, 3) == Matrix([[1, 0, 0], [0, 2, 0], [0, 0, 3]]))
chk("charpoly", expand(Matrix([[2, 0], [0, 3]]).charpoly(x).as_expr()) == expand((x - 2) * (x - 3)))
_Q, _R = Mx.QRdecomposition()
chk("QR_reconstruct", simplify(_Q * _R - Mx) == zeros(2, 2))
chk("QR_orthonormal", simplify(_Q.T * _Q - eye(2)) == zeros(2, 2))
chk("matrix_trace", Mx.trace() == 5)
chk("matrix_transpose", Mx.T == Matrix([[1, 3], [2, 4]]))
chk("matrix_norm", Matrix([3, 4]).norm() == 5)
chk("matrix_adjugate", Mx.adjugate() == Matrix([[4, -2], [-3, 1]]))
chk("matrix_cofactor", Mx.cofactor(0, 0) == 4)
chk("matrix_row_join", Matrix([[1], [2]]).row_join(Matrix([[3], [4]])) == Matrix([[1, 3], [2, 4]]))
chk("matrix_col_join", Matrix([[1, 2]]).col_join(Matrix([[3, 4]])) == Matrix([[1, 2], [3, 4]]))
chk("matrix_is_symmetric", Matrix([[1, 2], [2, 1]]).is_symmetric() and not Mx.is_symmetric())

# ---- polys: factor_list / div / rem / resultant / groebner / discriminant / eval / LC / TC / gcd / lcm ----
chk("factor_list", factor_list(x ** 2 - 1) == (1, [(x - 1, 1), (x + 1, 1)]))
chk("poly_div", div(x ** 2 - 1, x - 1) == (x + 1, 0))
chk("poly_rem", rem(x ** 2 + 1, x - 1) == 2)                          # x^2+1 at x=1 -> 2
chk("resultant", resultant(x ** 2 - 1, x - 1) == 0)                  # common root x=1
chk("groebner_nonempty", len(groebner([x ** 2 + y, x * y], x, y).exprs) >= 1)
chk("discriminant", discriminant(x ** 2 - 5 * x + 6) == 1)           # (roots 2,3) diff^2 = 1
chk("poly_eval", Poly(x ** 2 - 1, x).eval(3) == 8)
chk("poly_LC", Poly(x ** 2 - 1, x).LC() == 1)
chk("poly_TC", Poly(x ** 2 - 1, x).TC() == -1)
chk("poly_gcd_method", Poly(x ** 2 - 1, x).gcd(Poly(x - 1, x)) == Poly(x - 1, x))
chk("poly_lcm", lcm(x ** 2 - 1, x + 1) == x ** 2 - 1)

# ---- functions: log / Abs / gamma / beta / factorial / binomial / fibonacci / bernoulli / legendre / besselj / hyperbolic / inverse-trig / floor-ceil / re-im-conjugate / Min-Max ----
chk("log_E", log(E) == 1 and log(1) == 0)
chk("Abs", Abs(-5) == 5 and Abs(I) == 1)
chk("gamma", gamma(5) == 24)                                         # gamma(n) = (n-1)!
chk("beta", beta(2, 3) == Rational(1, 12))
chk("factorial", factorial(5) == 120)
chk("binomial", binomial(6, 2) == 15)
chk("fibonacci", fibonacci(10) == 55)
chk("bernoulli", bernoulli(2) == Rational(1, 6))
chk("legendre", simplify(legendre(2, x) - (3 * x ** 2 - 1) / 2) == 0)
chk("besselj_00", besselj(0, 0) == 1)
chk("hyperbolic", cosh(0) == 1 and sinh(0) == 0 and tanh(0) == 0)
chk("inverse_trig", atan(1) == pi / 4 and asin(1) == pi / 2 and acos(1) == 0)
chk("floor_ceiling", floor(pi) == 3 and ceiling(pi) == 4)
chk("re_im_conj", re(3 + 4 * I) == 3 and im(3 + 4 * I) == 4 and conjugate(3 + 4 * I) == 3 - 4 * I)
chk("min_max", Min(3, 1, 2) == 1 and Max(3, 1, 2) == 3)

# ---- simplify: radsimp / powsimp / logcombine / expand_log / expand_trig / ratsimp / combsimp ----
chk("radsimp", radsimp(1 / (sqrt(2) + 1)) == sqrt(2) - 1)
p_, q_ = symbols("p_ q_")
chk("powsimp", powsimp(x ** p_ * x ** q_) == x ** (p_ + q_))
chk("logcombine", logcombine(log(x) + log(y), force=True) == log(x * y))
chk("expand_log", expand_log(log(x * y), force=True) == log(x) + log(y))
chk("expand_trig", expand_trig(sin(2 * x)) == 2 * sin(x) * cos(x))
chk("ratsimp", simplify(ratsimp(1 / x + 1 / y) - (x + y) / (x * y)) == 0)
chk("combsimp", combsimp(factorial(n) / factorial(n - 1)) == n)

# ---- assumptions: Q / ask / refine / Symbol assumption querying ----
chk("ask_positive", ask(Q.positive(1)) is True)
chk("ask_prime", ask(Q.prime(7)) is True)
chk("ask_even", ask(Q.even(4)) is True)
chk("refine_sqrt", refine(sqrt(x ** 2), Q.positive(x)) == x)
chk("symbol_positive", Symbol("p", positive=True).is_positive is True)

# ---- sets: Intersection / S singletons / Complement / SymmetricDifference / ImageSet / contains / powerset ----
chk("Intersection", Intersection(Interval(0, 2), Interval(1, 3)) == Interval(1, 2))
chk("S_Reals_contains", S.Reals.contains(pi) == true and S.Naturals.contains(5) == true)
chk("S_EmptySet_measure", S.EmptySet.measure == 0 and FiniteSet().is_empty)
chk("Complement", Complement(FiniteSet(1, 2, 3), FiniteSet(2)) == FiniteSet(1, 3))
chk("Interval_contains", bool(Interval(0, 1).contains(Rational(1, 2))) is True)
chk("SymmetricDifference", SymmetricDifference(FiniteSet(1, 2, 3), FiniteSet(2, 3, 4)) == FiniteSet(1, 4))
chk("powerset_card", len(FiniteSet(1, 2).powerset()) == 4)
_ims = ImageSet(Lambda(x, 2 * x), S.Naturals)
chk("ImageSet", (4 in _ims) and (3 not in _ims))

# ---- logic: And / Or / Not / Xor / Implies / Equivalent / simplify_logic / to_cnf / to_dnf / model ----
chk("And", And(True, True) == true and simplify_logic(And(a, ~a)) == false)
chk("Or", Or(False, a) == a)
chk("Not", Not(True) is false and Not(Not(a)) == a)
chk("Xor", Xor(True, False) == true)
chk("Implies", Implies(True, False) is false)
chk("Equivalent", Equivalent(a, a) == true)
chk("simplify_logic", simplify_logic(a & (a | b)) == a)
chk("to_cnf", to_cnf(Or(a, And(b, symbols("c")))) == And(Or(a, b), Or(a, symbols("c"))))
chk("to_dnf", to_dnf(And(a, Or(b, symbols("c")))) == Or(And(a, b), And(a, symbols("c"))))
chk("satisfiable_model", satisfiable(a & b) == {a: True, b: True})

# ---- ntheory: divisors / primefactors / divisor_count / mobius / primepi / n_order / jacobi / nthroot_mod / continued_fraction ----
chk("divisors", divisors(12) == [1, 2, 3, 4, 6, 12])
chk("primefactors", primefactors(360) == [2, 3, 5])
chk("divisor_count", divisor_count(12) == 6)
chk("mobius", mobius(30) == -1)                                       # 30 = 2*3*5, squarefree, 3 primes -> (-1)^3
chk("primepi", primepi(10) == 4)                                     # primes <=10: 2,3,5,7
chk("n_order", n_order(2, 7) == 3)                                    # 2^3=8=1 mod 7
chk("jacobi_symbol", jacobi_symbol(2, 7) == 1)
chk("nthroot_mod", nthroot_mod(4, 2, 7) == 2)                        # sqrt(4) mod 7 = 2
chk("continued_fraction", continued_fraction(Rational(3, 2)) == [1, 2])

# ---- geometry: Point / Line / Circle / Triangle / Segment ----
chk("point_distance", Point(0, 0).distance(Point(3, 4)) == 5)
chk("line_slope", Line(Point(0, 0), Point(1, 1)).slope == 1)
chk("circle_area", Circle(Point(0, 0), 2).area == 4 * pi)
chk("circle_circumference", Circle(Point(0, 0), 2).circumference == 4 * pi)
chk("triangle_area", Triangle(Point(0, 0), Point(4, 0), Point(0, 3)).area == 6)
chk("triangle_centroid", Triangle(Point(0, 0), Point(6, 0), Point(0, 3)).centroid == Point(2, 1))
chk("segment_length", Segment(Point(0, 0), Point(3, 4)).length == 5)
chk("line_intersection", Line(Point(0, 0), Point(2, 2)).intersection(Line(Point(0, 2), Point(2, 0))) == [Point(1, 1)])

# ---- printing: latex / ccode / sstr / pretty / python / mathematica_code / octave_code ----
chk("latex_pow", latex(x ** 2) == "x^{2}")
chk("latex_rational", latex(Rational(1, 2)) == "\\frac{1}{2}")
chk("latex_integral", "int" in latex(Integral(x, x)))
chk("ccode", ccode(x ** 2 + 1) == "pow(x, 2) + 1")
chk("sstr", sstr(x + y) == "x + y")
chk("pretty", "2" in pretty(x ** 2))
chk("python_codegen", "x**2" in python(x ** 2))
chk("mathematica_code", mathematica_code(x ** 2) == "x^2")
chk("octave_code", octave_code(x ** 2) == "x.^2")

# ---- physics.units: convert_to / Quantity / dimensional analysis ----
from sympy.physics.units import (convert_to, meter, foot, second, centimeter,
                                 speed_of_light, Quantity)
from sympy.physics.units.systems.si import dimsys_SI
chk("convert_to_foot", convert_to(meter, foot) == Rational(1250, 381) * foot)
chk("units_add_convert", convert_to(3 * meter + 200 * centimeter, meter) == 5 * meter)
chk("units_meter_is_quantity", isinstance(meter, Quantity))
chk("units_c_dimension", str(speed_of_light.dimension) == "Dimension(velocity)")

# ---- combinatorics: Permutation / SymmetricGroup / partitions / subsets ----
from sympy.combinatorics import Permutation, PermutationGroup, SymmetricGroup
from sympy.functions.combinatorial.numbers import partition
from sympy.utilities.iterables import subsets
chk("perm_order", Permutation([1, 0, 2]).order() == 2)               # single transposition
chk("perm_cyclic_form", Permutation([[0, 1, 2]]).cyclic_form == [[0, 1, 2]])
chk("symmetric_group_order", SymmetricGroup(3).order() == 6)         # 3! = 6
chk("npartitions", partition(4) == 5)                                # 4=4,3+1,2+2,2+1+1,1+1+1+1
chk("subsets_count", len(list(subsets([1, 2, 3], 2))) == 3)          # C(3,2)=3

# ---- stats: Die / E / variance / P / Normal ----
from sympy.stats import Die, E as StatE, variance, P, Normal, density
_D = Die("D", 6)
chk("stats_die_E", StatE(_D) == Rational(7, 2))                      # (1+..+6)/6
chk("stats_die_variance", variance(_D) == Rational(35, 12))
chk("stats_die_P", P(_D > 4) == Rational(1, 3))                      # {5,6}/6
chk("stats_normal_E", StatE(Normal("N", 0, 1)) == 0)

# ---- series: fourier_series / fps ----
_fs = fourier_series(x, (x, -pi, pi))
chk("fourier_a0", _fs.a0 == 0)                                       # odd function -> a0 = 0
chk("fourier_truncate", _fs.truncate(2) == 2 * sin(x) - sin(2 * x))
chk("fps_exp", fps(exp(x), x).truncate(4) == 1 + x + x ** 2 / 2 + x ** 3 / 6 + series(exp(x), x, 0, 4).getO())
chk("series_cos", series(cos(x), x, 0, 6).removeO() == 1 - x ** 2 / 2 + x ** 4 / 24)
_ser_O = series(exp(x), x, 0, 3)
chk("series_O_term", _ser_O.getO() is not None)

# ---- HONEST SKIPS (documented, not asserted) ----
# tensor (sympy.tensor / IndexedBase heavy symbolic manipulation) - out of carpet scope,
#   large expression trees, no deterministic tiny golden; skipped per brief.
# plotting (sympy.plotting) - requires a display/matplotlib backend; StarryOS has no
#   display and this carpet targets pure-symbolic determinism; skipped per brief.
# pdsolve (partial ODE) - heavy/version-sensitive solver output form; brief marks skippable.
# aseries (asymptotic series) - output form is version-sensitive; covered instead by
#   the deterministic fps / fourier / series checks above.

print("SYMPY_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SYMPY_DONE")
    sys.exit(0)
sys.exit(1)
