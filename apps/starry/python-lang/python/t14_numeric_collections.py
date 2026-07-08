#!/usr/bin/env python3
"""Numeric stack + collections — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# =====================================================================
# math — https://docs.python.org/3/library/math.html
# 怎么测: 逐函数调用并与已知精确/近似值比较 (整数返回精确, 浮点用 isclose).
# 期望: 每个文档列出的函数都存在且返回 POSIX libm 一致的值.
# 为什么: math 是 C libm 的薄封装, 是验证 starry 浮点 ABI / softfloat 的关键面.
# =====================================================================
import math

# math.ceil/floor/trunc — 取整, 返回 int.
chk("math_ceil", math.ceil(4.2) == 5 and math.ceil(-4.2) == -4 and math.ceil(7) == 7)
chk("math_floor", math.floor(4.8) == 4 and math.floor(-4.2) == -5 and math.floor(7) == 7)
chk("math_trunc", math.trunc(4.8) == 4 and math.trunc(-4.8) == -4)
# math.fabs — 返回 float 绝对值 (不同于内置 abs 返回 int).
chk("math_fabs", math.fabs(-3) == 3.0 and isinstance(math.fabs(-3), float))
# math.factorial(n) — n! , 仅非负整数.
chk("math_factorial", math.factorial(0) == 1 and math.factorial(5) == 120 and math.factorial(10) == 3628800)
try:
    math.factorial(-1); _fe = False
except ValueError:
    _fe = True
chk("math_factorial_neg", _fe)
# math.gcd / math.lcm — 可变参数 (3.9+); gcd()==0, lcm()==1.
chk("math_gcd", math.gcd(48, 36) == 12 and math.gcd(0, 5) == 5 and math.gcd() == 0 and math.gcd(12, 18, 24) == 6)
chk("math_lcm", math.lcm(4, 6) == 12 and math.lcm() == 1 and math.lcm(2, 3, 4) == 12 and math.lcm(5, 0) == 0)
# math.isqrt — 整数平方根 (向下取整).
chk("math_isqrt", math.isqrt(0) == 0 and math.isqrt(16) == 4 and math.isqrt(17) == 4 and math.isqrt(99) == 9)
# math.prod — 可迭代乘积 (3.8+); 空序列返回 start (默认 1); start 关键字累乘.
chk("math_prod", math.prod([1, 2, 3, 4]) == 24 and math.prod([]) == 1
    and math.prod([], start=10) == 10 and math.prod([2, 3], start=4) == 24
    and math.prod([1.5, 2.0]) == 3.0)
# math.fsum — 高精度浮点求和 (Shewchuk 算法), 0.1*10 精确得 1.0 (朴素 sum 不准).
chk("math_fsum", math.fsum([0.1] * 10) == 1.0 and math.fsum([]) == 0.0
    and math.fsum([1, 2, 3]) == 6.0 and isinstance(math.fsum([1, 2]), float))
# math.comb / math.perm — 组合/排列 (3.8+).
chk("math_comb", math.comb(5, 2) == 10 and math.comb(10, 0) == 1 and math.comb(3, 5) == 0)
chk("math_perm", math.perm(5, 2) == 20 and math.perm(5) == 120 and math.perm(3, 5) == 0)
# math.copysign — 取 x 的量级, y 的符号.
chk("math_copysign", math.copysign(3, -1) == -3.0 and math.copysign(-3, 1) == 3.0 and math.copysign(1, -0.0) == -1.0)
# math.fmod — C fmod, 符号随被除数 (区别于 % 随除数).
chk("math_fmod", math.fmod(7, 3) == 1.0 and math.fmod(-7, 3) == -1.0)
# math.frexp — 返回 (m, e) 满足 x == m * 2**e, 0.5<=|m|<1.
_m, _e = math.frexp(8.0)
chk("math_frexp", _m == 0.5 and _e == 4 and math.frexp(0.0) == (0.0, 0))
# math.ldexp — frexp 的逆: m * 2**e.
chk("math_ldexp", math.ldexp(0.5, 4) == 8.0 and math.ldexp(1.0, 0) == 1.0)
# math.modf — 返回 (小数部分, 整数部分), 均为 float, 符号同 x.
_f, _i = math.modf(3.25)
chk("math_modf", _f == 0.25 and _i == 3.0 and math.modf(-1.5) == (-0.5, -1.0))
# math.remainder — IEEE 754 余数 (round-half-to-even).
chk("math_remainder", math.remainder(5, 2) == 1.0 and math.remainder(7, 2) == -1.0)
# math.exp / expm1 — e**x 与 e**x - 1 (后者小 x 精度高).
chk("math_exp", math.isclose(math.exp(0), 1.0) and math.isclose(math.exp(1), math.e))
chk("math_expm1", math.isclose(math.expm1(0), 0.0) and math.isclose(math.expm1(1), math.e - 1))
# math.log (可选底) / log2 / log10 / log1p.
chk("math_log", math.isclose(math.log(math.e), 1.0) and math.isclose(math.log(8, 2), 3.0) and math.isclose(math.log(100, 10), 2.0))
chk("math_log2", math.log2(8) == 3.0 and math.log2(1) == 0.0)
chk("math_log10", math.log10(1000) == 3.0 and math.log10(1) == 0.0)
chk("math_log1p", math.isclose(math.log1p(0), 0.0) and math.isclose(math.log1p(math.e - 1), 1.0))
# math.pow — 总返回 float; pow(1, x)==1.0, pow(x, 0)==1.0.
chk("math_pow", math.pow(2, 10) == 1024.0 and math.pow(1, 1e100) == 1.0 and math.pow(0, 0) == 1.0)
# math.sqrt / cbrt(3.11+).
chk("math_sqrt", math.sqrt(144) == 12.0 and math.sqrt(0) == 0.0)
if hasattr(math, "cbrt"):
    chk("math_cbrt", math.isclose(math.cbrt(27), 3.0) and math.isclose(math.cbrt(-8), -2.0))
else:
    chk("math_cbrt", True, "(skip: needs 3.11)")
# 三角: sin/cos/tan/asin/acos/atan/atan2.
chk("math_sin", math.isclose(math.sin(0), 0.0) and math.isclose(math.sin(math.pi / 2), 1.0))
chk("math_cos", math.isclose(math.cos(0), 1.0) and abs(math.cos(math.pi / 2)) < 1e-9)
chk("math_tan", math.isclose(math.tan(0), 0.0) and math.isclose(math.tan(math.pi / 4), 1.0))
chk("math_asin", math.isclose(math.asin(1), math.pi / 2) and math.isclose(math.asin(0), 0.0))
chk("math_acos", math.isclose(math.acos(1), 0.0) and math.isclose(math.acos(0), math.pi / 2))
chk("math_atan", math.isclose(math.atan(1), math.pi / 4) and math.isclose(math.atan(0), 0.0))
chk("math_atan2", math.isclose(math.atan2(1, 1), math.pi / 4) and math.isclose(math.atan2(0, -1), math.pi))
# math.hypot — 欧氏范数, 可变参数 (3.8+).
chk("math_hypot", math.hypot(3, 4) == 5.0 and math.isclose(math.hypot(1, 2, 2), 3.0))
# math.degrees / radians — 互逆.
chk("math_degrees", math.isclose(math.degrees(math.pi), 180.0))
chk("math_radians", math.isclose(math.radians(180), math.pi))
# 双曲: sinh/cosh/tanh/asinh/acosh/atanh.
chk("math_sinh", math.isclose(math.sinh(0), 0.0))
chk("math_cosh", math.isclose(math.cosh(0), 1.0))
chk("math_tanh", math.isclose(math.tanh(0), 0.0))
chk("math_asinh", math.isclose(math.asinh(math.sinh(1)), 1.0))
chk("math_acosh", math.isclose(math.acosh(1), 0.0))
chk("math_atanh", math.isclose(math.atanh(0), 0.0) and math.isclose(math.atanh(math.tanh(0.5)), 0.5))
# erf / erfc — 误差函数, erf(0)==0, erfc==1-erf.
chk("math_erf", math.isclose(math.erf(0), 0.0) and math.isclose(math.erf(float("inf")), 1.0))
chk("math_erfc", math.isclose(math.erfc(0), 1.0) and math.isclose(math.erf(1) + math.erfc(1), 1.0))
# gamma / lgamma — gamma(n+1)==n!, lgamma==ln|gamma|.
chk("math_gamma", math.isclose(math.gamma(5), 24.0) and math.isclose(math.gamma(1), 1.0))
chk("math_lgamma", math.isclose(math.lgamma(1), 0.0) and math.isclose(math.lgamma(5), math.log(24)))
# isfinite/isinf/isnan — 浮点分类谓词.
chk("math_isfinite", math.isfinite(1.0) and not math.isfinite(math.inf) and not math.isfinite(math.nan))
chk("math_isinf", math.isinf(math.inf) and math.isinf(-math.inf) and not math.isinf(1.0))
chk("math_isnan", math.isnan(math.nan) and not math.isnan(1.0))
# isclose — rel_tol/abs_tol 容差比较.
chk("math_isclose", math.isclose(1.0, 1.0 + 1e-10) and not math.isclose(1.0, 1.1)
    and math.isclose(0.0, 1e-12, abs_tol=1e-9))
# nextafter — 朝 y 方向的下一个可表示浮点 (3.9+); 含次正规/符号过渡边界.
chk("math_nextafter", math.nextafter(1.0, 2.0) > 1.0 and math.nextafter(1.0, 0.0) < 1.0
    and math.nextafter(1.0, 1.0) == 1.0
    and math.nextafter(0.0, 1.0) == 5e-324                  # 朝正向 -> 最小次正规
    and math.nextafter(0.0, -1.0) == -5e-324               # 朝负向 -> 负最小次正规
    and math.nextafter(-1.0, 0.0) > -1.0                    # 负数朝零量级缩小
    and math.nextafter(math.inf, 0.0) == 1.7976931348623157e308)  # inf 朝零 -> 最大有限值
# ulp — x 的最低有效位的值 (3.9+). 用非 1.0 的 x 验证 ulp(x)==nextafter(x,inf)-x (而非 -1.0).
chk("math_ulp", math.ulp(1.0) > 0 and math.ulp(1.0) == math.nextafter(1.0, math.inf) - 1.0
    and math.ulp(8.0) == math.nextafter(8.0, math.inf) - 8.0
    and math.ulp(1024.0) == math.nextafter(1024.0, math.inf) - 1024.0
    and math.ulp(1.0) == 2.0 ** -52)
# dist — 两点欧氏距离 (3.8+).
chk("math_dist", math.dist((0, 0), (3, 4)) == 5.0 and math.dist([1], [1]) == 0.0)
# sumprod — 成对乘积之和 (3.12+).
if hasattr(math, "sumprod"):
    chk("math_sumprod", math.sumprod([1, 2, 3], [4, 5, 6]) == 32)
else:
    chk("math_sumprod", True, "(skip: needs 3.12)")
# 常量: pi/e/tau/inf/nan.
chk("math_consts", math.isclose(math.pi, 3.141592653589793) and math.isclose(math.e, 2.718281828459045)
    and math.isclose(math.tau, 2 * math.pi) and math.inf > 1e308 and math.isnan(math.nan))


# =====================================================================
# cmath — https://docs.python.org/3/library/cmath.html
# 怎么测: 关键复数函数 (sqrt/exp/log/phase/polar/rect/分类谓词/常量).
# 期望: 复数运算遵循解析延拓; cmath.sqrt(-1)==1j.
# 为什么: 复数数学栈是 starry 浮点正确性的额外角度.
# =====================================================================
import cmath

chk("cmath_sqrt", cmath.sqrt(-1) == 1j)
chk("cmath_exp", cmath.isclose(cmath.exp(0), 1 + 0j))
chk("cmath_log", cmath.isclose(cmath.log(cmath.e), 1 + 0j) and cmath.isclose(cmath.log(100, 10), 2 + 0j))
chk("cmath_log10", cmath.isclose(cmath.log10(1000), 3 + 0j))
# phase — 复数辐角; polar — (模, 辐角); rect — 逆变换.
chk("cmath_phase", math.isclose(cmath.phase(1j), math.pi / 2) and cmath.phase(1 + 0j) == 0.0)
_r, _ph = cmath.polar(1j)
chk("cmath_polar", math.isclose(_r, 1.0) and math.isclose(_ph, math.pi / 2))
chk("cmath_rect", cmath.isclose(cmath.rect(1, math.pi / 2), 1j, abs_tol=1e-12)
    and cmath.rect(2, 0) == complex(2, 0)                              # 非单位模, 零角
    and cmath.isclose(cmath.rect(1, -math.pi / 2), -1j, abs_tol=1e-12)  # 负角
    and cmath.isclose(cmath.rect(2, math.pi), -2 + 0j, abs_tol=1e-12)   # 非单位模 + pi
    and cmath.rect(0, 1.5) == 0j)                                       # 零模 -> 原点
# 欧拉恒等式: e**(i*pi) == -1.
chk("cmath_euler", cmath.isclose(cmath.exp(1j * cmath.pi), -1 + 0j, abs_tol=1e-12))
# 分类谓词 + 常量.
chk("cmath_isfinite", cmath.isfinite(1 + 1j) and not cmath.isinf(1 + 1j) and not cmath.isnan(1 + 1j))
chk("cmath_isinf", cmath.isinf(complex(math.inf, 0)) and cmath.isnan(complex(math.nan, 0)))
chk("cmath_consts", cmath.isclose(cmath.pi, math.pi) and cmath.isclose(cmath.e, math.e)
    and cmath.isclose(cmath.tau, math.tau))
# 复数三角.
chk("cmath_sin", cmath.isclose(cmath.sin(0), 0 + 0j) and cmath.isclose(cmath.cos(0), 1 + 0j))
chk("cmath_tan", cmath.isclose(cmath.tan(0), 0 + 0j))
# 复数反三角 (解析延拓): asin(0)/acos(1)/atan(0) 取实轴值; 验证逆复合.
chk("cmath_asin", cmath.isclose(cmath.asin(0), 0 + 0j)
    and cmath.isclose(cmath.asin(cmath.sin(0.5 + 0.5j)), 0.5 + 0.5j))
chk("cmath_acos", cmath.isclose(cmath.acos(1), 0 + 0j, abs_tol=1e-12)
    and cmath.isclose(cmath.acos(cmath.cos(0.3 + 0.4j)), 0.3 + 0.4j))
chk("cmath_atan", cmath.isclose(cmath.atan(0), 0 + 0j)
    and cmath.isclose(cmath.atan(cmath.tan(0.2 + 0.3j)), 0.2 + 0.3j))
# 复数双曲 + 反双曲.
chk("cmath_sinh", cmath.isclose(cmath.sinh(0), 0 + 0j) and cmath.isclose(cmath.cosh(0), 1 + 0j))
chk("cmath_tanh", cmath.isclose(cmath.tanh(0), 0 + 0j))
chk("cmath_asinh", cmath.isclose(cmath.asinh(0), 0 + 0j)
    and cmath.isclose(cmath.asinh(cmath.sinh(0.4 + 0.1j)), 0.4 + 0.1j))
chk("cmath_acosh", cmath.isclose(cmath.acosh(1), 0 + 0j, abs_tol=1e-12)
    and cmath.isclose(cmath.acosh(cmath.cosh(0.6 + 0.2j)), 0.6 + 0.2j))
chk("cmath_atanh", cmath.isclose(cmath.atanh(0), 0 + 0j)
    and cmath.isclose(cmath.atanh(cmath.tanh(0.3 + 0.5j)), 0.3 + 0.5j))


# =====================================================================
# decimal — https://docs.python.org/3/library/decimal.html
# 怎么测: 精确十进制算术, 上下文 prec/rounding/traps, quantize, as_tuple.
# 期望: 0.1+0.1+0.1 == 0.3 (精确); 各 ROUND_* 模式产出文档值.
# 为什么: decimal 是纯 Python+_decimal C 库, 异常陷阱与上下文是关键面.
# =====================================================================
import decimal
from decimal import Decimal, getcontext, localcontext, ROUND_HALF_EVEN, ROUND_HALF_UP, \
    ROUND_DOWN, ROUND_UP, ROUND_FLOOR, ROUND_CEILING, ROUND_HALF_DOWN, ROUND_05UP, \
    InvalidOperation, DivisionByZero, Inexact, Overflow, Underflow

# 精确十进制 (浮点做不到).
chk("decimal_exact", Decimal("0.1") + Decimal("0.1") + Decimal("0.1") == Decimal("0.3"))
chk("decimal_arith", Decimal("1.5") * Decimal("2") == Decimal("3.0")
    and Decimal("10") / Decimal("4") == Decimal("2.5")
    and Decimal("7") % Decimal("3") == Decimal("1")
    and Decimal("2") ** 3 == Decimal("8"))
# 从浮点构造保留二进制误差 (与 Decimal('0.1') 不同).
chk("decimal_from_float", Decimal(0.5) == Decimal("0.5") and Decimal(0.1) != Decimal("0.1"))
# getcontext().prec — 有效数字位数, 影响乘除舍入.
_oldprec = getcontext().prec
getcontext().prec = 6
chk("decimal_prec", Decimal(1) / Decimal(7) == Decimal("0.142857"))
getcontext().prec = _oldprec
# localcontext — 临时上下文不污染全局.
with localcontext() as lctx:
    lctx.prec = 3
    chk("decimal_localcontext", Decimal(1) / Decimal(3) == Decimal("0.333"))
chk("decimal_ctx_restored", getcontext().prec == _oldprec)
# quantize + 各舍入模式.
chk("decimal_round_half_even", Decimal("2.5").quantize(Decimal("1"), rounding=ROUND_HALF_EVEN) == Decimal("2")
    and Decimal("3.5").quantize(Decimal("1"), rounding=ROUND_HALF_EVEN) == Decimal("4"))
chk("decimal_round_half_up", Decimal("2.5").quantize(Decimal("1"), rounding=ROUND_HALF_UP) == Decimal("3"))
chk("decimal_round_down", Decimal("2.9").quantize(Decimal("1"), rounding=ROUND_DOWN) == Decimal("2"))
chk("decimal_round_up", Decimal("2.1").quantize(Decimal("1"), rounding=ROUND_UP) == Decimal("3"))
chk("decimal_round_floor", Decimal("-2.1").quantize(Decimal("1"), rounding=ROUND_FLOOR) == Decimal("-3"))
chk("decimal_round_ceiling", Decimal("2.1").quantize(Decimal("1"), rounding=ROUND_CEILING) == Decimal("3"))
chk("decimal_round_half_down", Decimal("2.5").quantize(Decimal("1"), rounding=ROUND_HALF_DOWN) == Decimal("2"))
chk("decimal_round_05up", Decimal("2.0").quantize(Decimal("1"), rounding=ROUND_05UP) == Decimal("2"))
# quantize 设定小数位.
chk("decimal_quantize_places", Decimal("3.14159").quantize(Decimal("0.01")) == Decimal("3.14"))
# as_tuple — (sign, digits, exponent) 命名元组.
_t = Decimal("-12.3").as_tuple()
chk("decimal_as_tuple", _t.sign == 1 and _t.digits == (1, 2, 3) and _t.exponent == -1)
# traps — InvalidOperation 默认开启, sqrt(-1) 抛.
with localcontext() as lctx:
    try:
        Decimal(-1).sqrt(); _de = False
    except InvalidOperation:
        _de = True
chk("decimal_trap_invalid", _de)
# 关闭 trap 后产出 NaN.
with localcontext() as lctx:
    lctx.traps[InvalidOperation] = False
    chk("decimal_trap_off", Decimal(-1).sqrt().is_nan())
# DivisionByZero trap — 默认开启, 必须抛 decimal.DivisionByZero (它是 ZeroDivisionError 子类,
# 但仅捕获子类才能发现 starry 错抛了裸 ZeroDivisionError 的情形).
with localcontext() as lctx:
    try:
        Decimal(1) / Decimal(0); _dz = False
    except DivisionByZero:
        _dz = True
chk("decimal_div_zero", _dz)
# Overflow trap — 默认开启; 结果超出 Emax 抛 Overflow.
with localcontext() as lctx:
    lctx.prec = 3
    lctx.Emax = 5
    try:
        Decimal("9e5") * Decimal("9e5"); _ov = False
    except Overflow:
        _ov = True
chk("decimal_trap_overflow", _ov)
# Underflow trap — 默认关闭, 显式开启; 结果太小且非精确时抛.
with localcontext() as lctx:
    lctx.prec = 3
    lctx.Emin = -5
    lctx.traps[Underflow] = True
    try:
        Decimal("1e-5") * Decimal("1e-5"); _uf = False
    except Underflow:
        _uf = True
chk("decimal_trap_underflow", _uf)
# Inexact trap — 默认关闭, 显式开启; 非精确结果 (如 1/3) 抛.
with localcontext() as lctx:
    lctx.prec = 3
    lctx.traps[Inexact] = True
    try:
        Decimal(1) / Decimal(3); _ix = False
    except Inexact:
        _ix = True
chk("decimal_trap_inexact", _ix)
# 实用方法: sqrt/ln/log10/compare/copy_abs/is_*.
chk("decimal_sqrt", Decimal(2).sqrt() == Decimal("1.414213562373095048801688724"))
chk("decimal_predicates", Decimal("Infinity").is_infinite() and Decimal("NaN").is_nan()
    and Decimal("0").is_zero() and Decimal("1.5").is_finite())
chk("decimal_copy_abs", Decimal("-3.5").copy_abs() == Decimal("3.5"))
# ln / log10 / exp — 上下文舍入到当前 prec; 用精确边界值验证.
chk("decimal_log10", Decimal(1000).log10() == Decimal("3") and Decimal(1).log10() == Decimal("0"))
chk("decimal_exp", Decimal(0).exp() == Decimal("1")
    and Decimal(1).exp().quantize(Decimal("0.0001")) == Decimal("2.7183"))
chk("decimal_ln", Decimal(1).ln() == Decimal("0")
    and Decimal(1).exp().ln().quantize(Decimal("0.0001")) == Decimal("1.0000"))
# compare — 返回 Decimal(-1/0/1), 区别于布尔比较 (NaN 时返回 NaN 而非抛).
chk("decimal_compare", Decimal(1).compare(Decimal(2)) == Decimal("-1")
    and Decimal(2).compare(Decimal(1)) == Decimal("1")
    and Decimal(1).compare(Decimal(1)) == Decimal("0")
    and Decimal("NaN").compare(Decimal(1)).is_nan())
# to_integral_value — 取整到整数 (保留 Decimal 类型, 默认上下文舍入); 不抛 Inexact.
chk("decimal_to_integral_value", Decimal("3.7").to_integral_value(rounding=ROUND_DOWN) == Decimal("3")
    and Decimal("2.5").to_integral_value(rounding=ROUND_HALF_EVEN) == Decimal("2")
    and Decimal("-2.5").to_integral_value(rounding=ROUND_CEILING) == Decimal("-2"))
# to_integral_exact — 同上但非整数时发 Inexact 信号; 此处整数输入无信号.
chk("decimal_to_integral_exact", Decimal("3.0").to_integral_exact() == Decimal("3")
    and Decimal("3.7").to_integral_exact(rounding=ROUND_DOWN) == Decimal("3"))
# scaleb — 乘以 10**other (移动指数), 结果按当前 prec 舍入.
chk("decimal_scaleb", Decimal("1.23").scaleb(2) == Decimal("123")
    and Decimal("100").scaleb(-2) == Decimal("1.00"))
# normalize — 去除末尾零归一 (1.2000 -> 1.2, 用最少位数表示).
chk("decimal_normalize", Decimal("1.2000").normalize() == Decimal("1.2")
    and Decimal("1.2000").normalize().as_tuple().digits == (1, 2)
    and Decimal("0.00").normalize() == Decimal("0"))
# next_plus / next_minus — 当前上下文中朝 +/-Infinity 的下一个可表示值.
with localcontext() as lctx:
    lctx.prec = 9
    chk("decimal_next_plus", Decimal("1").next_plus() == Decimal("1.00000001")
        and Decimal("1").next_minus() == Decimal("0.999999999")
        and Decimal("1").next_toward(Decimal("2")) == Decimal("1.00000001")
        and Decimal("1").next_toward(Decimal("0")) == Decimal("0.999999999"))
# canonical — Decimal 已是规范形式, 返回自身相等值; is_canonical 恒真.
chk("decimal_canonical", Decimal("1.0").canonical() == Decimal("1.0")
    and Decimal("1.0").is_canonical())


# =====================================================================
# fractions — https://docs.python.org/3/library/fractions.html
# 怎么测: 精确有理数算术, 自动约分, from_float, limit_denominator, 分子分母.
# 期望: Fraction(1,2)+Fraction(1,3)==Fraction(5,6); from_float(0.5)==Fraction(1,2).
# 为什么: 任意精度整数除法/约分逻辑, 验证 bignum 与 gcd 正确性.
# =====================================================================
from fractions import Fraction

chk("fraction_reduce", Fraction(4, 8) == Fraction(1, 2) and Fraction(6, 3) == Fraction(2, 1))
chk("fraction_arith", Fraction(1, 2) + Fraction(1, 3) == Fraction(5, 6)
    and Fraction(2, 3) * Fraction(3, 4) == Fraction(1, 2)
    and Fraction(1, 2) - Fraction(1, 6) == Fraction(1, 3)
    and Fraction(1, 2) / Fraction(1, 4) == Fraction(2, 1)
    and Fraction(2, 3) ** 2 == Fraction(4, 9))
chk("fraction_numer_denom", Fraction(6, 4).numerator == 3 and Fraction(6, 4).denominator == 2)
chk("fraction_from_str", Fraction("3/7") == Fraction(3, 7) and Fraction("1.25") == Fraction(5, 4))
chk("fraction_from_float", Fraction.from_float(0.5) == Fraction(1, 2)
    and Fraction.from_float(0.25) == Fraction(1, 4))
# from_decimal — 从 Decimal 精确转换.
chk("fraction_from_decimal", Fraction.from_decimal(Decimal("1.1")) == Fraction(11, 10))
# limit_denominator — 找最佳近似 (pi).
chk("fraction_limit_denom", Fraction(math.pi).limit_denominator(100) == Fraction(311, 99)
    and Fraction(math.pi).limit_denominator(10) == Fraction(22, 7))
# 与 int/float 互操作.
chk("fraction_mixed", Fraction(1, 2) + 1 == Fraction(3, 2) and float(Fraction(1, 4)) == 0.25)
chk("fraction_compare", Fraction(1, 3) < Fraction(1, 2) and Fraction(2, 4) == Fraction(1, 2))
# round — 无 ndigits 返回 int (banker's rounding); 有 ndigits 返回 Fraction.
chk("fraction_round", round(Fraction(5, 2)) == 2 and round(Fraction(7, 2)) == 4
    and round(Fraction(8, 3)) == 3
    and round(Fraction(8, 3), 2) == Fraction(267, 100)
    and isinstance(round(Fraction(8, 3), 2), Fraction))
# math.floor/ceil/trunc 协议 + as_integer_ratio.
chk("fraction_floor_ceil", math.floor(Fraction(8, 3)) == 2 and math.ceil(Fraction(8, 3)) == 3
    and math.trunc(Fraction(-8, 3)) == -2
    and Fraction(-8, 3).as_integer_ratio() == (-8, 3))


# =====================================================================
# statistics — https://docs.python.org/3/library/statistics.html
# 怎么测: 集中趋势/离散/分位/相关; 与手算精确值比对.
# 期望: mean([1,2,3,4])==2.5, median([1,2,3,4])==2.5, stdev 样本方差等.
# 为什么: 纯 Python 实现, 验证 float/Fraction 算术与排序正确性.
# =====================================================================
import statistics as st

_d = [1, 2, 3, 4, 5]
chk("stat_mean", st.mean(_d) == 3 and st.mean([1, 2, 3, 4]) == 2.5)
chk("stat_fmean", st.fmean(_d) == 3.0 and isinstance(st.fmean(_d), float))
chk("stat_geometric_mean", math.isclose(st.geometric_mean([1, 4, 16]), 4.0))
# harmonic_mean — 文档示例 harmonic_mean([40,60])==48.0 (返回 float); 严格断值+类型, 不用 or 兜底.
chk("stat_harmonic_mean", st.harmonic_mean([40, 60]) == 48.0
    and isinstance(st.harmonic_mean([1, 2, 4]), float)
    and math.isclose(st.harmonic_mean([1, 2, 4]), 12 / 7))
chk("stat_median", st.median([1, 2, 3, 4]) == 2.5 and st.median(_d) == 3)
chk("stat_median_low", st.median_low([1, 2, 3, 4]) == 2)
chk("stat_median_high", st.median_high([1, 2, 3, 4]) == 3)
chk("stat_mode", st.mode([1, 1, 2, 3]) == 1 and st.mode("aabbbcc") == "b")
chk("stat_multimode", sorted(st.multimode([1, 1, 2, 2, 3])) == [1, 2])
chk("stat_pstdev", math.isclose(st.pstdev([2, 4, 4, 4, 5, 5, 7, 9]), 2.0))
chk("stat_pvariance", st.pvariance([2, 4, 4, 4, 5, 5, 7, 9]) == 4.0)
chk("stat_stdev", math.isclose(st.stdev([1.5, 2.5, 2.5, 2.75, 3.25, 4.75]), 1.0810874155219827))
chk("stat_variance", st.variance([1, 2, 3, 4, 5]) == 2.5)
# quantiles — 默认 n=4 (四分位), 排除式 (method='exclusive').
chk("stat_quantiles", st.quantiles([1, 2, 3, 4, 5, 6, 7, 8], n=4) == [2.25, 4.5, 6.75])
# quantiles method='inclusive' — 端点含数据极值, 与默认排除式产出不同切点.
chk("stat_quantiles_inclusive",
    st.quantiles([1, 2, 3, 4, 5, 6, 7, 8], n=4, method="inclusive") == [2.75, 4.5, 6.25]
    and st.quantiles([0, 100], n=2, method="inclusive") == [50.0])
# StatisticsError — 空数据 / 无众数等非法输入抛 statistics.StatisticsError.
try:
    st.mean([]); _se1 = False
except st.StatisticsError:
    _se1 = True
try:
    st.variance([1]); _se2 = False           # 样本方差至少需 2 个点
except st.StatisticsError:
    _se2 = True
chk("stat_error", _se1 and _se2)
# correlation / covariance / linear_regression (3.10+).
if hasattr(st, "correlation"):
    chk("stat_correlation", math.isclose(st.correlation([1, 2, 3], [1, 2, 3]), 1.0)
        and math.isclose(st.correlation([1, 2, 3], [3, 2, 1]), -1.0))
    chk("stat_covariance", math.isclose(st.covariance([1, 2, 3], [1, 2, 3]), 1.0))
    _lr = st.linear_regression([1, 2, 3], [2, 4, 6])
    chk("stat_linear_regression", math.isclose(_lr.slope, 2.0) and math.isclose(_lr.intercept, 0.0))
else:
    chk("stat_correlation", True, "(skip: needs 3.10)")
    chk("stat_covariance", True, "(skip: needs 3.10)")
    chk("stat_linear_regression", True, "(skip: needs 3.10)")


# =====================================================================
# random — https://docs.python.org/3/library/random.html
# 怎么测: 固定 seed -> 确定性序列; 验证各分布/选择函数的取值范围与不变量.
# 期望: 同 seed 重复产出同序列; randint 在闭区间内; sample 不重复.
# 为什么: 验证 Mersenne Twister 状态机与 os.urandom 入口在 starry 上正确.
# =====================================================================
import random

# seed 确定性: 重置后产出相同序列.
random.seed(12345)
_seq1 = [random.random() for _ in range(5)]
random.seed(12345)
_seq2 = [random.random() for _ in range(5)]
chk("random_seed_determinism", _seq1 == _seq2)
# random() 在 [0,1).
random.seed(1)
chk("random_random", all(0.0 <= random.random() < 1.0 for _ in range(50)))
# uniform(a,b) 在闭区间内.
chk("random_uniform", all(2.0 <= random.uniform(2, 5) <= 5.0 for _ in range(50)))
# randint(a,b) — 闭区间整数.
chk("random_randint", all(1 <= random.randint(1, 6) <= 6 for _ in range(100)))
# randrange — 半开区间, 支持 step.
chk("random_randrange", all(random.randrange(0, 10, 2) in (0, 2, 4, 6, 8) for _ in range(50)))
# choice — 单个随机元素.
chk("random_choice", all(random.choice("abc") in "abc" for _ in range(50)))
# choices — 有放回加权抽样.
_c = random.choices(["x", "y"], weights=[1, 0], k=10)
chk("random_choices_weights", _c == ["x"] * 10)
chk("random_choices_k", len(random.choices(range(100), k=7)) == 7)
# sample — 无放回抽样, 元素唯一.
_s = random.sample(range(20), 10)
chk("random_sample", len(_s) == len(set(_s)) == 10 and all(0 <= x < 20 for x in _s))
# shuffle — 原地打乱, 元素不变.
_lst = list(range(10))
random.shuffle(_lst)
chk("random_shuffle", sorted(_lst) == list(range(10)))
# getrandbits — n 比特随机整数, < 2**n.
chk("random_getrandbits", all(0 <= random.getrandbits(8) < 256 for _ in range(50)))
# randbytes (3.9+) — n 字节随机.
if hasattr(random, "randbytes"):
    chk("random_randbytes", len(random.randbytes(16)) == 16 and isinstance(random.randbytes(4), bytes))
else:
    chk("random_randbytes", True, "(skip: needs 3.9)")
# triangular — [low, high] 三角分布.
chk("random_triangular", all(0.0 <= random.triangular(0, 10, 5) <= 10.0 for _ in range(50)))
# gauss / normalvariate — 正态分布 (确定性: 验证有限).
random.seed(99)
chk("random_gauss", math.isfinite(random.gauss(0, 1)) and math.isfinite(random.normalvariate(0, 1)))
# expovariate — 指数分布, 非负.
chk("random_expovariate", all(random.expovariate(1.5) >= 0 for _ in range(50)))
# 其余连续分布 — 验证值域不变量 (beta in [0,1]; gamma/lognorm/pareto/weibull > 0; vonmises 有限).
random.seed(2024)
chk("random_betavariate", all(0.0 <= random.betavariate(2, 3) <= 1.0 for _ in range(50)))
chk("random_gammavariate", all(random.gammavariate(2.0, 1.5) > 0 for _ in range(50)))
chk("random_lognormvariate", all(random.lognormvariate(0, 1) > 0 for _ in range(50)))
chk("random_paretovariate", all(random.paretovariate(3) >= 1.0 for _ in range(50)))
chk("random_weibullvariate", all(random.weibullvariate(1, 1.5) >= 0 for _ in range(50)))
chk("random_vonmisesvariate", all(0.0 <= random.vonmisesvariate(0, 2) < 2 * math.pi for _ in range(50)))
# seed 接受 str/bytes (非仅 int); 相同字符串 seed -> 相同序列.
random.seed("starry")
_strseq1 = [random.random() for _ in range(5)]
random.seed("starry")
_strseq2 = [random.random() for _ in range(5)]
chk("random_seed_str", _strseq1 == _strseq2 and _strseq1 != _seq1)
# Random 实例 — 独立状态, 同 seed 同序列.
_r1 = random.Random(7)
_r2 = random.Random(7)
chk("random_instance", [_r1.random() for _ in range(5)] == [_r2.random() for _ in range(5)])
# getstate/setstate — 保存/恢复内部状态.
_state = random.getstate()
_a = random.random()
random.setstate(_state)
chk("random_state", random.random() == _a)


# =====================================================================
# collections.deque — https://docs.python.org/3/library/collections.html#collections.deque
# 怎么测: 双端增删/旋转/maxlen 溢出/extend(left).
# 期望: O(1) 两端操作; maxlen 满时挤出对端.
# 为什么: deque 是 C 实现的环形结构, 验证内存与边界处理.
# =====================================================================
from collections import deque

_dq = deque([1, 2, 3])
_dq.append(4)
_dq.appendleft(0)
chk("deque_append", list(_dq) == [0, 1, 2, 3, 4])
chk("deque_pop", _dq.pop() == 4 and _dq.popleft() == 0 and list(_dq) == [1, 2, 3])
# rotate(n>0) 右旋, n<0 左旋.
_dq2 = deque([1, 2, 3, 4, 5])
_dq2.rotate(2)
chk("deque_rotate_right", list(_dq2) == [4, 5, 1, 2, 3])
_dq2.rotate(-2)
chk("deque_rotate_left", list(_dq2) == [1, 2, 3, 4, 5])
# maxlen — 有界 deque, 溢出从对端丢弃.
_bd = deque(maxlen=3)
for i in range(5):
    _bd.append(i)
chk("deque_maxlen", list(_bd) == [2, 3, 4] and _bd.maxlen == 3)
# maxlen 对端语义: appendleft 满时丢右端; extend/extendleft 同样有界.
_bdl = deque([1, 2, 3], maxlen=3)
_bdl.appendleft(0)
chk("deque_maxlen_appendleft", list(_bdl) == [0, 1, 2])         # 右端 3 被挤出
_bde = deque(maxlen=2)
_bde.extend([1, 2, 3, 4])
chk("deque_maxlen_extend", list(_bde) == [3, 4] and _bde.maxlen == 2)
# clear / copy — 清空 (maxlen 保留) 与浅拷贝 (独立但同 maxlen).
_bc = deque([1, 2, 3], maxlen=5)
_bcopy = _bc.copy()
_bc.clear()
chk("deque_clear_copy", list(_bc) == [] and _bc.maxlen == 5
    and list(_bcopy) == [1, 2, 3] and _bcopy.maxlen == 5)
# extend / extendleft (后者逆序).
_de = deque([1])
_de.extend([2, 3])
_de.extendleft([0, -1])
chk("deque_extend", list(_de) == [-1, 0, 1, 2, 3])
# index/count/remove/insert.
_di = deque([1, 2, 2, 3])
chk("deque_ops", _di.count(2) == 2 and _di.index(3) == 3)
_di.remove(2)
chk("deque_remove", list(_di) == [1, 2, 3])
_di.insert(1, 99)
chk("deque_insert", list(_di) == [1, 99, 2, 3])
chk("deque_reverse", (lambda d: (d.reverse(), list(d))[1])(deque([1, 2, 3])) == [3, 2, 1])


# =====================================================================
# collections.Counter — https://docs.python.org/3/library/collections.html#collections.Counter
# 怎么测: 计数/most_common/elements/subtract 与集合代数 +,-,&,|/total(3.10).
# 期望: 缺失键返回 0; 算术按计数逐键运算.
# 为什么: 多重集语义, 验证 dict 子类与比较运算.
# =====================================================================
from collections import Counter

_ct = Counter("mississippi")
chk("counter_count", _ct["s"] == 4 and _ct["i"] == 4 and _ct["x"] == 0)
chk("counter_most_common", _ct.most_common(2) == [("i", 4), ("s", 4)])
chk("counter_elements", sorted(Counter(a=2, b=1).elements()) == ["a", "a", "b"])
_cs = Counter(a=4, b=2)
_cs.subtract(Counter(a=1, b=3))
chk("counter_subtract", _cs == Counter(a=3, b=-1))
# 集合代数 (结果丢弃 <=0 计数, 除 +/- 的 unary).
chk("counter_add", Counter(a=3, b=1) + Counter(a=1, b=2) == Counter(a=4, b=3))
chk("counter_sub", Counter(a=3, b=1) - Counter(a=1, b=2) == Counter(a=2))
chk("counter_and", Counter(a=3, b=1) & Counter(a=1, b=2) == Counter(a=1, b=1))
chk("counter_or", Counter(a=3, b=1) | Counter(a=1, b=2) == Counter(a=3, b=2))
# total() — 计数之和 (3.10+).
if hasattr(Counter, "total"):
    chk("counter_total", Counter(a=3, b=2, c=1).total() == 6)
else:
    chk("counter_total", True, "(skip: needs 3.10)")
# update — 累加计数.
_cu = Counter(a=1)
_cu.update("aab")
chk("counter_update", _cu == Counter(a=3, b=1))
# unary + / - — 保留正 (>0) / 取负后保留正, 即丢弃非正计数.
_cn = Counter(a=3, b=-1, c=0)
chk("counter_unary_pos", +_cn == Counter(a=3))                  # 仅 >0
chk("counter_unary_neg", -_cn == Counter(b=1))                  # 取负后仅 >0
# most_common() / most_common(None) — 返回全部按降序; n 截断.
_cmc = Counter(a=3, b=1, c=2)
chk("counter_most_common_all", _cmc.most_common() == [("a", 3), ("c", 2), ("b", 1)]
    and _cmc.most_common(None) == [("a", 3), ("c", 2), ("b", 1)])
# copy (浅拷贝, 独立) / clear (清空).
_ccp = Counter(a=1, b=2)
_ccopy = _ccp.copy()
_ccp.clear()
chk("counter_copy_clear", _ccp == Counter() and _ccopy == Counter(a=1, b=2))


# =====================================================================
# collections.OrderedDict — https://docs.python.org/3/library/collections.html#collections.OrderedDict
# 怎么测: move_to_end(last=True/False), popitem(last) 顺序敏感相等.
# 期望: 插入顺序保留; move_to_end 重排; OrderedDict 比较顺序敏感.
# 为什么: 验证有序映射的链表维护正确.
# =====================================================================
from collections import OrderedDict

_od = OrderedDict([("a", 1), ("b", 2), ("c", 3)])
_od.move_to_end("a")
chk("ordereddict_move_to_end_last", list(_od) == ["b", "c", "a"])
_od.move_to_end("a", last=False)
chk("ordereddict_move_to_end_first", list(_od) == ["a", "b", "c"])
chk("ordereddict_popitem_last", _od.popitem() == ("c", 3))
chk("ordereddict_popitem_first", _od.popitem(last=False) == ("a", 1))
# 顺序敏感相等 (vs 普通 dict).
chk("ordereddict_order_eq", OrderedDict([("a", 1), ("b", 2)]) != OrderedDict([("b", 2), ("a", 1)])
    and OrderedDict([("a", 1), ("b", 2)]) == {"b": 2, "a": 1})


# =====================================================================
# collections.defaultdict — https://docs.python.org/3/library/collections.html#collections.defaultdict
# 怎么测: 缺失键调用 default_factory; 各工厂 (list/int/set).
# 期望: 访问缺失键自动建条目; default_factory=None 抛 KeyError.
# 为什么: __missing__ 钩子 + 工厂调用.
# =====================================================================
from collections import defaultdict

_dl = defaultdict(list)
_dl["k"].append(1)
chk("defaultdict_list", _dl["k"] == [1])
_dc = defaultdict(int)
for ch in "aab":
    _dc[ch] += 1
chk("defaultdict_int", _dc == {"a": 2, "b": 1})
_ds = defaultdict(set)
_ds["g"].add(5)
chk("defaultdict_set", _ds["g"] == {5})
_dn = defaultdict(None)
try:
    _dn["missing"]; _dne = False
except KeyError:
    _dne = True
chk("defaultdict_none", _dne)
# __missing__ 钩子 — 访问缺失键调 default_factory 并插入条目 (副作用: len 增长).
_dm = defaultdict(int)
chk("defaultdict_missing_inserts", len(_dm) == 0 and _dm["new"] == 0
    and "new" in _dm and len(_dm) == 1)
# copy — 浅拷贝, 保留 default_factory.
_dcp = defaultdict(list)
_dcp["a"].append(1)
_dcopy = _dcp.copy()
chk("defaultdict_copy", _dcopy["a"] == [1] and _dcopy.default_factory is list)
# get() 不触发 default_factory (与 [] 索引区分).
_dg = defaultdict(int)
chk("defaultdict_get_no_insert", _dg.get("z") is None and "z" not in _dg)


# =====================================================================
# collections.ChainMap — https://docs.python.org/3/library/collections.html#collections.ChainMap
# 怎么测: maps 链查找 (前者优先), new_child, parents.
# 期望: 查找走第一个含 key 的 map; 写只改第一个; new_child 前插空 map.
# 为什么: 作用域链语义, 验证多映射视图.
# =====================================================================
from collections import ChainMap

_cm = ChainMap({"a": 1}, {"a": 2, "b": 3})
chk("chainmap_lookup", _cm["a"] == 1 and _cm["b"] == 3)
chk("chainmap_maps", _cm.maps == [{"a": 1}, {"a": 2, "b": 3}])
_child = _cm.new_child({"c": 9})
chk("chainmap_new_child", _child["c"] == 9 and _child["a"] == 1 and len(_child.maps) == 3)
chk("chainmap_parents", _child.parents.maps == _cm.maps)
# 写操作只影响第一个 map.
_cm["a"] = 100
chk("chainmap_write", _cm.maps[0] == {"a": 100} and _cm.maps[1] == {"a": 2, "b": 3})
# 缺失键抛 KeyError (查遍所有 map 未命中).
try:
    ChainMap({"a": 1})["zzz"]; _cme = False
except KeyError:
    _cme = True
chk("chainmap_keyerror", _cme)
# pop / popitem / delete — 仅作用于第一个 map; 键不在首 map 抛 KeyError.
_cmp = ChainMap({"x": 1, "y": 2}, {"z": 3})
chk("chainmap_pop", _cmp.pop("x") == 1 and "x" not in _cmp.maps[0])
try:
    _cmp.pop("z"); _cmpe = False                # z 在第二个 map, 不可 pop
except KeyError:
    _cmpe = True
chk("chainmap_pop_underlying_keyerror", _cmpe and _cmp["z"] == 3)
del _cmp["y"]
chk("chainmap_delitem", "y" not in _cmp.maps[0])


# =====================================================================
# collections.UserDict/UserList/UserString
# 怎么测: 子类化 + 覆写, 内部 .data 持有底层容器.
# 期望: 行为同内置但可被纯 Python 子类化.
# 为什么: 验证 MutableMapping/Sequence 协议的纯 Python 包装.
# =====================================================================
from collections import UserDict, UserList, UserString


class UpperDict(UserDict):
    def __setitem__(self, k, v):
        super().__setitem__(k.upper(), v)


_ud = UpperDict()
_ud["abc"] = 1
chk("userdict", _ud["ABC"] == 1 and _ud.data == {"ABC": 1})


class DoubleList(UserList):
    def append(self, item):
        super().append(item)
        super().append(item)


_ul = DoubleList([1])
_ul.append(2)
chk("userlist", _ul.data == [1, 2, 2] and len(_ul) == 3)

_us = UserString("hello")
chk("userstring", _us.upper() == "HELLO" and _us + " world" == "hello world" and _us[0] == "h"
    and len(_us) == 5 and _us.data == "hello")
# UserString 继承的 str 方法 (返回新 UserString, 非 str).
_us2 = UserString("Hello World")
chk("userstring_methods", _us2.startswith("Hello") and _us2.endswith("World")
    and _us2.find("World") == 6 and _us2.replace("o", "0").data == "Hell0 W0rld"
    and isinstance(_us2.replace("o", "0"), UserString)
    and _us2.lower().data == "hello world" and "x" * 0 == "" and _us2.count("o") == 2
    and _us2.split() == ["Hello", "World"])
# UserDict 继承的 MutableMapping 方法.
_udm = UserDict(a=1, b=2)
chk("userdict_methods", _udm.get("a") == 1 and _udm.get("z", -1) == -1
    and sorted(_udm.keys()) == ["a", "b"] and _udm.pop("a") == 1 and "a" not in _udm
    and (_udm.setdefault("c", 3) == 3) and _udm.data == {"b": 2, "c": 3})
# UserList 继承的 MutableSequence 方法.
_ulm = UserList([3, 1, 2])
_ulm.sort()
_ulm.append(5)
chk("userlist_methods", _ulm.data == [1, 2, 3, 5] and _ulm.pop() == 5
    and _ulm.index(2) == 1 and _ulm.count(1) == 1)
_ulm.insert(0, 9)
_ulm.reverse()
chk("userlist_methods2", _ulm.data == [3, 2, 1, 9])


# =====================================================================
# heapq — https://docs.python.org/3/library/heapq.html
# 怎么测: 最小堆不变量; push/pop/heapify/pushpop/replace; n大/n小.
# 期望: heap[0] 始终最小; pop 升序; merge 归并有序输入.
# 为什么: 优先队列基础, 验证就地数组堆算法.
# =====================================================================
import heapq

_h = []
for x in [5, 3, 8, 1, 9, 2]:
    heapq.heappush(_h, x)
chk("heapq_push_pop", _h[0] == 1 and [heapq.heappop(_h) for _ in range(6)] == [1, 2, 3, 5, 8, 9])
_hh = [9, 5, 1, 7, 3]
heapq.heapify(_hh)
chk("heapq_heapify", _hh[0] == 1)
# heappushpop — 先 push 再 pop (更高效).
_hp = [1, 3, 5]
chk("heapq_pushpop", heapq.heappushpop(_hp, 0) == 0 and heapq.heappushpop([1, 3, 5], 4) == 1)
# heapreplace — 先 pop 再 push.
_hr = [1, 3, 5]
chk("heapq_replace", heapq.heapreplace(_hr, 4) == 1 and _hr[0] == 3)
chk("heapq_nlargest", heapq.nlargest(3, [1, 8, 2, 23, 7, -4, 18]) == [23, 18, 8])
chk("heapq_nsmallest", heapq.nsmallest(3, [1, 8, 2, 23, 7, -4, 18]) == [-4, 1, 2])
# nlargest/nsmallest with key.
chk("heapq_nlargest_key", heapq.nlargest(2, ["a", "bbb", "cc"], key=len) == ["bbb", "cc"])
# merge — 归并多个有序迭代器.
chk("heapq_merge", list(heapq.merge([1, 4, 7], [2, 5, 8], [3, 6, 9])) == list(range(1, 10)))
chk("heapq_merge_reverse", list(heapq.merge([7, 4, 1], [8, 5, 2], reverse=True)) == [8, 7, 5, 4, 2, 1])
# 空堆 heappop / heapreplace 抛 IndexError (边界错误路径).
try:
    heapq.heappop([]); _he1 = False
except IndexError:
    _he1 = True
try:
    heapq.heapreplace([], 1); _he2 = False
except IndexError:
    _he2 = True
chk("heapq_empty_error", _he1 and _he2)
# merge with key — 按键归并保序.
chk("heapq_merge_key",
    list(heapq.merge(["bb", "dddd"], ["a", "ccc"], key=len)) == ["a", "bb", "ccc", "dddd"])


# =====================================================================
# bisect — https://docs.python.org/3/library/bisect.html
# 怎么测: 有序序列中查插入点; left/right 边界; insort 保序插入; key(3.10).
# 期望: bisect_left 返回首个 >=x 的位置; bisect_right 返回首个 >x 的位置.
# 为什么: 二分查找, 验证比较与索引算术.
# =====================================================================
import bisect

_sorted = [1, 3, 3, 5, 7]
chk("bisect_left", bisect.bisect_left(_sorted, 3) == 1 and bisect.bisect_left(_sorted, 4) == 3)
chk("bisect_right", bisect.bisect_right(_sorted, 3) == 3 and bisect.bisect(_sorted, 4) == 3)
_ins = [1, 3, 5]
bisect.insort_left(_ins, 4)
chk("bisect_insort_left", _ins == [1, 3, 4, 5])
_ins2 = [1, 3, 3, 5]
bisect.insort_right(_ins2, 3)
chk("bisect_insort_right", _ins2 == [1, 3, 3, 3, 5])
# insort 是 insort_right 的别名.
_ins3 = [1, 2, 4]
bisect.insort(_ins3, 3)
chk("bisect_insort_alias", _ins3 == [1, 2, 3, 4])
# lo / hi 参数 — 限定搜索/插入子区间 (区间外的命中被排除).
chk("bisect_lo_hi", bisect.bisect_left([1, 2, 3, 4, 5], 3, 0, 2) == 2      # hi=2 截断, 落在末尾
    and bisect.bisect_left([1, 2, 3, 4, 5], 2, 2, 5) == 2                  # lo=2, 2<a[2]=3
    and bisect.bisect_right([1, 2, 2, 2, 3], 2, 1, 3) == 3)
# key 参数 (3.10+).
try:
    _kd = [("a", 1), ("b", 3), ("c", 5)]
    _pos = bisect.bisect_left(_kd, 3, key=lambda t: t[1])
    chk("bisect_key", _pos == 1)
except TypeError:
    chk("bisect_key", True, "(skip: key needs 3.10)")


# =====================================================================
# array — https://docs.python.org/3/library/array.html
# 怎么测: 各 typecode 构造; append/extend; tobytes/frombytes 往返; byteswap.
# 期望: 紧凑同质数组; tobytes 长度 == itemsize*len; byteswap 翻转字节序.
# 为什么: 验证缓冲区协议与原生类型存储.
# =====================================================================
import array

_ai = array.array("i", [1, 2, 3])
chk("array_typecode", _ai.typecode == "i" and _ai.itemsize >= 2)
_ai.append(4)
_ai.extend([5, 6])
chk("array_append_extend", _ai.tolist() == [1, 2, 3, 4, 5, 6])
# tobytes / frombytes 往返.
_ab = array.array("h", [256, 1])
_bytes = _ab.tobytes()
_ar = array.array("h")
_ar.frombytes(_bytes)
chk("array_tobytes_frombytes", _ar.tolist() == [256, 1] and len(_bytes) == 4)
# byteswap — 翻转每个元素的字节序.
_bs = array.array("h", [1])
_bs.byteswap()
chk("array_byteswap", _bs[0] == 256)
# 各 typecode 可用 (b/B/h/H/i/I/l/L/q/Q/f/d/u).
_codes = "bBhHiIlLqQfd"
_all_codes = all((lambda c=c: array.array(c, []) is not None)() for c in _codes)
chk("array_all_codes", _all_codes)
# float / double 数组.
_af = array.array("d", [1.5, 2.5])
chk("array_double", _af.tolist() == [1.5, 2.5] and _af.itemsize == 8)
# index/count/insert/remove.
_aop = array.array("i", [10, 20, 20, 30])
chk("array_ops", _aop.count(20) == 2 and _aop.index(30) == 3)
_aop.insert(0, 5)
_aop.remove(20)
chk("array_insert_remove", _aop.tolist() == [5, 10, 20, 30])
# buffer_info — (内存地址, 元素个数); 长度匹配 len.
_abi = array.array("i", [1, 2, 3, 4])
_addr, _count = _abi.buffer_info()
chk("array_buffer_info", _count == 4 and len(_abi) == 4 and isinstance(_addr, int))
# pop / reverse — 序列变更 (pop 默认末尾, 可索引).
_apr = array.array("i", [1, 2, 3, 4])
chk("array_pop", _apr.pop() == 4 and _apr.pop(0) == 1 and _apr.tolist() == [2, 3])
_apr.reverse()
chk("array_reverse", _apr.tolist() == [3, 2])
# frombytes 字节数非 itemsize 倍数 -> ValueError (错误路径).
try:
    array.array("i").frombytes(b"\x01\x02\x03"); _afe = False
except ValueError:
    _afe = True
chk("array_frombytes_error", _afe)
# array.clear() (3.13+).
if hasattr(array.array("i"), "clear"):
    _acl = array.array("i", [1, 2, 3])
    _acl.clear()
    chk("array_clear", _acl.tolist() == [])
else:
    chk("array_clear", True, "(skip: needs 3.13)")


# =====================================================================
# collections.abc — https://docs.python.org/3/library/collections.abc.html
# 怎么测: 内置类型对各 ABC 的 isinstance 关系; 注册关系.
# 期望: list 是 MutableSequence; dict 是 MutableMapping; set 是 MutableSet; 等.
# 为什么: 验证抽象基类的虚拟子类注册在 starry 上一致.
# =====================================================================
import collections.abc as cabc

chk("abc_sequence", isinstance([1], cabc.Sequence) and isinstance((1,), cabc.Sequence)
    and isinstance("s", cabc.Sequence) and not isinstance({1}, cabc.Sequence))
chk("abc_mutable_sequence", isinstance([1], cabc.MutableSequence) and not isinstance((1,), cabc.MutableSequence))
chk("abc_mapping", isinstance({}, cabc.Mapping) and isinstance({}, cabc.MutableMapping))
chk("abc_set", isinstance(set(), cabc.MutableSet) and isinstance(frozenset(), cabc.Set)
    and not isinstance(frozenset(), cabc.MutableSet))
chk("abc_iterable", isinstance([1], cabc.Iterable) and isinstance(iter([1]), cabc.Iterator)
    and isinstance((x for x in []), cabc.Generator))
# Reversible (3.6+): list/tuple/dict/deque 可逆, set 不可逆.
chk("abc_reversible", isinstance([1], cabc.Reversible) and isinstance((1,), cabc.Reversible)
    and isinstance({}, cabc.Reversible) and isinstance(deque(), cabc.Reversible)
    and not isinstance(set(), cabc.Reversible))
chk("abc_callable", isinstance(len, cabc.Callable) and isinstance(lambda: 0, cabc.Callable))
chk("abc_hashable", isinstance(1, cabc.Hashable) and isinstance((1,), cabc.Hashable)
    and not isinstance([1], cabc.Hashable))
chk("abc_container_sized", isinstance([1], cabc.Container) and isinstance([1], cabc.Sized))
chk("abc_deque_counter", isinstance(deque(), cabc.MutableSequence)
    and isinstance(Counter(), cabc.Mapping))
chk("abc_keysview", isinstance({}.keys(), cabc.KeysView) and isinstance({}.values(), cabc.ValuesView)
    and isinstance({}.items(), cabc.ItemsView))
# range / bytes / memoryview 也是 Sequence (但不可变).
chk("abc_more_sequences", isinstance(range(3), cabc.Sequence)
    and isinstance(b"x", cabc.Sequence) and isinstance(memoryview(b"x"), cabc.Sequence)
    and not isinstance(range(3), cabc.MutableSequence))


# .register() — 虚拟子类注册 (无继承却 isinstance 为真); 验证 ABC 机制本身.
class _FakeSeq:
    def __getitem__(self, i):
        raise IndexError

    def __len__(self):
        return 0


chk("abc_register_before", not isinstance(_FakeSeq(), cabc.Sequence))
cabc.Sequence.register(_FakeSeq)
chk("abc_register_after", isinstance(_FakeSeq(), cabc.Sequence)
    and issubclass(_FakeSeq, cabc.Sequence))
# Set mixin 提供集合代数运算 (frozenset 经 Set ABC).
chk("abc_set_algebra", (frozenset({1, 2}) & frozenset({2, 3})) == frozenset({2})
    and (frozenset({1}) | frozenset({2})) == frozenset({1, 2})
    and frozenset({1, 2}).isdisjoint(frozenset({3})))


# =====================================================================
# 3.14-only numeric/collection touches (PEP-guarded, syntax-isolated)
# fractions/decimal/collections 在 3.14 无新增 *语法*; 这里仅占位 t-string
# 在数值格式化中的可用性 (PEP 750) 作为版本门控示例.
# =====================================================================
def _gated(name, min_ver, code, probe):
    if sys.version_info < min_ver:
        chk(name, True, "(skip: needs %d.%d)" % (min_ver[0], min_ver[1]))
        return
    ns = {}
    try:
        exec(code, ns)
    except SyntaxError:
        chk(name, True, "(skip: syntax absent)")
        return
    chk(name, probe(ns))


# PEP 750 (3.14): t-string 模板可承载数值插值, .values 持原值 (未格式化).
_gated(
    "pep750_tstring_numeric", (3, 14),
    "from fractions import Fraction as F\n"
    "v = F(1, 3)\n"
    "tmpl = t'val={v}'\n"
    "R = (type(tmpl).__name__, tmpl.values[0])\n",
    lambda ns: ns["R"][0] == "Template" and ns["R"][1] == __import__("fractions").Fraction(1, 3),
)


print(("PY_NUMCOLL_OK") if _ok else ("PY_NUMCOLL_FAIL"))
sys.exit(0 if _ok else 1)
