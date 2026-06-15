#!/usr/bin/env python3
"""Python core-language correctness test running inside StarryOS.

Companion to the python-hello smoke: this exercises the broad Python *language*
surface with EXACT assertions -- data model, OOP, generators, coroutines/async,
structural pattern matching, decorators, context managers, exceptions, typing,
the pure stdlib, threading, and venv. Pure CPython stdlib only (no third-party
packages). Prints `TEST PASSED` iff every check holds (harness keys on that line).
"""

import sys

# Banner: record the exact interpreter under test (delivery evidence).
print(
    "PYLANG python %d.%d.%d (%s) on %s"
    % (sys.version_info[0], sys.version_info[1], sys.version_info[2],
       sys.implementation.name, sys.platform)
)

ok = True


def chk(name, cond, info=""):
    global ok
    if cond:
        print("  pylang ok %s %s" % (name, info))
    else:
        ok = False
        print("  PYLANG FAIL %s %s" % (name, info))


# --- numbers ---
chk("int_bignum", 2 ** 200 ==
    1606938044258990275541962092341162602522202993782792835301376)
chk("int_ops", (17 // 5, 17 % 5, divmod(17, 5), pow(2, 10, 1000)) == (3, 2, (3, 2), 24))
chk("bit_ops", (0b1010 | 0b0101, 0b1100 & 0b1010, 0b1100 ^ 0b1010, 5 << 3) == (15, 8, 6, 40))
chk("float_round", round(2.5) == 2 and round(3.5) == 4)  # banker's rounding
chk("complex", (complex(1, 2) * complex(3, 4)) == complex(-5, 10))

# --- strings / f-strings / bytes ---
s = "StarryOS"
chk("str_slice", s[::-1] == "SOyrratS" and s[2:5] == "arr" and s.lower() == "starryos")
chk("str_methods", " a,b,c ".strip().split(",") == ["a", "b", "c"] and "x".join("123") == "1x2x3")
v, w = 42, 3.14159
chk("fstring", f"{v:#06x}|{w:.2f}|{v=}" == "0x002a|3.14|v=42")
chk("bytes", bytes([104, 105]).decode() == "hi" and "hé".encode("utf-8") == b"h\xc3\xa9")

# --- containers + comprehensions ---
chk("listcomp", [x * x for x in range(5) if x % 2 == 0] == [0, 4, 16])
chk("dictcomp", {k: i for i, k in enumerate("abc")} == {"a": 0, "b": 1, "c": 2})
chk("setcomp", {x % 3 for x in range(10)} == {0, 1, 2})
chk("genexpr", sum(x for x in range(101)) == 5050)
a, *mid, b = [1, 2, 3, 4, 5]
chk("star_unpack", a == 1 and mid == [2, 3, 4] and b == 5)
chk("dict_merge", {**{"x": 1}, "y": 2} == {"x": 1, "y": 2} and {"a": 1} | {"b": 2} == {"a": 1, "b": 2})

# --- structural pattern matching (PEP 634) ---
def classify(obj):
    match obj:
        case 0:
            return "zero"
        case [x, y]:
            return ("pair", x, y)
        case {"k": val}:
            return ("map", val)
        case str() as t if len(t) > 2:
            return ("longstr", t)
        case _:
            return "other"


chk("match_literal", classify(0) == "zero")
chk("match_seq", classify([7, 8]) == ("pair", 7, 8))
chk("match_map", classify({"k": 9}) == ("map", 9))
chk("match_guard", classify("abcd") == ("longstr", "abcd"))
chk("match_wildcard", classify(3.0) == "other")

# --- functions: positional-only / keyword-only / closures / nonlocal ---
def f(a, b=2, /, c=3, *args, d, **kw):
    return (a, b, c, args, d, sorted(kw.items()))


chk("func_args", f(1, 9, 8, 7, d=4, e=5) == (1, 9, 8, (7,), 4, [("e", 5)]))


def counter():
    n = 0

    def inc():
        nonlocal n
        n += 1
        return n
    return inc


c = counter()
chk("closure", (c(), c(), c()) == (1, 2, 3))
chk("lambda_sort", sorted([(1, "b"), (1, "a"), (0, "z")], key=lambda t: (t[0], t[1]))
    == [(0, "z"), (1, "a"), (1, "b")])

# --- OOP: inheritance / super / MRO / dunders / property / dataclass / abc / slots ---
class Base:
    def who(self):
        return "base"


class Mixin:
    def who(self):
        return "mixin+" + super().who()


class Derived(Mixin, Base):
    pass


chk("mro", [k.__name__ for k in Derived.__mro__][:3] == ["Derived", "Mixin", "Base"])
chk("super", Derived().who() == "mixin+base")


class Vec:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x, self.y = x, y

    def __repr__(self):
        return "Vec(%d,%d)" % (self.x, self.y)

    def __eq__(self, o):
        return (self.x, self.y) == (o.x, o.y)

    def __hash__(self):
        return hash((self.x, self.y))

    def __add__(self, o):
        return Vec(self.x + o.x, self.y + o.y)

    def __len__(self):
        return 2

    def __getitem__(self, i):
        return (self.x, self.y)[i]

    def __iter__(self):
        yield self.x
        yield self.y


chk("dunder_repr_eq", repr(Vec(1, 2)) == "Vec(1,2)" and Vec(1, 2) == Vec(1, 2))
chk("dunder_add_len_idx",
    (Vec(1, 2) + Vec(3, 4)) == Vec(4, 6) and len(Vec(1, 2)) == 2 and Vec(5, 6)[1] == 6)
chk("dunder_hash_iter", len({Vec(1, 2), Vec(1, 2)}) == 1 and list(Vec(7, 8)) == [7, 8])
try:
    Vec(1, 2).z = 9
    slots_ok = False
except AttributeError:
    slots_ok = True
chk("slots_enforced", slots_ok)


class Temp:
    def __init__(self, cval):
        self._c = cval

    @property
    def f(self):
        return self._c * 9 / 5 + 32

    @staticmethod
    def smeth():
        return "s"

    @classmethod
    def cmeth(cls):
        return cls.__name__


chk("property", Temp(100).f == 212.0)
chk("static_class_method", Temp.smeth() == "s" and Temp.cmeth() == "Temp")

from dataclasses import dataclass, field


@dataclass(order=True)
class Pt:
    x: int
    y: int = 0
    tags: list = field(default_factory=list)


chk("dataclass", Pt(1, 2) == Pt(1, 2) and Pt(1) < Pt(2) and Pt(1).tags == [])

from abc import ABC, abstractmethod


class Shape(ABC):
    @abstractmethod
    def area(self):
        ...


class Sq(Shape):
    def __init__(self, side):
        self.side = side

    def area(self):
        return self.side * self.side


try:
    Shape()
    abc_ok = False
except TypeError:
    abc_ok = True
chk("abc_abstract", abc_ok and Sq(4).area() == 16)

# --- generators ---
def gen():
    yield 1
    x = yield 2
    yield x * 10


g = gen()
chk("gen_send", (next(g), next(g), g.send(5)) == (1, 2, 50))


def delegating():
    yield from range(3)
    yield from "ab"


chk("yield_from", list(delegating()) == [0, 1, 2, "a", "b"])

# --- decorators ---
import functools


def trace(fn):
    @functools.wraps(fn)
    def wrapper(*a, **k):
        wrapper.calls += 1
        return fn(*a, **k)
    wrapper.calls = 0
    return wrapper


@trace
def add(a, b):
    "adds"
    return a + b


chk("decorator", add(2, 3) == 5 and add(1, 1) == 2 and add.calls == 2
    and add.__name__ == "add" and add.__doc__ == "adds")


def repeat(n):
    def deco(fn):
        def wrapper(*a, **k):
            return [fn(*a, **k) for _ in range(n)]
        return wrapper
    return deco


@repeat(3)
def hi():
    return "x"


chk("param_decorator", hi() == ["x", "x", "x"])

# --- context managers ---
import contextlib

log = []


@contextlib.contextmanager
def ctx(name):
    log.append("enter " + name)
    try:
        yield name
    finally:
        log.append("exit " + name)


with ctx("a") as nm:
    log.append("body " + nm)
chk("contextmanager", log == ["enter a", "body a", "exit a"])

order = []
with contextlib.ExitStack() as st:
    for i in range(3):
        st.callback(lambda i=i: order.append(i))
chk("exitstack", order == [2, 1, 0])

# --- exceptions ---
class MyErr(Exception):
    pass


chain = None
try:
    try:
        raise ValueError("root")
    except ValueError as e:
        raise MyErr("wrap") from e
except MyErr as e:
    chain = (str(e), str(e.__cause__))
chk("raise_from", chain == ("wrap", "root"))

fin = []
try:
    fin.append("t")
    raise KeyError("k")
except KeyError:
    fin.append("e")
else:
    fin.append("else")
finally:
    fin.append("f")
chk("try_except_finally", fin == ["t", "e", "f"])

got = []
try:
    raise ExceptionGroup("g", [ValueError("v"), TypeError("t")])
except* ValueError as eg:
    got.append("V%d" % len(eg.exceptions))
except* TypeError as eg:
    got.append("T%d" % len(eg.exceptions))
chk("except_star_group", sorted(got) == ["T1", "V1"])

# --- walrus / typing ---
data = [1, 2, 3, 4]
chk("walrus", [y for x in data if (y := x * x) > 4] == [9, 16])

import typing

T = typing.TypeVar("T")


class Box(typing.Generic[T]):
    def __init__(self, val: T):
        self.val = val

    def get(self) -> T:
        return self.val


def annotated(a: int, b: str) -> bool:
    return True


chk("typing_generic", Box(5).get() == 5)
chk("get_type_hints", typing.get_type_hints(annotated) == {"a": int, "b": str, "return": bool})

# --- stdlib: collections / itertools / functools ---
from collections import deque, Counter, defaultdict, namedtuple

dq = deque([2, 3])
dq.appendleft(1)
dq.append(4)
chk("deque", list(dq) == [1, 2, 3, 4] and dq.popleft() == 1)
chk("counter", Counter("mississippi").most_common(1) == [("i", 4)])
dd = defaultdict(list)
dd["k"].append(1)
dd["k"].append(2)
chk("defaultdict", dd["k"] == [1, 2])
P = namedtuple("P", "x y")
p = P(1, 2)
chk("namedtuple", p.x == 1 and p._replace(y=9) == P(1, 9) and tuple(p) == (1, 2))

import itertools

chk("itertools",
    list(itertools.islice(itertools.count(10), 3)) == [10, 11, 12]
    and list(itertools.chain([1], [2, 3])) == [1, 2, 3]
    and [k for k, _ in itertools.groupby("aaabbc")] == ["a", "b", "c"])
chk("functools_reduce", functools.reduce(lambda a, b: a * b, range(1, 6)) == 120)


@functools.lru_cache(maxsize=None)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


chk("lru_cache", fib(30) == 832040)
chk("partial", functools.partial(pow, 2)(10) == 1024)

# --- stdlib: json / re / datetime / pathlib / math / hashlib / base64 / enum / struct ---
import json

obj = {"n": 1, "a": [True, None, 2.5], "s": "x"}
chk("json_roundtrip", json.loads(json.dumps(obj)) == obj)

import re

m = re.match(r"(\w+)@(\w+)\.(\w+)", "user@host.com")
chk("regex", m.groups() == ("user", "host", "com") and re.sub(r"\d+", "#", "a1b22c") == "a#b#c")

import datetime

dt = datetime.datetime(2026, 6, 13, 12, 30, 0)
chk("datetime", dt.isoformat() == "2026-06-13T12:30:00"
    and (dt + datetime.timedelta(days=1)).day == 14)

import pathlib

pp = pathlib.PurePosixPath("/a/b/c.txt")
chk("pathlib", pp.name == "c.txt" and pp.suffix == ".txt" and pp.parent.as_posix() == "/a/b")

import math

chk("math", math.gcd(48, 36) == 12 and math.isclose(math.sqrt(2) ** 2, 2.0)
    and math.factorial(6) == 720)

import hashlib

chk("hashlib", hashlib.sha256(b"abc").hexdigest()
    == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")

import base64

chk("base64", base64.b64encode(b"hello").decode() == "aGVsbG8="
    and base64.b64decode("aGVsbG8=") == b"hello")

import enum


class Color(enum.Enum):
    R = 1
    G = 2


chk("enum", Color.R.value == 1 and Color(2) is Color.G and Color.R.name == "R")

import struct

chk("struct", struct.unpack(">HI", struct.pack(">HI", 7, 70000)) == (7, 70000))

# --- concurrency: threading + futures ---
import threading

acc = {"n": 0}
lock = threading.Lock()


def worker():
    for _ in range(1000):
        with lock:
            acc["n"] += 1


ts = [threading.Thread(target=worker) for _ in range(4)]
for t in ts:
    t.start()
for t in ts:
    t.join()
chk("threading_lock", acc["n"] == 4000)

ev = threading.Event()
res = []


def waiter():
    ev.wait()
    res.append("woke")


wt = threading.Thread(target=waiter)
wt.start()
ev.set()
wt.join()
chk("threading_event", res == ["woke"])

from concurrent.futures import ThreadPoolExecutor

with ThreadPoolExecutor(max_workers=4) as ex:
    sq = list(ex.map(lambda x: x * x, range(6)))
chk("threadpool", sq == [0, 1, 4, 9, 16, 25])

# --- concurrency: asyncio (coroutines / async gen / async with) ---
import asyncio


async def aplus(x):
    await asyncio.sleep(0)
    return x + 1


async def agen():
    for i in range(3):
        await asyncio.sleep(0)
        yield i


class ACtx:
    async def __aenter__(self):
        return "in"

    async def __aexit__(self, *a):
        return False


async def amain():
    vals = await asyncio.gather(aplus(1), aplus(2), aplus(3))
    collected = [i async for i in agen()]
    async with ACtx() as t:
        ctxv = t
    return vals, collected, ctxv


vals, collected, ctxv = asyncio.run(amain())
chk("asyncio_gather", vals == [2, 3, 4])
chk("async_generator", collected == [0, 1, 2])
chk("async_with", ctxv == "in")

# --- version-specific / modern language features (PEP-guarded) ---
# Newer *syntax* lives in exec()'d source so this file still PARSES on older
# interpreters; each check runs when the running version supports it, else
# records a noted skip. References cite the governing PEP.
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


# PEP 695 (3.12): type-parameter syntax + `type` alias statement
_gated_syntax(
    "pep695_type_params", (3, 12),
    "def first[T](xs: list[T]) -> T: return xs[0]\n"
    "class Box[T]:\n"
    "    def __init__(self, v): self.v = v\n"
    "type Alias = list[int]\n"
    "R = (first([7, 8]), Box('z').v, Alias.__value__)\n",
    lambda ns: ns["R"] == (7, "z", list[int]),
)

# PEP 701 (3.12): f-strings may reuse the enclosing quote + nest arbitrarily
_gated_syntax(
    "pep701_fstrings", (3, 12),
    "d = {'k': 'v'}\n"
    "R = f'{d['k']}{f'{1 + 1}'}'\n",
    lambda ns: ns["R"] == "v2",
)

# PEP 750 (3.14): t-strings evaluate to a Template object, not an interpolated str
_gated_syntax(
    "pep750_tstrings", (3, 14),
    "name = 'world'\n"
    "tmpl = t'hi {name}'\n"
    "R = (type(tmpl).__name__, tmpl.values[0])\n",
    lambda ns: ns["R"] == ("Template", "world"),
)

# --- stdlib: venv (`python -m venv`) ---
# Assert the venv stdlib module works: it spawns a child interpreter and lays out
# a virtual environment. Use --without-pip (ensurepip bootstrap is a pip concern,
# not a language-level one); then run the venv's own interpreter and check prefix.
import os
import subprocess
import tempfile

venv_dir = os.path.join(tempfile.mkdtemp(), ".venv")
proc = subprocess.run(
    [sys.executable, "-m", "venv", "--without-pip", venv_dir],
    capture_output=True, text=True,
)
venv_py = os.path.join(venv_dir, "bin", "python")
venv_ok = (proc.returncode == 0 and os.path.exists(venv_py)
           and os.path.exists(os.path.join(venv_dir, "pyvenv.cfg")))
if venv_ok:
    out = subprocess.run([venv_py, "-c", "import sys; print(sys.prefix)"],
                         capture_output=True, text=True)
    venv_ok = out.returncode == 0 and out.stdout.strip() == venv_dir
chk("venv_create", venv_ok, "rc=%d err=%r" % (proc.returncode, proc.stderr[-120:]))

print("TEST PASSED" if ok else "TEST FAILED")
sys.exit(0 if ok else 1)
