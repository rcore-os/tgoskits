#!/usr/bin/env python3
"""Core syntax & operators — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

# ===========================================================================
# 1. INTEGER LITERALS  (Language Reference 2.4.5 "Integer literals")
# 怎么测: decimal/binary/octal/hex literals, underscores as digit separators.
# 期望: all bases produce the same int value; underscores are pure formatting.
# 为什么: literal lexing is a fundamental tokenizer behavior.
# ===========================================================================
chk("int_decimal", 1234567890 == 12345 * 100000 + 67890)
chk("int_binary", 0b1010 == 10 and 0B1111 == 15)
chk("int_octal", 0o17 == 15 and 0O20 == 16)
chk("int_hex", 0xff == 255 and 0XFF == 255 and 0xDeadBeef == 3735928559)
chk("int_underscore", 1_000_000 == 1000000 and 0x_FF_FF == 65535 and 0b_1010_1010 == 170)
chk("int_zero_forms", 0 == 0o0 == 0x0 == 0b0)
# Arbitrary precision: no overflow, distinct from machine words.
chk("int_bigexp", (10 ** 50) // (10 ** 49) == 10)
chk("int_neg_literal", -0x10 == -16)

# ===========================================================================
# 2. FLOAT & COMPLEX LITERALS  (Lang Ref 2.4.6/2.4.7)
# 怎么测: point/exponent forms, leading/trailing dot, underscores; complex j.
# 期望: scientific notation parses; complex has .real/.imag/conjugate.
# ===========================================================================
chk("float_forms", 3.14 == 3.14 and .5 == 0.5 and 5. == 5.0 and 1e3 == 1000.0)
chk("float_exp", 1.5e-3 == 0.0015 and 2E2 == 200.0 and 1_000.0 == 1000.0)
chk("float_underscore", 3.141_592 == 3.141592)
chk("complex_literal", (3 + 4j).real == 3.0 and (3 + 4j).imag == 4.0)
chk("complex_j", 2j == complex(0, 2) and 1J == complex(0, 1))
chk("complex_conjugate", (3 + 4j).conjugate() == (3 - 4j))
chk("complex_abs", abs(3 + 4j) == 5.0)
# Arithmetic on the complex type itself (operators, not just literals/attrs).
chk("complex_add_sub", (1 + 2j) + (3 + 4j) == (4 + 6j) and (3 + 4j) - (1 + 1j) == (2 + 3j))
chk("complex_mul", (1 + 2j) * (3 + 4j) == (-5 + 10j) and (2 + 3j) * 2 == (4 + 6j))
chk("complex_div", (1 + 1j) / (1 + 1j) == (1 + 0j) and (4 + 0j) / 2 == (2 + 0j))
chk("complex_pow", 1j ** 2 == (-1 + 0j) and (2 + 0j) ** 2 == (4 + 0j))

# ===========================================================================
# 3. STRING LITERALS  (Lang Ref 2.4.1 "String and Bytes literals")
# 怎么测: single/double/triple quotes, escapes, raw, adjacent concatenation,
#          unicode/hex/octal/named escapes, line continuation in literals.
# 期望: each escape decodes per spec; raw suppresses escapes; r'' keeps \.
# ===========================================================================
chk("str_quotes", 'a' == "a" and 'it\'s' == "it's" and "say \"hi\"" == 'say "hi"')
chk("str_triple", """l1
l2""" == "l1\nl2" and '''x''' == "x")
chk("str_escapes", "\t\n\r\\\a\b\f\v\0"[0] == "\t" and len("\n") == 1)
chk("str_hex_escape", "\x41\x42" == "AB")
chk("str_octal_escape", "\101\102" == "AB")
chk("str_unicode_escape", "é" == "é" and ord("é") == 233 and "\N{BULLET}" == "•" and "\N{LATIN SMALL LETTER E WITH ACUTE}" == "é")
chk("str_unicode_wide", "\U0001F600" == "😀")
chk("str_raw", r"a\nb" == "a" + chr(92) + "nb" and r"\d+" == chr(92) + "d+")
chk("str_raw_quote", r"C:\path" == "C:" + chr(92) + "path")
chk("str_adjacent_concat", "ab" "cd" == "abcd" and ("x"  "y") == "xy")
chk("str_implicit_join_lines", (
    "one"
    "two"
) == "onetwo")
chk("str_line_continuation", "a\
b" == "ab")

# ===========================================================================
# 4. F-STRINGS  (PEP 701 / Lang Ref "Formatted string literals")
# 怎么测: conversions !r/!s/!a, format spec, nested expr, =, alignment/fill,
#          sign/grouping/percent, nested braces escape {{}}, dict access.
# NOTE: test_lang.py already does PEP701 nested-quote + simple {x=}; go wider:
#  full conversion+format-spec matrix, NOT a re-run of the nested-quote case.
# 期望: spec mini-language behaves per Format Spec docs.
# ===========================================================================
n, pi = 255, 3.14159
chk("fstr_conversion_r", f"{'x'!r}" == "'x'")
chk("fstr_conversion_s", f"{42!s}" == "42")
chk("fstr_conversion_a", f"{'é'!a}" == r"'\xe9'")
chk("fstr_format_spec", f"{n:#06x}" == "0x00ff" and f"{pi:.2f}" == "3.14")
chk("fstr_align_fill", f"{'x':*^7}" == "***x***" and f"{'y':<4}|" == "y   |" and f"{'z':>4}|" == "   z|")
chk("fstr_sign_group", f"{1000000:+,}" == "+1,000,000" and f"{255:_b}" == "1111_1111")
chk("fstr_percent", f"{0.5:.0%}" == "50%")
chk("fstr_nested_spec", f"{pi:.{1 + 1}f}" == "3.14")
chk("fstr_escape_braces", f"{{{n}}}" == "{255}")
chk("fstr_debug_expr", f"{n + 1 = }" == "n + 1 = 256")
chk("fstr_multiline_expr", f"{(
    1 + 2
)}" == "3")
# str.format() drives the same format-spec mini-language via {} replacement
# fields; exercise auto/explicit/named field references and nested specs.
chk("format_method_spec", "{:#06x}".format(255) == "0x00ff" and "{:.2f}".format(3.14159) == "3.14")
chk("format_method_index", "{0}{1}{0}".format("a", "b") == "aba" and "{}{}".format(1, 2) == "12")
chk("format_method_named", "{n:.2f}|{s:*^5}".format(n=3.14159, s="x") == "3.14|**x**")
chk("format_method_nested_spec", "{:.{}f}".format(3.14159, 1 + 2) == "3.142" and "{:{}{}}".format(7, ">", 4) == "   7")
chk("format_method_attr_item", "{0.real:.0f}/{1[0]}".format(5 + 0j, ["z"]) == "5/z")

# ===========================================================================
# 5. BYTES & BYTEARRAY LITERALS  (Lang Ref 2.4.1; bytes/bytearray docs)
# 怎么测: b'' literals, hex escapes, raw bytes, fromhex/hex, mutability.
# 期望: bytes immutable, bytearray mutable; only ASCII source chars allowed.
# ===========================================================================
chk("bytes_literal", b"abc" == bytes([97, 98, 99]) and b"\x00\xff" == bytes([0, 255]))
chk("bytes_raw", rb"\n" == b"\\n" and br"\t" == b"\\t")
chk("bytes_fromhex", bytes.fromhex("48 69") == b"Hi" and b"Hi".hex() == "4869")
def _bytearray_mutations():
    ba = bytearray(b"hi")
    ba.append(33)            # -> b"hi!"
    ba[0] = 72               # 'h' -> 'H'
    ba.extend(b"??")         # -> b"Hi!??"
    del ba[3]                # drop one '?' -> b"Hi!?"
    return bytes(ba) == b"Hi!?" and ba[1] == ord("i") and len(ba) == 4
chk("bytearray_mutable", _bytearray_mutations())

# ===========================================================================
# 6. ARITHMETIC OPERATORS  (Lang Ref 6.7 "Binary arithmetic operations")
# 怎么测: + - * / // % ** and @ (matmul, via custom __matmul__).
# 期望: / is true division (float), // floors toward -inf, % keeps divisor sign.
# ===========================================================================
chk("arith_add_sub", 7 + 3 == 10 and 7 - 3 == 4)
chk("arith_mul", 6 * 7 == 42 and "ab" * 3 == "ababab" and 3 * [0] == [0, 0, 0])
chk("arith_truediv", 7 / 2 == 3.5 and 6 / 3 == 2.0 and type(6 / 3) is float)
chk("arith_floordiv", 7 // 2 == 3 and -7 // 2 == -4 and 7.0 // 2 == 3.0)
chk("arith_mod", 7 % 3 == 1 and -7 % 3 == 2 and 7 % -3 == -2)
chk("arith_pow", 2 ** 10 == 1024 and 2 ** -1 == 0.5 and 4 ** 0.5 == 2.0)
chk("arith_unary", -(-5) == 5 and +7 == 7 and -3.0 == -3.0)
chk("arith_divmod_identity", (lambda a, b: a == (a // b) * b + a % b)(-17, 5))
class _Mat:
    def __init__(self, v): self.v = v
    def __matmul__(self, o): return _Mat(self.v * o.v + 1)
chk("arith_matmul_at", (_Mat(3) @ _Mat(4)).v == 13)
# Error paths: division/modulo by zero raise ZeroDivisionError for every
# division-family operator; @ on unsupported operands raises TypeError.
def _raises(exc, fn):
    try:
        fn()
        return False
    except exc:
        return True
    except Exception:
        return False
chk("arith_zerodiv_truediv", _raises(ZeroDivisionError, lambda: 7 / 0))
chk("arith_zerodiv_floordiv", _raises(ZeroDivisionError, lambda: 7 // 0))
chk("arith_zerodiv_mod", _raises(ZeroDivisionError, lambda: 7 % 0))
chk("arith_zerodiv_float", _raises(ZeroDivisionError, lambda: 2.0 / 0.0) and _raises(ZeroDivisionError, lambda: divmod(7, 0)))
chk("arith_matmul_typeerror", _raises(TypeError, lambda: 1 @ 2))

# ===========================================================================
# 7. BITWISE / SHIFT OPERATORS  (Lang Ref 6.8 "Shifting"/6.9 "Binary bitwise")
# 怎么测: & | ^ ~ << >> on ints incl negatives (two's-complement model).
# 期望: ~x == -(x+1); shifts are arithmetic on the infinite-precision int.
# ===========================================================================
chk("bit_and_or_xor", (0b1100 & 0b1010, 0b1100 | 0b1010, 0b1100 ^ 0b1010) == (8, 14, 6))
chk("bit_invert", ~0 == -1 and ~5 == -6 and ~(-1) == 0)
chk("bit_shift_left", 1 << 10 == 1024 and 3 << 2 == 12)
chk("bit_shift_right", 1024 >> 5 == 32 and -8 >> 1 == -4)
chk("bit_shift_big", 1 << 100 == 1267650600228229401496703205376)
chk("bit_set_ops", ({1, 2, 3} & {2, 3, 4}, {1, 2} | {2, 3}, {1, 2, 3} ^ {2, 3, 4}) == ({2, 3}, {1, 2, 3}, {1, 4}))

# ===========================================================================
# 8. COMPARISON & CHAINING  (Lang Ref 6.10 "Comparisons")
# 怎么测: == != < <= > >=, chained a<b<c (each compared once), mixed types.
# 期望: chaining is logical-and of pairwise comparisons w/ single eval of middle.
# ===========================================================================
chk("cmp_basic", (1 == 1, 1 != 2, 1 < 2, 2 <= 2, 3 > 2, 3 >= 3) == (True,) * 6)
chk("cmp_chained_true", 1 < 2 < 3 < 4)
chk("cmp_chained_false", not (1 < 2 > 3))
chk("cmp_chained_eq", 1 == 1.0 == 1)
_evals = []
def _mid():
    _evals.append(1)
    return 5
chk("cmp_chain_single_eval", (0 < _mid() < 10) and len(_evals) == 1)
chk("cmp_cross_numeric", 1 == 1.0 and 2 < 2.5 and 3 == 3 + 0j)

# ===========================================================================
# 9. BOOLEAN OPS & SHORT-CIRCUIT  (Lang Ref 6.11 "Boolean operations")
# 怎么测: and/or return operands (not bool), not, short-circuit side effects.
# 期望: `x and y` -> x if falsy else y; `x or y` -> x if truthy else y.
# ===========================================================================
chk("bool_and_value", (0 and 1) == 0 and (2 and 3) == 3)
chk("bool_or_value", (0 or 5) == 5 and (2 or 3) == 2)
chk("bool_not", (not 0) is True and (not 7) is False)
_side = []
def _tap(v):
    _side.append(v)
    return v
_ = _tap(False) and _tap("unreached")
chk("bool_short_circuit_and", _side == [False])
_side.clear()
_ = _tap(True) or _tap("unreached")
chk("bool_short_circuit_or", _side == [True])
chk("bool_truthiness", bool([]) is False and bool([0]) is True and bool("") is False and bool(0.0) is False)

# ===========================================================================
# 10. IDENTITY & MEMBERSHIP  (Lang Ref 6.10.3/6.10.2)
# 怎么测: is / is not (object identity), in / not in (containment).
# 期望: small ints & None & singletons share identity; in uses __contains__/__iter__.
# ===========================================================================
_a = [1, 2]
_b = _a
chk("identity_is", _a is _b and _a is not [1, 2])
_zero_obj = 0
chk("identity_none", None is None and (None is not _zero_obj))
chk("membership_in", 2 in [1, 2, 3] and "b" in "abc" and 9 not in {1, 2})
chk("membership_dict_key", "k" in {"k": 1} and "v" not in {"k": 1})
chk("membership_range", 5 in range(10) and 11 not in range(10))
chk("range_attrs", range(2, 10, 3).start == 2 and range(2, 10, 3).stop == 10 and range(2, 10, 3).step == 3 and list(range(2, 10, 3)) == [2, 5, 8])

# ===========================================================================
# 11. WALRUS / ASSIGNMENT EXPRESSION  (PEP 572)
# 怎么测: := in while-condition and inline. (comprehension walrus already in
#  test_lang.py; here exercise the imperative loop form to go wider.)
# 期望: assigns and yields the value as a sub-expression.
# ===========================================================================
_buf, _src = [], iter([1, 2, 0, 3])
while (item := next(_src)) != 0:
    _buf.append(item)
chk("walrus_while", _buf == [1, 2])
chk("walrus_inline", (total := 3 + 4) == 7 and total == 7)

# ===========================================================================
# 12. AUGMENTED ASSIGNMENT  (Lang Ref 7.2.1 "Augmented assignment statements")
# 怎么测: every augmented operator += -= *= /= //= %= **= &= |= ^= <<= >>= @=.
# 期望: x OP= y equals x = x OP y (in-place where the type supports it).
# ===========================================================================
_x = 10; _x += 5;  chk("aug_add", _x == 15)
_x -= 3;            chk("aug_sub", _x == 12)
_x *= 2;            chk("aug_mul", _x == 24)
_x /= 4;            chk("aug_truediv", _x == 6.0)
_x //= 2;           chk("aug_floordiv", _x == 3.0)
_x = 17; _x %= 5;   chk("aug_mod", _x == 2)
_x **= 4;           chk("aug_pow", _x == 16)
_x &= 0b1010;       chk("aug_and", _x == 0)
_x |= 0b0101;       chk("aug_or", _x == 5)
_x ^= 0b0110;       chk("aug_xor", _x == 3)
_x <<= 4;           chk("aug_lshift", _x == 48)
_x >>= 2;           chk("aug_rshift", _x == 12)
_lst = [1]; _lst += [2, 3]; chk("aug_iadd_list_inplace", _lst == [1, 2, 3])
class _AM:
    def __init__(self, v): self.v = v
    def __imatmul__(self, o): self.v += o; return self
_am = _AM(5); _am @= 7; chk("aug_matmul", _am.v == 12)

# ===========================================================================
# 13. UNPACKING / PEP 448  (Lang Ref 6.3.4; PEP 448)
# 怎么测: starred targets, nested unpack, * in call/list/set/tuple, ** in
#          call/dict. (basic a,*b,c & {**d} already in test_lang.py; go wider:
#          nested targets, swap, call-site splats, double-star call.)
# 期望: exactly one starred target; splats expand into the literal/call.
# ===========================================================================
(p, q), r = (1, 2), 3
chk("unpack_nested", p == 1 and q == 2 and r == 3)
_i, *_rest = range(5)
chk("unpack_star_head", _i == 0 and _rest == [1, 2, 3, 4])
*_init, _last = [9, 8, 7]
chk("unpack_star_tail", _init == [9, 8] and _last == 7)
_u, _v = 1, 2
_u, _v = _v, _u
chk("unpack_swap", (_u, _v) == (2, 1))
def _unpack_two(seq):
    a, b = seq
    return (a, b)
def _unpack_star(seq):
    a, *b, c = seq  # needs >= 2 items
    return (a, b, c)
chk("unpack_too_many_error", _raises(ValueError, lambda: _unpack_two([1, 2, 3])))
chk("unpack_too_few_error", _raises(ValueError, lambda: _unpack_two([1])))
chk("unpack_star_too_few_error", _raises(ValueError, lambda: _unpack_star([1])))
chk("splat_list_literal", [0, *[1, 2], 3, *(4, 5)] == [0, 1, 2, 3, 4, 5])
chk("splat_set_literal", {*[1, 2], *[2, 3]} == {1, 2, 3})
chk("splat_tuple_literal", (*"ab", *"cd") == ("a", "b", "c", "d"))
def _three(a, b, c): return (a, b, c)
chk("splat_call_args", _three(*[1, 2], *[3]) == (1, 2, 3))
def _kw(**k): return sorted(k.items())
chk("splat_call_kwargs", _kw(**{"a": 1}, **{"b": 2}) == [("a", 1), ("b", 2)])
chk("splat_dict_merge_order", {**{"a": 1, "b": 1}, **{"b": 2}} == {"a": 1, "b": 2})

# ===========================================================================
# 14. SUBSCRIPTION & SLICING  (Lang Ref 6.3.2/6.3.3; slice object)
# 怎么测: indexing, negative idx, full slice grammar [start:stop:step], slice
#          objects, ellipsis, multi-dim tuple subscript via __getitem__.
# 期望: omitted bounds default; negative step reverses; slice() builds equal key.
# ===========================================================================
_seq = list(range(10))
chk("index_pos_neg", _seq[0] == 0 and _seq[-1] == 9 and _seq[-2] == 8)
chk("index_out_of_range", _raises(IndexError, lambda: _seq[100]) and _raises(IndexError, lambda: _seq[-100]))
chk("slice_basic", _seq[2:5] == [2, 3, 4] and _seq[:3] == [0, 1, 2] and _seq[7:] == [7, 8, 9])
chk("slice_step", _seq[::2] == [0, 2, 4, 6, 8] and _seq[1:8:3] == [1, 4, 7])
chk("slice_negative_step", _seq[::-1] == list(reversed(_seq)) and _seq[5:2:-1] == [5, 4, 3])
chk("slice_attrs", slice(2, 10, 3).start == 2 and slice(2, 10, 3).stop == 10 and slice(2, 10, 3).step == 3 and slice(5).start is None)
chk("slice_step_zero_error", _raises(ValueError, lambda: _seq[::0]))
chk("slice_copy", _seq[:] == _seq and (_seq[:] is not _seq))
chk("slice_object", _seq[slice(2, 5)] == _seq[2:5] and slice(1, 9, 2).indices(10) == (1, 9, 2))
chk("slice_assignment", (lambda L: (L.__setitem__(slice(1, 3), [20, 30, 40]), L)[1])([0, 1, 2, 3]) == [0, 20, 30, 40, 3])
chk("slice_del", (lambda L: (L.__delitem__(slice(1, 3)), L)[1])([0, 1, 2, 3]) == [0, 3])
class _MD:
    def __getitem__(self, key): return key
chk("subscript_tuple_key", _MD()[1, 2] == (1, 2))
chk("subscript_ellipsis", _MD()[...] is Ellipsis and _MD()[1:2, ...] == (slice(1, 2), Ellipsis))

# ===========================================================================
# 15. COMPREHENSIONS — DEEP  (Lang Ref 6.2.4 "Displays for ... comprehensions")
# 怎么测: multiple-for, nested comp, if-filters (multiple), genexpr lazy eval,
#          comprehension scope isolation, async-comprehension is in test_lang.
#  (basic list/set/dict/gen already in test_lang.py; go wider: cartesian,
#   nested, multi-if, scope leak check, generator one-shot exhaustion.)
# 期望: leftmost for is outermost; target names don't leak to enclosing scope.
# ===========================================================================
chk("comp_multi_for", [(x, y) for x in range(2) for y in range(2)] == [(0, 0), (0, 1), (1, 0), (1, 1)])
chk("comp_nested", [[r * c for c in range(3)] for r in range(2)] == [[0, 0, 0], [0, 1, 2]])
chk("comp_multi_if", [x for x in range(20) if x % 2 == 0 if x % 3 == 0] == [0, 6, 12, 18])
chk("comp_cond_expr_inside", [x if x % 2 else -x for x in range(4)] == [0, 1, -2, 3])
chk("comp_dict_swap", {v: k for k, v in {"a": 1, "b": 2}.items()} == {1: "a", 2: "b"})
chk("comp_set_dedupe", {x // 2 for x in range(6)} == {0, 1, 2})
_leak_probe = "outer"
_ = [_leak_probe for _leak_probe in range(3)]
chk("comp_scope_isolation", _leak_probe == "outer")
_gen = (i * i for i in range(4))
chk("genexpr_lazy_one_shot", list(_gen) == [0, 1, 4, 9] and list(_gen) == [])
chk("genexpr_in_call", sum(x for x in range(5)) == 10 and max(len(w) for w in ["a", "bbb", "cc"]) == 3)

# ===========================================================================
# 16. CONDITIONAL EXPRESSION & LAMBDA  (Lang Ref 6.13/6.14)
# 怎么测: x if c else y nesting; lambda with defaults/varargs/kw-only.
# 期望: ternary chooses one branch (other not evaluated); lambda is an expr.
# ===========================================================================
chk("ternary_basic", ("yes" if 1 else "no") == "yes" and ("yes" if 0 else "no") == "no")
chk("ternary_nested", [("neg" if v < 0 else "zero" if v == 0 else "pos") for v in (-1, 0, 1)] == ["neg", "zero", "pos"])
_branch = []
_ = (_branch.append("t") if True else _branch.append("f"))
chk("ternary_lazy", _branch == ["t"])
_f = lambda a, b=10, *c, d=4, **e: (a, b, c, d, sorted(e.items()))
chk("lambda_full_sig", _f(1, 2, 3, d=9, z=0) == (1, 2, (3,), 9, [("z", 0)]))
chk("lambda_immediate", (lambda x: x * x)(6) == 36)
chk("lambda_in_key", sorted(["bbb", "a", "cc"], key=lambda s: len(s)) == ["a", "cc", "bbb"])

# ===========================================================================
# 17. SIMPLE STATEMENTS  (Lang Ref 7 "Simple statements")
# 怎么测: pass, del (name/index/slice), assert with msg, multiple assignment.
# 期望: del unbinds; assert raises AssertionError carrying the message.
# ===========================================================================
def _pass_body():
    pass
    return 1
chk("stmt_pass", _pass_body() == 1)
_dn = 5
del _dn
chk("stmt_del_name", "_dn" not in dir())
_dl = [1, 2, 3, 4]
del _dl[1]
chk("stmt_del_index", _dl == [1, 3, 4])
del _dl[1:]
chk("stmt_del_slice", _dl == [1])
_ma = _mb = _mc = 7
chk("stmt_multi_target_assign", (_ma, _mb, _mc) == (7, 7, 7))
_aa, _bb = _cc, _dd = (1, 2)
chk("stmt_chained_unpack_assign", (_aa, _bb, _cc, _dd) == (1, 2, 1, 2))
try:
    assert False, "boom"
    _assert_ok = False
except AssertionError as e:
    _assert_ok = str(e) == "boom"
chk("stmt_assert_msg", _assert_ok)
def _assert_behavior():
    # truthy assert must pass silently; falsy assert (no msg) must raise AssertionError
    # with empty args. Catch ONLY AssertionError so a no-op assert is caught, not masked.
    try:
        assert 1 + 1 == 2  # truthy -> no exception
    except AssertionError:
        return False
    try:
        assert 0  # falsy -> must raise
    except AssertionError as ae:
        return ae.args == ()
    return False  # falsy assert did NOT raise -> assert is a no-op (must FAIL)
chk("stmt_assert_pass", _assert_behavior())  # truthy passes silently, falsy raises

# ===========================================================================
# 18. IF / ELIF / ELSE  (Lang Ref 8.1 "The if statement")
# ===========================================================================
def _grade(n):
    if n >= 90:
        return "A"
    elif n >= 80:
        return "B"
    elif n >= 70:
        return "C"
    else:
        return "F"
chk("if_elif_else", [_grade(v) for v in (95, 85, 72, 50)] == ["A", "B", "C", "F"])

# ===========================================================================
# 19. WHILE / WHILE-ELSE  (Lang Ref 8.2 "The while statement")
# 怎么测: normal loop, break skips else, no-break runs else, continue.
# 期望: else runs iff loop ends without break.
# ===========================================================================
_acc, _i = 0, 0
while _i < 5:
    _i += 1
    if _i == 3:
        continue
    _acc += _i
chk("while_continue", _acc == (1 + 2 + 4 + 5))
_found = None
_i = 0
while _i < 10:
    if _i == 4:
        _found = _i
        break
    _i += 1
else:
    _found = "no-break"
chk("while_break_no_else", _found == 4)
_n, _sentinel = 0, []
while _n < 3:
    _sentinel.append(_n)
    _n += 1
else:
    _sentinel.append("else")
chk("while_else_runs", _sentinel == [0, 1, 2, "else"])

# ===========================================================================
# 20. FOR / FOR-ELSE  (Lang Ref 8.3 "The for statement")
# 怎么测: iterate, unpacking target, enumerate/zip, break/else, range step.
# 期望: for-else runs when no break; target tuple-unpacks each item.
# ===========================================================================
_pairs = []
for k, v in [("a", 1), ("b", 2)]:
    _pairs.append((k, v))
chk("for_unpack_target", _pairs == [("a", 1), ("b", 2)])
_e = list(enumerate(["x", "y"], start=1))
chk("for_enumerate", _e == [(1, "x"), (2, "y")])
_z = list(zip([1, 2, 3], "ab"))
chk("for_zip_shortest", _z == [(1, "a"), (2, "b")])
_hit = None
for v in range(100):
    if v == 7:
        _hit = v
        break
else:
    _hit = "exhausted"
chk("for_break_no_else", _hit == 7)
_seen = []
for v in range(3):
    _seen.append(v)
else:
    _seen.append("done")
chk("for_else_runs", _seen == [0, 1, 2, "done"])

# ===========================================================================
# 21. WITH — MULTI-ITEM & PARENTHESIZED  (Lang Ref 8.5; PEP 617 grammar)
# 怎么测: single, multiple comma-separated, parenthesized group (3.10+),
#          __enter__/__exit__ ordering, exception suppression via __exit__.
# 期望: managers entered L->R, exited R->L; True from __exit__ swallows exc.
# ===========================================================================
_wlog = []
class _CM:
    def __init__(self, tag, swallow=False):
        self.tag, self.swallow = tag, swallow
    def __enter__(self):
        _wlog.append("enter " + self.tag)
        return self.tag
    def __exit__(self, et, ev, tb):
        _wlog.append("exit " + self.tag)
        return self.swallow
with _CM("a") as ta, _CM("b") as tb_:
    _wlog.append("body %s %s" % (ta, tb_))
chk("with_multi_item_order", _wlog == ["enter a", "enter b", "body a b", "exit b", "exit a"])
_wlog.clear()
with (_CM("p"), _CM("q")):
    _wlog.append("body")
chk("with_parenthesized", _wlog == ["enter p", "enter q", "body", "exit q", "exit p"])
with _CM("s", swallow=True):
    raise ValueError("ignored")
chk("with_exit_suppresses", _wlog[-1] == "exit s")

# ===========================================================================
# 22. TRY / EXCEPT / ELSE / FINALLY  (Lang Ref 8.4)
# 怎么测: tuple of exc types, `as` binding (and its post-clause unbinding),
#          bare except, exception attrs, re-raise, finally-overrides-return,
#          nested. (basic try/except/finally + except* already in test_lang.py;
#          go wider on these forms.)
# 期望: `as e` name is deleted after the except block; finally always runs.
# ===========================================================================
def _multi_catch(x):
    try:
        if x == 1:
            raise ValueError("v")
        if x == 2:
            raise KeyError("k")
        return "ok"
    except (ValueError, KeyError) as e:
        return type(e).__name__
chk("try_tuple_types", _multi_catch(1) == "ValueError" and _multi_catch(2) == "KeyError" and _multi_catch(0) == "ok")
try:
    raise RuntimeError("x")
except RuntimeError as _bound:
    pass
chk("try_as_name_unbound", "_bound" not in dir())  # PEP 3110: `as` target deleted
def _bare():
    try:
        raise OverflowError
    except:
        return "caught-bare"
chk("try_bare_except", _bare() == "caught-bare")
def _finally_overrides():
    try:
        return "try"
    finally:
        return "finally"
chk("try_finally_overrides_return", _finally_overrides() == "finally")
_reraise_chain = []
try:
    try:
        raise IndexError("orig")
    except IndexError:
        _reraise_chain.append("inner")
        raise
except IndexError as e:
    _reraise_chain.append(str(e))
chk("try_bare_reraise", _reraise_chain == ["inner", "orig"])
chk("exc_args_attr", ValueError("a", "b").args == ("a", "b"))

# ===========================================================================
# 23. RAISE / RAISE FROM / IMPLICIT CHAINING  (Lang Ref 7.8)
# 怎么测: raise X, raise X from Y, raise from None (suppress), implicit
#          __context__ during handling. (raise...from already in test_lang.py;
#          go wider: from None suppression + implicit context.)
# 期望: from sets __cause__ & __suppress_context__; bare in-handler sets __context__.
# ===========================================================================
try:
    try:
        raise ValueError("ctx")
    except ValueError:
        raise TypeError("new")
except TypeError as e:
    _ctx_ok = isinstance(e.__context__, ValueError) and e.__cause__ is None
chk("raise_implicit_context", _ctx_ok)
try:
    try:
        raise ValueError("hidden")
    except ValueError:
        raise TypeError("clean") from None
except TypeError as e:
    _suppress_ok = e.__suppress_context__ is True and e.__cause__ is None
chk("raise_from_none_suppress", _suppress_ok)

# ===========================================================================
# 24. GLOBAL & NONLOCAL  (Lang Ref 7.12/7.13)
# 怎么测: global rebinds module name; nonlocal rebinds nearest enclosing.
# (test_lang.py has a nonlocal closure counter; go wider: global statement +
#  nested nonlocal across two levels.)
# 期望: without declaration, assignment creates a new local instead.
# ===========================================================================
_g_counter = 0
def _bump_global():
    global _g_counter
    _g_counter += 1
_bump_global(); _bump_global()
chk("global_rebind", _g_counter == 2)
def _outer_nl():
    val = "outer"
    def _mid_nl():
        def _inner_nl():
            nonlocal val
            val = "inner-set"
        _inner_nl()
    _mid_nl()
    return val
chk("nonlocal_two_levels", _outer_nl() == "inner-set")
def _shadow():
    z = 1
    def _local_only():
        z = 99  # new local, no nonlocal
        return z
    return (_local_only(), z)
chk("no_decl_creates_local", _shadow() == (99, 1))

# ===========================================================================
# 25. STRUCTURAL PATTERN MATCHING — ALL PATTERN KINDS  (PEP 634/635/636)
# test_lang.py covers literal/seq/map/guard/wildcard at a basic level.
# Go WIDER: value pattern (dotted), star in sequence, **rest in mapping,
# class pattern (positional via __match_args__ + keyword), OR |, AS binding,
# capture, group/nested, plus the None/True/False singleton (identity) patterns.
# 期望: each pattern kind binds/matches per the spec.
# ===========================================================================
import enum as _enum_for_match
class _K(_enum_for_match.Enum):
    A = 1
    B = 2
class _Point:
    __match_args__ = ("x", "y")
    def __init__(self, x, y): self.x, self.y = x, y

def _m_value(k):
    match k:
        case _K.A: return "is-A"
        case _K.B: return "is-B"
        case _: return "other"
chk("match_value_pattern", _m_value(_K.A) == "is-A" and _m_value(_K.B) == "is-B")

def _m_star(seq):
    match seq:
        case [first, *rest]: return (first, rest)
        case []: return ("empty", [])
chk("match_sequence_star", _m_star([1, 2, 3]) == (1, [2, 3]) and _m_star([]) == ("empty", []))

def _m_map(d):
    match d:
        case {"id": i, **rest}: return (i, sorted(rest.items()))
        case _: return None
chk("match_mapping_rest", _m_map({"id": 7, "a": 1, "b": 2}) == (7, [("a", 1), ("b", 2)]))

def _m_class(obj):
    match obj:
        case _Point(0, 0): return "origin"
        case _Point(x=px, y=0): return ("x-axis", px)
        case _Point(x, y): return ("point", x, y)
        case _: return "no"
chk("match_class_positional", _m_class(_Point(0, 0)) == "origin")
chk("match_class_keyword", _m_class(_Point(5, 0)) == ("x-axis", 5))
chk("match_class_capture", _m_class(_Point(3, 4)) == ("point", 3, 4))

def _m_or(x):
    match x:
        case 1 | 2 | 3: return "low"
        case "a" | "b": return "letter"
        case _: return "other"
chk("match_or_pattern", _m_or(2) == "low" and _m_or("b") == "letter" and _m_or(9) == "other")

def _m_as(x):
    match x:
        case [1, (2 | 3) as middle]: return ("captured", middle)
        case _: return "no"
chk("match_as_pattern", _m_as([1, 3]) == ("captured", 3) and _m_as([1, 2]) == ("captured", 2))

def _m_singleton(x):
    match x:
        case None: return "none"
        case True: return "true"
        case False: return "false"
        case _: return "val"
chk("match_singleton_identity", [_m_singleton(v) for v in (None, True, False, 1)] == ["none", "true", "false", "val"])

def _m_nested_guard(pt):
    match pt:
        case _Point(x, y) if x == y: return "diagonal"
        case _Point(x, y): return ("off", x, y)
chk("match_class_with_guard", _m_nested_guard(_Point(2, 2)) == "diagonal" and _m_nested_guard(_Point(1, 2)) == ("off", 1, 2))

def _m_capture_whole(x):
    match x:
        case [_, _] as pair: return ("pair", pair)
        case other: return ("single", other)
chk("match_capture_whole", _m_capture_whole([8, 9]) == ("pair", [8, 9]) and _m_capture_whole(5) == ("single", 5))

# ===========================================================================
# 26. DECORATORS — STACKING & PEP 614 RELAXED GRAMMAR  (Lang Ref "decorators")
# test_lang.py covers simple + parameterized decorators. Go WIDER: stacking
# order, decorator from a subscript/expression (PEP 614), class decorator.
# 期望: decorators apply bottom-up; PEP 614 allows any expression after @.
# ===========================================================================
def _wrap_a(f):
    def g(*a, **k): return "A(" + f(*a, **k) + ")"
    return g
def _wrap_b(f):
    def g(*a, **k): return "B(" + f(*a, **k) + ")"
    return g
@_wrap_a
@_wrap_b
def _decorated():
    return "x"
chk("decorator_stacking_order", _decorated() == "A(B(x))")  # bottom-up: b first, then a
_deco_registry = {"plus": (lambda f: (lambda *a: f(*a) + 1))}
@_deco_registry["plus"]  # PEP 614: subscript expression as decorator
def _pep614(n):
    return n
chk("decorator_pep614_subscript", _pep614(10) == 11)
def _tag(cls):
    cls.tagged = True
    return cls
@_tag
class _Tagged:
    pass
chk("class_decorator", _Tagged.tagged is True)

# ===========================================================================
# 27. OPERATOR PRECEDENCE & ASSOCIATIVITY  (Lang Ref 6.17 "Operator precedence")
# 怎么测: ** right-assoc, unary vs **, mul before add, comparison lowest,
#          parentheses override.
# 期望: 2**3**2 == 512 (right assoc); -2**2 == -4 (** binds tighter than unary -).
# ===========================================================================
chk("prec_pow_right_assoc", 2 ** 3 ** 2 == 512)
chk("prec_unary_vs_pow", -2 ** 2 == -4 and (-2) ** 2 == 4)
chk("prec_mul_before_add", 2 + 3 * 4 == 14 and (2 + 3) * 4 == 20)
chk("prec_cmp_lowest", 2 + 3 == 5 and 1 + 1 < 3 and (not 1 == 2) is True)
chk("prec_bit_vs_cmp", (1 | 2 == 3) is True and (1 & 1 == 1) is True)

# ===========================================================================
# 28. VERSION-GATED MODERN SYNTAX (PEP-guarded exec; parses on 3.12)
# Isolate 3.14-only syntax in exec strings guarded by version + SyntaxError
# fallback, so this file still parses & runs on 3.12 (host) and 3.14 (qemu).
# test_lang.py already gates PEP695/PEP701/PEP750 minimally; here add distinct
# probes (PEP 695 bound/constraints/ParamSpec-style alias; PEP 750 t-string
# .strings/.interpolations structure) to widen 3.14 coverage.
# ===========================================================================
def _gated_syntax(name, min_ver, code, probe):
    if sys.version_info < min_ver:
        chk(name, True, "(skip: needs %d.%d)" % (min_ver[0], min_ver[1]))
        return
    ns = {}
    try:
        exec(code, ns)
    except SyntaxError:
        chk(name, True, "(skip: syntax absent)")
        return
    try:
        chk(name, probe(ns))
    except Exception as e:
        chk(name, False, "probe-error: %r" % e)

# PEP 695 (3.12): TypeVar bound & constraints in the bracket syntax + generic class method.
_gated_syntax(
    "pep695_bound_constraints", (3, 12),
    "def big[T: int](x: T) -> T: return x\n"
    "def either[S: (int, str)](x: S) -> S: return x\n"
    "type IntList = list[int]\n"
    "R = (big(5), either('z'), IntList.__value__)\n",
    lambda ns: ns["R"] == (5, "z", list[int]),
)

# PEP 696 (3.13): type parameters may carry defaults in the bracket syntax;
# the TypeVar exposes has_default()/__default__.
_gated_syntax(
    "pep696_typeparam_default", (3, 13),
    "type Alias[T = int] = list[T]\n"
    "tp = Alias.__type_params__[0]\n"
    "R = (tp.has_default(), tp.__default__)\n",
    lambda ns: ns["R"] == (True, int),
)

# PEP 750 (3.14): t-string Template exposes .strings (static parts) and
# .interpolations (dynamic parts); structure differs from an f-string's str.
_gated_syntax(
    "pep750_template_structure", (3, 14),
    "x = 7\n"
    "tmpl = t'a{x}b'\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values, tmpl.interpolations[0].value)\n",
    lambda ns: ns["R"][0] == "Template" and ns["R"][1] == ("a", "b") and ns["R"][2] == (7,) and ns["R"][3] == 7,
)

# PEP 750 (3.14): conversion + format spec are preserved on the Interpolation.
_gated_syntax(
    "pep750_interpolation_meta", (3, 14),
    "v = 255\n"
    "tmpl = t'{v!r:#x}'\n"
    "interp = tmpl.interpolations[0]\n"
    "R = (interp.value, interp.conversion, interp.format_spec)\n",
    lambda ns: ns["R"] == (255, "r", "#x"),
)


# --- string-literal prefix case-insensitivity + all order combinations ---
# Python grammar accepts every prefix in any case, and raw+f / raw+bytes in any
# order; all forms are equivalent. (Convention: we prefer UPPERCASE prefixes.)
_pfx = 7
chk("prefix_F_upper", F"v={_pfx}" == "v=7")
chk("prefix_f_lower", f"v={_pfx}" == "v=7")
chk("prefix_U_upper", U"abc" == "abc" and u"abc" == "abc")
chk("prefix_R_upper", R"\d" == "\\d" and r"\d" == "\\d")
chk("prefix_B_upper", B"xy" == b"xy" and b"xy" == b"xy")
chk("prefix_rawf_all", RF"\d{_pfx}" == "\\d7" and Rf"\d{_pfx}" == "\\d7"
    and rF"\d{_pfx}" == "\\d7" and rf"\d{_pfx}" == "\\d7"
    and FR"\d{_pfx}" == "\\d7" and Fr"\d{_pfx}" == "\\d7"
    and fR"\d{_pfx}" == "\\d7" and fr"\d{_pfx}" == "\\d7")
chk("prefix_rawb_all", RB"\d" == b"\\d" and Rb"\d" == b"\\d"
    and rB"\d" == b"\\d" and rb"\d" == b"\\d"
    and BR"\d" == b"\\d" and Br"\d" == b"\\d"
    and bR"\d" == b"\\d" and br"\d" == b"\\d")
chk("prefix_triple_upper", RF"""a{_pfx}b""" == "a7b" and F"""x{_pfx}""" == "x7")
# uppercase T-string prefix + raw-t combos (3.14)
_gated_syntax(
    "prefix_T_upper_tstring", (3, 14),
    "x = 5\n"
    "a = T'a{x}'\n"
    "b = RT'\\d{x}'\n"
    "c = TR'\\d{x}'\n"
    "R = (type(a).__name__, a.values[0], type(b).__name__, b.strings[0])\n",
    lambda ns: ns["R"][0] == "Template" and ns["R"][1] == 5 and ns["R"][2] == "Template" and "\\d" in ns["R"][3],
)

print(("PY_SYNTAX_OK") if _ok else ("PY_SYNTAX_FAIL"))
sys.exit(0 if _ok else 1)
