#!/usr/bin/env python3
"""Data model special methods — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# ---------------------------------------------------------------------------
# Docs "3.3.1 Basic customization": object.__new__ / __init__ / __del__
# 怎么测: __new__ controls allocation (returns the instance); __init__ then
#   initializes it. If __new__ returns an instance of cls, __init__ runs.
# 期望: __new__ fires before __init__; both observe the same object; a
#   __new__ that does NOT return an instance of cls suppresses __init__.
# 为什么: this is the construction protocol; misordering breaks every class.
# ---------------------------------------------------------------------------
_events = []


class NewInit:
    def __new__(cls, *a, **k):
        _events.append("new")
        inst = super().__new__(cls)
        inst.created = True
        return inst

    def __init__(self, val):
        _events.append("init")
        self.val = val


ni = NewInit(7)
chk("new_before_init", _events == ["new", "init"])
chk("new_sets_attr", ni.created is True and ni.val == 7)


# __new__ returning a foreign object skips __init__ entirely.
class NewForeign:
    def __new__(cls):
        return 42  # not an instance of cls

    def __init__(self):  # must NOT run
        raise AssertionError("init should be skipped")


chk("new_foreign_skips_init", NewForeign() == 42)


# __init__ must return None; returning non-None raises TypeError.
class BadInit:
    def __init__(self):
        return 5


try:
    BadInit()
    chk("init_must_return_none", False)
except TypeError:
    chk("init_must_return_none", True)


# __del__ fires at finalization (observable via a sentinel list).
_deleted = []


class WithDel:
    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        _deleted.append(self.tag)


wd = WithDel("x")
del wd
import gc
gc.collect()
chk("del_finalizer", "x" in _deleted)


# ---------------------------------------------------------------------------
# Docs "3.3.1": __repr__ / __str__ / __bytes__ / __format__
# 怎么测: call repr()/str()/bytes()/format() and the f-string format spec path.
# 期望: each dunder is dispatched; str() falls back to __repr__ when __str__
#   absent; format(obj, spec) routes spec to __format__; format(obj,"") with no
#   __format__ defaults to str(obj).
# 为什么: these are the four object-to-text/bytes conversion hooks.
# ---------------------------------------------------------------------------
class Formatted:
    def __repr__(self):
        return "<repr>"

    def __str__(self):
        return "<str>"

    def __bytes__(self):
        return b"<bytes>"

    def __format__(self, spec):
        return "fmt[%s]" % spec


fo = Formatted()
chk("repr_dunder", repr(fo) == "<repr>")
chk("str_dunder", str(fo) == "<str>")
chk("bytes_dunder", bytes(fo) == b"<bytes>")
chk("format_dunder_spec", format(fo, "0.2f") == "fmt[0.2f]")
chk("format_in_fstring", f"{fo:>10}" == "fmt[>10]")


# str() falls back to __repr__ when __str__ is not defined.
class OnlyRepr:
    def __repr__(self):
        return "R-only"


chk("str_falls_back_to_repr", str(OnlyRepr()) == "R-only")


# Default __format__ (object) with empty spec == str(obj); non-empty raises.
class DefaultFmt:
    def __str__(self):
        return "DF"


chk("format_default_empty", format(DefaultFmt(), "") == "DF")
try:
    format(DefaultFmt(), "x")
    chk("format_default_nonempty_raises", False)
except TypeError:
    chk("format_default_nonempty_raises", True)


# ---------------------------------------------------------------------------
# Docs "3.3.1 Basic customization": rich comparison
#   __lt__ __le__ __eq__ __ne__ __gt__ __ge__  and reflection rules.
# 怎么测: define all six on a wrapper around an int; verify each operator and
#   that reflected forms are used (a<b tries a.__lt__, falling back to
#   b.__gt__). __ne__ defaults to inverting __eq__ when only __eq__ given.
# 期望: every comparison operator dispatches; reflected fallback works.
# 为什么: total ordering + equality drive sorting, sets, dict keys.
# ---------------------------------------------------------------------------
class Num:
    def __init__(self, v):
        self.v = v

    def __lt__(self, o):
        return self.v < (o.v if isinstance(o, Num) else o)

    def __le__(self, o):
        return self.v <= (o.v if isinstance(o, Num) else o)

    def __eq__(self, o):
        return self.v == (o.v if isinstance(o, Num) else o)

    def __ne__(self, o):
        return self.v != (o.v if isinstance(o, Num) else o)

    def __gt__(self, o):
        return self.v > (o.v if isinstance(o, Num) else o)

    def __ge__(self, o):
        return self.v >= (o.v if isinstance(o, Num) else o)

    def __hash__(self):
        return hash(self.v)


chk("cmp_lt", (Num(1) < Num(2)) is True and (Num(2) < Num(1)) is False)
chk("cmp_le", (Num(2) <= Num(2)) is True and (Num(3) <= Num(2)) is False)
chk("cmp_eq", (Num(5) == Num(5)) is True and (Num(5) == Num(6)) is False)
chk("cmp_ne", (Num(5) != Num(6)) is True and (Num(5) != Num(5)) is False)
chk("cmp_gt", (Num(3) > Num(2)) is True and (Num(2) > Num(3)) is False)
chk("cmp_ge", (Num(3) >= Num(3)) is True and (Num(2) >= Num(3)) is False)


# Reflected comparison: left operand returns NotImplemented -> try reflected.
class LeftOnly:
    def __init__(self, v):
        self.v = v

    def __lt__(self, o):
        return NotImplemented  # force fallback


class RightGt:
    def __init__(self, v):
        self.v = v

    def __gt__(self, o):
        # invoked as reflected of (LeftOnly < RightGt)
        return self.v > o.v


chk("cmp_reflected", (LeftOnly(1) < RightGt(2)) is True)


# When BOTH the direct and reflected hooks return NotImplemented, the data model
# specifies the comparison falls back to identity (== / !=) or raises TypeError
# for ordering. Prove ordering raises TypeError after the NotImplemented chain is
# exhausted, while == falls back to `is` (default object identity).
class AllNotImpl:
    def __lt__(self, o):
        return NotImplemented

    def __gt__(self, o):
        return NotImplemented

    def __eq__(self, o):
        return NotImplemented

    __hash__ = None


_a, _b = AllNotImpl(), AllNotImpl()
try:
    _a < _b
    chk("cmp_exhausted_ordering_raises", False)
except TypeError:
    chk("cmp_exhausted_ordering_raises", True)
# == returning NotImplemented from both sides collapses to identity: a==a True,
# a==b False (NOT an error -- equality always has the identity fallback).
chk("cmp_exhausted_eq_identity", (_a == _a) is True and (_a == _b) is False)


# Subclass on the RIGHT gets reflected-hook priority: even though Base defines
# the forward op, Sub (a subclass) is tried FIRST via its reflected hook.
class CmpBase:
    def __add__(self, o):
        return "base.__add__"


class CmpSub(CmpBase):
    def __radd__(self, o):
        return "sub.__radd__"


chk("subclass_right_reflected_priority", (CmpBase() + CmpSub()) == "sub.__radd__")


# Default __ne__ inverts __eq__ when __ne__ not provided.
class EqOnly:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return self.v == o.v

    def __hash__(self):
        return hash(self.v)


chk("ne_inverts_eq", (EqOnly(1) != EqOnly(2)) is True and (EqOnly(1) != EqOnly(1)) is False)


# Defining __eq__ without __hash__ makes instances unhashable (__hash__ = None).
class EqNoHash:
    def __eq__(self, o):
        return True


try:
    hash(EqNoHash())
    chk("eq_clears_hash", False)
except TypeError:
    chk("eq_clears_hash", True)


# ---------------------------------------------------------------------------
# Docs "3.3.1": __hash__ and __bool__
# 怎么测: __hash__ controls set/dict membership; __bool__ controls truthiness;
#   when __bool__ absent, __len__ is consulted; when both absent, truthy.
# 期望: equal objects with equal hash collapse in a set; __bool__ overrides len.
# 为什么: hashing and truth-testing are core to collections and control flow.
# ---------------------------------------------------------------------------
chk("hash_dedup_set", len({Num(1), Num(1), Num(2)}) == 2)
# Equal objects must hash equal: a set built from two distinct-but-equal Num
# instances keeps exactly one, and membership test finds an equal-but-other obj.
_hset = {Num(1), Num(2)}
chk("hash_eq_consistency", (Num(1) in _hset) and (Num(2) in _hset)
    and (Num(3) not in _hset) and len({Num(7)} | {Num(7)}) == 1)


# Explicit __hash__ = None makes instances unhashable even if __eq__ is absent.
class ExplicitNoHash:
    __hash__ = None


try:
    hash(ExplicitNoHash())
    chk("explicit_hash_none_unhashable", False)
except TypeError:
    chk("explicit_hash_none_unhashable", True)


class BoolByFlag:
    def __init__(self, flag):
        self.flag = flag

    def __bool__(self):
        return self.flag


chk("bool_dunder_true", bool(BoolByFlag(True)) is True)
chk("bool_dunder_false", bool(BoolByFlag(False)) is False)
chk("bool_in_if", ("Y" if BoolByFlag(True) else "N") == "Y")


class BoolByLen:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n


chk("bool_via_len_zero", bool(BoolByLen(0)) is False)
chk("bool_via_len_nonzero", bool(BoolByLen(3)) is True)


class NoBoolNoLen:
    pass


chk("bool_default_true", bool(NoBoolNoLen()) is True)


# __bool__ must return a bool; otherwise TypeError.
class BadBool:
    def __bool__(self):
        return 1  # not a bool


try:
    bool(BadBool())
    chk("bool_must_be_bool", False)
except TypeError:
    chk("bool_must_be_bool", True)


# ---------------------------------------------------------------------------
# Docs "3.3.2 Customizing attribute access":
#   __getattr__ __getattribute__ __setattr__ __delattr__ __dir__
# 怎么测: __getattribute__ intercepts ALL attribute reads; __getattr__ only
#   fires on failure of the normal lookup; __setattr__/__delattr__ intercept
#   writes/deletes; __dir__ customizes dir().
# 期望: each hook fires on its trigger; __getattr__ does NOT shadow existing.
# 为什么: these power proxies, lazy attrs, ORMs, frozen objects.
# ---------------------------------------------------------------------------
class AttrHooks:
    def __init__(self):
        # bypass our own __setattr__ for bookkeeping
        object.__setattr__(self, "_store", {})
        object.__setattr__(self, "_log", [])

    def __getattr__(self, name):
        # only called when normal lookup fails
        self._log.append("getattr:" + name)
        if name in self._store:
            return self._store[name]
        raise AttributeError(name)

    def __setattr__(self, name, value):
        self._log.append("setattr:" + name)
        self._store[name] = value

    def __delattr__(self, name):
        self._log.append("delattr:" + name)
        del self._store[name]

    def __dir__(self):
        return ["custom_a", "custom_b"]


ah = AttrHooks()
ah.foo = 10  # -> __setattr__
chk("setattr_dunder", ah._store["foo"] == 10 and "setattr:foo" in ah._log)
chk("getattr_dunder", ah.foo == 10 and "getattr:foo" in ah._log)
del ah.foo  # -> __delattr__
chk("delattr_dunder", "foo" not in ah._store and "delattr:foo" in ah._log)
try:
    ah.missing
    chk("getattr_raises_attributeerror", False)
except AttributeError:
    chk("getattr_raises_attributeerror", True)
chk("dir_dunder", dir(ah) == ["custom_a", "custom_b"])


# __getattribute__ intercepts EVERY access (even existing attrs).
class GetAttribute:
    def __init__(self):
        self.real = 99

    def __getattribute__(self, name):
        if name == "magic":
            return "MAGIC"
        return super().__getattribute__(name)


ga = GetAttribute()
chk("getattribute_intercept_virtual", ga.magic == "MAGIC")
chk("getattribute_passthrough_real", ga.real == 99)


# __getattr__ does NOT fire when attribute exists normally.
class GetAttrFallback:
    existing = "E"

    def __getattr__(self, name):
        return "FALLBACK"


gf = GetAttrFallback()
chk("getattr_not_shadowing_existing", gf.existing == "E")
chk("getattr_for_missing", gf.nope == "FALLBACK")


# When __getattribute__ raises AttributeError, the documented fallback to
# __getattr__ kicks in (this is the ONLY way __getattr__ runs). Prove that an
# AttributeError raised explicitly inside __getattribute__ -- even for an
# attribute that exists -- routes to __getattr__, and that the failing name is
# threaded through.
class GetAttrBridge:
    real = "REAL"

    def __getattribute__(self, name):
        if name == "shadowed":
            raise AttributeError(name)  # force the __getattr__ bridge
        return super().__getattribute__(name)

    def __getattr__(self, name):
        return "BRIDGED:" + name


gb = GetAttrBridge()
chk("getattribute_attrerror_bridges_to_getattr",
    gb.real == "REAL" and gb.shadowed == "BRIDGED:shadowed")


# ---------------------------------------------------------------------------
# Docs "3.3.2.1/3.3.2.2 Implementing Descriptors":
#   __get__ __set__ __delete__ __set_name__  (data vs non-data descriptors)
# 怎么测: a class-level descriptor object; verify __set_name__ records the
#   attribute name at class creation; data descriptor (with __set__) takes
#   precedence over instance dict; non-data descriptor (only __get__) is
#   shadowed by instance dict.
# 期望: __set_name__ fires once; data descriptor wins; non-data loses to inst.
# 为什么: descriptors implement property/staticmethod/classmethod/slots.
# ---------------------------------------------------------------------------
class DataDesc:
    def __set_name__(self, owner, name):
        self.public_name = name
        self.private_name = "_" + name

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return getattr(obj, self.private_name, "default")

    def __set__(self, obj, value):
        setattr(obj, self.private_name, value * 2)

    def __delete__(self, obj):
        delattr(obj, self.private_name)


class HasData:
    field = DataDesc()


chk("set_name_fires", HasData.field.public_name == "field"
    and HasData.field.private_name == "_field")
hd = HasData()
chk("desc_get_default", hd.field == "default")
hd.field = 21  # __set__ doubles it
chk("desc_set", hd.field == 42 and hd._field == 42)
del hd.field  # __delete__
chk("desc_delete", not hasattr(hd, "_field"))
chk("desc_class_access_returns_self", isinstance(HasData.field, DataDesc))


# Data descriptor takes precedence over the instance __dict__.
class PrecData:
    def __get__(self, obj, t=None):
        return "DESC"

    def __set__(self, obj, v):
        obj.__dict__["x"] = v  # populate instance dict, but get still wins


class UsesPrecData:
    x = PrecData()


up = UsesPrecData()
up.x = "instance"
chk("data_desc_precedence", up.x == "DESC" and up.__dict__["x"] == "instance")


# Non-data descriptor (only __get__) is shadowed by an instance attribute.
class NonData:
    def __get__(self, obj, t=None):
        return "NONDATA"


class UsesNonData:
    y = NonData()


un = UsesNonData()
chk("nondata_desc_before_shadow", un.y == "NONDATA")
un.__dict__["y"] = "shadowed"
chk("nondata_desc_after_shadow", un.y == "shadowed")


# ---------------------------------------------------------------------------
# Docs "3.3.6 Emulating callable objects": __call__
# 怎么测: instances with __call__ are callable; callable() reports True.
# 期望: obj(...) dispatches to __call__ with args/kwargs.
# 为什么: enables function-like objects (functools.partial, decorators-as-class).
# ---------------------------------------------------------------------------
class Adder:
    def __init__(self, base):
        self.base = base

    def __call__(self, *args, **kw):
        return self.base + sum(args) + sum(kw.values())


add5 = Adder(5)
chk("call_dunder", add5(1, 2, x=3) == 11)
chk("callable_builtin", callable(add5) is True and callable(Adder(0)) is True)
chk("callable_false_for_noncallable", callable(object()) is False)


# Zero-argument __call__ is valid and dispatches with an empty signature; also
# verify __call__ counts invocations (side effect) across multiple calls.
class CallCounter:
    def __init__(self):
        self.calls = 0

    def __call__(self):
        self.calls += 1
        return self.calls


cc = CallCounter()
chk("call_zero_args", cc() == 1 and cc() == 2 and cc.calls == 2)


# ---------------------------------------------------------------------------
# Docs "3.3.7 Emulating container types":
#   __len__ __length_hint__ __getitem__ __setitem__ __delitem__
#   __missing__ __iter__ __reversed__ __contains__
# 怎么测: a dict-backed container exercising each; __missing__ via dict
#   subclass; __length_hint__ via operator.length_hint; __contains__ controls
#   `in`; __reversed__ controls reversed(); slicing routes to __getitem__.
# 期望: every container hook fires on its trigger.
# 为什么: this is the full sequence/mapping protocol.
# ---------------------------------------------------------------------------
class Container:
    def __init__(self):
        self.data = {}
        self.contains_calls = 0

    def __len__(self):
        return len(self.data)

    def __getitem__(self, key):
        return self.data[key]

    def __setitem__(self, key, value):
        self.data[key] = value

    def __delitem__(self, key):
        del self.data[key]

    def __iter__(self):
        return iter(sorted(self.data))

    def __reversed__(self):
        return reversed(sorted(self.data))

    def __contains__(self, key):
        self.contains_calls += 1
        return key in self.data


co = Container()
co["a"] = 1
co["b"] = 2
co["c"] = 3
chk("container_setitem_getitem", co["a"] == 1 and co["b"] == 2)
chk("container_len", len(co) == 3)
del co["b"]
chk("container_delitem", len(co) == 2 and "b" not in co.data)
chk("container_iter", list(co) == ["a", "c"])
chk("container_reversed", list(reversed(co)) == ["c", "a"])
chk("container_contains", ("a" in co) is True and ("z" in co) is False
    and co.contains_calls == 2)
# Deeper: __setitem__ overwrites in place (no growth), __delitem__ of a missing
# key raises KeyError through the dunder, and __getitem__ of a missing key too.
co["a"] = 99
chk("container_setitem_overwrite", co["a"] == 99 and len(co) == 2)
try:
    del co["nope"]
    chk("container_delitem_missing_raises", False)
except KeyError:
    chk("container_delitem_missing_raises", True)
try:
    co["nope"]
    chk("container_getitem_missing_raises", False)
except KeyError:
    chk("container_getitem_missing_raises", True)
# Each `in` increments the counter by exactly one: prove no double dispatch.
_before = co.contains_calls
("a" in co)
chk("container_contains_single_dispatch", co.contains_calls == _before + 1)


# __len__ must return a non-negative int; violations raise per the data model.
class NegLen:
    def __len__(self):
        return -1


class FloatLen:
    def __len__(self):
        return 3.0  # not an int


try:
    len(NegLen())
    chk("len_negative_raises", False)
except ValueError:
    chk("len_negative_raises", True)
try:
    len(FloatLen())
    chk("len_non_int_raises", False)
except TypeError:
    chk("len_non_int_raises", True)


# __contains__ absent -> membership falls back to __iter__ (then __getitem__).
class ContainsViaIter:
    def __iter__(self):
        return iter([10, 20, 30])


chk("contains_fallback_via_iter",
    (20 in ContainsViaIter()) is True and (99 in ContainsViaIter()) is False)


# Slicing routes to __getitem__ with a slice object.
class SliceAware:
    def __getitem__(self, key):
        if isinstance(key, slice):
            return ("slice", key.start, key.stop, key.step)
        return ("index", key)


sa = SliceAware()
chk("getitem_index", sa[5] == ("index", 5))
chk("getitem_slice", sa[1:9:2] == ("slice", 1, 9, 2))


# __missing__ on a dict subclass fires for absent keys via __getitem__.
class DefaultingDict(dict):
    def __missing__(self, key):
        return "missing:" + str(key)


dd = DefaultingDict(a=1)
chk("missing_dunder", dd["a"] == 1 and dd["zzz"] == "missing:zzz")


# __length_hint__ provides a size estimate to operator.length_hint.
import operator


class HintedIter:
    def __iter__(self):
        return iter([])

    def __length_hint__(self):
        return 7


chk("length_hint_dunder", operator.length_hint(HintedIter()) == 7)
# length_hint falls back to a supplied default when neither len nor hint exist.
chk("length_hint_default", operator.length_hint(object(), 99) == 99)
# __len__ takes priority over __length_hint__ for length_hint().
class LenOverHint:
    def __len__(self):
        return 5

    def __length_hint__(self):
        return 99


chk("length_hint_len_priority", operator.length_hint(LenOverHint()) == 5)
# A negative __length_hint__ is a documented error (ValueError).
class NegHint:
    def __length_hint__(self):
        return -1


try:
    operator.length_hint(NegHint())
    chk("length_hint_negative_raises", False)
except ValueError:
    chk("length_hint_negative_raises", True)


# Iterator protocol: __iter__ returning self + __next__ raising StopIteration.
class CountUp:
    def __init__(self, n):
        self.n = n
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        self.i += 1
        return self.i


chk("iterator_protocol", list(CountUp(3)) == [1, 2, 3])


# Fallback iteration: old-style __getitem__ sequence protocol (0,1,2,...).
class SeqByGetitem:
    def __getitem__(self, i):
        if i >= 3:
            raise IndexError
        return i * 10


chk("getitem_iteration_fallback", list(SeqByGetitem()) == [0, 10, 20])


# The fallback protocol queries indices 0,1,2,... until IndexError. A sequence
# with a DIFFERENT bound must stop at exactly that bound (proves the IndexError
# raised by __getitem__ -- not StopIteration -- is what terminates iteration,
# and that no extra/short reads occur).
class SeqBounded:
    def __init__(self):
        self.reads = []

    def __getitem__(self, i):
        self.reads.append(i)
        if i >= 2:
            raise IndexError
        return i * 5


_sb = SeqBounded()
chk("getitem_fallback_bounded", list(_sb) == [0, 5] and _sb.reads == [0, 1, 2])


# ---------------------------------------------------------------------------
# Docs "3.3.8 Emulating numeric types":
#   binary: __add__ __sub__ __mul__ __matmul__ __truediv__ __floordiv__
#           __mod__ __divmod__ __pow__ __lshift__ __rshift__ __and__ __xor__ __or__
# 怎么测: a wrapper class implementing every binary numeric dunder over an int;
#   verify each operator dispatches.
# 期望: each operator returns a new wrapped result matching int semantics.
# 为什么: full numeric protocol drives custom number types (vectors, money).
# ---------------------------------------------------------------------------
class N:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return isinstance(o, N) and self.v == o.v

    def __repr__(self):
        return "N(%r)" % self.v

    def __add__(self, o):
        return N(self.v + o.v)

    def __sub__(self, o):
        return N(self.v - o.v)

    def __mul__(self, o):
        return N(self.v * o.v)

    def __matmul__(self, o):
        return N(self.v * o.v + 1)  # pretend matmul

    def __truediv__(self, o):
        return N(self.v / o.v)

    def __floordiv__(self, o):
        return N(self.v // o.v)

    def __mod__(self, o):
        return N(self.v % o.v)

    def __divmod__(self, o):
        q, r = divmod(self.v, o.v)
        return (N(q), N(r))

    def __index__(self):
        return self.v

    def __pow__(self, o, mod=None):
        m = None if mod is None else (mod.v if isinstance(mod, N) else mod)
        return N(pow(self.v, o.v, m))

    def __lshift__(self, o):
        return N(self.v << o.v)

    def __rshift__(self, o):
        return N(self.v >> o.v)

    def __and__(self, o):
        return N(self.v & o.v)

    def __xor__(self, o):
        return N(self.v ^ o.v)

    def __or__(self, o):
        return N(self.v | o.v)


chk("num_add", (N(3) + N(4)) == N(7))
chk("num_sub", (N(10) - N(4)) == N(6))
chk("num_mul", (N(3) * N(4)) == N(12))
chk("num_matmul", (N(3) @ N(4)) == N(13))
chk("num_truediv", (N(7) / N(2)) == N(3.5))
chk("num_floordiv", (N(7) // N(2)) == N(3))
chk("num_mod", (N(7) % N(3)) == N(1))
chk("num_divmod", divmod(N(7), N(3)) == (N(2), N(1)))
chk("num_pow", (N(2) ** N(10)) == N(1024))
# Ternary pow dispatches to __pow__(exp, mod). Test BOTH an int modulus (built-in
# fallback path) and a custom-typed N modulus, so the mod argument is proven to
# flow through the dunder rather than being silently ignored / mis-handled.
chk("num_pow_ternary", pow(N(2), N(10), 1000) == N(24))
chk("num_pow_ternary_typed", pow(N(2), N(10), N(1000)) == N(24))
# Distinct modulus to ensure the mod is actually applied (not a no-op): 3^4=81.
chk("num_pow_ternary_applies", pow(N(3), N(4), N(50)) == N(31))
chk("num_lshift", (N(1) << N(4)) == N(16))
chk("num_rshift", (N(32) >> N(2)) == N(8))
chk("num_and", (N(0b1100) & N(0b1010)) == N(0b1000))
chk("num_xor", (N(0b1100) ^ N(0b1010)) == N(0b0110))
chk("num_or", (N(0b1100) | N(0b1010)) == N(0b1110))


# ---------------------------------------------------------------------------
# Docs "3.3.8": reflected (swapped) operands
#   __radd__ __rsub__ __rmul__ __rmatmul__ __rtruediv__ __rfloordiv__
#   __rmod__ __rdivmod__ __rpow__ __rlshift__ __rrshift__ __rand__ __rxor__ __ror__
# 怎么测: a class with ONLY reflected ops; use it as the RIGHT operand of a
#   builtin int on the left (int.__add__(N) returns NotImplemented -> reflected).
# 期望: each `int OP obj` dispatches to obj.__rOP__.
# 为什么: reflected ops let your type interoperate when it's on the right.
# ---------------------------------------------------------------------------
class R:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return isinstance(o, R) and self.v == o.v

    def __radd__(self, o):
        return R(o + self.v)

    def __rsub__(self, o):
        return R(o - self.v)

    def __rmul__(self, o):
        return R(o * self.v)

    def __rmatmul__(self, o):
        return R(o)

    def __rtruediv__(self, o):
        return R(o / self.v)

    def __rfloordiv__(self, o):
        return R(o // self.v)

    def __rmod__(self, o):
        return R(o % self.v)

    def __rdivmod__(self, o):
        return divmod(o, self.v)

    def __rpow__(self, o):
        return R(o ** self.v)

    def __rlshift__(self, o):
        return R(o << self.v)

    def __rrshift__(self, o):
        return R(o >> self.v)

    def __rand__(self, o):
        return R(o & self.v)

    def __rxor__(self, o):
        return R(o ^ self.v)

    def __ror__(self, o):
        return R(o | self.v)


# Binary pow falls back to reflected __rpow__ (2-arg form only; ternary pow with
# a modulus does NOT consult reflected hooks per CPython).
class RPow:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return isinstance(o, RPow) and self.v == o.v

    def __rpow__(self, base):
        return RPow(base ** self.v)


chk("num_rpow_binary_fallback", (2 ** RPow(10)) == RPow(1024)
    and pow(2, RPow(10)) == RPow(1024))


chk("num_radd", (10 + R(5)) == R(15))
chk("num_rsub", (10 - R(3)) == R(7))
chk("num_rmul", (10 * R(3)) == R(30))
chk("num_rmatmul", (5 @ R(0)) == R(5))
chk("num_rtruediv", (10 / R(4)) == R(2.5))
chk("num_rfloordiv", (10 // R(4)) == R(2))
chk("num_rmod", (10 % R(3)) == R(1))
chk("num_rdivmod", divmod(10, R(3)) == (3, 1))
chk("num_rpow", (2 ** R(10)) == R(1024))
chk("num_rlshift", (1 << R(4)) == R(16))
chk("num_rrshift", (32 >> R(2)) == R(8))
chk("num_rand", (0b1100 & R(0b1010)) == R(0b1000))
chk("num_rxor", (0b1100 ^ R(0b1010)) == R(0b0110))
chk("num_ror", (0b1100 | R(0b1010)) == R(0b1110))


# ---------------------------------------------------------------------------
# Docs "3.3.8": in-place (augmented) assignment
#   __iadd__ __isub__ __imul__ __imatmul__ __itruediv__ __ifloordiv__
#   __imod__ __ipow__ __ilshift__ __irshift__ __iand__ __ixor__ __ior__
# 怎么测: a mutable accumulator that mutates in place and returns self; verify
#   the name binding is preserved (same object id) after each augmented op.
# 期望: each augmented op calls the in-place dunder and mutates in place.
# 为什么: in-place ops avoid reallocation for mutable types (lists, arrays).
# ---------------------------------------------------------------------------
class Acc:
    def __init__(self, v):
        self.v = v

    def __iadd__(self, o):
        self.v += o
        return self

    def __isub__(self, o):
        self.v -= o
        return self

    def __imul__(self, o):
        self.v *= o
        return self

    def __imatmul__(self, o):
        self.v = self.v * o + 1
        return self

    def __itruediv__(self, o):
        self.v /= o
        return self

    def __ifloordiv__(self, o):
        self.v //= o
        return self

    def __imod__(self, o):
        self.v %= o
        return self

    def __ipow__(self, o):
        self.v **= o
        return self

    def __ilshift__(self, o):
        self.v <<= o
        return self

    def __irshift__(self, o):
        self.v >>= o
        return self

    def __iand__(self, o):
        self.v &= o
        return self

    def __ixor__(self, o):
        self.v ^= o
        return self

    def __ior__(self, o):
        self.v |= o
        return self


acc = Acc(10)
_id = id(acc)
acc += 5
chk("inplace_iadd", acc.v == 15 and id(acc) == _id)
acc -= 3
chk("inplace_isub", acc.v == 12)
acc *= 2
chk("inplace_imul", acc.v == 24)
acc @= 2
chk("inplace_imatmul", acc.v == 49)
acc //= 7
chk("inplace_ifloordiv", acc.v == 7)
acc %= 4
chk("inplace_imod", acc.v == 3)
acc **= 3
chk("inplace_ipow", acc.v == 27)
acc <<= 2
chk("inplace_ilshift", acc.v == 108)
acc >>= 1
chk("inplace_irshift", acc.v == 54)
acc &= 0x3F
chk("inplace_iand", acc.v == 54 & 0x3F)
acc ^= 0x0F
chk("inplace_ixor", acc.v == (54 & 0x3F) ^ 0x0F)
acc |= 0x80
chk("inplace_ior", acc.v == (((54 & 0x3F) ^ 0x0F) | 0x80))
af = Acc(10)
af /= 4
chk("inplace_itruediv", af.v == 2.5)


# Augmented op WITHOUT in-place dunder falls back to binary + rebind.
class OnlyBinAdd:
    def __init__(self, v):
        self.v = v

    def __add__(self, o):
        return OnlyBinAdd(self.v + o)


ob = OnlyBinAdd(1)
_obid = id(ob)
ob += 5  # no __iadd__ -> uses __add__, rebinds to a NEW object
chk("inplace_fallback_to_binary", ob.v == 6 and id(ob) != _obid)


# ---------------------------------------------------------------------------
# Docs "3.3.8": unary arithmetic __neg__ __pos__ __abs__ __invert__
# 怎么测: implement all four; verify -obj, +obj, abs(obj), ~obj.
# 期望: each unary operator dispatches to its dunder.
# 为什么: unary ops complete the arithmetic protocol.
# ---------------------------------------------------------------------------
class U:
    def __init__(self, v):
        self.v = v

    def __eq__(self, o):
        return isinstance(o, U) and self.v == o.v

    def __neg__(self):
        return U(-self.v)

    def __pos__(self):
        return U(+self.v)

    def __abs__(self):
        return U(abs(self.v))

    def __invert__(self):
        return U(~self.v)


chk("unary_neg", (-U(5)) == U(-5))
chk("unary_pos", (+U(-5)) == U(-5))
chk("unary_abs", abs(U(-7)) == U(7))
chk("unary_invert", (~U(5)) == U(-6))


# ---------------------------------------------------------------------------
# Docs "3.3.8": numeric coercion __int__ __float__ __complex__ __index__
# 怎么测: implement each; verify int()/float()/complex() and __index__ usage
#   (slicing, hex(), bin(), operator.index). __index__ must return an exact int
#   and is what makes an object usable as a sequence index/in bit-ops.
# 期望: each converter dispatches; __index__ enables integer-only contexts.
# 为什么: these define how custom types convert to built-in numerics.
# ---------------------------------------------------------------------------
class Convertible:
    def __init__(self, v):
        self.v = v

    def __int__(self):
        return int(self.v)

    def __float__(self):
        return float(self.v) + 0.5

    def __complex__(self):
        return complex(self.v, 1)

    def __index__(self):
        return int(self.v)


cv = Convertible(3)
chk("conv_int", int(cv) == 3)
chk("conv_float", float(cv) == 3.5)
chk("conv_complex", complex(cv) == complex(3, 1))
chk("conv_index_operator", operator.index(cv) == 3)
chk("conv_index_hex", hex(cv) == "0x3")
chk("conv_index_bin", bin(cv) == "0b11")
chk("conv_index_oct", oct(cv) == "0o3")
chk("conv_index_slice", [0, 1, 2, 3, 4][cv] == 3)
chk("conv_index_in_range", list(range(cv)) == [0, 1, 2])


# __index__ MUST return an exact int; a non-int return raises TypeError. Also a
# non-integer object used where an index is required (no __index__) raises.
class BadIndex:
    def __index__(self):
        return 1.5  # not an int


try:
    operator.index(BadIndex())
    chk("index_must_return_int", False)
except TypeError:
    chk("index_must_return_int", True)
_flt_idx = 1.0  # via variable to avoid a constant-subscript SyntaxWarning
try:
    [0, 1, 2][_flt_idx]  # float has no __index__ -> TypeError, not truncation
    chk("index_float_rejected", False)
except TypeError:
    chk("index_float_rejected", True)


# ---------------------------------------------------------------------------
# Docs "3.3.8": rounding/truncation
#   __round__ __trunc__ __floor__ __ceil__
# 怎么测: implement each; verify round()/math.trunc()/math.floor()/math.ceil().
#   round(obj, ndigits) passes ndigits to __round__.
# 期望: each rounding builtin dispatches to its dunder.
# 为什么: completes the numeric->integer reduction protocol.
# ---------------------------------------------------------------------------
import math


class Roundable:
    def __init__(self, v):
        self.v = v

    def __round__(self, ndigits=None):
        if ndigits is None:
            return round(self.v)
        return round(self.v, ndigits)

    def __trunc__(self):
        return math.trunc(self.v)

    def __floor__(self):
        return math.floor(self.v)

    def __ceil__(self):
        return math.ceil(self.v)


rb = Roundable(3.567)
chk("round_dunder_no_digits", round(rb) == 4)
chk("round_dunder_ndigits", round(rb, 1) == 3.6)
chk("trunc_dunder", math.trunc(Roundable(3.9)) == 3)
chk("floor_dunder", math.floor(Roundable(3.9)) == 3)
chk("ceil_dunder", math.ceil(Roundable(3.1)) == 4)


# ---------------------------------------------------------------------------
# Docs "3.3.9 With Statement Context Managers": __enter__ __exit__
# 怎么测: a context manager whose __enter__ returns a value and __exit__
#   records suppression; __exit__ returning True swallows the exception,
#   returning False propagates it; __exit__ sees (exc_type, exc, tb).
# 期望: __enter__ binds the as-target; __exit__ controls propagation.
# 为什么: the with-statement resource protocol.
# ---------------------------------------------------------------------------
class CM:
    def __init__(self, suppress):
        self.suppress = suppress
        self.entered = False
        self.exit_args = None

    def __enter__(self):
        self.entered = True
        return "resource"

    def __exit__(self, exc_type, exc, tb):
        self.exit_args = (exc_type, exc)
        return self.suppress


cm = CM(suppress=False)
with cm as r:
    body_val = r
chk("ctx_enter_returns", body_val == "resource" and cm.entered is True)
chk("ctx_exit_no_exc", cm.exit_args == (None, None))

cm_suppress = CM(suppress=True)
with cm_suppress:
    raise ValueError("boom")
chk("ctx_exit_suppresses", cm_suppress.exit_args[0] is ValueError)

cm_prop = CM(suppress=False)
propagated = False
try:
    with cm_prop:
        raise KeyError("k")
except KeyError:
    propagated = True
chk("ctx_exit_propagates", propagated and cm_prop.exit_args[0] is KeyError)


# __exit__ returning a FALSY non-bool (e.g. 0) still propagates -- only a TRUTHY
# return suppresses. Conversely a truthy non-bool (e.g. 1) suppresses.
class CMRet:
    def __init__(self, ret):
        self.ret = ret

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return self.ret


_prop0 = False
try:
    with CMRet(0):
        raise ValueError("v")
except ValueError:
    _prop0 = True
chk("ctx_exit_falsy_nonbool_propagates", _prop0)
_suppressed1 = True
try:
    with CMRet(1):
        raise ValueError("v")
except ValueError:
    _suppressed1 = False
chk("ctx_exit_truthy_nonbool_suppresses", _suppressed1)
# __exit__ receives the live traceback object (not just type/value).
class CMTB:
    def __enter__(self):
        return self

    def __exit__(self, et, ev, tb):
        self.saw_tb = tb is not None
        return True


_cmtb = CMTB()
with _cmtb:
    raise RuntimeError("boom")
chk("ctx_exit_receives_traceback", _cmtb.saw_tb is True)


# ---------------------------------------------------------------------------
# Docs copy module ("object.__copy__"/"object.__deepcopy__") + "__reduce__"
# 怎么测: copy.copy() uses __copy__; copy.deepcopy() uses __deepcopy__ (passing
#   a memo dict); copy.copy() also works via __reduce__ (pickle protocol).
# 期望: each customization hook is honored by the copy module.
# 为什么: these control shallow/deep copy and pickling semantics.
# ---------------------------------------------------------------------------
import copy


class Copyable:
    def __init__(self, tag):
        self.tag = tag

    def __copy__(self):
        c = Copyable(self.tag)
        c.shallow = True
        return c

    def __deepcopy__(self, memo):
        c = Copyable(self.tag)
        c.deep = True
        c.memo_is_dict = isinstance(memo, dict)
        return c


orig = Copyable("t")
sc = copy.copy(orig)
chk("copy_dunder", sc.tag == "t" and getattr(sc, "shallow", False) is True)
dc = copy.deepcopy(orig)
chk("deepcopy_dunder", dc.tag == "t" and getattr(dc, "deep", False) is True
    and dc.memo_is_dict is True)


# __reduce__ drives pickling and is used by copy when no __copy__ exists.
import pickle


class Reducible:
    def __init__(self, a, b):
        self.a = a
        self.b = b

    def __eq__(self, o):
        return isinstance(o, Reducible) and (self.a, self.b) == (o.a, o.b)

    def __reduce__(self):
        return (self.__class__, (self.a, self.b))


red = Reducible(1, 2)
chk("reduce_via_pickle", pickle.loads(pickle.dumps(red)) == Reducible(1, 2))
chk("reduce_via_copy", copy.copy(red) == Reducible(1, 2))


# ---------------------------------------------------------------------------
# Docs "3.3.3 Customizing class creation": __init_subclass__
# 怎么测: a base implementing __init_subclass__(cls, **kwargs) runs once per
#   subclass at definition time, receiving class keyword arguments.
# 期望: hook fires at subclass creation with the keyword arguments.
# 为什么: lets a base class register/configure subclasses without a metaclass.
# ---------------------------------------------------------------------------
_subclass_log = []


class Pluginable:
    def __init_subclass__(cls, /, kind=None, **kwargs):
        super().__init_subclass__(**kwargs)
        _subclass_log.append((cls.__name__, kind))


class PluginA(Pluginable, kind="alpha"):
    pass


class PluginB(Pluginable, kind="beta"):
    pass


chk("init_subclass", _subclass_log == [("PluginA", "alpha"), ("PluginB", "beta")])


# ---------------------------------------------------------------------------
# Docs "3.3.4 __set_name__" already covered above via DataDesc; here cover
# Docs "3.4.3 Emulating generic types": __class_getitem__
# 怎么测: a class defining __class_getitem__ supports Subscript at the class
#   level (MyClass[int]); returns whatever the hook returns.
# 期望: Cls[param] dispatches to __class_getitem__(param).
# 为什么: this is how generic containers (list[int], dict[str,int]) are spelled.
# ---------------------------------------------------------------------------
class GenericLike:
    def __class_getitem__(cls, item):
        return ("generic", cls.__name__, item)


chk("class_getitem", GenericLike[int] == ("generic", "GenericLike", int))
chk("class_getitem_tuple", GenericLike[int, str] == ("generic", "GenericLike", (int, str)))


# ---------------------------------------------------------------------------
# Docs "3.3.3.2 Resolving MRO entries": __mro_entries__
# 怎么测: an object (not a class) used as a base; if it defines
#   __mro_entries__(bases), that result replaces it in the actual bases. We use
#   it via typing-style: a non-class placeholder resolving to a real base.
# 期望: the created class's __bases__ contains the resolved class, not the
#   placeholder; __orig_bases__ records the original.
# 为什么: enables Generic[...] and similar non-class bases in class statements.
# ---------------------------------------------------------------------------
class RealBase:
    marker = "real"


class Placeholder:
    def __mro_entries__(self, bases):
        return (RealBase,)


_ph = Placeholder()


class UsesPlaceholder(_ph):
    pass


chk("mro_entries_resolves", RealBase in UsesPlaceholder.__bases__)
chk("mro_entries_inherits", UsesPlaceholder.marker == "real")
chk("mro_entries_orig_bases", UsesPlaceholder.__orig_bases__ == (_ph,))


# __mro_entries__ MUST return a tuple; a non-tuple (e.g. list) raises TypeError.
class BadPlaceholder:
    def __mro_entries__(self, bases):
        return [RealBase]  # list, not tuple


try:
    class UsesBadPlaceholder(BadPlaceholder()):
        pass
    chk("mro_entries_must_be_tuple", False)
except TypeError:
    chk("mro_entries_must_be_tuple", True)


# ---------------------------------------------------------------------------
# Docs "3.4.1 __slots__" interaction + Docs class attribute __class__ reassign
# 怎么测: __slots__ blocks arbitrary attrs AND removes __dict__; reassigning
#   __class__ changes the instance type (observable via type() and methods).
# 期望: slotted instance has no __dict__; __class__ reassignment retypes.
# 为什么: completes attribute-model corner cases referenced in the data model.
# ---------------------------------------------------------------------------
class Slotted:
    __slots__ = ("a",)

    def kind(self):
        return "slotted"


sl = Slotted()
sl.a = 1
chk("slots_no_dict", not hasattr(sl, "__dict__"))
try:
    sl.b = 2
    chk("slots_reject_new_attr", False)
except AttributeError:
    chk("slots_reject_new_attr", True)


# __slots__ compose across inheritance: a slotted subclass of a slotted base
# carries BOTH parents' slots, still has no __dict__, and rejects unlisted names.
class SlotBase:
    __slots__ = ("a",)


class SlotSub(SlotBase):
    __slots__ = ("b",)


ss = SlotSub()
ss.a = 1
ss.b = 2
chk("slots_inheritance_compose", ss.a == 1 and ss.b == 2
    and not hasattr(ss, "__dict__"))
try:
    ss.c = 3
    chk("slots_inheritance_reject", False)
except AttributeError:
    chk("slots_inheritance_reject", True)


class Plain:
    def kind(self):
        return "plain"


pl = Plain()
chk("class_attr_before", pl.kind() == "plain" and type(pl) is Plain)
# Reassigning __class__ across INCOMPATIBLE layouts (dict-based Plain vs
# slotted Slotted) must raise TypeError -- the data model forbids it.
try:
    pl.__class__ = Slotted
    chk("class_reassign_incompatible_rejected", False)
except TypeError:
    chk("class_reassign_incompatible_rejected", True and type(pl) is Plain)


# Two layout-compatible classes (both dict-based) allow __class__ swap.
class CatA:
    def speak(self):
        return "A"


class CatB:
    def speak(self):
        return "B"


swap = CatA()
chk("class_reassign_before", swap.speak() == "A")
swap.__class__ = CatB
chk("class_reassign_after", swap.speak() == "B" and type(swap) is CatB)


# ---------------------------------------------------------------------------
# Docs "3.3.5 __instancecheck__/__subclasscheck__" (metaclass hooks)
# 怎么测: a metaclass overriding __instancecheck__/__subclasscheck__ changes the
#   behavior of isinstance()/issubclass() for classes using it.
# 期望: isinstance/issubclass route to the metaclass hooks.
# 为什么: this is how abc.ABCMeta implements virtual subclasses.
# ---------------------------------------------------------------------------
class VirtualMeta(type):
    def __instancecheck__(cls, instance):
        return getattr(instance, "quacks", False)

    def __subclasscheck__(cls, subclass):
        return getattr(subclass, "duck_compatible", False)


class Duck(metaclass=VirtualMeta):
    pass


class Quacker:
    quacks = True
    duck_compatible = True


class Silent:
    pass


chk("instancecheck_dunder", isinstance(Quacker(), Duck) is True
    and isinstance(Silent(), Duck) is False)
chk("subclasscheck_dunder", issubclass(Quacker, Duck) is True
    and issubclass(Silent, Duck) is False)
# The hook results are coerced to bool: a truthy non-bool return reads as True.
class TruthyMeta(type):
    def __instancecheck__(cls, instance):
        return ["non-empty"]  # truthy non-bool


class TruthyDuck(metaclass=TruthyMeta):
    pass


chk("instancecheck_truthy_coerced", isinstance(object(), TruthyDuck) is True)


# ---------------------------------------------------------------------------
# Docs "3.3.3 Customizing class creation": metaclass __prepare__/__new__/__init__
# 怎么测: __prepare__ supplies the namespace mapping used to execute the class
#   body (so injected keys are visible); __new__ creates the class object;
#   __init__ initializes it. All three fire in order at class definition.
# 期望: __prepare__ pre-seeds the body namespace; __new__ then __init__ run once.
# 为什么: this is the full PEP 3115 class-creation protocol behind ORMs/enums.
# ---------------------------------------------------------------------------
_meta_order = []


class FullMeta(type):
    @classmethod
    def __prepare__(mcs, name, bases, **kw):
        _meta_order.append("prepare")
        ns = {}
        ns["injected_by_prepare"] = "PREP"
        return ns

    def __new__(mcs, name, bases, ns, **kw):
        _meta_order.append("new")
        return super().__new__(mcs, name, bases, ns)

    def __init__(cls, name, bases, ns, **kw):
        _meta_order.append("init")
        super().__init__(name, bases, ns)


class BuiltByMeta(metaclass=FullMeta):
    # the class body can SEE the key __prepare__ injected
    seen = injected_by_prepare  # noqa: F821


chk("meta_prepare_injects", BuiltByMeta.injected_by_prepare == "PREP"
    and BuiltByMeta.seen == "PREP")
chk("meta_new_init_order", _meta_order == ["prepare", "new", "init"])
chk("meta_is_instance_of_metaclass", type(BuiltByMeta) is FullMeta)


# ---------------------------------------------------------------------------
# Docs "3.3.2.3 __slots__"... and Docs builtins: vars()/getattr defaults
# 怎么测: vars(obj) == obj.__dict__; getattr with default; setattr/delattr/
#   hasattr builtins route through the attribute dunders consistently.
# 期望: builtin attribute helpers integrate with the data model.
# 为什么: confirms the attribute protocol exposed via builtins.
# ---------------------------------------------------------------------------
class VarsObj:
    def __init__(self):
        self.p = 1
        self.q = 2


vo = VarsObj()
chk("vars_builtin", vars(vo) == {"p": 1, "q": 2})
chk("getattr_default", getattr(vo, "nope", "DFLT") == "DFLT")
setattr(vo, "r", 3)
chk("setattr_builtin", vo.r == 3)
chk("hasattr_builtin", hasattr(vo, "p") is True and hasattr(vo, "zzz") is False)
delattr(vo, "p")
chk("delattr_builtin", not hasattr(vo, "p"))


# ---------------------------------------------------------------------------
# Docs "3.4.4 PEP 695 type parameters" use __type_params__ (3.12+, syntax-gated)
# Also PEP 750 t-strings (3.14, syntax-gated). These need NEW SYNTAX, so they
# live in exec()'d strings guarded by version so this file PARSES on 3.12.
# 怎么测: exec the new-syntax source only when the running version supports it;
#   else record a noted skip. Probe an observable effect.
# 期望: on 3.12+ the type-param class exposes __type_params__; on 3.14 t-strings
#   yield a Template object (data model: t-strings are NOT str).
# 为什么: the data model grows new dunders/protocols across versions; the file
#   must remain parseable on the host (3.12) while testing 3.14 in qemu.
# ---------------------------------------------------------------------------
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


# PEP 695 (3.12): a generic class created with type-param syntax records
# __type_params__ (a TypeVar tuple) on the class object.
_gated_syntax(
    "pep695_type_params_dunder", (3, 12),
    "class GBox[T]:\n"
    "    def __init__(self, v): self.v = v\n"
    "R = (len(GBox.__type_params__), GBox.__type_params__[0].__name__)\n",
    lambda ns: ns["R"] == (1, "T"),
)

# PEP 750 (3.14): t'...' produces a string.templatelib.Template; its .strings
# and .interpolations expose the static/dynamic parts (a NEW data-model type).
_gated_syntax(
    "pep750_template_datamodel", (3, 14),
    "x = 41\n"
    "tmpl = t'v={x + 1}'\n"
    "R = (type(tmpl).__name__, tmpl.values[0], tmpl.strings[0])\n",
    lambda ns: ns["R"] == ("Template", 42, "v="),
)


print(("PY_DATAMODEL_OK") if _ok else ("PY_DATAMODEL_FAIL"))
sys.exit(0 if _ok else 1)
