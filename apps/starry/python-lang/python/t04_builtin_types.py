#!/usr/bin/env python3
"""Built-in Types (str/bytes/list/dict/set/int/float/range/memoryview/...) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# ======================================================================
# str — Text Sequence Type (docs: "Built-in Types > str").
# Cover EVERY str method item-by-item. How: call each method with the
# documented arguments and assert exact results; expected per CPython
# docs; why: str is the most-used type, every method must behave.
# ======================================================================

# str.capitalize/casefold/lower/upper/title/swapcase (case operations).
chk("str_capitalize", "hELLO wORLD".capitalize() == "Hello world")
chk("str_casefold", "Straße".casefold() == "strasse")
chk("str_lower", "ÀBC".lower() == "àbc")
chk("str_upper", "àbc".upper() == "ÀBC")
chk("str_title", "they're bill's".title() == "They'Re Bill'S")
chk("str_swapcase", "Hello World".swapcase() == "hELLO wORLD")

# str.center/ljust/rjust/zfill (alignment/padding); width<=len returns unchanged.
chk("str_center", "ab".center(6, "*") == "**ab**" and "ab".center(1) == "ab")
chk("str_center_odd", "ab".center(5, "-") == "--ab-")  # odd pad: extra goes left
chk("str_ljust", "ab".ljust(5, ".") == "ab...")
chk("str_rjust", "ab".rjust(5, ".") == "...ab")
chk("str_zfill", "42".zfill(5) == "00042" and "-3".zfill(5) == "-0003" and "+3".zfill(5) == "+0003")

# str.count(sub[,start[,end]]) — non-overlapping count.
chk("str_count", "abababa".count("aba") == 2 and "abababa".count("a") == 4)
chk("str_count_range", "aXaXa".count("a", 1) == 2 and "aaaa".count("a", 1, 3) == 2)
chk("str_count_empty", "abc".count("") == 4)

# str.find/rfind (-1 if absent) vs str.index/rindex (ValueError if absent).
chk("str_find", "hello".find("l") == 2 and "hello".find("z") == -1)
chk("str_rfind", "hello".rfind("l") == 3 and "hello".rfind("z") == -1)
chk("str_find_range", "abcabc".find("a", 1) == 3 and "abcabc".find("c", 0, 2) == -1)
chk("str_index", "hello".index("e") == 1 and "hello".rindex("l") == 3)
try:
    "abc".index("z"); _r = False
except ValueError:
    _r = True
chk("str_index_raises", _r)
# str.find/index/count are positional-only: keyword 'sub=' is a TypeError.
try:
    "abc".find(sub="b"); _r = False
except TypeError:
    _r = True
chk("str_find_positional_only", _r)

# str.startswith/endswith — accept a str or a tuple of prefixes/suffixes.
chk("str_startswith", "filename.py".startswith("file") and not "x".startswith("y"))
chk("str_startswith_tuple", "abc".startswith(("z", "ab")))
chk("str_startswith_range", "abcdef".startswith("cd", 2))
chk("str_endswith", "a.py".endswith((".pyc", ".py")) and not "a.txt".endswith(".py"))
chk("str_endswith_range", "abcdef".endswith("cd", 0, 4))

# str.split/rsplit (default whitespace collapses; maxsplit; sep keeps empties).
chk("str_split_ws", "  a  b   c ".split() == ["a", "b", "c"])
chk("str_split_sep", "a,b,,c".split(",") == ["a", "b", "", "c"])
chk("str_split_max", "a-b-c-d".split("-", 2) == ["a", "b", "c-d"])
chk("str_rsplit_max", "a-b-c-d".rsplit("-", 2) == ["a-b", "c", "d"])
chk("str_split_empty", "".split(",") == [""] and "".split() == [])
# split/rsplit accept keyword args (sep=, maxsplit=); partition is positional-only.
chk("str_split_kwargs", "a,b,c".split(sep=",", maxsplit=1) == ["a", "b,c"])
chk("str_rsplit_kwargs", "a-b-c".rsplit(sep="-", maxsplit=1) == ["a-b", "c"])
try:
    "a=b".partition(sep="="); _r = False
except TypeError:
    _r = True
chk("str_partition_positional_only", _r)

# str.splitlines(keepends) — splits on universal line boundaries.
chk("str_splitlines", "a\nb\r\nc\rd".splitlines() == ["a", "b", "c", "d"])
chk("str_splitlines_keep", "a\nb\n".splitlines(True) == ["a\n", "b\n"])
chk("str_splitlines_empty", "".splitlines() == [] and "one".splitlines() == ["one"])
chk("str_splitlines_kwargs", "a\nb".splitlines(keepends=True) == ["a\n", "b"])

# str.partition/rpartition — 3-tuple; if sep absent, head holds the whole (rpartition: tail).
chk("str_partition", "a=b=c".partition("=") == ("a", "=", "b=c"))
chk("str_rpartition", "a=b=c".rpartition("=") == ("a=b", "=", "c"))
chk("str_partition_absent", "abc".partition("=") == ("abc", "", ""))
chk("str_rpartition_absent", "abc".rpartition("=") == ("", "", "abc"))
# empty separator is rejected with ValueError ("empty separator") for both.
try:
    "abc".partition(""); _r = False
except ValueError:
    _r = True
chk("str_partition_empty_raises", _r)
try:
    "abc".rpartition(""); _r = False
except ValueError:
    _r = True
chk("str_rpartition_empty_raises", _r)

# str.strip/lstrip/rstrip — strip set of chars (default whitespace).
chk("str_strip", "  xy  ".strip() == "xy")
chk("str_strip_chars", "xxabcxx".strip("x") == "abc")
chk("str_lstrip", "..abc..".lstrip(".") == "abc..")
chk("str_rstrip", "..abc..".rstrip(".") == "..abc")

# str.removeprefix/removesuffix (PEP 616, 3.9). Returns unchanged if no match.
chk("str_removeprefix", "TestHook".removeprefix("Test") == "Hook" and "abc".removeprefix("z") == "abc")
chk("str_removesuffix", "file.txt".removesuffix(".txt") == "file" and "abc".removesuffix("z") == "abc")
chk("str_removeprefix_empty", "abc".removeprefix("") == "abc")

# str.join/replace — replace(old,new[,count]).
chk("str_join", ",".join(["a", "b", "c"]) == "a,b,c" and "".join([]) == "")
chk("str_replace", "aaa".replace("a", "b") == "bbb" and "aaa".replace("a", "b", 2) == "bba")
chk("str_replace_empty", "abc".replace("", "-") == "-a-b-c-")

# str.expandtabs(tabsize) — replaces tabs to next multiple of tabsize.
chk("str_expandtabs", "a\tbc\tdef".expandtabs(4) == "a   bc  def")
chk("str_expandtabs_default", "1\t2".expandtabs() == "1       2")

# str.encode(encoding, errors) — round-trips with bytes.decode.
chk("str_encode", "héllo".encode("utf-8") == b"h\xc3\xa9llo")
chk("str_encode_latin1", "café".encode("latin-1") == b"caf\xe9")
chk("str_encode_errors", "a€b".encode("ascii", "ignore") == b"ab")
chk("str_encode_replace", "€".encode("ascii", "replace") == b"?")
chk("str_encode_xmlref", "€".encode("ascii", "xmlcharrefreplace") == b"&#8364;")
chk("str_encode_backslash", "€".encode("ascii", "backslashreplace") == b"\\u20ac")
chk("str_encode_namereplace", "€".encode("ascii", "namereplace") == b"\\N{EURO SIGN}")
# str.encode accepts keyword args (encoding=, errors=).
chk("str_encode_kwargs", "abc".encode(encoding="ascii", errors="strict") == b"abc")
# error paths: default 'strict' raises UnicodeEncodeError; unknown codec raises LookupError.
try:
    "€".encode("ascii"); _r = False
except UnicodeEncodeError:
    _r = True
chk("str_encode_strict_raises", _r)
try:
    "x".encode("no-such-codec-xyz"); _r = False
except LookupError:
    _r = True
chk("str_encode_unknown_codec_raises", _r)

# str.format / str.format_map — format mini-language.
chk("str_format_pos", "{0}-{1}-{0}".format("a", "b") == "a-b-a")
chk("str_format_name", "{x}/{y}".format(x=1, y=2) == "1/2")
chk("str_format_spec", "{:>6.2f}".format(3.14159) == "  3.14")
chk("str_format_fill", "{:*^8}".format("hi") == "***hi***")
chk("str_format_attr", "{0.real}".format(3 + 4j) == "3.0")
chk("str_format_item", "{0[1]}".format(["a", "b"]) == "b")
chk("str_format_map", "{k}".format_map({"k": "v"}) == "v")
class _Default(dict):
    def __missing__(self, key):
        return "<%s>" % key
chk("str_format_map_missing", "{a}{b}".format_map(_Default(a="X")) == "X<b>")

# str.maketrans / str.translate — table-based char translation.
_tbl = str.maketrans("abc", "xyz", "d")
chk("str_maketrans_translate", "abcd abc".translate(_tbl) == "xyz xyz")
_tbl2 = str.maketrans({"a": "1", "b": None})
chk("str_maketrans_dict", "abab".translate(_tbl2) == "11")

# str isX predicates — full family; test true and false cases.
chk("str_isalnum", "abc123".isalnum() and not "a b".isalnum())
chk("str_isalpha", "abc".isalpha() and not "a1".isalpha())
chk("str_isascii", "abc~".isascii() and not "é".isascii() and "".isascii())
chk("str_isdecimal", "123".isdecimal() and not "½".isdecimal())
chk("str_isdigit", "123".isdigit() and "²".isdigit() and not "½".isdigit())
chk("str_isnumeric", "½".isnumeric() and "Ⅷ".isnumeric() and not "abc".isnumeric())
chk("str_isidentifier", "var_1".isidentifier() and not "1var".isidentifier() and not "a-b".isidentifier())
chk("str_islower", "abc".islower() and not "Abc".islower() and not "123".islower())
chk("str_isupper", "ABC".isupper() and not "Abc".isupper())
chk("str_isprintable", "abc 123".isprintable() and not "a\nb".isprintable() and "".isprintable())
chk("str_isspace", "  \t\n".isspace() and not " a ".isspace() and not "".isspace())
chk("str_istitle", "Hello World".istitle() and not "Hello world".istitle())

# str operators / sequence protocol: +, *, in, indexing, len, comparison.
chk("str_concat_repeat", "ab" + "cd" == "abcd" and "ab" * 3 == "ababab")
chk("str_in", "ell" in "hello" and "z" not in "hello")
chk("str_index_neg", "hello"[-1] == "o" and "hello"[0] == "h")
chk("str_compare", "apple" < "banana" and "Z" < "a")

# str.__format__ via format() builtin and conversions.
chk("str_format_builtin", format(255, "x") == "ff" and format(255, "#x") == "0xff")
chk("str_format_bin", format(10, "08b") == "00001010")
chk("str_format_pct", format(0.1234, ".1%") == "12.3%")
chk("str_format_sign", format(5, "+") == "+5" and format(-5, " ") == "-5")
chk("str_format_thousands", format(1234567, ",") == "1,234,567" and format(255, "_x") == "ff")


# ======================================================================
# bytes & bytearray — Binary Sequence Types (docs: "Built-in Types >
# bytes/bytearray"). Cover constructors, hex/fromhex, all methods, and
# bytearray mutation. How: build immutable bytes and mutable bytearray,
# assert each operation; expected per docs.
# ======================================================================

# bytes constructors & basic ops.
chk("bytes_ctor_int", bytes(3) == b"\x00\x00\x00")
chk("bytes_ctor_iter", bytes([65, 66, 67]) == b"ABC")
chk("bytes_ctor_str", bytes("ABC", "ascii") == b"ABC")
chk("bytes_index_returns_int", b"ABC"[0] == 65)
chk("bytes_slice", b"ABCDE"[1:3] == b"BC" and b"ABC"[::-1] == b"CBA")

# bytes.hex / bytes.fromhex (PEP 358 + 3.8 sep/bytes_per_sep).
chk("bytes_hex", b"\xde\xad\xbe\xef".hex() == "deadbeef")
chk("bytes_hex_sep", b"\xde\xad\xbe\xef".hex(":") == "de:ad:be:ef")
chk("bytes_hex_sep_n", b"\xde\xad\xbe\xef".hex(" ", 2) == "dead beef")
chk("bytes_fromhex", bytes.fromhex("de ad be ef") == b"\xde\xad\xbe\xef")

# bytes methods shared with str: split/join/strip/replace/find/count/startswith/etc.
chk("bytes_split", b"a,b,c".split(b",") == [b"a", b"b", b"c"])
chk("bytes_rsplit", b"a-b-c".rsplit(b"-", 1) == [b"a-b", b"c"])
chk("bytes_join", b"-".join([b"a", b"b"]) == b"a-b")
chk("bytes_strip", b"  ab  ".strip() == b"ab" and b"xxabxx".strip(b"x") == b"ab")
chk("bytes_replace", b"aaa".replace(b"a", b"bb") == b"bbbbbb")
chk("bytes_find_index", b"hello".find(b"l") == 2 and b"hello".index(b"e") == 1)
chk("bytes_count", b"aaaa".count(b"a") == 4)
chk("bytes_startswith", b"hello".startswith(b"he") and b"a.py".endswith(b".py"))
chk("bytes_partition", b"a=b".partition(b"=") == (b"a", b"=", b"b"))
try:
    b"abc".partition(b""); _r = False
except ValueError:
    _r = True
chk("bytes_partition_empty_raises", _r)
chk("bytes_splitlines", b"a\nb\nc".splitlines() == [b"a", b"b", b"c"])

# bytes case / predicate methods.
chk("bytes_upper_lower", b"AbC".upper() == b"ABC" and b"AbC".lower() == b"abc")
chk("bytes_title_swapcase", b"ab cd".title() == b"Ab Cd" and b"aB".swapcase() == b"Ab")
chk("bytes_isalpha_isdigit", b"abc".isalpha() and b"123".isdigit() and b"a1".isalnum())
chk("bytes_isspace_isupper", b"  ".isspace() and b"ABC".isupper() and b"abc".islower())
chk("bytes_center_just", b"ab".center(6, b"*") == b"**ab**" and b"x".zfill(3) == b"00x")
chk("bytes_expandtabs", b"a\tb".expandtabs(4) == b"a   b")
chk("bytes_translate", b"abc".translate(bytes.maketrans(b"ab", b"AB")) == b"ABc")
chk("bytes_removeprefix", b"foobar".removeprefix(b"foo") == b"bar")
chk("bytes_decode", b"h\xc3\xa9".decode("utf-8") == "hé")
# decode default codec is utf-8; accepts keyword args; errors='replace' substitutes U+FFFD.
chk("bytes_decode_default", b"h\xc3\xa9".decode() == "hé")
chk("bytes_decode_kwargs", b"abc".decode(encoding="ascii", errors="strict") == "abc")
chk("bytes_decode_replace", b"a\x80b".decode("utf-8", "replace") == "a�b")
chk("bytes_decode_ignore", b"a\x80b".decode("utf-8", "ignore") == "ab")
# decode error paths: invalid utf-8 -> UnicodeDecodeError; unknown codec -> LookupError.
try:
    b"\x80".decode("utf-8"); _r = False
except UnicodeDecodeError:
    _r = True
chk("bytes_decode_invalid_raises", _r)
try:
    b"x".decode("no-such-codec-xyz"); _r = False
except LookupError:
    _r = True
chk("bytes_decode_unknown_codec_raises", _r)

# bytearray — mutable: all bytes methods plus in-place mutation.
ba = bytearray(b"hello")
ba[0] = ord("H")
chk("bytearray_setitem", ba == bytearray(b"Hello"))
ba.append(33)
chk("bytearray_append", ba == bytearray(b"Hello!"))
ba.extend(b"??")
chk("bytearray_extend", ba == bytearray(b"Hello!??"))
ba2 = bytearray(b"abcdef")
chk("bytearray_pop", ba2.pop() == ord("f") and ba2 == bytearray(b"abcde"))
ba2.insert(0, ord("Z"))
chk("bytearray_insert", ba2 == bytearray(b"Zabcde"))
ba2.remove(ord("Z"))
chk("bytearray_remove", ba2 == bytearray(b"abcde"))
ba3 = bytearray(b"abcde")
ba3.reverse()
chk("bytearray_reverse", ba3 == bytearray(b"edcba"))
ba4 = bytearray(b"xyz")
ba4[1:2] = b"AB"
chk("bytearray_slice_assign", ba4 == bytearray(b"xABz"))
del ba4[0]
chk("bytearray_delitem", ba4 == bytearray(b"ABz"))
ba5 = bytearray(b"data")
ba5.clear()
chk("bytearray_clear", ba5 == bytearray(b""))
chk("bytearray_hex_fromhex", bytearray.fromhex("4142").hex() == "4142")
chk("bytearray_copy", bytearray(b"ab").copy() == bytearray(b"ab"))
# bytearray mutation error paths.
try:
    bytearray(b"").pop(); _r = False
except IndexError:
    _r = True
chk("bytearray_pop_empty_raises", _r)
try:
    bytearray(b"a").remove(99); _r = False
except ValueError:
    _r = True
chk("bytearray_remove_missing_raises", _r)
# elements must be ints in range(256): out-of-range or wrong type raises.
try:
    bytearray(b"a")[0] = 256; _r = False
except ValueError:
    _r = True
chk("bytearray_setitem_range_raises", _r)


# ======================================================================
# list — Mutable Sequence (docs: "Built-in Types > list" + "Mutable
# Sequence Types"). Cover EVERY method + sort(key,reverse) + copy.
# ======================================================================
lst = [3, 1, 2]
lst.append(4)
chk("list_append", lst == [3, 1, 2, 4])
lst.insert(0, 0)
chk("list_insert", lst == [0, 3, 1, 2, 4])
lst.extend([5, 6])
chk("list_extend", lst == [0, 3, 1, 2, 4, 5, 6])
chk("list_pop", lst.pop() == 6 and lst.pop(0) == 0)
lst.remove(3)
chk("list_remove", lst == [1, 2, 4, 5])
chk("list_index", [10, 20, 30].index(20) == 1)
chk("list_index_range", [1, 2, 1, 2].index(2, 2) == 3)
chk("list_index_start_stop", [1, 2, 1, 2, 1].index(1, 1, 4) == 2)
try:
    [1, 2, 3].index(2, 2, 3); _r = False  # 2 not in slice [2:3]
except ValueError:
    _r = True
chk("list_index_range_raises", _r)
chk("list_count", [1, 1, 2, 1].count(1) == 3)
_rev = [1, 2, 3]
_rev.reverse()
chk("list_reverse", _rev == [3, 2, 1])
_cp = [1, 2, 3]
_cp2 = _cp.copy()
_cp2.append(4)
chk("list_copy", _cp == [1, 2, 3] and _cp2 == [1, 2, 3, 4])
_clr = [1, 2]
_clr.clear()
chk("list_clear", _clr == [])
# list.sort — stable, in-place; key + reverse. sorted() returns new list.
_s = [3, 1, 2]
_s.sort()
chk("list_sort", _s == [1, 2, 3])
_s.sort(reverse=True)
chk("list_sort_reverse", _s == [3, 2, 1])
_sk = ["bbb", "a", "cc"]
_sk.sort(key=len)
chk("list_sort_key", _sk == ["a", "cc", "bbb"])
_stable = [(1, "a"), (0, "b"), (1, "c"), (0, "d")]
_stable.sort(key=lambda t: t[0])
chk("list_sort_stable", _stable == [(0, "b"), (0, "d"), (1, "a"), (1, "c")])
chk("list_sorted_builtin", sorted([3, 1, 2]) == [1, 2, 3] and sorted("cba") == ["a", "b", "c"])
# list operators: +, *, slice-assign, del, in.
chk("list_concat", [1, 2] + [3] == [1, 2, 3] and [0] * 3 == [0, 0, 0])
_sa = [1, 2, 3, 4, 5]
_sa[1:3] = [9]
chk("list_slice_assign", _sa == [1, 9, 4, 5])
# extended slice assignment requires matching length (len(_sa[::2]) == 2 here).
_sa[::2] = [0, 0]
chk("list_ext_slice_assign", _sa == [0, 9, 0, 5])
try:
    _bad = [1, 2, 3, 4]; _bad[::2] = [0, 0, 0]; _r = False
except ValueError:
    _r = True
chk("list_ext_slice_len_mismatch", _r)
del _sa[0]
chk("list_del", _sa == [9, 0, 5])
chk("list_in_len", 9 in _sa and 99 not in _sa and len([1, 2, 3]) == 3)
try:
    [1, 2].remove(9); _r = False
except ValueError:
    _r = True
chk("list_remove_raises", _r)


# ======================================================================
# tuple — Immutable Sequence (docs: "Built-in Types > tuple"). Methods:
# count, index; plus immutability and single-element syntax.
# ======================================================================
chk("tuple_count_index", (1, 2, 2, 3).count(2) == 2 and (5, 6, 7).index(6) == 1)
chk("tuple_single", type((1,)) is tuple and type((1)) is int)
chk("tuple_concat_repeat", (1, 2) + (3,) == (1, 2, 3) and (0,) * 3 == (0, 0, 0))
try:
    t = (1, 2); t[0] = 9; _r = False
except TypeError:
    _r = True
chk("tuple_immutable", _r)
chk("tuple_empty", () == tuple() and len(()) == 0)


# ======================================================================
# dict — Mapping Type (docs: "Built-in Types > dict"). Cover EVERY
# method: get/setdefault/pop/popitem/update/keys/values/items/fromkeys/
# copy/clear + | and |= merge (PEP 584, 3.9) + view set-ops.
# ======================================================================
d = {"a": 1, "b": 2}
chk("dict_get", d.get("a") == 1 and d.get("z") is None and d.get("z", 0) == 0)
chk("dict_setdefault", d.setdefault("a", 99) == 1 and d.setdefault("c", 3) == 3 and d["c"] == 3)
chk("dict_pop", d.pop("c") == 3 and d.pop("zz", "dflt") == "dflt")
try:
    {}.pop("x"); _r = False
except KeyError:
    _r = True
chk("dict_pop_raises", _r)
_pi = {"only": 1}
chk("dict_popitem", _pi.popitem() == ("only", 1) and _pi == {})
# popitem is LIFO (3.7+) and raises KeyError on an empty dict.
_pio = {"a": 1, "b": 2, "c": 3}
chk("dict_popitem_lifo", _pio.popitem() == ("c", 3) and _pio == {"a": 1, "b": 2})
try:
    {}.popitem(); _r = False
except KeyError:
    _r = True
chk("dict_popitem_empty_raises", _r)
# setdefault mutates only when key absent; returns existing/new value + leaves state coherent.
_sd = {"x": 1}
chk("dict_setdefault_state", _sd.setdefault("x", 99) == 1 and _sd["x"] == 1 and
    _sd.setdefault("y") is None and _sd == {"x": 1, "y": None})
_u = {"a": 1}
_u.update({"a": 9, "b": 2})
_u.update(c=3)
_u.update([("d", 4)])
chk("dict_update", _u == {"a": 9, "b": 2, "c": 3, "d": 4})
chk("dict_fromkeys", dict.fromkeys(["x", "y"], 0) == {"x": 0, "y": 0})
chk("dict_fromkeys_none", dict.fromkeys("ab") == {"a": None, "b": None})
_dc = {"a": 1}
_dc2 = _dc.copy()
_dc2["b"] = 2
chk("dict_copy", _dc == {"a": 1} and _dc2 == {"a": 1, "b": 2})
_dcl = {"a": 1}
_dcl.clear()
chk("dict_clear", _dcl == {})
# views: keys/values/items; keys & items behave as set-like.
_dv = {"a": 1, "b": 2, "c": 3}
chk("dict_keys", set(_dv.keys()) == {"a", "b", "c"})
chk("dict_values", sorted(_dv.values()) == [1, 2, 3])
chk("dict_items", set(_dv.items()) == {("a", 1), ("b", 2), ("c", 3)})
chk("dict_keys_setop", (_dv.keys() & {"a", "z"}) == {"a"} and (_dv.keys() | {"d"}) == {"a", "b", "c", "d"})
chk("dict_items_setop", (_dv.items() & {("a", 1), ("a", 9)}) == {("a", 1)})
# PEP 584 merge operators.
chk("dict_merge_op", ({"a": 1} | {"b": 2, "a": 9}) == {"a": 9, "b": 2})
_de = {"a": 1}
_de |= {"b": 2}
chk("dict_merge_iop", _de == {"a": 1, "b": 2})
# insertion order preserved (3.7+ language guarantee).
chk("dict_order", list({"z": 0, "a": 0, "m": 0}.keys()) == ["z", "a", "m"])
# membership / len / reversed (3.8+).
chk("dict_in_len", "a" in _dv and "z" not in _dv and len(_dv) == 3)
chk("dict_reversed", list(reversed({"a": 1, "b": 2, "c": 3})) == ["c", "b", "a"])
try:
    {}["missing"]; _r = False
except KeyError:
    _r = True
chk("dict_keyerror", _r)


# ======================================================================
# set & frozenset — Set Types (docs: "Built-in Types > set/frozenset").
# Cover EVERY method + operators + algebra. frozenset is immutable +
# hashable. How: assert each set operation in both method and operator
# form; assert mutating methods raise on frozenset.
# ======================================================================
s1 = {1, 2, 3}
s2 = {2, 3, 4}
chk("set_union", s1 | s2 == {1, 2, 3, 4} and s1.union(s2, {5}) == {1, 2, 3, 4, 5})
chk("set_intersection", s1 & s2 == {2, 3} and s1.intersection(s2) == {2, 3})
chk("set_difference", s1 - s2 == {1} and s1.difference(s2) == {1})
chk("set_symdiff", s1 ^ s2 == {1, 4} and s1.symmetric_difference(s2) == {1, 4})
chk("set_subset", {1, 2}.issubset({1, 2, 3}) and {1, 2} <= {1, 2})
chk("set_proper_subset", {1, 2} < {1, 2, 3} and not ({1, 2} < {1, 2}))
chk("set_superset", {1, 2, 3}.issuperset({1, 2}) and {1, 2, 3} >= {1, 2})
chk("set_disjoint", {1, 2}.isdisjoint({3, 4}) and not {1, 2}.isdisjoint({2}))
# mutating set methods.
_ms = {1, 2, 3}
_ms.add(4)
chk("set_add", _ms == {1, 2, 3, 4})
_ms.discard(4)
_ms.discard(99)  # discard is silent on missing
chk("set_discard", _ms == {1, 2, 3})
_ms.remove(3)
chk("set_remove", _ms == {1, 2})
try:
    {1}.remove(9); _r = False
except KeyError:
    _r = True
chk("set_remove_raises", _r)
_pp = {7}
chk("set_pop", _pp.pop() == 7 and _pp == set())
_us = {1, 2}
_us.update({2, 3}, {4})
chk("set_update", _us == {1, 2, 3, 4})
_is = {1, 2, 3}
_is.intersection_update({2, 3, 9})
chk("set_intersection_update", _is == {2, 3})
_ds = {1, 2, 3}
_ds.difference_update({2})
chk("set_difference_update", _ds == {1, 3})
_xs = {1, 2, 3}
_xs.symmetric_difference_update({3, 4})
chk("set_symdiff_update", _xs == {1, 2, 4})
_iup = {1, 2}; _iup |= {3}; _iup &= {2, 3}; _iup ^= {2}
chk("set_aug_ops", _iup == {3})
_cs = {1, 2}
_cs2 = _cs.copy()
_cs2.add(9)
chk("set_copy", _cs == {1, 2})
_cl = {1, 2}
_cl.clear()
chk("set_clear", _cl == set())
chk("set_in_len", 2 in {1, 2, 3} and 9 not in {1, 2} and len({1, 2, 3}) == 3)
# frozenset — immutable + hashable, supports non-mutating set algebra.
fs = frozenset([1, 2, 3])
chk("frozenset_ops", (fs | {4}) == frozenset([1, 2, 3, 4]) and (fs & {2}) == frozenset([2]))
chk("frozenset_hashable", len({frozenset([1, 2]), frozenset([2, 1])}) == 1)
chk("frozenset_methods", fs.union({4}) == frozenset([1, 2, 3, 4]) and fs.issubset({1, 2, 3, 4}))
try:
    fs.add(9); _r = False
except AttributeError:
    _r = True
chk("frozenset_immutable", _r)


# ======================================================================
# int — Numeric Type (docs: "Built-in Types > int"). Cover bit_length,
# bit_count, to_bytes/from_bytes, as_integer_ratio, is_integer,
# numerator/denominator, conjugate, __index__, and int() bases.
# ======================================================================
chk("int_bit_length", (0).bit_length() == 0 and (5).bit_length() == 3 and (255).bit_length() == 8)
chk("int_bit_count", (7).bit_count() == 3 and (255).bit_count() == 8 and (0).bit_count() == 0)
# to_bytes/from_bytes defaults (3.11+): length=1, byteorder="big".
chk("int_to_bytes_default", (5).to_bytes() == b"\x05" and int.from_bytes(b"\x05") == 5)
try:
    (256).to_bytes(1, "big"); _r = False  # does not fit in 1 byte
except OverflowError:
    _r = True
chk("int_to_bytes_overflow_raises", _r)
chk("int_to_bytes_be", (1024).to_bytes(2, "big") == b"\x04\x00")
chk("int_to_bytes_le", (1024).to_bytes(2, "little") == b"\x00\x04")
chk("int_to_bytes_signed", (-1).to_bytes(2, "big", signed=True) == b"\xff\xff")
chk("int_from_bytes_be", int.from_bytes(b"\x04\x00", "big") == 1024)
chk("int_from_bytes_le", int.from_bytes(b"\x00\x04", "little") == 1024)
chk("int_from_bytes_signed", int.from_bytes(b"\xff\xff", "big", signed=True) == -1)
chk("int_as_integer_ratio", (5).as_integer_ratio() == (5, 1))
chk("int_is_integer", (7).is_integer() is True)  # 3.12+: int.is_integer always True
chk("int_numerator_denominator", (12).numerator == 12 and (12).denominator == 1)
chk("int_conjugate_real_imag", (5).conjugate() == 5 and (5).real == 5 and (5).imag == 0)
chk("int_bases", int("ff", 16) == 255 and int("0b101", 0) == 5 and int("777", 8) == 511)
chk("int_index", hex(0xABCD) == "0xabcd" and oct(8) == "0o10" and bin(5) == "0b101")
chk("int_float_int", int(3.9) == 3 and int(-3.9) == -3)
chk("int_round_ndigits", round(12345, -2) == 12300)
# int base conversion error path: invalid literal for the given base raises ValueError.
try:
    int("ff", 10); _r = False
except ValueError:
    _r = True
chk("int_invalid_base_raises", _r)


# ======================================================================
# float — Numeric Type (docs: "Built-in Types > float"). Cover
# is_integer, hex/fromhex, as_integer_ratio, conjugate, real/imag,
# and special values (inf/nan) behavior.
# ======================================================================
chk("float_is_integer", (2.0).is_integer() and not (2.5).is_integer())
chk("float_hex", (3.5).hex() == "0x1.c000000000000p+1")
chk("float_fromhex", float.fromhex("0x1.8p+1") == 3.0 and float.fromhex("0x1.c000000000000p+1") == 3.5)
chk("float_as_integer_ratio", (0.5).as_integer_ratio() == (1, 2) and (2.0).as_integer_ratio() == (2, 1))
chk("float_real_imag_conj", (1.5).real == 1.5 and (1.5).imag == 0.0 and (1.5).conjugate() == 1.5)
_inf = float("inf"); _ninf = float("-inf"); _nan = float("nan")
chk("float_inf", _inf > 1e308 and _ninf < -1e308 and _inf + 1 == _inf)
chk("float_nan", _nan != _nan and not (_nan < 1) and not (_nan > 1))
import math as _m
chk("float_isnan_isinf", _m.isnan(_nan) and _m.isinf(_inf) and _m.isfinite(1.0))
# round(float): banker's rounding to even; ndigits omitted yields int, given yields float.
chk("float_round_even", round(0.5) == 0 and round(1.5) == 2 and round(2.5) == 2)
chk("float_round_ndigits", round(3.14159, 2) == 3.14 and round(2.675, 2) == 2.67)
chk("float_round_types", type(round(2.7)) is int and type(round(2.7, 0)) is float)
# float() string parsing accepts inf/nan spellings; bad literal raises ValueError.
chk("float_from_str", float("inf") == _inf and float("  1.5  ") == 1.5 and _m.isnan(float("nan")))
try:
    float("not-a-number"); _r = False
except ValueError:
    _r = True
chk("float_from_str_raises", _r)


# ======================================================================
# complex — Numeric Type (docs: "Built-in Types > complex"). Cover
# real/imag/conjugate, abs, arithmetic, and constructor from string.
# ======================================================================
z = complex(3, 4)
chk("complex_real_imag", z.real == 3.0 and z.imag == 4.0)
chk("complex_conjugate", z.conjugate() == complex(3, -4))
chk("complex_abs", abs(z) == 5.0)
chk("complex_arith", (complex(1, 1) + complex(2, 3)) == complex(3, 4))
chk("complex_div", (complex(1, 0) / complex(0, 1)) == complex(0, -1))
chk("complex_from_str", complex("1+2j") == complex(1, 2) and complex("3j") == complex(0, 3))
chk("complex_pow", (1j) ** 2 == complex(-1, 0))
# malformed complex string raises ValueError; embedded spaces also rejected.
try:
    complex("1 + 2j"); _r = False
except ValueError:
    _r = True
chk("complex_from_str_raises", _r)


# ======================================================================
# bool — Boolean Type (docs: "Built-in Types > Boolean", bool subclasses
# int). Cover truthiness, int relationship, and logical operators.
# ======================================================================
chk("bool_is_int_subclass", issubclass(bool, int) and True == 1 and False == 0)
chk("bool_arith", True + True == 2 and True * 5 == 5)
chk("bool_constructor", bool(0) is False and bool("") is False and bool([1]) is True and bool(None) is False)
chk("bool_logical_short", (0 or "x") == "x" and (1 and "y") == "y" and (None or 0 or 5) == 5)
chk("bool_repr", repr(True) == "True" and str(False) == "False")
chk("bool_and_or_xor", (True & False) is False and (True | False) is True and (True ^ True) is False)


# ======================================================================
# range — Sequence (docs: "Built-in Types > range"). Cover start/stop/
# step attrs, indexing, slicing, len, in, index/count, reversed, equality.
# ======================================================================
r = range(2, 20, 3)
chk("range_attrs", r.start == 2 and r.stop == 20 and r.step == 3)
chk("range_list", list(range(5)) == [0, 1, 2, 3, 4] and list(range(1, 6, 2)) == [1, 3, 5])
chk("range_neg_step", list(range(5, 0, -1)) == [5, 4, 3, 2, 1])
# range step of zero is rejected with ValueError.
try:
    range(0, 10, 0); _r = False
except ValueError:
    _r = True
chk("range_step_zero_raises", _r)
chk("range_index_slice", r[0] == 2 and r[-1] == 17 and list(r[1:3]) == [5, 8])
chk("range_len", len(range(0, 10, 3)) == 4 and len(range(10, 0)) == 0)
chk("range_in", 8 in r and 9 not in r)
chk("range_index_count", range(0, 10, 2).index(4) == 2 and range(0, 10, 2).count(4) == 1)
chk("range_reversed", list(reversed(range(3))) == [2, 1, 0])
chk("range_equality", range(0, 5) == range(0, 5) and range(0, 3, 2) == range(0, 4, 2))


# ======================================================================
# memoryview — (docs: "Built-in Types > memoryview"). Cover cast,
# tobytes, tolist, format, itemsize, nbytes, ndim, shape, readonly,
# slicing, and writing through a mutable buffer.
# ======================================================================
mv = memoryview(b"abcd")
chk("memoryview_tobytes", mv.tobytes() == b"abcd")
chk("memoryview_tolist", mv.tolist() == [97, 98, 99, 100])
chk("memoryview_format", mv.format == "B" and mv.itemsize == 1)
chk("memoryview_nbytes", mv.nbytes == 4 and mv.ndim == 1 and mv.shape == (4,))
chk("memoryview_readonly", mv.readonly is True)
chk("memoryview_index_slice", mv[0] == 97 and mv[1:3].tobytes() == b"bc")
chk("memoryview_in_len", 97 in mv and len(mv) == 4)
# cast: reinterpret bytes as another format.
mvc = memoryview(b"\x01\x00\x00\x00\x02\x00\x00\x00").cast("I")
chk("memoryview_cast", mvc.tolist() == [1, 2] and mvc.itemsize == 4 and mvc.format == "I")
mvc2 = memoryview(bytearray(b"\x00\x00\x00\x00")).cast("H")
chk("memoryview_cast_shape", mvc2.shape == (2,) and mvc2.nbytes == 4)
# cast to other format codes — verify signed/multi-byte/float interpretation
# (native byte order, so build buffers with struct using '=' native std layout).
import struct as _struct
chk("memoryview_cast_b", memoryview(b"\xff").cast("b").tolist() == [-1] and
    memoryview(b"\xff").cast("b").itemsize == 1)
chk("memoryview_cast_h", memoryview(_struct.pack("=h", -1)).cast("h").tolist() == [-1] and
    memoryview(_struct.pack("=h", -1)).cast("h").itemsize == 2)
chk("memoryview_cast_i", memoryview(_struct.pack("=i", -2)).cast("i").tolist() == [-2] and
    memoryview(_struct.pack("=i", -2)).cast("i").itemsize == 4)
chk("memoryview_cast_f", memoryview(_struct.pack("=f", 1.5)).cast("f").tolist() == [1.5] and
    memoryview(_struct.pack("=f", 1.5)).cast("f").itemsize == 4)
chk("memoryview_cast_d", memoryview(_struct.pack("=d", 2.5)).cast("d").tolist() == [2.5] and
    memoryview(_struct.pack("=d", 2.5)).cast("d").itemsize == 8)
# write through a mutable buffer.
buf = bytearray(b"xxxx")
mvw = memoryview(buf)
mvw[0] = ord("Y")
mvw[1:3] = b"AB"
chk("memoryview_write", buf == bytearray(b"YABx") and mvw.readonly is False)
# cast back to bytes view and verify round-trip via tobytes.
chk("memoryview_roundtrip", memoryview(buf).cast("B").tobytes() == bytes(buf))
# contiguity / structural attributes (strides, suboffsets, obj, c_/f_contiguous).
_src = bytearray(b"abcd")
mvs = memoryview(_src)
chk("memoryview_strides", mvs.strides == (1,) and mvs.suboffsets == () and mvs.contiguous is True)
chk("memoryview_contiguous_flags", mvs.c_contiguous is True and mvs.f_contiguous is True)
chk("memoryview_obj", mvs.obj is _src)
# toreadonly (3.8): yields a read-only view over the same buffer; original stays writable.
_mvro = mvs.toreadonly()
chk("memoryview_toreadonly", _mvro.readonly is True and mvs.readonly is False and _mvro.tobytes() == b"abcd")
# explicit release(): subsequent access raises ValueError; release is idempotent.
_mvr = memoryview(bytearray(b"qq"))
_mvr.release()
_mvr.release()  # idempotent, no error
try:
    _mvr.tobytes(); _r = False
except ValueError:
    _r = True
chk("memoryview_release", _r)
# memoryview supports release / context manager (auto-release on exit).
_mvbuf = bytearray(b"zz")
with memoryview(_mvbuf) as _mvctx:
    chk("memoryview_contextmanager", _mvctx.tobytes() == b"zz")
try:
    _mvctx.tobytes(); _r = False  # released after the with-block
except ValueError:
    _r = True
chk("memoryview_contextmanager_released", _r)


# ======================================================================
# Cross-type builtins on these types: hash, repr/eval round-trips,
# comparison chains, and type identity (docs: "Built-in Functions").
# ======================================================================
chk("hash_immutables", hash((1, 2)) == hash((1, 2)) and hash("ab") == hash("ab"))
chk("repr_eval_roundtrip", eval(repr([1, "a", (2, 3)])) == [1, "a", (2, 3)])
chk("comparison_chain", (1 < 2 < 3) and not (1 < 2 > 3))
chk("type_identity", type([]) is list and type(()) is tuple and type({}) is dict and type(set()) is set)


# ======================================================================
# 3.14 version-gated SYNTAX (PEP 750 t-strings). Isolated in exec()'d
# source so this file PARSES on 3.12; runs only when the interpreter
# supports the syntax, else records a noted skip. t-strings produce a
# string.templatelib.Template, NOT an interpolated str.
# ======================================================================
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
    chk(name, probe(ns))


# PEP 750 (3.14): t-string yields a Template whose .strings/.values/.interpolations
# expose static text and interpolated values separately (not auto-joined to str).
_gated_syntax(
    "pep750_tstring_template", (3, 14),
    "x = 7\n"
    "tmpl = t'val={x}!'\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values)\n",
    lambda ns: ns["R"] == ("Template", ("val=", "!"), (7,)),
)

# PEP 750 (3.14): t-string Interpolation objects carry expression/conversion/format_spec.
_gated_syntax(
    "pep750_tstring_interp", (3, 14),
    "n = 'NAME'\n"
    "tmpl = t'{n!r:>10}'\n"
    "interp = tmpl.interpolations[0]\n"
    "R = (interp.value, interp.expression, interp.conversion, interp.format_spec)\n",
    lambda ns: ns["R"] == ("NAME", "n", "r", ">10"),
)


print(("PY_BTYPES_OK") if _ok else ("PY_BTYPES_FAIL"))
sys.exit(0 if _ok else 1)
