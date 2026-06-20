#!/usr/bin/env python3
"""Generators, the iterator protocol, and itertools — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

import itertools
import operator

# ===========================================================================
# SECTION 1 — Generator functions: yield, yield-expression value
# ===========================================================================
# Ref: Language Reference "6.2.9 Yield expressions" / "8.8 The yield statement".
# A generator function returns a generator iterator. Each `yield` produces a
# value to the consumer; resuming via next() makes the yield-expression evaluate
# to None. How: drive a plain generator with next(); expect the yielded sequence.
def simple_gen():
    yield 1
    yield 2
    yield 3

g = simple_gen()
chk("gen_is_generator", type(g).__name__ == "generator")
chk("gen_iter_self", iter(g) is g, "iter(gen) is gen")
chk("gen_next_seq", (next(g), next(g), next(g)) == (1, 2, 3))
try:
    next(g)
    _exhausted = False
except StopIteration:
    _exhausted = True
chk("gen_stopiteration", _exhausted, "StopIteration after exhaustion")
chk("gen_list", list(simple_gen()) == [1, 2, 3])
chk("gen_sum", sum(simple_gen()) == 6)

# yield as expression: the value of `yield X` is whatever send() passes in
# (None for next()). Ref: "6.2.9.1 Generator-iterator methods" (send).
def echo_gen():
    received = []
    while True:
        x = yield received
        received.append(x)

eg = echo_gen()
next(eg)                       # prime: advance to first yield, value of yield pending
chk("gen_yield_expr_send", eg.send("a") == ["a"])
chk("gen_yield_expr_send2", eg.send("b") == ["a", "b"])

# Generator introspection attributes (Ref: "4.5 Iterator Types" / datamodel).
# gi_code: the code object; gi_running: True only while executing; gi_frame:
# the current frame (None once the generator finishes); gi_yieldfrom: the
# sub-iterator while delegating via `yield from`, else None.
def _introspect():
    yield 1
    yield 2

ig = _introspect()
chk("gi_code_attr", ig.gi_code.co_name == "_introspect", "gi_code is the code object")
chk("gi_running_false_paused", ig.gi_running is False, "not running while paused")
chk("gi_frame_present", ig.gi_frame is not None, "frame exists before start")
next(ig)
chk("gi_frame_after_next", ig.gi_frame is not None, "frame persists while suspended")
chk("gi_yieldfrom_none", ig.gi_yieldfrom is None, "no delegation -> None")
# gi_running is observably True only from inside the running generator body.
_run_flag = []
def _running_probe():
    _run_flag.append(rp.gi_running)
    yield 0
rp = _running_probe()
next(rp)
chk("gi_running_true_inside", _run_flag == [True], "running flag set during execution")
# After exhaustion the frame is released (None).
for _ in ig:
    pass
chk("gi_frame_exhausted_none", ig.gi_frame is None, "frame is None once finished")
chk("gi_running_false_done", ig.gi_running is False)

# gi_yieldfrom reflects the active delegate during `yield from`.
def _delegating():
    yield from iter(["p", "q"])

dgi = _delegating()
next(dgi)
chk("gi_yieldfrom_active", type(dgi.gi_yieldfrom).__name__ == "list_iterator",
    "delegate exposed while yielding from")

# ===========================================================================
# SECTION 2 — generator.send()
# ===========================================================================
# Ref: generator.send(value). Resumes execution; `value` becomes the result of
# the current yield expression. send(None) is equivalent to next(). Sending a
# non-None value to a just-started (not-yet-primed) generator raises TypeError.
def adder():
    total = 0
    while True:
        n = yield total
        total += n

ad = adder()
chk("send_prime_none", ad.send(None) == 0, "send(None) == next()")
chk("send_value1", ad.send(10) == 10)
chk("send_value2", ad.send(5) == 15)

fresh = adder()
try:
    fresh.send(1)
    _send_unstarted = False
except TypeError:
    _send_unstarted = True
chk("send_unstarted_typeerror", _send_unstarted, "send(non-None) before priming")

# ===========================================================================
# SECTION 3 — generator.throw()
# ===========================================================================
# Ref: generator.throw(value) / throw(type, value, tb). Raises an exception at
# the point where the generator was paused. If the generator catches it and
# yields again, throw() returns the new yielded value; if it propagates, throw()
# re-raises. How: throw into a generator that handles vs. one that does not.
def catcher():
    while True:
        try:
            yield "running"
        except ValueError:
            yield "caught"

ct = catcher()
chk("throw_prime", next(ct) == "running")
chk("throw_caught", ct.throw(ValueError("x")) == "caught", "throw returns post-catch yield")

def noncatcher():
    yield 1
    yield 2

nc = noncatcher()
next(nc)
try:
    nc.throw(KeyError("k"))
    _throw_prop = False
except KeyError:
    _throw_prop = True
chk("throw_propagates", _throw_prop, "uncaught throw re-raises")

# Single-arg throw with an exception instance (the recommended modern form;
# the legacy 3-arg (type, value, tb) signature is deprecated since 3.12).
ct2 = catcher()
next(ct2)
chk("throw_instance", ct2.throw(ValueError("y")) == "caught", "single-arg instance form")

# ===========================================================================
# SECTION 4 — generator.close() and GeneratorExit
# ===========================================================================
# Ref: generator.close(). Raises GeneratorExit at the paused point. If the
# generator yields a value in response, RuntimeError is raised. A normal close
# runs finally blocks. close() on an already-finished generator is a no-op.
closed_log = []
def closeable():
    try:
        yield 1
        yield 2
    finally:
        closed_log.append("cleanup")

cg = closeable()
next(cg)
cg.close()
chk("close_runs_finally", closed_log == ["cleanup"])
# close() after close is harmless
cg.close()
chk("close_idempotent", closed_log == ["cleanup"], "second close is no-op")

# A generator that catches GeneratorExit and yields raises RuntimeError.
def bad_close():
    try:
        yield 1
    except GeneratorExit:
        yield 99   # illegal: must not yield while handling GeneratorExit

bc = bad_close()
next(bc)
try:
    bc.close()
    _bad_close = False
except RuntimeError:
    _bad_close = True
chk("close_yield_runtimeerror", _bad_close, "yield during GeneratorExit -> RuntimeError")

# GeneratorExit derives from BaseException (not Exception) in 3.x.
chk("generatorexit_baseexc",
    issubclass(GeneratorExit, BaseException) and not issubclass(GeneratorExit, Exception))

# ===========================================================================
# SECTION 5 — return in a generator -> StopIteration.value (PEP 380)
# ===========================================================================
# Ref: "6.2.9 Yield expressions" — a return statement in a generator raises
# StopIteration; the return value is stored in StopIteration.value.
def returning():
    yield 1
    yield 2
    return "done"

rg = returning()
next(rg); next(rg)
try:
    next(rg)
    _ret_val = None
    _ret_caught = False
except StopIteration as e:
    _ret_val = e.value
    _ret_caught = True
chk("return_stopiteration_value", _ret_caught and _ret_val == "done")

# bare `return` -> StopIteration.value is None
def bare_return():
    yield 1
    return

brg = bare_return()
next(brg)
try:
    next(brg)
    _bare = "no-stop"
except StopIteration as e:
    _bare = e.value
chk("bare_return_value_none", _bare is None)

# ===========================================================================
# SECTION 6 — yield from delegation (PEP 380)
# ===========================================================================
# Ref: "6.2.9.1 yield from". Delegates iteration to a subiterator. The value of
# the `yield from` expression is the subgenerator's StopIteration.value (return
# value). send()/throw()/close() pass through to the delegate.
def inner_returns():
    yield "a"
    yield "b"
    return "inner-ret"

def outer_capture():
    captured = yield from inner_returns()
    yield captured

chk("yieldfrom_values_then_ret", list(outer_capture()) == ["a", "b", "inner-ret"])

# yield from over any iterable (not just generators)
def chain_iters():
    yield from [1, 2]
    yield from (3, 4)
    yield from "x"

chk("yieldfrom_iterables", list(chain_iters()) == [1, 2, 3, 4, "x"])

# send() passes through yield from to the delegate.
def inner_echo():
    while True:
        x = yield
        if x is None:
            return "stop"
        yield ("got", x)

def outer_echo():
    r = yield from inner_echo()
    yield r

oe = outer_echo()
next(oe)                       # prime to inner's first (bare) yield
chk("yieldfrom_send_passthrough", oe.send(7) == ("got", 7))

# `yield from` requires an iterable; delegating to a non-iterable raises TypeError.
def yieldfrom_noniter():
    yield from 5
yfn = yieldfrom_noniter()
try:
    next(yfn)
    _yf_noniter = False
except TypeError:
    _yf_noniter = True
chk("yieldfrom_noniterable_typeerror", _yf_noniter, "yield from non-iterable -> TypeError")

# ===========================================================================
# SECTION 7 — Iterator protocol: __iter__ / __next__, StopIteration
# ===========================================================================
# Ref: "4.5 Iterator Types". An iterator implements __next__ (raising
# StopIteration when done) and __iter__ returning self. An iterable implements
# __iter__ returning a fresh iterator.
class CountUp:
    def __init__(self, limit):
        self.limit = limit
        self.i = 0
    def __iter__(self):
        return self
    def __next__(self):
        if self.i >= self.limit:
            raise StopIteration
        self.i += 1
        return self.i

chk("custom_iterator", list(CountUp(4)) == [1, 2, 3, 4])
ci = CountUp(2)
chk("custom_iter_self", iter(ci) is ci)

# Separate iterable + iterator: __iter__ returns a new object each call.
class Range3:
    def __iter__(self):
        return CountUp(3)

r3 = Range3()
chk("iterable_reusable", list(r3) == [1, 2, 3] and list(r3) == [1, 2, 3], "fresh iterator each time")

# __getitem__ fallback: old-style sequence iteration via integer indexing.
class SeqByIndex:
    def __init__(self, data):
        self.data = data
    def __getitem__(self, i):
        return self.data[i]   # raises IndexError past end -> stops iteration

chk("getitem_iteration", list(SeqByIndex([10, 20, 30])) == [10, 20, 30])

# next() with a default suppresses StopIteration.
it = iter([1])
chk("next_default", (next(it), next(it, "DEF"), next(it, "DEF")) == (1, "DEF", "DEF"))

# StopIteration carries a value attribute (default None).
chk("stopiteration_value_attr", StopIteration().value is None and StopIteration(5).value == 5)
# Multiple constructor args are stored in .args; .value is the first arg.
_si = StopIteration(1, 2, 3)
chk("stopiteration_multi_args", _si.args == (1, 2, 3) and _si.value == 1,
    "args tuple preserved, value == first arg")

# ABC relationships (Ref: collections.abc). Generators and custom iterators
# register as Iterator/Iterable; generators additionally satisfy Generator.
import collections.abc as _abc
chk("abc_gen_is_iterator", isinstance(simple_gen(), _abc.Iterator))
chk("abc_gen_is_iterable", isinstance(simple_gen(), _abc.Iterable))
chk("abc_gen_is_generator", isinstance(simple_gen(), _abc.Generator))
chk("abc_custom_iterator", isinstance(CountUp(1), _abc.Iterator) and isinstance(CountUp(1), _abc.Iterable))
# A __getitem__-only sequence is NOT an Iterator (it iterates via the old protocol).
chk("abc_getitem_not_iterator", not isinstance(SeqByIndex([1]), _abc.Iterator),
    "__getitem__ fallback is not a registered Iterator")
# list_iterator from iter([...]) is a concrete Iterator.
chk("abc_listiter_is_iterator", isinstance(iter([1, 2]), _abc.Iterator))

# Slicing protocol: a zero step is illegal for ordinary sequence slicing too,
# raising ValueError("slice step cannot be zero") — distinct from islice's check.
try:
    [1, 2, 3][::0]
    _list_step0 = False
except ValueError:
    _list_step0 = True
chk("list_slice_step0_valueerror", _list_step0, "list slice step=0 -> ValueError")
try:
    "abc"[::0]
    _str_step0 = False
except ValueError:
    _str_step0 = True
chk("str_slice_step0_valueerror", _str_step0, "str slice step=0 -> ValueError")

# ===========================================================================
# SECTION 8 — iter() two-argument (sentinel) form
# ===========================================================================
# Ref: builtin iter(callable, sentinel). Calls `callable` with no args until it
# returns `sentinel`, which terminates iteration (sentinel itself not yielded).
_seq = iter([1, 2, 3, 0, 4])
chk("iter_sentinel", list(iter(lambda: next(_seq), 0)) == [1, 2, 3], "stops at sentinel, excl.")

# Real-world idiom: read fixed chunks until ''.
import io
buf = io.StringIO("abcdefg")
chunks = list(iter(lambda: buf.read(3), ""))
chk("iter_sentinel_read", chunks == ["abc", "def", "g"])

# ===========================================================================
# SECTION 9 — itertools: infinite iterators
# ===========================================================================
# itertools.count(start=0, step=1) — Ref: itertools.count.
chk("count_default", list(itertools.islice(itertools.count(), 4)) == [0, 1, 2, 3])
chk("count_start_step", list(itertools.islice(itertools.count(10, 2), 4)) == [10, 12, 14, 16])
chk("count_float_step", list(itertools.islice(itertools.count(0.0, 0.5), 3)) == [0.0, 0.5, 1.0])
chk("count_negative_step", list(itertools.islice(itertools.count(5, -1), 4)) == [5, 4, 3, 2])

# itertools.cycle(iterable) — repeats the sequence indefinitely.
chk("cycle", list(itertools.islice(itertools.cycle("AB"), 5)) == ["A", "B", "A", "B", "A"])
chk("cycle_list", list(itertools.islice(itertools.cycle([1, 2, 3]), 7)) == [1, 2, 3, 1, 2, 3, 1])
chk("cycle_empty", list(itertools.islice(itertools.cycle([]), 3)) == [],
    "cycling an empty iterable yields nothing")

# itertools.repeat(object[, times]) — yields object n times (or forever).
chk("repeat_times", list(itertools.repeat(9, 4)) == [9, 9, 9, 9])
chk("repeat_infinite", list(itertools.islice(itertools.repeat("z"), 3)) == ["z", "z", "z"])
chk("repeat_zero", list(itertools.repeat(1, 0)) == [])
chk("repeat_negative", list(itertools.repeat(1, -5)) == [], "times<0 yields nothing")

# ===========================================================================
# SECTION 10 — itertools: terminating iterators
# ===========================================================================
# itertools.accumulate(iterable[, func, *, initial=None]) — running totals.
chk("accumulate_default", list(itertools.accumulate([1, 2, 3, 4])) == [1, 3, 6, 10])
chk("accumulate_mul", list(itertools.accumulate([1, 2, 3, 4], operator.mul)) == [1, 2, 6, 24])
chk("accumulate_max", list(itertools.accumulate([3, 1, 4, 1, 5], max)) == [3, 3, 4, 4, 5])
chk("accumulate_initial",
    list(itertools.accumulate([1, 2, 3], initial=100)) == [100, 101, 103, 106],
    "initial= prepends and seeds")
chk("accumulate_empty", list(itertools.accumulate([])) == [])
chk("accumulate_empty_initial", list(itertools.accumulate([], initial=5)) == [5])
chk("accumulate_single", list(itertools.accumulate([42])) == [42], "lone element passes through")

# itertools.chain(*iterables) and chain.from_iterable(iterable).
chk("chain", list(itertools.chain([1, 2], (3,), "ab")) == [1, 2, 3, "a", "b"])
chk("chain_empty", list(itertools.chain()) == [])
chk("chain_from_iterable",
    list(itertools.chain.from_iterable([[1, 2], [3], [4, 5]])) == [1, 2, 3, 4, 5])
# Depth: chain is itself a one-shot iterator — a second pass yields nothing.
_ch = itertools.chain([1], [2, 3])
chk("chain_one_shot", (list(_ch), list(_ch)) == ([1, 2, 3], []), "exhausts after one pass")

# itertools.compress(data, selectors) — pick where selector is truthy.
chk("compress", list(itertools.compress("ABCDEF", [1, 0, 1, 0, 1, 1])) == ["A", "C", "E", "F"])
chk("compress_bool", list(itertools.compress(range(5), [True, False, True, False, True]))
    == [0, 2, 4])
chk("compress_short_selectors", list(itertools.compress("ABCD", [1, 1])) == ["A", "B"],
    "stops at shorter selectors")
chk("compress_short_data", list(itertools.compress("AB", [1, 1, 1, 1])) == ["A", "B"],
    "stops at shorter data")

# itertools.dropwhile(pred, iterable) — drop while true, then yield the rest.
chk("dropwhile", list(itertools.dropwhile(lambda x: x < 3, [1, 2, 3, 4, 1, 0]))
    == [3, 4, 1, 0], "no re-drop after first false")
chk("dropwhile_all", list(itertools.dropwhile(lambda x: True, [1, 2, 3])) == [])

# itertools.takewhile(pred, iterable) — yield while true, then stop.
chk("takewhile", list(itertools.takewhile(lambda x: x < 3, [1, 2, 3, 4, 1])) == [1, 2])
chk("takewhile_none", list(itertools.takewhile(lambda x: x > 100, [1, 2])) == [])

# itertools.filterfalse(pred, iterable) — opposite of filter.
chk("filterfalse", list(itertools.filterfalse(lambda x: x % 2, range(8))) == [0, 2, 4, 6])
chk("filterfalse_none_pred", list(itertools.filterfalse(None, [0, 1, 0, 2, "", "x"]))
    == [0, 0, ""], "None pred filters out truthy")

# itertools.groupby(iterable, key=None) — group consecutive equal-key runs.
groups = [(k, list(v)) for k, v in itertools.groupby("aaabbbcca")]
chk("groupby_consecutive", groups == [("a", ["a", "a", "a"]), ("b", ["b", "b", "b"]),
                                      ("c", ["c", "c"]), ("a", ["a"])],
    "groups only consecutive runs")
keyed = [(k, list(v)) for k, v in itertools.groupby([1, 1, 2, 3, 3], key=lambda x: x % 2)]
chk("groupby_key", keyed == [(1, [1, 1]), (0, [2]), (1, [3, 3])])
chk("groupby_empty", list(itertools.groupby([])) == [])
# Depth: advancing the outer groupby invalidates the previous group iterator
# (it shares the underlying source), so a not-yet-consumed group becomes empty.
_gb = itertools.groupby("aabb")
_k1, _v1 = next(_gb)
_k2, _v2 = next(_gb)
chk("groupby_group_invalidated", (_k1, _k2) == ("a", "b") and list(_v1) == [],
    "old group exhausted once outer advances")

# itertools.islice(iterable, [start,] stop[, step]).
chk("islice_stop", list(itertools.islice("ABCDEFG", 3)) == ["A", "B", "C"])
chk("islice_start_stop", list(itertools.islice("ABCDEFG", 2, 5)) == ["C", "D", "E"])
chk("islice_step", list(itertools.islice("ABCDEFG", 0, None, 2)) == ["A", "C", "E", "G"])
chk("islice_start_none", list(itertools.islice(range(10), 7, None)) == [7, 8, 9])
# Depth: islice consumes the underlying iterator's state (it does not copy).
# After taking 2 items, the source resumes from where islice stopped.
_isrc = iter([0, 1, 2, 3, 4, 5])
_taken = list(itertools.islice(_isrc, 2))
chk("islice_consumes_source", _taken == [0, 1] and list(_isrc) == [2, 3, 4, 5],
    "islice advances the shared source")
# islice step must be a positive integer (or None); step=0 raises ValueError.
try:
    list(itertools.islice("ABC", 0, None, 0))
    _islice_step0 = False
except ValueError:
    _islice_step0 = True
chk("islice_step0_valueerror", _islice_step0, "step must be positive")

# itertools.pairwise(iterable) (3.10+) — consecutive overlapping pairs.
chk("pairwise", list(itertools.pairwise([1, 2, 3, 4])) == [(1, 2), (2, 3), (3, 4)])
chk("pairwise_single", list(itertools.pairwise([1])) == [], "fewer than 2 -> empty")
chk("pairwise_str", list(itertools.pairwise("abc")) == [("a", "b"), ("b", "c")])

# itertools.starmap(func, iterable) — like map but unpacks each tuple as args.
chk("starmap", list(itertools.starmap(pow, [(2, 3), (3, 2), (10, 0)])) == [8, 9, 1])
chk("starmap_add", list(itertools.starmap(operator.add, [(1, 2), (3, 4)])) == [3, 7])

# itertools.tee(iterable, n=2) — n independent iterators sharing the source.
t1, t2 = itertools.tee([1, 2, 3])
chk("tee_independent", (list(t1), list(t2)) == ([1, 2, 3], [1, 2, 3]))
ta, tb, tc = itertools.tee("xy", 3)
chk("tee_n3", (list(ta), list(tb), list(tc)) == (["x", "y"], ["x", "y"], ["x", "y"]))
chk("tee_n0", itertools.tee([1, 2, 3], 0) == (), "n=0 -> empty tuple")

# itertools.zip_longest(*iterables, fillvalue=None) — pad shorter inputs.
chk("zip_longest_default",
    list(itertools.zip_longest([1, 2, 3], "ab")) == [(1, "a"), (2, "b"), (3, None)])
chk("zip_longest_fill",
    list(itertools.zip_longest([1], [9, 8, 7], fillvalue=0)) == [(1, 9), (0, 8), (0, 7)])
chk("zip_longest_equal", list(itertools.zip_longest([1, 2], [3, 4])) == [(1, 3), (2, 4)])

# ===========================================================================
# SECTION 11 — itertools: combinatoric iterators
# ===========================================================================
# itertools.product(*iterables, repeat=1) — cartesian product.
chk("product", list(itertools.product([1, 2], "ab"))
    == [(1, "a"), (1, "b"), (2, "a"), (2, "b")])
chk("product_repeat", list(itertools.product([0, 1], repeat=2))
    == [(0, 0), (0, 1), (1, 0), (1, 1)])
chk("product_three", len(list(itertools.product("ab", "cd", "ef"))) == 8)
chk("product_empty_factor", list(itertools.product([1, 2], [])) == [],
    "any empty factor -> empty product")

# itertools.permutations(iterable, r=None) — r-length ordered arrangements.
chk("permutations_full",
    list(itertools.permutations([1, 2, 3]))
    == [(1, 2, 3), (1, 3, 2), (2, 1, 3), (2, 3, 1), (3, 1, 2), (3, 2, 1)])
chk("permutations_r",
    list(itertools.permutations("ABC", 2))
    == [("A", "B"), ("A", "C"), ("B", "A"), ("B", "C"), ("C", "A"), ("C", "B")])
chk("permutations_r_gt_n", list(itertools.permutations([1, 2], 3)) == [], "r>n -> empty")
chk("permutations_r0", list(itertools.permutations([1, 2], 0)) == [()])

# itertools.combinations(iterable, r) — r-length sorted subsequences, no repeats.
chk("combinations",
    list(itertools.combinations([1, 2, 3, 4], 2))
    == [(1, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 4)])
chk("combinations_r0", list(itertools.combinations("ABC", 0)) == [()])
chk("combinations_r_eq_n", list(itertools.combinations([1, 2, 3], 3)) == [(1, 2, 3)])
chk("combinations_r_gt_n", list(itertools.combinations([1, 2], 3)) == [])

# itertools.combinations_with_replacement(iterable, r) — allow repeated elements.
chk("combinations_with_replacement",
    list(itertools.combinations_with_replacement([1, 2, 3], 2))
    == [(1, 1), (1, 2), (1, 3), (2, 2), (2, 3), (3, 3)])
chk("cwr_single",
    list(itertools.combinations_with_replacement("AB", 3))
    == [("A", "A", "A"), ("A", "A", "B"), ("A", "B", "B"), ("B", "B", "B")])

# Count cross-check: |comb(n,r)| * (binomial) vs permutations etc.
chk("comb_count_math",
    len(list(itertools.combinations(range(5), 3))) == 10
    and len(list(itertools.permutations(range(5), 3))) == 60)

# ===========================================================================
# SECTION 12 — itertools.batched (3.12+)
# ===========================================================================
# Ref: itertools.batched(iterable, n) — batches of length n; final batch may be
# shorter. Requires n >= 1 (ValueError otherwise). 3.13 added strict= kwarg.
if hasattr(itertools, "batched"):
    chk("batched_even", list(itertools.batched("ABCDEF", 2))
        == [("A", "B"), ("C", "D"), ("E", "F")])
    chk("batched_remainder", list(itertools.batched(range(7), 3))
        == [(0, 1, 2), (3, 4, 5), (6,)], "last batch shorter")
    chk("batched_n1", list(itertools.batched([1, 2, 3], 1)) == [(1,), (2,), (3,)])
    chk("batched_large_n", list(itertools.batched([1, 2], 10)) == [(1, 2)],
        "n larger than input -> one batch")
    try:
        list(itertools.batched([1, 2], 0))
        _batched_err = False
    except ValueError:
        _batched_err = True
    chk("batched_n0_valueerror", _batched_err, "n<1 raises ValueError")
    # strict= (3.13+): raises ValueError if final batch is short
    if sys.version_info >= (3, 13):
        try:
            list(itertools.batched(range(5), 2, strict=True))
            _strict = False
        except ValueError:
            _strict = True
        chk("batched_strict", _strict, "strict=True on uneven raises")
    else:
        chk("batched_strict", True, "(skip: needs 3.13)")
else:
    chk("batched_even", True, "(skip: needs 3.12)")
    chk("batched_remainder", True, "(skip: needs 3.12)")
    chk("batched_n1", True, "(skip: needs 3.12)")
    chk("batched_large_n", True, "(skip: needs 3.12)")
    chk("batched_n0_valueerror", True, "(skip: needs 3.12)")
    chk("batched_strict", True, "(skip: needs 3.13)")

# ===========================================================================
# SECTION 13 — Generator expressions: laziness, scoping, and consumption
# ===========================================================================
# Ref: "6.2.8 Generator expressions". A genexpr is lazily evaluated; the leftmost
# for-clause's iterable is evaluated immediately, the rest on demand. It is a
# one-shot iterator (exhausts after one pass).
gx = (x * 2 for x in range(4))
chk("genexpr_type", type(gx).__name__ == "generator")
chk("genexpr_values", list(gx) == [0, 2, 4, 6])
chk("genexpr_exhausted", list(gx) == [], "one-shot: second pass empty")

# Lazy: an infinite source consumed only partially.
chk("genexpr_lazy",
    list(itertools.islice((n * n for n in itertools.count(1)), 4)) == [1, 4, 9, 16])

# Multiple for-clauses: leftmost is the outer loop (evaluated first/slowest).
chk("genexpr_multiclause",
    list(x + y for x in [1, 2] for y in [10, 20]) == [11, 21, 12, 22])
# Genexpr with a filter clause.
chk("genexpr_filter",
    list(x for x in range(10) if x % 3 == 0) == [0, 3, 6, 9])

# Leftmost iterable evaluated eagerly at creation time (closure capture point).
_src = [1, 2, 3]
gx2 = (v for v in _src)
_src = [9, 9]                 # rebinding the name does NOT affect captured iterable
chk("genexpr_capture_iterable", list(gx2) == [1, 2, 3], "outermost iter bound at creation")

# ===========================================================================
# SECTION 14 — Recipe-style composition (real itertools usage patterns)
# ===========================================================================
# These mirror documented itertools "recipes" to confirm composition works.
def take(n, it):
    return list(itertools.islice(it, n))

def flatten(list_of_lists):
    return list(itertools.chain.from_iterable(list_of_lists))

def ncycles(it, n):
    return list(itertools.chain.from_iterable(itertools.repeat(tuple(it), n)))

chk("recipe_take", take(3, itertools.count(100)) == [100, 101, 102])
chk("recipe_flatten", flatten([[1, 2], [3], [4, 5, 6]]) == [1, 2, 3, 4, 5, 6])
chk("recipe_ncycles", ncycles([1, 2], 3) == [1, 2, 1, 2, 1, 2])

# dot-product via starmap+sum over zipped vectors.
def dotproduct(a, b):
    return sum(itertools.starmap(operator.mul, zip(a, b)))
chk("recipe_dotproduct", dotproduct([1, 2, 3], [4, 5, 6]) == 32)

# grouper using zip_longest (fixed-length chunks with padding).
def grouper(iterable, n, fillvalue=None):
    args = [iter(iterable)] * n
    return list(itertools.zip_longest(*args, fillvalue=fillvalue))
chk("recipe_grouper", grouper("ABCDEFG", 3, "x")
    == [("A", "B", "C"), ("D", "E", "F"), ("G", "x", "x")])

print(("PY_GENITER_OK") if _ok else ("PY_GENITER_FAIL"))
sys.exit(0 if _ok else 1)
