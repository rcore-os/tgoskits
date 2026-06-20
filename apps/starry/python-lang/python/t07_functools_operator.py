#!/usr/bin/env python3
"""functools + operator stdlib modules — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

import functools
import operator

# ============================================================================
# functools.reduce(function, iterable[, initializer])
# docs: "Apply function of two arguments cumulatively to the items of iterable,
#        from left to right, so as to reduce the iterable to a single value."
# how/expected/why: fold a 2-arg callable across a sequence. Verify left-to-right
# order (subtraction is order sensitive), the optional initializer seeds the
# accumulator (and is the result for an empty iterable), and an empty iterable
# without an initializer raises TypeError.
# ============================================================================
chk("reduce_product", functools.reduce(lambda a, b: a * b, [1, 2, 3, 4, 5]) == 120)
chk("reduce_left_to_right", functools.reduce(operator.sub, [10, 1, 2, 3]) == 4)  # ((10-1)-2)-3
chk("reduce_initial", functools.reduce(operator.add, [1, 2, 3], 100) == 106)
chk("reduce_initial_empty", functools.reduce(operator.add, [], 42) == 42)
chk("reduce_single_no_init", functools.reduce(operator.add, [7]) == 7)
chk("reduce_concat_order", functools.reduce(operator.add, ["a", "b", "c"]) == "abc")
try:
    functools.reduce(operator.add, [])
    _re = False
except TypeError:
    _re = True
chk("reduce_empty_no_init_raises", _re)

# ============================================================================
# functools.partial(func, /, *args, **keywords)
# docs: "Return a new partial object which when called will behave like func
#        called with the positional arguments args and keyword arguments keywords."
# how/expected/why: partial pre-binds leading positional args and keyword
# defaults. Later positional args append after the bound ones; later keywords can
# override bound keywords. Inspect the .func/.args/.keywords attributes.
# ============================================================================
p_pow = functools.partial(pow, 2)
chk("partial_positional", p_pow(10) == 1024)               # 2 ** 10
chk("partial_append_args", functools.partial(pow, 2, 10)() == 1024)
p_int = functools.partial(int, base=2)
chk("partial_keyword", p_int("1010") == 10)
chk("partial_kw_override", functools.partial(int, base=2)("17", base=10) == 17)
chk("partial_attr_func", p_pow.func is pow)
chk("partial_attr_args", functools.partial(pow, 2, 3).args == (2, 3))
chk("partial_attr_keywords", functools.partial(int, base=8).keywords == {"base": 8})
def _three(a, b, c):
    return (a, b, c)
chk("partial_mix", functools.partial(_three, 1, c=9)(2) == (1, 2, 9))

# ============================================================================
# functools.partialmethod(func, /, *args, **keywords)
# docs: "Return a new partialmethod descriptor which behaves like partial except
#        that it is designed to be used as a method definition rather than being
#        directly callable."
# how/expected/why: declared in a class body, it binds args and receives `self`
# at call time. Verify it works wrapping a regular method and that bound keyword
# specializations differ per descriptor.
# ============================================================================
class _Cell:
    def __init__(self):
        self._alive = False
    def set_state(self, state):
        self._alive = bool(state)
        return self._alive
    set_alive = functools.partialmethod(set_state, True)
    set_dead = functools.partialmethod(set_state, False)
    @property
    def alive(self):
        return self._alive
_cell = _Cell()
_cell.set_alive()
chk("partialmethod_bind_true", _cell.alive is True)
_cell.set_dead()
chk("partialmethod_bind_false", _cell.alive is False)
chk("partialmethod_returns", _Cell().set_alive() is True)

# ============================================================================
# functools.lru_cache(maxsize=128, typed=False)
# docs: "Decorator to wrap a function with a memoizing callable that saves up to
#        the maxsize most recent calls." Adds .cache_info() and .cache_clear().
# how/expected/why: memoization must return identical results, populate hits on
# repeated args, evict LRU entries when maxsize is exceeded, distinguish typed
# args (1 vs 1.0) only when typed=True, and cache_clear() resets stats. Unhashable
# args raise TypeError. maxsize=None gives an unbounded cache.
# ============================================================================
_calls = {"n": 0}
@functools.lru_cache(maxsize=2)
def _sq(x):
    _calls["n"] += 1
    return x * x
chk("lru_compute", _sq(2) == 4 and _sq(3) == 9 and _calls["n"] == 2)
chk("lru_hit", _sq(2) == 4 and _calls["n"] == 2)            # served from cache
_ci = _sq.cache_info()
chk("lru_cache_info_hits", _ci.hits == 1)
chk("lru_cache_info_misses", _ci.misses == 2)
chk("lru_cache_info_maxsize", _ci.maxsize == 2)
chk("lru_cache_info_currsize", _ci.currsize == 2)
# cache_info() returns a named tuple with exactly these documented fields, and the
# fields are also positionally addressable (hits, misses, maxsize, currsize).
chk("lru_cache_info_namedtuple",
    _ci._fields == ("hits", "misses", "maxsize", "currsize")
    and tuple(_ci) == (1, 2, 2, 2))
_sq(4)                                                       # evicts LRU (3)
_before = _sq.cache_info().misses
_sq(3)                                                       # recomputed -> miss
chk("lru_eviction", _sq.cache_info().misses == _before + 1)
_sq.cache_clear()
chk("lru_cache_clear", _sq.cache_info().hits == 0 and _sq.cache_info().currsize == 0)

@functools.lru_cache(maxsize=None)
def _fib(n):
    return n if n < 2 else _fib(n - 1) + _fib(n - 2)
chk("lru_unbounded", _fib(30) == 832040)
chk("lru_unbounded_maxsize_none", _fib.cache_info().maxsize is None)

# lru_cache can be applied as a bare decorator (no parentheses); docs say this is
# equivalent to lru_cache(maxsize=128) (the default). It must still memoize.
_bare = {"n": 0}
@functools.lru_cache
def _trip(x):
    _bare["n"] += 1
    return x * 3
chk("lru_bare_decorator", _trip(4) == 12 and _trip(4) == 12 and _bare["n"] == 1)
chk("lru_bare_default_maxsize", _trip.cache_info().maxsize == 128)

@functools.lru_cache(typed=True)
def _idf(x):
    return type(x).__name__
chk("lru_typed_distinct", (_idf(1), _idf(1.0)) == ("int", "float"))
chk("lru_typed_two_misses", _idf.cache_info().misses == 2)

# typed=False: repeating the SAME argument value is a cache hit (not a new miss).
# (Note: whether int 1 and float 1.0 share a slot is a CPython _make_key
# implementation detail, so we only assert the documented same-value contract.)
@functools.lru_cache(typed=False)
def _idf2(x):
    return type(x).__name__
_idf2(1); _idf2(1)
chk("lru_untyped_same_value_hit", _idf2.cache_info().misses == 1 and _idf2.cache_info().hits == 1)

@functools.lru_cache(maxsize=4)
def _need_hash(x):
    return x
try:
    _need_hash([1, 2])
    _uh = False
except TypeError:
    _uh = True
chk("lru_unhashable_raises", _uh)

# ============================================================================
# functools.cache(user_function)  (3.9+)
# docs: "Simple lightweight unbounded function cache. Sometimes called memoize.
#        Returns the same as lru_cache(maxsize=None)."
# how/expected/why: unbounded memoization with cache_info()/cache_clear(); the
# wrapped fn should run exactly once per distinct argument.
# ============================================================================
if hasattr(functools, "cache"):
    _runs = {"n": 0}
    @functools.cache
    def _double(x):
        _runs["n"] += 1
        return x * 2
    chk("cache_value", _double(21) == 42 and _double(21) == 42)
    chk("cache_runs_once", _runs["n"] == 1)
    chk("cache_info_unbounded", _double.cache_info().maxsize is None)
    # Distinct args each run the body exactly once; repeated args never re-run.
    _double(10); _double(10); _double(21)
    chk("cache_distinct_runs", _runs["n"] == 2)
    _dci = _double.cache_info()
    chk("cache_info_hits_misses", _dci.hits == 3 and _dci.misses == 2 and _dci.currsize == 2)
    _double.cache_clear()
    chk("cache_clear", _double.cache_info().currsize == 0)
    chk("cache_clear_resets_stats", _double.cache_info().hits == 0
        and _double.cache_info().misses == 0)
else:
    chk("cache_value", True, "(skip: needs 3.9)")
    chk("cache_runs_once", True, "(skip: needs 3.9)")
    chk("cache_info_unbounded", True, "(skip: needs 3.9)")
    chk("cache_clear", True, "(skip: needs 3.9)")

# ============================================================================
# functools.cached_property(func)  (3.8+)
# docs: "Transform a method of a class into a property whose value is computed
#        once and then cached as an ordinary attribute for the life of the
#        instance." Requires a writable instance __dict__ (no __slots__).
# how/expected/why: the underlying method runs once; subsequent reads return the
# cached value from the instance dict. Deleting the attribute forces recompute.
# A separate instance computes independently.
# ============================================================================
class _Lazy:
    def __init__(self, base):
        self.base = base
        self.computed = 0
    @functools.cached_property
    def value(self):
        self.computed += 1
        return self.base * 10
_lz = _Lazy(5)
chk("cached_property_value", _lz.value == 50)
chk("cached_property_cached", _lz.value == 50 and _lz.computed == 1)
chk("cached_property_in_dict", _lz.__dict__["value"] == 50)
del _lz.value
chk("cached_property_recompute", _lz.value == 50 and _lz.computed == 2)
chk("cached_property_per_instance", _Lazy(7).value == 70)

# ============================================================================
# functools.wraps / functools.update_wrapper
# docs: update_wrapper copies __module__/__name__/__qualname__/__doc__/__dict__,
#        updates __dict__, and sets __wrapped__ to the wrapped function. wraps is
#        the decorator-factory convenience form.
# how/expected/why: a decorated function must retain the original metadata so
# introspection/tooling sees the wrapped fn. Verify __name__, __doc__,
# __wrapped__ identity, and that wrapper-added attrs survive.
# ============================================================================
def _deco(fn):
    @functools.wraps(fn)
    def _w(*a, **k):
        _w.calls += 1
        return fn(*a, **k)
    _w.calls = 0
    return _w
@_deco
def _greet(name):
    "say hi"
    return "hi " + name
chk("wraps_name", _greet.__name__ == "_greet")
chk("wraps_doc", _greet.__doc__ == "say hi")
chk("wraps_wrapped", _greet.__wrapped__.__name__ == "_greet")
# __qualname__ is in WRAPPER_ASSIGNMENTS so it must be copied EXACTLY from the
# wrapped fn, not merely share a suffix.
chk("wraps_qualname", _greet.__qualname__ == _greet.__wrapped__.__qualname__
    and _greet.__qualname__.endswith("_greet"))
chk("wraps_callable", _greet("x") == "hi x" and _greet.calls == 1)

# functools.wraps(wrapped, assigned=..., updated=...) — only the named attrs are
# copied; everything else stays the wrapper's. Exclude __name__ so it must keep
# the wrapper name, and keep __doc__ so it must be copied from the wrapped fn.
def _deco_sel(fn):
    @functools.wraps(fn, assigned=("__doc__",), updated=())
    def _ws(*a, **k):
        return fn(*a, **k)
    return _ws
@_deco_sel
def _wrapped_sel():
    "sel doc"
    return 1
chk("wraps_assigned_excludes_name", _wrapped_sel.__name__ == "_ws")
chk("wraps_assigned_copies_doc", _wrapped_sel.__doc__ == "sel doc")

def _raw(a, b):
    "raw doc"
    return a + b
def _man(*a, **k):
    return _raw(*a, **k)
functools.update_wrapper(_man, _raw)
chk("update_wrapper_name", _man.__name__ == "_raw")
chk("update_wrapper_doc", _man.__doc__ == "raw doc")
chk("update_wrapper_wrapped", _man.__wrapped__ is _raw)

# update_wrapper with custom assigned/updated: only __doc__ is copied, __name__
# is left as the wrapper's. (__wrapped__ is always set regardless of assigned.)
def _raw2(a, b):
    "raw2 doc"
    return a - b
def _man2(*a, **k):
    return _raw2(*a, **k)
functools.update_wrapper(_man2, _raw2, assigned=("__doc__",), updated=())
chk("update_wrapper_assigned_keeps_name", _man2.__name__ == "_man2")
chk("update_wrapper_assigned_copies_doc", _man2.__doc__ == "raw2 doc")
chk("update_wrapper_assigned_wrapped_always", _man2.__wrapped__ is _raw2)

# ============================================================================
# functools.total_ordering
# docs: "Given a class defining one or more rich comparison ordering methods,
#        this class decorator supplies the rest." Requires __eq__ plus one of
#        __lt__/__le__/__gt__/__ge__.
# how/expected/why: define only __eq__ and __lt__, then verify the decorator
# synthesizes le/gt/ge consistently, including for equal and reversed operands.
# ============================================================================
@functools.total_ordering
class _Ver:
    def __init__(self, n):
        self.n = n
    def __eq__(self, o):
        return self.n == o.n
    def __lt__(self, o):
        return self.n < o.n
chk("total_ordering_lt", _Ver(1) < _Ver(2))
chk("total_ordering_le", _Ver(1) <= _Ver(1) and _Ver(1) <= _Ver(2))
chk("total_ordering_gt", _Ver(3) > _Ver(2))
chk("total_ordering_ge", _Ver(2) >= _Ver(2) and _Ver(3) >= _Ver(2))
chk("total_ordering_ne", _Ver(1) != _Ver(2))
chk("total_ordering_sort", [v.n for v in sorted([_Ver(3), _Ver(1), _Ver(2)])] == [1, 2, 3])

# ============================================================================
# functools.singledispatch + .register + .dispatch + .registry
# docs: "Transform a function into a single-dispatch generic function." Dispatch
#        is on the type of the first argument; register adds type-specific impls;
#        registry is a read-only mapping; dispatch() resolves a type to its impl.
# how/expected/why: cover the default impl, register via decorator with explicit
# type, register via type annotation, register an ABC (numbers.Integral),
# stacked registration, and the .dispatch()/.registry introspection API.
# ============================================================================
@functools.singledispatch
def _fmt(arg):
    return "default:" + str(arg)
@_fmt.register(int)
def _(arg):
    return "int:" + str(arg)
@_fmt.register
def _(arg: str):                       # registration via annotation
    return "str:" + arg
@_fmt.register(list)
@_fmt.register(tuple)                   # stacked: one impl for two types
def _(arg):
    return "seq:" + str(len(arg))
chk("singledispatch_default", _fmt(2.5) == "default:2.5")
chk("singledispatch_int", _fmt(7) == "int:7")
chk("singledispatch_str_annot", _fmt("hi") == "str:hi")
chk("singledispatch_stacked_list", _fmt([1, 2, 3]) == "seq:3")
chk("singledispatch_stacked_tuple", _fmt((1, 2)) == "seq:2")
chk("singledispatch_dispatch", _fmt.dispatch(int) is _fmt.registry[int])
chk("singledispatch_registry_default", object in _fmt.registry)
chk("singledispatch_dispatch_fallback", _fmt.dispatch(float) is _fmt.registry[object])
import numbers
@functools.singledispatch
def _kind(arg):
    return "other"
@_kind.register(numbers.Integral)
def _(arg):
    return "integral"
chk("singledispatch_abc", _kind(10) == "integral" and _kind(1.5) == "other")
chk("singledispatch_subclass", _kind(True) == "integral")   # bool is Integral

# ============================================================================
# functools.singledispatchmethod  (3.8+)
# docs: "Transform a method into a single-dispatch generic function." Dispatch is
#        on the type of the first non-self argument; composes with classmethod.
# how/expected/why: register int/str overloads on an instance method and verify
# the right branch fires per argument type, including the fallback.
# ============================================================================
if hasattr(functools, "singledispatchmethod"):
    class _Neg:
        @functools.singledispatchmethod
        def neg(self, arg):
            return "?"
        @neg.register
        def _(self, arg: int):
            return -arg
        @neg.register
        def _(self, arg: str):
            return arg[::-1]
    _n = _Neg()
    chk("singledispatchmethod_int", _n.neg(5) == -5)
    chk("singledispatchmethod_str", _n.neg("abc") == "cba")
    chk("singledispatchmethod_default", _n.neg(1.5) == "?")
else:
    chk("singledispatchmethod_int", True, "(skip: needs 3.8)")
    chk("singledispatchmethod_str", True, "(skip: needs 3.8)")
    chk("singledispatchmethod_default", True, "(skip: needs 3.8)")

# ============================================================================
# functools.cmp_to_key(func)
# docs: "Transform an old-style comparison function to a key function." The cmp
#        returns negative/zero/positive; the key object is usable with sorted,
#        min, max, etc.
# how/expected/why: build a reverse-numeric comparator and confirm sorted() and
# the resulting key object's rich comparisons behave like the cmp result.
# ============================================================================
def _cmp(a, b):
    return (a > b) - (a < b)            # ascending
chk("cmp_to_key_sort_asc",
    sorted([3, 1, 2], key=functools.cmp_to_key(_cmp)) == [1, 2, 3])
def _rcmp(a, b):
    return (b > a) - (b < a)            # descending
chk("cmp_to_key_sort_desc",
    sorted([3, 1, 2], key=functools.cmp_to_key(_rcmp)) == [3, 2, 1])
_K = functools.cmp_to_key(_cmp)
chk("cmp_to_key_obj_lt", _K(1) < _K(2) and not (_K(2) < _K(1)))
# The key object must support ALL six rich comparisons consistently with the cmp.
chk("cmp_to_key_obj_eq",
    (_K(2) == _K(2)) is True and (_K(1) == _K(2)) is False
    and not (_K(2) < _K(2)) and not (_K(2) > _K(2)))
chk("cmp_to_key_obj_le", _K(1) <= _K(2) and _K(2) <= _K(2) and not (_K(3) <= _K(2)))
chk("cmp_to_key_obj_ge", _K(3) >= _K(2) and _K(2) >= _K(2) and not (_K(1) >= _K(2)))
chk("cmp_to_key_obj_gt", _K(2) > _K(1) and not (_K(1) > _K(2)))
chk("cmp_to_key_obj_ne", (_K(1) != _K(2)) is True and (_K(2) != _K(2)) is False)
chk("cmp_to_key_min_max",
    min([5, 2, 9], key=functools.cmp_to_key(_cmp)) == 2
    and max([5, 2, 9], key=functools.cmp_to_key(_cmp)) == 9)

# ============================================================================
# functools.reduce edge: works with generators / strings already covered above.
# functools.WRAPPER_ASSIGNMENTS / WRAPPER_UPDATES module constants exist.
# docs: module-level tuples used by update_wrapper.
# how/expected/why: ensure these documented attributes are present and shaped.
# ============================================================================
chk("WRAPPER_ASSIGNMENTS", "__name__" in functools.WRAPPER_ASSIGNMENTS
    and "__doc__" in functools.WRAPPER_ASSIGNMENTS)
chk("WRAPPER_UPDATES", "__dict__" in functools.WRAPPER_UPDATES)

# ============================================================================
# operator: arithmetic functions
# docs: operator.add/sub/mul/truediv/floordiv/mod/pow/neg/pos/abs/inv/index/matmul
#        "Return a + b", etc. — function equivalents of the syntactic operators.
# how/expected/why: each must equal the corresponding expression. matmul (@) is
# tested via a class implementing __matmul__; index() calls __index__.
# ============================================================================
chk("op_add", operator.add(3, 4) == 7)
chk("op_sub", operator.sub(10, 3) == 7)
chk("op_mul", operator.mul(6, 7) == 42)
chk("op_truediv", operator.truediv(7, 2) == 3.5)
chk("op_floordiv", operator.floordiv(7, 2) == 3)
chk("op_mod", operator.mod(7, 3) == 1)
chk("op_pow", operator.pow(2, 10) == 1024)
chk("op_neg", operator.neg(5) == -5)
chk("op_pos", operator.pos(-5) == -5)             # __pos__ of int is identity
chk("op_abs", operator.abs(-9) == 9)
chk("op_inv", operator.inv(0) == -1 and operator.invert(5) == -6)   # ~x == -x-1
chk("op_index", operator.index(True) == 1)
class _Idx:
    def __index__(self):
        return 7
chk("op_index_dunder", operator.index(_Idx()) == 7)
class _Mat:
    def __init__(self, v):
        self.v = v
    def __matmul__(self, o):
        return self.v * o.v
chk("op_matmul", operator.matmul(_Mat(3), _Mat(4)) == 12)

# ============================================================================
# operator: bitwise functions
# docs: operator.and_/or_/xor/lshift/rshift — "Return a & b", etc.
# how/expected/why: match the bit expressions exactly.
# ============================================================================
chk("op_and", operator.and_(0b1100, 0b1010) == 0b1000)
chk("op_or", operator.or_(0b1100, 0b1010) == 0b1110)
chk("op_xor", operator.xor(0b1100, 0b1010) == 0b0110)
chk("op_lshift", operator.lshift(1, 4) == 16)
chk("op_rshift", operator.rshift(64, 2) == 16)

# ============================================================================
# operator: comparison functions
# docs: operator.lt/le/eq/ne/ge/gt — "Perform 'rich comparisons'."
# how/expected/why: exercise true and false sides of each ordering.
# ============================================================================
chk("op_lt", operator.lt(1, 2) and not operator.lt(2, 2))
chk("op_le", operator.le(2, 2) and not operator.le(3, 2))
chk("op_eq", operator.eq(5, 5) and not operator.eq(5, 6))
chk("op_ne", operator.ne(5, 6) and not operator.ne(5, 5))
chk("op_ge", operator.ge(3, 2) and operator.ge(2, 2) and not operator.ge(1, 2))
chk("op_gt", operator.gt(3, 2) and not operator.gt(2, 2))

# ============================================================================
# operator: logical / object-identity functions
# docs: operator.not_(obj) "Return not obj"; truth(obj) "True if obj is true";
#        is_(a,b)/is_not(a,b) "Return a is b / a is not b".
# how/expected/why: not_/truth invert/coerce truthiness; is_/is_not test object
# identity (interned small ints / singletons / fresh objects).
# ============================================================================
chk("op_not", operator.not_(0) is True and operator.not_(1) is False)
chk("op_truth", operator.truth([1]) is True and operator.truth([]) is False)
chk("op_truth_zero", operator.truth(0) is False and operator.truth("x") is True)
_xobj = object()
chk("op_is", operator.is_(_xobj, _xobj) and operator.is_(None, None))
chk("op_is_not", operator.is_not(object(), object()))
chk("op_is_singleton", operator.is_(True, True) and not operator.is_(object(), object()))

# ============================================================================
# operator: sequence / container functions
# docs: concat(a,b) "a + b for sequences"; contains(a,b) "b in a";
#        countOf(a,b) "number of occurrences of b in a"; indexOf(a,b)
#        "index of first occurrence of b in a"; getitem/setitem/delitem;
#        length_hint(obj, default=0).
# how/expected/why: cover lists/strings/dicts; verify error paths (indexOf
# missing raises ValueError, getitem missing key raises KeyError). length_hint
# returns len() for sized objects and honours __length_hint__.
# ============================================================================
chk("op_concat", operator.concat([1, 2], [3, 4]) == [1, 2, 3, 4])
chk("op_concat_str", operator.concat("ab", "cd") == "abcd")
chk("op_contains", operator.contains([1, 2, 3], 2) and not operator.contains([1, 2, 3], 9))
chk("op_contains_dict", operator.contains({"k": 1}, "k"))
chk("op_countOf", operator.countOf([1, 2, 2, 3, 2], 2) == 3)
chk("op_indexOf", operator.indexOf([10, 20, 30], 20) == 1)
try:
    operator.indexOf([1, 2, 3], 99)
    _io = False
except ValueError:
    _io = True
chk("op_indexOf_missing_raises", _io)
_lst = [0, 1, 2]
operator.setitem(_lst, 1, 99)
chk("op_setitem", _lst == [0, 99, 2])
chk("op_getitem", operator.getitem(_lst, 2) == 2)
chk("op_getitem_slice", operator.getitem([0, 1, 2, 3], slice(1, 3)) == [1, 2])
operator.delitem(_lst, 0)
chk("op_delitem", _lst == [99, 2])
_d = {"a": 1}
operator.setitem(_d, "b", 2)
chk("op_setitem_dict", _d == {"a": 1, "b": 2})
operator.delitem(_d, "a")
chk("op_delitem_dict", _d == {"b": 2})
try:
    operator.delitem(_d, "missing")
    _dk = False
except KeyError:
    _dk = True
chk("op_delitem_missing_raises", _dk)
try:
    operator.getitem({"a": 1}, "z")
    _gk = False
except KeyError:
    _gk = True
chk("op_getitem_missing_raises", _gk)
chk("op_length_hint", operator.length_hint([1, 2, 3]) == 3)
# A list_iterator DOES provide __length_hint__, so its precise hint (0 for an
# emptied iterator) is returned and the default is IGNORED — assert exactly 0.
chk("op_length_hint_sized_iter", operator.length_hint(iter([]), 5) == 0)
# A bare generator has NO __length_hint__ and is not Sized, so the default value
# must be returned VERBATIM (catches a default-arg-ignored bug).
def _gen_no_hint():
    yield from ()
chk("op_length_hint_default", operator.length_hint(_gen_no_hint(), 5) == 5)
chk("op_length_hint_default_zero", operator.length_hint(_gen_no_hint()) == 0)
class _LH:
    def __length_hint__(self):
        return 42
chk("op_length_hint_dunder", operator.length_hint(_LH()) == 42)

# ============================================================================
# operator.attrgetter(*attrs)
# docs: "Return a callable object that fetches attr from its operand. If more
#        than one attribute is requested, returns a tuple. Dotted names traverse."
# how/expected/why: single attr returns the value; multiple returns a tuple in
# order; dotted name follows nested attribute chains; usable as a sort key.
# ============================================================================
class _Rec:
    def __init__(self, a, b):
        self.a = a
        self.b = b
class _Nest:
    def __init__(self, inner):
        self.inner = inner
_r = _Rec(1, 2)
chk("attrgetter_single", operator.attrgetter("a")(_r) == 1)
chk("attrgetter_multi", operator.attrgetter("a", "b")(_r) == (1, 2))
_nest = _Nest(_Rec(7, 8))
chk("attrgetter_dotted", operator.attrgetter("inner.a")(_nest) == 7)
chk("attrgetter_dotted_multi",
    operator.attrgetter("inner.a", "inner.b")(_nest) == (7, 8))
_recs = [_Rec(3, "z"), _Rec(1, "y"), _Rec(2, "x")]
chk("attrgetter_sort_key",
    [r.a for r in sorted(_recs, key=operator.attrgetter("a"))] == [1, 2, 3])
# Missing attribute (including a missing leaf in a dotted chain) raises AttributeError.
try:
    operator.attrgetter("nope")(_r)
    _ag = False
except AttributeError:
    _ag = True
chk("attrgetter_missing_raises", _ag)
try:
    operator.attrgetter("inner.missing")(_nest)
    _agd = False
except AttributeError:
    _agd = True
chk("attrgetter_dotted_missing_raises", _agd)

# ============================================================================
# operator.itemgetter(*items)
# docs: "Return a callable object that fetches item from its operand using
#        operand.__getitem__(). Multiple items -> tuple."
# how/expected/why: single index/key returns the value; multiple returns a tuple;
# works on lists, tuples, dicts, strings; usable as a multi-key sort key.
# ============================================================================
chk("itemgetter_index", operator.itemgetter(1)([10, 20, 30]) == 20)
chk("itemgetter_multi", operator.itemgetter(0, 2)([10, 20, 30]) == (10, 30))
chk("itemgetter_key", operator.itemgetter("name")({"name": "x", "v": 1}) == "x")
chk("itemgetter_multi_key",
    operator.itemgetter("a", "b")({"a": 1, "b": 2, "c": 3}) == (1, 2))
chk("itemgetter_slice", operator.itemgetter(slice(1, 3))([0, 1, 2, 3]) == [1, 2])
chk("itemgetter_str", operator.itemgetter(0)("abc") == "a")
_rows = [("c", 3), ("a", 1), ("b", 2)]
chk("itemgetter_sort_key",
    sorted(_rows, key=operator.itemgetter(0)) == [("a", 1), ("b", 2), ("c", 3)])
chk("itemgetter_sort_secondary",
    sorted([("x", 2), ("x", 1)], key=operator.itemgetter(1, 0))
    == [("x", 1), ("x", 2)])

# ============================================================================
# operator.methodcaller(name, /, *args, **kwargs)
# docs: "Return a callable object that calls the method name on its operand. If
#        additional arguments and/or keyword arguments are given, they will be
#        passed to the method as well."
# how/expected/why: call no-arg, positional-arg, and keyword-arg methods on str
# and on a custom object; reuse the caller across operands.
# ============================================================================
chk("methodcaller_noargs", operator.methodcaller("upper")("abc") == "ABC")
chk("methodcaller_args", operator.methodcaller("replace", "a", "X")("banana") == "bXnXnX")
chk("methodcaller_kwargs",
    operator.methodcaller("split", sep=",", maxsplit=1)("a,b,c") == ["a", "b,c"])
class _Adder:
    def __init__(self, base):
        self.base = base
    def plus(self, x, y=0):
        return self.base + x + y
_call = operator.methodcaller("plus", 10, y=5)
chk("methodcaller_custom", _call(_Adder(100)) == 115)
chk("methodcaller_reuse",
    [operator.methodcaller("upper")(s) for s in ["a", "b"]] == ["A", "B"])

# ============================================================================
# operator: in-place ("i") functions
# docs: iadd/isub/imul/itruediv/ifloordiv/imod/ipow/iand/ior/ixor/ilshift/
#        irshift/iconcat — "a = iadd(a, b) is equivalent to a += b". For immutable
#        operands they behave like the non-inplace form (return new object); for
#        mutable ones (list) they mutate and return the same object.
# how/expected/why: confirm iconcat mutates a list in place (identity preserved),
# while iadd on ints returns the arithmetic result; cover the remaining i-ops.
# ============================================================================
_L = [1, 2]
_L2 = operator.iconcat(_L, [3, 4])
chk("op_iconcat_mutates", _L == [1, 2, 3, 4] and _L2 is _L)
chk("op_iadd_int", operator.iadd(3, 4) == 7)
chk("op_isub", operator.isub(10, 3) == 7)
chk("op_imul", operator.imul(6, 7) == 42)
chk("op_imul_list", operator.imul([1, 2], 3) == [1, 2, 1, 2, 1, 2])
chk("op_itruediv", operator.itruediv(7, 2) == 3.5)
chk("op_ifloordiv", operator.ifloordiv(7, 2) == 3)
chk("op_imod", operator.imod(7, 3) == 1)
chk("op_ipow", operator.ipow(2, 8) == 256)
chk("op_iand", operator.iand(0b110, 0b011) == 0b010)
chk("op_ior", operator.ior(0b100, 0b001) == 0b101)
chk("op_ixor", operator.ixor(0b110, 0b011) == 0b101)
chk("op_ilshift", operator.ilshift(1, 5) == 32)
chk("op_irshift", operator.irshift(128, 3) == 16)

# ============================================================================
# operator: dunder-name aliases
# docs: "The operator module also defines tools for generalized attribute and
#        item lookups. ... For example, operator.__add__ is the same as
#        operator.add." Verify a few alias identities.
# how/expected/why: ensure the __dunder__ spellings resolve to the same callables.
# ============================================================================
chk("op_dunder_add_alias", operator.__add__ is operator.add)
chk("op_dunder_getitem_alias", operator.__getitem__ is operator.getitem)
chk("op_dunder_contains_alias", operator.__contains__ is operator.contains)
# More dunder aliases across the arithmetic/bitwise/comparison families: each
# __spelling__ must resolve to the SAME callable as its short name.
chk("op_dunder_neg_alias", operator.__neg__ is operator.neg)
chk("op_dunder_mul_alias", operator.__mul__ is operator.mul)
chk("op_dunder_sub_alias", operator.__sub__ is operator.sub)
chk("op_dunder_and_alias", operator.__and__ is operator.and_)
chk("op_dunder_lt_alias", operator.__lt__ is operator.lt)
chk("op_dunder_setitem_alias", operator.__setitem__ is operator.setitem)
chk("op_dunder_delitem_alias", operator.__delitem__ is operator.delitem)
chk("op_dunder_iadd_alias", operator.__iadd__ is operator.iadd)

# ============================================================================
# operator.call(obj, /, *args, **kwargs)  (3.11+)
# docs: "Return obj(*args, **kwargs)."  Equivalent to a plain call expression.
# how/expected/why: the function-call operator surfaced as a callable, with both
# positional and keyword forwarding.
# ============================================================================
if hasattr(operator, "call"):
    chk("op_call_simple", operator.call(lambda x: x * 2, 5) == 10)
    chk("op_call_args_kwargs",
        operator.call(lambda a, b, c=0: (a, b, c), 1, 2, c=3) == (1, 2, 3))
    chk("op_call_dunder_alias", operator.__call__ is operator.call)
else:
    chk("op_call_simple", True, "(skip: needs 3.11)")
    chk("op_call_args_kwargs", True, "(skip: needs 3.11)")
    chk("op_call_dunder_alias", True, "(skip: needs 3.11)")

# ============================================================================
# operator functions honour reflected (non-commutative) dunder fallbacks: when
# the left operand's __add__ returns NotImplemented (or has none), Python tries
# the right operand's __radd__. operator.add must follow the same protocol as the
# `+` syntax — it is not a fixed left-only dispatch.
# ============================================================================
class _Reflected:
    def __radd__(self, other):
        return ("radd", other)
    def __rsub__(self, other):
        return ("rsub", other)
chk("op_add_reflected", operator.add(5, _Reflected()) == ("radd", 5))
chk("op_sub_reflected", operator.sub(9, _Reflected()) == ("rsub", 9))
# Left operand's own dunder wins when it does NOT return NotImplemented.
class _LeftWins:
    def __add__(self, other):
        return "left"
chk("op_add_left_priority", operator.add(_LeftWins(), 1) == "left")

print(("PY_FUNCTIONAL_OK") if _ok else ("PY_FUNCTIONAL_FAIL"))
sys.exit(0 if _ok else 1)
