#!/usr/bin/env python3
"""Built-in functions (Python "Built-in Functions" stdlib chapter) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

print("PY_BFUNCS python %d.%d.%d (%s) on %s" % (
    sys.version_info[0], sys.version_info[1], sys.version_info[2],
    sys.implementation.name, sys.platform))


# ===========================================================================
# abs(x)
# docs: "Return the absolute value of a number." Works for int, float,
# complex (-> magnitude), and any object defining __abs__.
# how: cover int/float/complex + custom __abs__; why: numeric protocol.
# ===========================================================================
chk("abs_int", abs(-7) == 7 and abs(7) == 7 and abs(0) == 0)
chk("abs_float", abs(-3.5) == 3.5)
chk("abs_complex", abs(complex(3, 4)) == 5.0)
class _Abs:
    def __abs__(self): return 99
chk("abs_dunder", abs(_Abs()) == 99)


# ===========================================================================
# all(iterable) / any(iterable)
# docs: all -> True if all elements truthy (True for empty);
# any -> True if any element truthy (False for empty). Short-circuiting.
# ===========================================================================
chk("all_true", all([1, 2, 3]) is True)
chk("all_false", all([1, 0, 3]) is False)
chk("all_empty", all([]) is True)
chk("all_gen", all(x > 0 for x in range(1, 5)) is True)
chk("any_true", any([0, 0, 1]) is True)
chk("any_false", any([0, 0, 0]) is False)
chk("any_empty", any([]) is False)


# ===========================================================================
# ascii(object)
# docs: like repr() but escape non-ASCII chars with \x \u \U.
# ===========================================================================
chk("ascii_basic", ascii("abc") == "'abc'")
chk("ascii_nonascii", ascii("hé") == "'h\\xe9'")
chk("ascii_unicode", ascii("中") == "'\\u4e2d'")


# ===========================================================================
# bin(x) / oct(x) / hex(x)
# docs: convert integer to base-2/8/16 string prefixed 0b/0o/0x.
# Accepts objects with __index__. Negative keeps sign before prefix.
# ===========================================================================
chk("bin", bin(10) == "0b1010" and bin(-10) == "-0b1010" and bin(0) == "0b0")
chk("oct", oct(64) == "0o100" and oct(-8) == "-0o10")
chk("hex", hex(255) == "0xff" and hex(-255) == "-0xff" and hex(0) == "0x0")
class _Idx:
    def __index__(self): return 12
chk("bin_index", bin(_Idx()) == "0b1100")


# ===========================================================================
# bool([x])
# docs: Boolean value of x via truth testing; subclass of int; bool()==False.
# ===========================================================================
chk("bool_default", bool() is False)
chk("bool_truthy", bool(1) is True and bool("x") is True and bool([0]) is True)
chk("bool_falsy", bool(0) is False and bool("") is False and bool([]) is False and bool(None) is False)
chk("bool_is_int", issubclass(bool, int) and True == 1 and False == 0)


# ===========================================================================
# int(x, base) / int()
# docs: int() -> 0; int(str/number); base 2..36 (0 = autodetect by prefix);
# truncates floats toward zero; accepts __int__ then __index__ (the older
# __trunc__ delegation was deprecated in 3.11 and REMOVED in 3.14).
# errors: ValueError on bad literal, TypeError on bad base type.
# ===========================================================================
chk("int_default", int() == 0)
chk("int_from_float", int(3.9) == 3 and int(-3.9) == -3)
chk("int_from_str", int("42") == 42 and int("  -7 ") == -7)
chk("int_base2", int("1010", 2) == 10)
chk("int_base16", int("ff", 16) == 255 and int("0xff", 16) == 255)
chk("int_base36", int("z", 36) == 35)
chk("int_base0", int("0o17", 0) == 15 and int("0b101", 0) == 5)
chk("int_underscore", int("1_000") == 1000)
# int() honours the __int__ protocol, and falls back to __index__ for objects
# that define only the latter (both documented; __trunc__ delegation gone in 3.14).
class _IntDunder:
    def __int__(self): return 42
class _IndexOnly:
    def __index__(self): return 17
chk("int_dunder", int(_IntDunder()) == 42 and type(int(_IntDunder())) is int)
chk("int_index_fallback", int(_IndexOnly()) == 17)
try:
    int("x"); _e = None
except ValueError as e:
    _e = ValueError
chk("int_bad_literal", _e is ValueError)
try:
    int("12", 1); _e2 = None
except ValueError as e:
    _e2 = ValueError
chk("int_bad_base", _e2 is ValueError)


# ===========================================================================
# float(x) / float()
# docs: float() -> 0.0; parse strings incl "inf"/"nan"/exponent; __float__.
# ===========================================================================
chk("float_default", float() == 0.0)
chk("float_str", float("3.14") == 3.14 and float(" -2e3 ") == -2000.0)
chk("float_int", float(5) == 5.0)
chk("float_inf", float("inf") == float("inf") and float("-inf") < 0)
import math as _m
chk("float_nan", _m.isnan(float("nan")))
try:
    float("abc"); _ef = None
except ValueError:
    _ef = ValueError
chk("float_bad", _ef is ValueError)


# ===========================================================================
# complex([real[, imag]]) / complex(str)
# docs: complex() -> 0j; complex(re, im); parse "1+2j" (no spaces in string form).
# ===========================================================================
chk("complex_default", complex() == 0j)
chk("complex_pair", complex(2, 3) == (2 + 3j))
chk("complex_str", complex("1+2j") == (1 + 2j))
chk("complex_attrs", (3 + 4j).real == 3.0 and (3 + 4j).imag == 4.0 and (3 + 4j).conjugate() == (3 - 4j))


# ===========================================================================
# round(number[, ndigits])
# docs: round to ndigits (None -> nearest int as int). Banker's rounding
# (round half to even). ndigits negative rounds left of decimal point.
# ===========================================================================
chk("round_none", round(2.5) == 2 and round(3.5) == 4 and round(2.4) == 2)
chk("round_ndigits", round(3.14159, 2) == 3.14 and round(2.675, 2) == 2.67)
chk("round_negative", round(12345, -2) == 12300)
chk("round_int", round(7) == 7 and isinstance(round(2.5), int))
chk("round_ndigits_kw", round(3.14159, ndigits=2) == 3.14 and round(number=2.5) == 2)  # keyword form
class _Round:
    def __round__(self, n=None): return ("r", n)
chk("round_dunder", round(_Round()) == ("r", None) and round(_Round(), 3) == ("r", 3))


# ===========================================================================
# divmod(a, b) / pow(base, exp[, mod])
# docs: divmod -> (a//b, a%b). pow(b,e) == b**e; pow(b,e,m) == b**e % m
# (modular; supports modular inverse for exp=-1 since 3.8).
# ===========================================================================
chk("divmod_int", divmod(17, 5) == (3, 2) and divmod(-17, 5) == (-4, 3))
chk("divmod_float", divmod(7.5, 2) == (3.0, 1.5))
chk("pow_two", pow(2, 10) == 1024)
chk("pow_mod", pow(2, 10, 1000) == 24)
chk("pow_neg_exp", pow(2, -1) == 0.5)
chk("pow_mod_inverse", pow(3, -1, 11) == 4)  # 3*4 = 12 == 1 (mod 11)


# ===========================================================================
# chr(i) / ord(c)
# docs: chr -> str of one Unicode char for codepoint; ord -> codepoint int.
# Inverse of each other. chr range 0..0x10FFFF; ValueError otherwise.
# ===========================================================================
chk("chr", chr(65) == "A" and chr(0x4e2d) == "中" and chr(0x1F600) == "\U0001F600")
chk("ord", ord("A") == 65 and ord("中") == 0x4e2d)
chk("chr_ord_inverse", ord(chr(12345)) == 12345)
try:
    chr(0x110000); _ec = None
except ValueError:
    _ec = ValueError
chk("chr_overflow", _ec is ValueError)
try:
    ord("ab"); _eo = None
except TypeError:
    _eo = TypeError
chk("ord_bad_len", _eo is TypeError)


# ===========================================================================
# format(value[, format_spec])
# docs: convert value using __format__ + the format spec mini-language.
# Equivalent to str.format single field.
# ===========================================================================
chk("format_default", format(42) == "42")
chk("format_hex", format(255, "#06x") == "0x00ff")
chk("format_float", format(3.14159, ".2f") == "3.14")
chk("format_align", format("x", ">5") == "    x" and format("x", "*^5") == "**x**")
chk("format_pct", format(0.25, ".0%") == "25%")
chk("format_comma", format(1234567, ",") == "1,234,567")
# integer presentation type codes: binary/octal/decimal/hex(upper)/char
chk("format_int_types", format(10, "b") == "1010" and format(64, "o") == "100"
    and format(255, "X") == "FF" and format(255, "d") == "255" and format(65, "c") == "A")
# float presentation types: scientific (e/E) and general (g)
chk("format_float_types", format(1234.5, ".2e") == "1.23e+03" and format(1234.5, ".2E") == "1.23E+03"
    and format(0.0001, "g") == "0.0001")
# sign control (+ / space), zero-padding, and '=' alignment (pad between sign and digits)
chk("format_sign_pad", format(5, "+") == "+5" and format(5, " ") == " 5" and format(-5, "+") == "-5"
    and format(42, "08.2f") == "00042.00" and format(42, "=+8") == "+     42")
# format() delegates to the object's __format__ with the raw spec
class _Fmt:
    def __format__(self, spec): return "F[" + spec + "]"
chk("format_dunder", format(_Fmt(), "spec") == "F[spec]" and format(_Fmt()) == "F[]")


# ===========================================================================
# repr(object) / str(object)
# docs: repr -> "official" representation (eval-able when possible);
# str -> "informal"/printable. str() -> "". For containers repr recurses.
# ===========================================================================
chk("repr_str", repr("a\nb") == "'a\\nb'" and repr([1, 2]) == "[1, 2]")
chk("str_default", str() == "" and str(42) == "42" and str([1, 2]) == "[1, 2]")
chk("str_bytes_decode", str(b"hi", "utf-8") == "hi")
class _Rs:
    def __repr__(self): return "R"
    def __str__(self): return "S"
chk("repr_str_dunder", repr(_Rs()) == "R" and str(_Rs()) == "S")
class _Ronly:
    def __repr__(self): return "RO"
chk("str_falls_to_repr", str(_Ronly()) == "RO")


# ===========================================================================
# hash(object) / id(object)
# docs: hash returns int; equal objects must hash equal; id is unique
# identity int for the object's lifetime (is operator basis).
# ===========================================================================
chk("hash_eq", hash(1) == hash(1.0) and hash((1, 2)) == hash((1, 2)))
chk("hash_str", isinstance(hash("x"), int))
try:
    hash([1, 2]); _eh = None
except TypeError:
    _eh = TypeError
chk("hash_unhashable", _eh is TypeError)
_o1 = object()
chk("id_identity", id(_o1) == id(_o1) and (id(_o1) != id(object())))


# ===========================================================================
# callable(object)
# docs: True if object appears callable (functions, classes, __call__).
# ===========================================================================
chk("callable_func", callable(len) and callable(lambda: 0) and callable(int))
chk("callable_not", callable(5) is False and callable("x") is False)
class _Call:
    def __call__(self): return 1
chk("callable_dunder", callable(_Call()) is True and callable(_Call) is True)


# ===========================================================================
# len(s)
# docs: number of items; calls __len__. TypeError if no length.
# ===========================================================================
chk("len_seq", len([1, 2, 3]) == 3 and len("abc") == 3 and len({"a": 1}) == 1 and len(range(10)) == 10)
class _Len:
    def __len__(self): return 7
chk("len_dunder", len(_Len()) == 7)
try:
    len(5); _el = None
except TypeError:
    _el = TypeError
chk("len_no_len", _el is TypeError)


# ===========================================================================
# list / tuple / set / frozenset / dict / bytes / bytearray (constructors)
# docs: container constructors; empty form, from-iterable form, copy semantics.
# ===========================================================================
chk("list_ctor", list() == [] and list("ab") == ["a", "b"] and list(range(3)) == [0, 1, 2])
chk("tuple_ctor", tuple() == () and tuple([1, 2]) == (1, 2) and tuple("ab") == ("a", "b"))
chk("set_ctor", set() == set() and set([1, 1, 2]) == {1, 2})
chk("frozenset_ctor", frozenset([1, 2, 2]) == frozenset({1, 2}) and isinstance(hash(frozenset([1])), int))
chk("dict_ctor", dict() == {} and dict(a=1, b=2) == {"a": 1, "b": 2}
    and dict([("x", 1)]) == {"x": 1} and dict({"k": 9}) == {"k": 9})
# dict(mapping, **kwargs): positional mapping merged with keyword args (kwargs win on clash)
chk("dict_mapping_kwargs", dict({"a": 1}, b=2) == {"a": 1, "b": 2}
    and dict({"a": 1, "b": 9}, b=2) == {"a": 1, "b": 2}
    and dict([("a", 1)], a=5) == {"a": 5})
chk("bytes_ctor", bytes() == b"" and bytes(3) == b"\x00\x00\x00"
    and bytes([65, 66]) == b"AB" and bytes("hé", "utf-8") == b"h\xc3\xa9")
ba = bytearray(b"abc")
ba[0] = 90
chk("bytearray_ctor", bytearray() == bytearray(b"") and bytearray(2) == bytearray(b"\x00\x00")
    and ba == bytearray(b"Zbc"))


# ===========================================================================
# range(stop) / range(start, stop[, step])
# docs: immutable arithmetic sequence; supports len, index, slicing, contains.
# ===========================================================================
chk("range_stop", list(range(4)) == [0, 1, 2, 3])
chk("range_start_stop", list(range(2, 6)) == [2, 3, 4, 5])
chk("range_step", list(range(0, 10, 3)) == [0, 3, 6, 9] and list(range(5, 0, -1)) == [5, 4, 3, 2, 1])
chk("range_features", len(range(0, 10, 2)) == 5 and 6 in range(0, 10, 2) and 7 not in range(0, 10, 2)
    and range(10).index(4) == 4 and range(10)[2:5] == range(2, 5))


# ===========================================================================
# enumerate(iterable, start=0)
# docs: yields (index, value) pairs; start customizes the first index.
# ===========================================================================
chk("enumerate_default", list(enumerate("ab")) == [(0, "a"), (1, "b")])
chk("enumerate_start", list(enumerate("ab", start=10)) == [(10, "a"), (11, "b")])
chk("enumerate_start_pos", list(enumerate("ab", 5)) == [(5, "a"), (6, "b")])  # start positional
# enumerate yields a lazy one-shot iterator (it IS its own iterator, advances once)
_en = enumerate("xyz")
chk("enumerate_lazy", iter(_en) is _en and next(_en) == (0, "x") and next(_en) == (1, "y"))


# ===========================================================================
# zip(*iterables, strict=False)
# docs: yields tuples until shortest exhausted; strict=True raises ValueError
# on unequal lengths (3.10+).
# ===========================================================================
chk("zip_basic", list(zip([1, 2, 3], "ab")) == [(1, "a"), (2, "b")])
chk("zip_three", list(zip([1, 2], [3, 4], [5, 6])) == [(1, 3, 5), (2, 4, 6)])
chk("zip_empty", list(zip()) == [])
chk("zip_unzip", list(zip(*[(1, "a"), (2, "b")])) == [(1, 2), ("a", "b")])
if sys.version_info >= (3, 10):
    try:
        list(zip([1, 2], [3], strict=True)); _ez = None
    except ValueError:
        _ez = ValueError
    chk("zip_strict", _ez is ValueError)
else:
    chk("zip_strict", True, "(skip: needs 3.10)")


# ===========================================================================
# map(function, *iterables)
# docs: apply function lazily over one or more iterables (multi -> N-arg call,
# stops at shortest).
# ===========================================================================
chk("map_one", list(map(str, [1, 2, 3])) == ["1", "2", "3"])
chk("map_multi", list(map(lambda a, b: a + b, [1, 2, 3], [10, 20, 30])) == [11, 22, 33])
chk("map_shortest", list(map(lambda a, b: a * b, [1, 2, 3], [10, 20])) == [10, 40])


# ===========================================================================
# filter(function, iterable)
# docs: keep items where function(item) is truthy; None -> keep truthy items.
# ===========================================================================
chk("filter_func", list(filter(lambda x: x % 2 == 0, range(10))) == [0, 2, 4, 6, 8])
chk("filter_none", list(filter(None, [0, 1, "", "x", [], [0]])) == [1, "x", [0]])


# ===========================================================================
# sorted(iterable, *, key=None, reverse=False)
# docs: new sorted list; stable; key transforms; reverse flips order.
# ===========================================================================
chk("sorted_basic", sorted([3, 1, 2]) == [1, 2, 3])
chk("sorted_reverse", sorted([3, 1, 2], reverse=True) == [3, 2, 1])
chk("sorted_key", sorted(["bb", "a", "ccc"], key=len) == ["a", "bb", "ccc"])
chk("sorted_key_none", sorted([3, 1, 2], key=None) == [1, 2, 3])  # explicit key=None == default
chk("sorted_stable", sorted([(1, "b"), (1, "a"), (0, "z")], key=lambda t: t[0]) == [(0, "z"), (1, "b"), (1, "a")])
# sorted() returns a NEW list; the source is left untouched (unlike list.sort in place)
_src = [3, 1, 2]
_new = sorted(_src)
chk("sorted_returns_new", _new == [1, 2, 3] and _src == [3, 1, 2] and _new is not _src)
# key=reverse interaction preserves stability among equal keys (reverse=True keeps relative order)
chk("sorted_key_reverse", sorted(["a", "bb", "cc", "d"], key=len, reverse=True) == ["bb", "cc", "a", "d"])


# ===========================================================================
# reversed(seq)
# docs: reverse iterator over a sequence (len + __getitem__, or __reversed__).
# ===========================================================================
chk("reversed_list", list(reversed([1, 2, 3])) == [3, 2, 1])
chk("reversed_str", "".join(reversed("abc")) == "cba")
chk("reversed_range", list(reversed(range(3))) == [2, 1, 0])
class _Rev:
    def __reversed__(self): return iter(["z", "y"])
chk("reversed_dunder", list(reversed(_Rev())) == ["z", "y"])
# reversed() yields a lazy iterator (one-shot, advances on next) and leaves source intact
_rsrc = [1, 2, 3]
_rev = reversed(_rsrc)
chk("reversed_lazy", iter(_rev) is _rev and next(_rev) == 3 and next(_rev) == 2 and _rsrc == [1, 2, 3])
# reversed over a custom sequence using __len__ + __getitem__ (no __reversed__)
class _Seq:
    def __len__(self): return 3
    def __getitem__(self, i): return ("a", "b", "c")[i]
chk("reversed_getitem", list(reversed(_Seq())) == ["c", "b", "a"])


# ===========================================================================
# sum(iterable, /, start=0)
# docs: sum of items + start (default 0). start may be any addable type.
# ===========================================================================
chk("sum_default", sum([1, 2, 3]) == 6 and sum([]) == 0)
chk("sum_start", sum([1, 2, 3], 100) == 106)
chk("sum_lists", sum([[1], [2]], []) == [1, 2])
chk("sum_float", sum([0.5, 0.5, 0.5]) == 1.5)


# ===========================================================================
# min / max (iterable or args, key=, default=)
# docs: smallest/largest; key for comparison; default returned when iterable
# is empty (ValueError otherwise).
# ===========================================================================
chk("max_args", max(3, 1, 2) == 3 and min(3, 1, 2) == 1)
chk("max_iter", max([3, 1, 2]) == 3 and min([3, 1, 2]) == 1)
chk("max_key", max(["a", "ccc", "bb"], key=len) == "ccc" and min(["a", "ccc", "bb"], key=len) == "a")
chk("max_default", max([], default="d") == "d" and min([], default="d") == "d")
try:
    max([]); _emx = None
except ValueError:
    _emx = ValueError
chk("max_empty_raises", _emx is ValueError)


# ===========================================================================
# iter(object[, sentinel]) / next(iterator[, default])
# docs: iter(obj) -> iterator (via __iter__ or __getitem__); iter(callable,
# sentinel) calls until sentinel returned. next advances; StopIteration or
# default at end.
# ===========================================================================
it = iter([10, 20])
chk("iter_next", next(it) == 10 and next(it) == 20)
try:
    next(it); _en = None
except StopIteration:
    _en = StopIteration
chk("next_stop", _en is StopIteration)
chk("next_default", next(iter([]), "D") == "D")
# iter(callable, sentinel): call the zero-arg callable until it returns sentinel
_counter = [0]
def _nextn():
    _counter[0] += 1
    return _counter[0]
chk("iter_sentinel", list(iter(_nextn, 4)) == [1, 2, 3])


# ===========================================================================
# slice(stop) / slice(start, stop[, step])
# docs: slice object with .start/.stop/.step; .indices(len) normalizes.
# ===========================================================================
sl = slice(1, 5, 2)
chk("slice_attrs", sl.start == 1 and sl.stop == 5 and sl.step == 2)
chk("slice_use", [0, 1, 2, 3, 4, 5][slice(1, 4)] == [1, 2, 3])
chk("slice_indices", slice(None, None, -1).indices(5) == (4, -1, -1))


# ===========================================================================
# getattr / setattr / hasattr / delattr / vars / dir
# docs: dynamic attribute access. getattr(o, name[, default]); hasattr ->
# bool (suppresses exceptions); setattr/delattr mutate; vars -> __dict__;
# dir -> sorted attribute name list.
# ===========================================================================
class _Attr:
    cls_attr = 1
    def __init__(self): self.inst = 2
_a = _Attr()
chk("getattr", getattr(_a, "inst") == 2 and getattr(_a, "cls_attr") == 1)
chk("getattr_default", getattr(_a, "nope", "D") == "D")
try:
    getattr(_a, "nope"); _eg = None
except AttributeError:
    _eg = AttributeError
chk("getattr_no_default", _eg is AttributeError)
chk("hasattr", hasattr(_a, "inst") is True and hasattr(_a, "nope") is False)
setattr(_a, "newattr", 42)
chk("setattr", _a.newattr == 42 and getattr(_a, "newattr") == 42)
delattr(_a, "newattr")
chk("delattr", hasattr(_a, "newattr") is False)
chk("vars", vars(_a) == {"inst": 2} and "cls_attr" in vars(_Attr))
_d = dir(_a)
chk("dir", isinstance(_d, list) and _d == sorted(_d) and "inst" in _d and "cls_attr" in _d)
chk("dir_no_arg", isinstance(dir(), list))


# ===========================================================================
# isinstance / issubclass (single class or tuple of classes)
# docs: isinstance(obj, cls_or_tuple); issubclass(cls, cls_or_tuple).
# ===========================================================================
chk("isinstance_single", isinstance(5, int) and isinstance("x", str) and not isinstance(5, str))
chk("isinstance_tuple", isinstance(5, (str, int)) and not isinstance(5.0, (str, int)))
chk("isinstance_bool_int", isinstance(True, int))
chk("issubclass_single", issubclass(bool, int) and issubclass(int, object) and not issubclass(int, str))
chk("issubclass_tuple", issubclass(bool, (str, int)))
try:
    isinstance(5, 5); _ei = None
except TypeError:
    _ei = TypeError
chk("isinstance_bad_type", _ei is TypeError)
# ABCs use __instancecheck__/__subclasscheck__ hooks (virtual subclass relationship)
import collections.abc as _abc
chk("isinstance_abc", isinstance([1], _abc.Iterable) and isinstance({}, _abc.Mapping)
    and issubclass(list, _abc.Sequence) and not isinstance(5, _abc.Iterable))


# ===========================================================================
# type(object) (1-arg) and type(name, bases, dict) (3-arg dynamic class)
# docs: 1-arg returns the type; 3-arg dynamically creates a new class.
# ===========================================================================
chk("type_one_arg", type(5) is int and type("x") is str and type([]) is list)
Dyn = type("Dyn", (object,), {"greet": lambda self: "hi", "x": 7})
_dyn = Dyn()
chk("type_three_arg", _dyn.greet() == "hi" and _dyn.x == 7 and Dyn.__name__ == "Dyn"
    and isinstance(_dyn, Dyn) and type(_dyn) is Dyn)
class _Sub(int): pass
chk("type_with_bases", issubclass(type("D2", (_Sub,), {}), int))


# ===========================================================================
# object()
# docs: base of all classes; featureless instances; distinct identities.
# ===========================================================================
_ob = object()
chk("object_base", isinstance(_ob, object) and type(_ob) is object and object() is not object())
chk("object_no_dict", not hasattr(_ob, "__dict__"))


# ===========================================================================
# compile / eval / exec
# docs: compile(source, filename, mode) -> code obj (mode 'eval'/'exec'/'single');
# eval evaluates an expression; exec runs statements; both accept globals/locals.
# ===========================================================================
_code_eval = compile("1 + 2 * 3", "<test>", "eval")
chk("compile_eval", eval(_code_eval) == 7)
_code_exec = compile("a = 10\nb = a * 2", "<test>", "exec")
_ns = {}
exec(_code_exec, _ns)
chk("compile_exec", _ns["a"] == 10 and _ns["b"] == 20)
chk("eval_expr", eval("len('abcd')") == 4)
chk("eval_globals", eval("x + y", {"x": 3, "y": 4}) == 7)
chk("eval_locals", eval("a * b", {}, {"a": 5, "b": 6}) == 30)
_eg2 = {}
exec("def square(n): return n * n", _eg2)
chk("exec_defines", _eg2["square"](9) == 81)
try:
    eval("a = 5"); _ee = None  # assignment is a statement, illegal in eval
except SyntaxError:
    _ee = SyntaxError
chk("eval_rejects_stmt", _ee is SyntaxError)
# compile(..., flags=ast.PyCF_ONLY_AST) returns an AST node instead of a code object
import ast as _ast
_tree = compile("1 + 2", "<t>", "eval", flags=_ast.PyCF_ONLY_AST)
chk("compile_flags_ast", isinstance(_tree, _ast.Expression) and eval(compile(_tree, "<t>", "eval")) == 3)
# compile(..., optimize=2) strips assert statements (and __debug__-guarded code)
_opt = compile("assert False, 'boom'\nresult = 7", "<t>", "exec", optimize=2)
_optns = {}
exec(_opt, _optns)
chk("compile_optimize", _optns["result"] == 7)  # assert removed at optimize level 2


# ===========================================================================
# globals() / locals()
# docs: globals() -> module namespace dict; locals() -> current local
# namespace (read-only-ish in functions).
# ===========================================================================
chk("globals_dict", isinstance(globals(), dict) and "chk" in globals())
def _loc():
    z = 123
    return locals()
chk("locals_func", _loc() == {"z": 123})


# ===========================================================================
# classmethod / staticmethod / property (as builtins, beyond decorator sugar)
# docs: classmethod -> method bound to class; staticmethod -> plain function
# in class; property -> managed attribute with fget/fset/fdel.
# ===========================================================================
class _CSP:
    val = "cls"
    def _f(cls): return cls.val
    cm = classmethod(_f)
    def _g(): return "static"
    sm = staticmethod(_g)
    def __init__(self): self._p = 0
    def _get(self): return self._p
    def _set(self, v): self._p = v * 2
    prop = property(_get, _set)
chk("classmethod_builtin", _CSP.cm() == "cls" and _CSP().cm() == "cls")
chk("staticmethod_builtin", _CSP.sm() == "static" and _CSP().sm() == "static")
_csp = _CSP()
_csp.prop = 5
chk("property_builtin", _csp.prop == 10)
# property with deleter + docstring
class _Pd:
    def __init__(self): self._x = 1
    @property
    def x(self):
        "the x value"
        return self._x
    @x.setter
    def x(self, v): self._x = v
    @x.deleter
    def x(self): self._x = None
_pd = _Pd()
_pd.x = 9
chk("property_setter", _pd.x == 9)
del _pd.x
chk("property_deleter", _pd.x is None)
chk("property_doc", _Pd.x.__doc__ == "the x value")


# ===========================================================================
# super() / super(type, obj)
# docs: proxy delegating to parent/sibling per MRO; zero-arg and explicit forms.
# ===========================================================================
class _SA:
    def m(self): return "A"
class _SB(_SA):
    def m(self): return "B+" + super().m()
class _SC(_SB):
    def m(self): return "C+" + super().m()
chk("super_zero_arg", _SC().m() == "C+B+A")
class _SE(_SB):
    def m(self): return "E+" + super(_SE, self).m()  # explicit form
chk("super_explicit", _SE().m() == "E+B+A")


# ===========================================================================
# print(*objects, sep, end, file, flush)
# docs: write objects to file (default sys.stdout); sep between, end after.
# how: capture via io.StringIO to assert sep/end/file behavior.
# ===========================================================================
import io
_buf = io.StringIO()
print("a", "b", "c", sep="-", end="!", file=_buf)
chk("print_sep_end_file", _buf.getvalue() == "a-b-c!")
_buf2 = io.StringIO()
print(1, 2, 3, file=_buf2)
chk("print_defaults", _buf2.getvalue() == "1 2 3\n")
_buf3 = io.StringIO()
print(file=_buf3)  # no args -> just newline
chk("print_no_args", _buf3.getvalue() == "\n")
_buf4 = io.StringIO()
print("x", flush=True, file=_buf4)  # flush accepted as keyword
chk("print_flush", _buf4.getvalue() == "x\n")


# ===========================================================================
# aiter(async_iterable) / anext(async_iterator[, default])  (3.10+)
# docs: async analogues of iter/next. how: drive a tiny async iterator via
# asyncio.run; expected: collected values; why: async builtin surface.
# ===========================================================================
if sys.version_info >= (3, 10) and hasattr(__builtins__, "aiter") if not isinstance(__builtins__, dict) else (sys.version_info >= (3, 10) and "aiter" in __builtins__):
    import asyncio
    class _AIter:
        def __init__(self): self.i = 0
        def __aiter__(self): return self
        async def __anext__(self):
            if self.i >= 3:
                raise StopAsyncIteration
            self.i += 1
            return self.i
    async def _drive():
        ai = aiter(_AIter())          # noqa: F821 (3.10+ builtin)
        out = []
        while True:
            v = await anext(ai, None)  # noqa: F821
            if v is None:
                break
            out.append(v)
        return out
    chk("aiter_anext", asyncio.run(_drive()) == [1, 2, 3])
else:
    chk("aiter_anext", True, "(skip: needs 3.10 aiter/anext)")


# ===========================================================================
# __import__(name) (low-level import hook)
# docs: invoked by the import statement; returns the top-level module.
# Prefer importlib in real code, but the builtin must exist & work.
# ===========================================================================
_math_mod = __import__("math")
chk("dunder_import", _math_mod.sqrt(16) == 4.0 and _math_mod.__name__ == "math")


# ===========================================================================
# memoryview(obj)
# docs: zero-copy view over a bytes-like object; supports indexing, slicing,
# .tobytes(), .nbytes, format/itemsize.
# ===========================================================================
mv = memoryview(b"abcdef")
chk("memoryview_index", mv[0] == ord("a") and bytes(mv[1:3]) == b"bc")
chk("memoryview_meta", mv.nbytes == 6 and mv.itemsize == 1 and mv.tobytes() == b"abcdef")
_mba = bytearray(b"xyz")
_mvw = memoryview(_mba)
_mvw[0] = ord("Q")
chk("memoryview_write", _mba == bytearray(b"Qyz"))


# ===========================================================================
# Final tally
# ===========================================================================
print(("PY_BFUNCS_OK") if _ok else ("PY_BFUNCS_FAIL"))
sys.exit(0 if _ok else 1)
