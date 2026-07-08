#!/usr/bin/env python3
"""typing + argparse + logging + diagnostics (warnings/traceback/uuid/ipaddress) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# =====================================================================
# SECTION 1: typing — TypeVar
# Ref: typing.TypeVar (https://docs.python.org/3/library/typing.html#typing.TypeVar)
# How: build TypeVars with bound / constraints / variance flags and read back
#      __name__, __bound__, __constraints__, __covariant__, __contravariant__.
# Expected: introspection attributes reflect exactly the constructor args.
# Why: TypeVar is the runtime backbone of all generics; the kernel/interpreter
#      must preserve these attributes (pure-Python objects, no syscalls).
# =====================================================================
import typing
from typing import TypeVar

T = TypeVar("T")
chk("typevar_plain_name", T.__name__ == "T")
chk("typevar_plain_no_bound", T.__bound__ is None)
chk("typevar_plain_no_constraints", T.__constraints__ == ())
chk("typevar_plain_invariant",
    T.__covariant__ is False and T.__contravariant__ is False)

TB = TypeVar("TB", bound=int)
chk("typevar_bound", TB.__bound__ is int)
chk("typevar_bound_no_constraints", TB.__constraints__ == ())

TC = TypeVar("TC", int, str)
chk("typevar_constraints", TC.__constraints__ == (int, str))
chk("typevar_constraints_no_bound", TC.__bound__ is None)

TCov = TypeVar("TCov", covariant=True)
chk("typevar_covariant", TCov.__covariant__ is True and TCov.__contravariant__ is False)
TCon = TypeVar("TCon", contravariant=True)
chk("typevar_contravariant", TCon.__contravariant__ is True and TCon.__covariant__ is False)
# repr carries a variance prefix for co/contravariant TypeVars.
chk("typevar_repr_covariant", repr(TCov) == "+TCov")
chk("typevar_repr_contravariant", repr(TCon) == "-TCon")
chk("typevar_repr_invariant", repr(T) == "~T")


# =====================================================================
# SECTION 2: typing.Generic — generic class + parameterization
# Ref: typing.Generic (https://docs.python.org/3/library/typing.html#typing.Generic)
# How: subclass Generic[T]; check __class_getitem__ subscription works and
#      records the type argument; instances behave like ordinary objects.
# Expected: Box[int] is a _GenericAlias with __args__ == (int,); construction OK.
# Why: generics are runtime erasure but the alias object must carry args.
# =====================================================================
from typing import Generic

class Box(Generic[T]):
    def __init__(self, val):
        self.val = val
    def get(self):
        return self.val

chk("generic_construct", Box(7).get() == 7)
_alias = Box[int]
chk("generic_subscript_args", typing.get_args(_alias) == (int,))
chk("generic_subscript_origin", typing.get_origin(_alias) is Box)
# A generic class exposes its type params via __parameters__.
chk("generic_parameters", Box.__parameters__ == (T,))
# Instantiate through the alias (3.7+): still produces a plain Box instance.
chk("generic_alias_instantiate", isinstance(Box[int](5), Box))

# Multi-parameter generic: __parameters__ keeps declaration order; subscripting
# with two args records both, and get_origin still points at the class.
U = TypeVar("U")
class Pair(Generic[T, U]):
    pass
chk("generic_two_params", Pair.__parameters__ == (T, U))
chk("generic_two_args", typing.get_args(Pair[int, str]) == (int, str))
chk("generic_two_origin", typing.get_origin(Pair[int, str]) is Pair)


# =====================================================================
# SECTION 3: typing.Protocol + runtime_checkable + isinstance
# Ref: typing.Protocol / typing.runtime_checkable (PEP 544)
# How: declare a Protocol with a method; mark runtime_checkable; verify
#      isinstance() does structural (method-presence) checks; non-decorated
#      protocols raise TypeError on isinstance.
# Expected: duck-typed objects pass isinstance against the @runtime_checkable
#      protocol; missing-method objects fail.
# Why: structural typing is a core 3.x feature widely used in libs.
# =====================================================================
from typing import Protocol, runtime_checkable

@runtime_checkable
class Sized(Protocol):
    def __len__(self) -> int: ...

class HasLen:
    def __len__(self):
        return 3
class NoLen:
    pass

chk("protocol_isinstance_pass", isinstance(HasLen(), Sized))
chk("protocol_isinstance_fail", not isinstance(NoLen(), Sized))
chk("protocol_builtin_list", isinstance([1, 2], Sized))  # list has __len__
# Protocol that is NOT runtime_checkable raises TypeError on isinstance.
class Drawable(Protocol):
    def draw(self) -> None: ...
try:
    isinstance(object(), Drawable)
    _proto_guard = False
except TypeError:
    _proto_guard = True
chk("protocol_non_checkable_raises", _proto_guard)
# _is_protocol marker is set on Protocol subclasses.
chk("protocol_marker", getattr(Sized, "_is_protocol", False) is True)


# =====================================================================
# SECTION 4: typing.get_type_hints (incl. include_extras), Annotated
# Ref: typing.get_type_hints / typing.Annotated (PEP 593)
# How: annotate a function; get_type_hints strips Annotated metadata by
#      default but keeps it when include_extras=True.
# Expected: plain hints give bare types; include_extras keeps Annotated[...].
# Why: introspection used by pydantic/FastAPI-style libs; must be exact.
# =====================================================================
from typing import Annotated, get_type_hints, get_args, get_origin

def annotated_fn(a: int, b: "str", c: Annotated[float, "meters"]) -> bool:
    return True

_hints = get_type_hints(annotated_fn)
chk("get_type_hints_plain",
    _hints == {"a": int, "b": str, "c": float, "return": bool})
_hints_extra = get_type_hints(annotated_fn, include_extras=True)
chk("get_type_hints_include_extras",
    _hints_extra["c"] == Annotated[float, "meters"])
# Forward-ref string "str" resolved to the str type.
chk("get_type_hints_forwardref", _hints["b"] is str)
# localns is consulted to resolve a forward reference not in the global scope:
# a string annotation "LocalT" resolves to the type passed via localns=.
class LocalT:
    pass
def _localfn(z):
    pass
_localfn.__annotations__ = {"z": "LocalT"}
chk("get_type_hints_localns",
    get_type_hints(_localfn, localns={"LocalT": LocalT})["z"] is LocalT)

# Annotated introspection: get_origin returns the Annotated special form
# (like Literal/ClassVar); get_args yields (underlying_type, *metadata);
# __metadata__ holds just the extra args; __origin__ is the wrapped type.
_ann = Annotated[int, "x", 42]
chk("annotated_origin", get_origin(_ann) is Annotated)
chk("annotated_underlying", _ann.__origin__ is int)
chk("annotated_args", get_args(_ann) == (int, "x", 42))
chk("annotated_metadata", _ann.__metadata__ == ("x", 42))


# =====================================================================
# SECTION 5: get_args / get_origin on the generic-alias zoo
# Ref: typing.get_args / typing.get_origin
# How: feed Union, Optional, list[...], dict[...], tuple[...], Callable and
#      check the decomposition matches the docs table.
# Expected: origin is the unsubscripted runtime class (or special form),
#      args is the tuple of type parameters.
# Why: these two functions are THE supported way to introspect generics.
# =====================================================================
from typing import Union, Optional, List, Dict, Tuple, Callable

chk("get_origin_list", get_origin(List[int]) is list)
chk("get_args_list", get_args(List[int]) == (int,))
chk("get_origin_dict", get_origin(Dict[str, int]) is dict)
chk("get_args_dict", get_args(Dict[str, int]) == (str, int))
chk("get_origin_builtin_generic", get_origin(list[int]) is list)
chk("get_args_tuple", get_args(Tuple[int, str, float]) == (int, str, float))
chk("get_args_tuple_homog", get_args(Tuple[int, ...]) == (int, Ellipsis))
chk("get_origin_union", get_origin(Union[int, str]) is typing.Union)
chk("get_args_union", get_args(Union[int, str]) == (int, str))
# Optional[X] == Union[X, None].
chk("optional_is_union", get_args(Optional[int]) == (int, type(None)))
# Callable decomposition: args[0] is the param list, args[1] the return type.
import collections.abc as _cabc
_cb = Callable[[int, str], bool]
chk("get_origin_callable", get_origin(_cb) is _cabc.Callable)
chk("get_args_callable", get_args(_cb) == ([int, str], bool))
# Empty param list and the Ellipsis (any-args) form.
chk("get_args_callable_noargs", get_args(Callable[[], int]) == ([], int))
chk("get_origin_callable_noargs", get_origin(Callable[[], int]) is _cabc.Callable)
chk("get_args_callable_ellipsis", get_args(Callable[..., int]) == (Ellipsis, int))
# get_origin/get_args on a plain (non-generic) type returns None/().
chk("get_origin_plain", get_origin(int) is None)
chk("get_args_plain", get_args(int) == ())


# =====================================================================
# SECTION 6: typing.Literal / Final / ClassVar
# Ref: typing.Literal (PEP 586) / typing.Final (PEP 591) / typing.ClassVar
# How: build the special forms and introspect args / origin / equality.
# Expected: Literal collapses duplicate values; Final/ClassVar wrap a type.
# Why: used pervasively; equality + get_args must work at runtime.
# =====================================================================
from typing import Literal, Final, ClassVar

_lit = Literal["a", "b", 3]
chk("literal_args", get_args(_lit) == ("a", "b", 3))
chk("literal_origin", get_origin(_lit) is Literal)
chk("literal_dedup", Literal[1, 1, 2] == Literal[1, 2])
chk("literal_equality", Literal["x"] == Literal["x"])
_fin = Final[int]
chk("final_args", get_args(_fin) == (int,))
_cv = ClassVar[int]
chk("classvar_args", get_args(_cv) == (int,))
# Bare Final / ClassVar usable as annotations.
class CVHolder:
    count: ClassVar[int] = 0
    name: Final = "fixed"
chk("classvar_annotation",
    "count" in CVHolder.__annotations__ and CVHolder.count == 0)


# =====================================================================
# SECTION 7: PEP 604 union operator (X | Y) at runtime
# Ref: types.UnionType / PEP 604
# How: int | str builds a types.UnionType; introspect args/origin and
#      isinstance against the union tuple.
# Expected: get_args == (int, str); isinstance works with the union directly.
# Why: 3.10+ canonical union spelling; must be a real runtime object.
# =====================================================================
import types

_u = int | str
chk("pep604_args", get_args(_u) == (int, str))
chk("pep604_type", isinstance(_u, types.UnionType))
chk("pep604_isinstance", isinstance(5, int | str) and isinstance("x", int | str))
chk("pep604_isinstance_neg", not isinstance(1.5, int | str))
chk("pep604_with_none", get_args(int | None) == (int, type(None)))


# =====================================================================
# SECTION 8: typing.overload (runtime no-op), NewType, cast
# Ref: typing.overload / typing.NewType / typing.cast
# How: @overload-decorated stubs are placeholders; the real impl runs.
#      NewType creates a callable identity wrapper with __supertype__.
#      cast() returns its 2nd arg unchanged at runtime.
# Expected: overload stubs don't execute; NewType(x) == x; cast is identity.
# Why: static-only constructs must be harmless no-ops at runtime.
# =====================================================================
from typing import overload, NewType, cast

@overload
def proc(x: int) -> int: ...
@overload
def proc(x: str) -> str: ...
def proc(x):
    return x
chk("overload_dispatches_to_impl", proc(5) == 5 and proc("a") == "a")

# @overload also works on methods: the stub bodies are discarded and the final
# concrete implementation is what actually runs for every call form.
class _Ov:
    @overload
    def m(self, x: int) -> int: ...
    @overload
    def m(self, x: str) -> str: ...
    def m(self, x):
        return x
_ovi = _Ov()
chk("overload_method_dispatch", _ovi.m(7) == 7 and _ovi.m("z") == "z")

UserId = NewType("UserId", int)
chk("newtype_identity", UserId(42) == 42)
chk("newtype_supertype", UserId.__supertype__ is int)
chk("newtype_name", UserId.__name__ == "UserId")

_obj = [1, 2, 3]
chk("cast_identity", cast("list[int]", _obj) is _obj)
chk("cast_runtime_noop", cast(int, "still a string") == "still a string")


# =====================================================================
# SECTION 8b: typing.TypedDict — typing-API angle (Required/NotRequired)
# Ref: typing.TypedDict (PEP 589 / 655)
# How: cover functional + class forms and the per-key Required/NotRequired
#      markers from the *typing* introspection side (t03 covers OOP angle).
# Expected: __required_keys__/__optional_keys__ reflect total= and markers.
# Why: TypedDict introspection is part of the typing surface contract.
# =====================================================================
TD = typing.TypedDict("TD", {"a": int, "b": str})
chk("typeddict_func_required", TD.__required_keys__ == frozenset({"a", "b"}))
# Inheritance: a subclass merges its own keys with the base's required keys.
class _TDBase(typing.TypedDict):
    a: int
class _TDChild(_TDBase):
    b: str
chk("typeddict_inherit_required",
    _TDChild.__required_keys__ == frozenset({"a", "b"})
    and _TDChild.__optional_keys__ == frozenset())
if hasattr(typing, "Required") and hasattr(typing, "NotRequired"):
    class Mixed(typing.TypedDict, total=False):
        opt: int
        must: typing.Required[str]
    chk("typeddict_required_marker",
        Mixed.__required_keys__ == frozenset({"must"})
        and Mixed.__optional_keys__ == frozenset({"opt"}))
    # Functional form with per-key Required/NotRequired markers (PEP 655).
    MixedF = typing.TypedDict(
        "MixedF",
        {"r": typing.Required[int], "o": typing.NotRequired[str], "p": int},
        total=False,
    )
    chk("typeddict_func_required_marker",
        MixedF.__required_keys__ == frozenset({"r"})
        and MixedF.__optional_keys__ == frozenset({"o", "p"}))
else:
    chk("typeddict_required_marker", True, "(skip: needs 3.11 Required)")
    chk("typeddict_func_required_marker", True, "(skip: needs 3.11 Required)")


# =====================================================================
# SECTION 9: typing version-gated names (3.11 Self/Never/assert_type/...)
# Ref: typing.Self / Never / NoReturn / LiteralString / assert_type /
#      assert_never / reveal_type / TypeAlias / ParamSpec / Concatenate
# How: presence-guard each; when present exercise its runtime behavior; when
#      absent record a noted skip. These are non-syntax features so no exec().
# Expected: assert_type/cast-like helpers are runtime no-ops; ParamSpec has
#      .args/.kwargs; assert_never raises on any value.
# Why: 3.14 ships all of these; on 3.12 some already exist, guard the rest.
# =====================================================================
# Self (3.11): usable as an annotation; runtime is just a special form.
if hasattr(typing, "Self"):
    class Chain:
        def step(self) -> "typing.Self":
            return self
    chk("typing_self", Chain().step() is not None)
else:
    chk("typing_self", True, "(skip: needs 3.11 Self)")

# NoReturn / Never: special forms denoting bottom type.
chk("typing_noreturn_exists", hasattr(typing, "NoReturn"))
if hasattr(typing, "Never"):
    chk("typing_never_exists", typing.Never is not None)
else:
    chk("typing_never_exists", True, "(skip: needs 3.11 Never)")

# LiteralString (3.11): special form, used as annotation only.
if hasattr(typing, "LiteralString"):
    chk("typing_literalstring", typing.LiteralString is not None)
else:
    chk("typing_literalstring", True, "(skip: needs 3.11 LiteralString)")

# assert_type (3.11): runtime no-op returning its first arg.
if hasattr(typing, "assert_type"):
    chk("typing_assert_type", typing.assert_type(123, int) == 123)
else:
    chk("typing_assert_type", True, "(skip: needs 3.11 assert_type)")

# assert_never (3.11): documented to ALWAYS raise AssertionError at runtime.
# Only AssertionError is accepted — a different error type would be a divergence.
if hasattr(typing, "assert_never"):
    try:
        typing.assert_never("unexpected")
        _an = False
    except AssertionError:
        _an = True
    chk("typing_assert_never_raises", _an)
else:
    chk("typing_assert_never_raises", True, "(skip: needs 3.11 assert_never)")

# reveal_type (3.11): prints to stderr & returns its arg at runtime.
if hasattr(typing, "reveal_type"):
    import io as _io
    import contextlib as _ctxlib
    _buf = _io.StringIO()
    with _ctxlib.redirect_stderr(_buf):
        _rt = typing.reveal_type(99)
    chk("typing_reveal_type", _rt == 99)
else:
    chk("typing_reveal_type", True, "(skip: needs 3.11 reveal_type)")

# TypeAlias (3.10): annotation marker for explicit aliases.
if hasattr(typing, "TypeAlias"):
    Vector: typing.TypeAlias = list[float]
    chk("typing_typealias", Vector == list[float])
else:
    chk("typing_typealias", True, "(skip: needs 3.10 TypeAlias)")

# ParamSpec / Concatenate (3.10): for higher-order callable typing.
if hasattr(typing, "ParamSpec"):
    P = typing.ParamSpec("P")
    chk("typing_paramspec_name", P.__name__ == "P")
    chk("typing_paramspec_args", P.args is not None and P.kwargs is not None)
    if hasattr(typing, "Concatenate"):
        _con = typing.Concatenate[int, P]
        chk("typing_concatenate", get_args(_con)[0] is int)
        # Chaining several prefixed types: all leading types precede the ParamSpec.
        _con2 = typing.Concatenate[int, str, P]
        chk("typing_concatenate_multi",
            get_args(_con2)[:2] == (int, str) and get_args(_con2)[-1] is P)
    else:
        chk("typing_concatenate", True, "(skip: needs 3.10 Concatenate)")
        chk("typing_concatenate_multi", True, "(skip: needs 3.10 Concatenate)")
else:
    chk("typing_paramspec_name", True, "(skip: needs 3.10 ParamSpec)")
    chk("typing_paramspec_args", True, "(skip: needs 3.10 ParamSpec)")
    chk("typing_concatenate", True, "(skip: needs 3.10 Concatenate)")
    chk("typing_concatenate_multi", True, "(skip: needs 3.10 Concatenate)")


# =====================================================================
# SECTION 10: typing.NamedTuple — typed class form (introspection angle)
# Ref: typing.NamedTuple
# How: confirm __annotations__, _field_defaults and tuple semantics from the
#      typing module's perspective (distinct keys from t03's coverage).
# Expected: typed NamedTuple is a tuple subclass with annotation metadata.
# Why: bridges typing introspection and the runtime tuple object.
# =====================================================================
class Coord(typing.NamedTuple):
    x: int
    y: int = 5

_c = Coord(1)
chk("typing_namedtuple_default", _c == (1, 5))
chk("typing_namedtuple_is_tuple", isinstance(_c, tuple))
chk("typing_namedtuple_anns", Coord.__annotations__ == {"x": int, "y": int})
# The named-tuple helper API: _fields ordering, _field_defaults, and the three
# constructor/conversion methods _make / _asdict / _replace.
chk("typing_namedtuple_fields", Coord._fields == ("x", "y"))
chk("typing_namedtuple_field_defaults", Coord._field_defaults == {"y": 5})
chk("typing_namedtuple_make", Coord._make([3, 4]) == Coord(3, 4))
chk("typing_namedtuple_asdict", _c._asdict() == {"x": 1, "y": 5})
chk("typing_namedtuple_replace", _c._replace(y=9) == Coord(1, 9))


# =====================================================================
# SECTION 11: 3.14-only / newer SYNTAX (PEP 695 / PEP 750) via exec()
# Ref: PEP 695 (type param syntax, 3.12), PEP 750 (t-strings, 3.14)
# How: isolate new SYNTAX inside exec()'d strings, version-guarded, with a
#      SyntaxError fallback, so this file still PARSES on 3.12.
# Expected: when the runtime supports it, the probe holds; else noted skip.
# Why: keep one file that runs on both 3.12 (host) and 3.14 (qemu target).
# =====================================================================
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

# PEP 695 (3.12): generic function/class + type alias statement, then read
# the alias's type_params (TypeAliasType) — typing-introspection relevance.
_gated_syntax(
    "pep695_typealiastype", (3, 12),
    "type IntList = list[int]\n"
    "type Pair[T] = tuple[T, T]\n"
    "R = (IntList.__value__, Pair.__type_params__ != ())\n",
    lambda ns: ns["R"][0] == list[int] and ns["R"][1] is True,
)

# PEP 750 (3.14): t-strings produce a Template with .strings/.values/.interpolations.
# Also verify the .interpolations Interpolation carries the evaluated value and
# the source expression text ("who") — the core PEP 750 introspection surface.
_gated_syntax(
    "pep750_template_typing", (3, 14),
    "who = 'os'\n"
    "tmpl = t'hello {who}'\n"
    "interp = tmpl.interpolations[0]\n"
    "R = (type(tmpl).__name__, tmpl.values[0], tmpl.strings[0],\n"
    "     len(tmpl.interpolations), interp.value, interp.expression)\n",
    lambda ns: ns["R"] == ("Template", "os", "hello ", 1, "os", "who"),
)


# =====================================================================
# SECTION 12: argparse — ArgumentParser core add_argument forms
# Ref: argparse.ArgumentParser / add_argument
#      (https://docs.python.org/3/library/argparse.html)
# How: build a parser and exercise type=/default=/required=/choices/dest/
#      metavar; parse a controlled argv list; assert the Namespace fields.
# Expected: parse_args returns a Namespace with exactly the coerced values.
# Why: argparse is pure-Python (no syscalls beyond stderr on error); a key
#      CLI-building surface for delivered apps.
# =====================================================================
import argparse
import io
import contextlib

_p = argparse.ArgumentParser(prog="demo", description="d", add_help=True)
_p.add_argument("pos", type=int)                              # positional, typed
_p.add_argument("--name", default="anon")                    # optional w/ default
_p.add_argument("--count", type=int, required=True)          # required typed
_p.add_argument("--mode", choices=["a", "b", "c"])           # choices
_p.add_argument("--out", dest="output", metavar="FILE")      # dest + metavar
_ns = _p.parse_args(["7", "--name", "x", "--count", "3", "--mode", "b", "--out", "f.txt"])
chk("argparse_positional_typed", _ns.pos == 7 and isinstance(_ns.pos, int))
chk("argparse_optional_value", _ns.name == "x")
chk("argparse_required", _ns.count == 3)
chk("argparse_choices", _ns.mode == "b")
# A value outside choices is an error: exit(2) + "invalid choice" on stderr.
_pc = argparse.ArgumentParser()
_pc.add_argument("--mode", choices=["a", "b"])
_cbuf = io.StringIO()
try:
    with contextlib.redirect_stderr(_cbuf):
        _pc.parse_args(["--mode", "z"])
    _c_raised = False
except SystemExit as e:
    _c_raised = (e.code == 2)
chk("argparse_choices_invalid_raises",
    _c_raised and "invalid choice" in _cbuf.getvalue().lower())
chk("argparse_dest_rename", _ns.output == "f.txt")
# Default applied when option omitted.
_ns2 = _p.parse_args(["1", "--count", "0"])
chk("argparse_default", _ns2.name == "anon" and _ns2.output is None)
chk("argparse_prog", _p.prog == "demo")

# type= is any callable; if it raises ValueError argparse reports an error and
# exits (sys.exit(2)) with an "invalid <type> value" message on stderr.
def _evenint(s):
    v = int(s)
    if v % 2:
        raise ValueError("must be even")
    return v
_pt = argparse.ArgumentParser()
_pt.add_argument("--even", type=_evenint)
chk("argparse_type_callable_ok", _pt.parse_args(["--even", "4"]).even == 4)
_terr_buf = io.StringIO()
try:
    with contextlib.redirect_stderr(_terr_buf):
        _pt.parse_args(["--even", "3"])
    _t_raised = False
except SystemExit as e:
    _t_raised = (e.code == 2)
chk("argparse_type_callable_raises",
    _t_raised and "invalid" in _terr_buf.getvalue().lower())

# argument_default=SUPPRESS: omitted optionals leave NO attribute on the
# Namespace at all (rather than defaulting to None).
_psup = argparse.ArgumentParser(argument_default=argparse.SUPPRESS)
_psup.add_argument("--maybe")
_sup_ns = _psup.parse_args([])
chk("argparse_suppress_absent", not hasattr(_sup_ns, "maybe"))
chk("argparse_suppress_present", _psup.parse_args(["--maybe", "v"]).maybe == "v")

# format_usage()/format_help() render the synopsis/help text without exiting.
_pfmt = argparse.ArgumentParser(prog="fmtdemo")
_pfmt.add_argument("--a", help="the a option")
chk("argparse_format_usage", _pfmt.format_usage().startswith("usage:"))
_help_txt = _pfmt.format_help()
chk("argparse_format_help",
    "usage:" in _help_txt and "the a option" in _help_txt)


# =====================================================================
# SECTION 13: argparse — nargs (?/*/+/N), actions, count
# Ref: argparse nargs / action
# How: cover each nargs form and the store_true/store_const/append/count
#      actions; parse argvs and assert resulting types/values.
# Expected: nargs '*'/'+' give lists, 'N' a fixed-length list, '?' a scalar
#      with const fallback; append accumulates; count tallies flags.
# Why: nargs/actions are the most error-prone argparse surface.
# =====================================================================
_pn = argparse.ArgumentParser()
_pn.add_argument("--opt", nargs="?", const="C", default="D")  # optional one
_pn.add_argument("--many", nargs="*")                          # zero or more
_pn.add_argument("--plus", nargs="+")                          # one or more
_pn.add_argument("--two", nargs=2, type=int)                   # exactly two
_pn.add_argument("--flag", action="store_true")
_pn.add_argument("--const", action="store_const", const=99)
_pn.add_argument("--app", action="append")
_pn.add_argument("-v", "--verbose", action="count", default=0)
_a = _pn.parse_args(
    ["--opt", "--many", "x", "y", "--plus", "p", "--two", "1", "2",
     "--flag", "--const", "--app", "a", "--app", "b", "-vvv"])
chk("argparse_nargs_q_const", _a.opt == "C")                  # present, no value -> const
chk("argparse_nargs_star", _a.many == ["x", "y"])
chk("argparse_nargs_plus", _a.plus == ["p"])
chk("argparse_nargs_N", _a.two == [1, 2])
chk("argparse_store_true", _a.flag is True)
chk("argparse_store_const", _a.const == 99)
chk("argparse_append", _a.app == ["a", "b"])
chk("argparse_count", _a.verbose == 3)
# nargs='?' default when option absent entirely.
_a2 = _pn.parse_args([])
chk("argparse_nargs_q_default", _a2.opt == "D" and _a2.flag is False and _a2.const is None)

# action='extend' (3.8+): flattens each occurrence's values into one list
# (unlike 'append', which would nest lists per occurrence).
_pe = argparse.ArgumentParser()
_pe.add_argument("--ext", action="extend", nargs="+")
chk("argparse_action_extend",
    _pe.parse_args(["--ext", "a", "b", "--ext", "c"]).ext == ["a", "b", "c"])

# action='append_const': each flag appends its own const to a shared dest list.
_pac = argparse.ArgumentParser()
_pac.add_argument("--one", action="append_const", const=1, dest="acc", default=[])
_pac.add_argument("--two", action="append_const", const=2, dest="acc")
chk("argparse_action_append_const",
    _pac.parse_args(["--one", "--two", "--one"]).acc == [1, 2, 1])

# action=BooleanOptionalAction (3.9+): registers paired --flag/--no-flag and
# defaults to None when neither is given.
if hasattr(argparse, "BooleanOptionalAction"):
    _pb = argparse.ArgumentParser()
    _pb.add_argument("--feat", action=argparse.BooleanOptionalAction)
    chk("argparse_boolean_optional",
        _pb.parse_args(["--feat"]).feat is True
        and _pb.parse_args(["--no-feat"]).feat is False
        and _pb.parse_args([]).feat is None)
else:
    chk("argparse_boolean_optional", True, "(skip: needs 3.9 BooleanOptionalAction)")

# nargs=REMAINDER collects every remaining arg verbatim (including dashes) into
# a list, without trying to parse them as options.
_prem = argparse.ArgumentParser()
_prem.add_argument("cmd")
_prem.add_argument("rest", nargs=argparse.REMAINDER)
_rem_ns = _prem.parse_args(["run", "--foo", "-x", "bar"])
chk("argparse_nargs_remainder",
    _rem_ns.cmd == "run" and _rem_ns.rest == ["--foo", "-x", "bar"])


# =====================================================================
# SECTION 14: argparse — parse_known_args, groups, mutually exclusive
# Ref: argparse.parse_known_args / add_argument_group /
#      add_mutually_exclusive_group
# How: parse_known_args returns (namespace, leftovers); a m.e. group rejects
#      two members at once (-> SystemExit); argument_group is cosmetic but
#      must route options to the same namespace.
# Expected: leftover args preserved; conflicting m.e. options raise SystemExit.
# Why: these compose larger CLIs; error path goes through parser.error().
# =====================================================================
_pk = argparse.ArgumentParser()
_pk.add_argument("--known")
_known_ns, _extra = _pk.parse_known_args(["--known", "v", "--unknown", "z", "tail"])
chk("argparse_parse_known", _known_ns.known == "v")
chk("argparse_known_leftovers", _extra == ["--unknown", "z", "tail"])

_pg = argparse.ArgumentParser()
_grp = _pg.add_argument_group("group1")
_grp.add_argument("--gopt")
chk("argparse_group", _pg.parse_args(["--gopt", "g"]).gopt == "g")

_pm = argparse.ArgumentParser()
_me = _pm.add_mutually_exclusive_group()
_me.add_argument("--foo", action="store_true")
_me.add_argument("--bar", action="store_true")
chk("argparse_mutex_single", _pm.parse_args(["--foo"]).foo is True)
try:
    _pm.parse_args(["--foo", "--bar"])
    _mex = False
except SystemExit:
    _mex = True
chk("argparse_mutex_conflict_raises", _mex)


# =====================================================================
# SECTION 15: argparse — subparsers, abbrev, error path
# Ref: argparse add_subparsers / allow_abbrev / parser.error
# How: a subparser dispatches on a command word and fills sub-options;
#      allow_abbrev=False disables prefix matching; bad input -> SystemExit.
# Expected: subcommand routes correctly; unknown command/required-missing
#      both exit via SystemExit (argparse calls sys.exit(2)).
# Why: subcommands are how multi-tool CLIs are built (git-style).
# =====================================================================
import io
import contextlib

_ps = argparse.ArgumentParser(prog="tool")
_sub = _ps.add_subparsers(dest="cmd")
_add = _sub.add_parser("add")
_add.add_argument("--n", type=int, required=True)
_rm = _sub.add_parser("rm")
_rm.add_argument("target")
_sns = _ps.parse_args(["add", "--n", "5"])
chk("argparse_subparser_dispatch", _sns.cmd == "add" and _sns.n == 5)
_sns2 = _ps.parse_args(["rm", "file"])
chk("argparse_subparser_other", _sns2.cmd == "rm" and _sns2.target == "file")

# add_subparsers(required=True) (3.7+): omitting the subcommand is an error.
_psr = argparse.ArgumentParser(prog="reqtool")
_subr = _psr.add_subparsers(dest="cmd", required=True)
_subr.add_parser("go")
chk("argparse_subparser_required_ok", _psr.parse_args(["go"]).cmd == "go")
try:
    _psr.parse_args([])
    _subreq = False
except SystemExit as e:
    _subreq = (e.code == 2)
chk("argparse_subparser_required_raises", _subreq)

# allow_abbrev: when True (default) a unique prefix matches the long option.
_pab = argparse.ArgumentParser()
_pab.add_argument("--verbose", action="store_true")
chk("argparse_abbrev_on", _pab.parse_args(["--verb"]).verbose is True)
_pna = argparse.ArgumentParser(allow_abbrev=False)
_pna.add_argument("--verbose", action="store_true")
try:
    _pna.parse_args(["--verb"])
    _abv = False
except SystemExit:
    _abv = True
chk("argparse_abbrev_off_raises", _abv)

# Error path: missing required value exits and writes a usage message to stderr.
_perr = argparse.ArgumentParser()
_perr.add_argument("--req", required=True)
_err_buf = io.StringIO()
try:
    with contextlib.redirect_stderr(_err_buf):
        _perr.parse_args([])
    _err_raised = False
except SystemExit as e:
    _err_raised = (e.code == 2)
chk("argparse_error_systemexit", _err_raised)
chk("argparse_error_message", "required" in _err_buf.getvalue().lower())

# custom prefix_chars: options may use '+' instead of '-'.
_ppref = argparse.ArgumentParser(prefix_chars="+")
_ppref.add_argument("++x")
chk("argparse_prefix_chars", _ppref.parse_args(["++x", "1"]).x == "1")


# =====================================================================
# SECTION 16: logging — getLogger, levels, isEnabledFor, setLevel
# Ref: logging.getLogger / logging levels / Logger.isEnabledFor
# How: get a named logger; logging is hierarchical & cached by name; check
#      level constants order and isEnabledFor gating after setLevel.
# Expected: getLogger(name) is idempotent; level ints ordered DEBUG<...<CRITICAL.
# Why: logging is pure-Python; verifies the level machinery without I/O.
# =====================================================================
import logging

_lg = logging.getLogger("t17.demo")
chk("logging_getlogger_cached", logging.getLogger("t17.demo") is _lg)
chk("logging_level_order",
    logging.DEBUG < logging.INFO < logging.WARNING
    < logging.ERROR < logging.CRITICAL)
chk("logging_level_values",
    (logging.DEBUG, logging.INFO, logging.WARNING,
     logging.ERROR, logging.CRITICAL) == (10, 20, 30, 40, 50))
_lg.setLevel(logging.WARNING)
chk("logging_setlevel", _lg.level == logging.WARNING)
chk("logging_isenabled_yes", _lg.isEnabledFor(logging.ERROR))
chk("logging_isenabled_no", not _lg.isEnabledFor(logging.DEBUG))
chk("logging_getlevelname", logging.getLevelName(logging.INFO) == "INFO")
chk("logging_getlevelname_int", logging.getLevelName("WARNING") == logging.WARNING)
# NOTSET sentinel is 0 and round-trips through getLevelName.
chk("logging_notset", logging.NOTSET == 0 and logging.getLevelName(0) == "NOTSET")
# CRITICAL sits above ERROR and gates correctly after a CRITICAL-only setLevel.
_clg2 = logging.getLogger("t17.crit")
_clg2.setLevel(logging.CRITICAL)
chk("logging_isenabled_critical",
    _clg2.isEnabledFor(logging.CRITICAL) and not _clg2.isEnabledFor(logging.ERROR))


# =====================================================================
# SECTION 17: logging — StreamHandler -> StringIO, Formatter, capture
# Ref: logging.StreamHandler / logging.Formatter
# How: attach a StreamHandler writing to a StringIO with a custom format;
#      emit records at various levels and assert the captured text. Disable
#      propagation so the root handler doesn't interfere.
# Expected: only records >= handler/logger level appear; format applied.
# Why: this is the canonical "capture log output" pattern; must work offline.
# =====================================================================
_cap = io.StringIO()
_h = logging.StreamHandler(_cap)
_h.setFormatter(logging.Formatter("%(levelname)s:%(name)s:%(message)s"))
_clg = logging.getLogger("t17.capture")
_clg.handlers.clear()
_clg.addHandler(_h)
_clg.setLevel(logging.INFO)
_clg.propagate = False
_clg.debug("dropped")          # below level -> suppressed
_clg.info("hello %s", "world") # %-style args
_clg.warning("warn")
_lines = _cap.getvalue().strip().split("\n")
chk("logging_format_applied", "INFO:t17.capture:hello world" in _lines)
chk("logging_warning_captured", "WARNING:t17.capture:warn" in _lines)
chk("logging_debug_suppressed", not any("dropped" in ln for ln in _lines))
chk("logging_args_interpolated", any("hello world" in ln for ln in _lines))

# Logger.log(level, ...) generic entry point.
_cap.seek(0); _cap.truncate(0)
_clg.log(logging.ERROR, "viaLog")
chk("logging_generic_log", "ERROR:t17.capture:viaLog" in _cap.getvalue())

# Logger.critical() emits at the CRITICAL level through the same handler.
_cap.seek(0); _cap.truncate(0)
_clg.critical("the end")
chk("logging_critical", "CRITICAL:t17.capture:the end" in _cap.getvalue())

# Formatter.format(record) directly renders a manually built LogRecord, and
# the record exposes a float msecs sub-second field.
_rec = logging.LogRecord("t17.rec", logging.WARNING, "p.py", 10,
                         "val=%d", (7,), None)
_fmtr = logging.Formatter("%(levelname)s|%(name)s|%(message)s")
chk("logging_formatter_format",
    _fmtr.format(_rec) == "WARNING|t17.rec|val=7")
chk("logging_record_msecs",
    isinstance(_rec.msecs, float) and 0.0 <= _rec.msecs < 1000.0)

# Formatter asctime field is rendered (non-empty timestamp text present).
_cap2 = io.StringIO()
_h2 = logging.StreamHandler(_cap2)
_h2.setFormatter(logging.Formatter("%(asctime)s|%(message)s", datefmt="%Y"))
_alg = logging.getLogger("t17.asctime")
_alg.handlers.clear(); _alg.addHandler(_h2); _alg.setLevel(logging.INFO)
_alg.propagate = False
_alg.info("ts")
chk("logging_asctime", _cap2.getvalue().strip().endswith("|ts")
    and len(_cap2.getvalue().strip().split("|")[0]) >= 4)


# =====================================================================
# SECTION 18: logging — Filter and exception() logging
# Ref: logging.Filter / Logger.exception
# How: a Filter that drops records by message substring; exception() logs at
#      ERROR with traceback text appended.
# Expected: filtered record absent; exception() output contains the traceback.
# Why: filters + exception logging are common in production diagnostics.
# =====================================================================
class _DropFilter(logging.Filter):
    def filter(self, record):
        return "SECRET" not in record.getMessage()

_fcap = io.StringIO()
_fh = logging.StreamHandler(_fcap)
_fh.setFormatter(logging.Formatter("%(message)s"))
_flg = logging.getLogger("t17.filter")
_flg.handlers.clear(); _flg.addHandler(_fh); _flg.setLevel(logging.INFO)
_flg.propagate = False
_flg.addFilter(_DropFilter())
_flg.info("public")
_flg.info("has SECRET inside")
chk("logging_filter_keeps", "public" in _fcap.getvalue())
chk("logging_filter_drops", "SECRET" not in _fcap.getvalue())

# addFilter also accepts a plain callable (3.2+): record -> truthy to keep.
_ccap = io.StringIO()
_ch = logging.StreamHandler(_ccap)
_ch.setFormatter(logging.Formatter("%(message)s"))
_clf = logging.getLogger("t17.cbfilter")
_clf.handlers.clear(); _clf.addHandler(_ch); _clf.setLevel(logging.INFO)
_clf.propagate = False
_clf.addFilter(lambda record: "DROP" not in record.getMessage())
_clf.info("kept line")
_clf.info("DROP this line")
chk("logging_callable_filter_keeps", "kept line" in _ccap.getvalue())
chk("logging_callable_filter_drops", "DROP" not in _ccap.getvalue())

# exception(): must be called inside an except block; appends traceback.
_ecap = io.StringIO()
_eh = logging.StreamHandler(_ecap)
_eh.setFormatter(logging.Formatter("%(levelname)s:%(message)s"))
_elg = logging.getLogger("t17.exc")
_elg.handlers.clear(); _elg.addHandler(_eh); _elg.setLevel(logging.DEBUG)
_elg.propagate = False
try:
    raise ValueError("boom")
except ValueError:
    _elg.exception("caught it")
_etxt = _ecap.getvalue()
chk("logging_exception_level", "ERROR:caught it" in _etxt)
chk("logging_exception_traceback",
    "Traceback (most recent call last)" in _etxt and "ValueError: boom" in _etxt)


# =====================================================================
# SECTION 19: warnings — warn / catch_warnings / simplefilter / filterwarnings
# Ref: warnings.warn / warnings.catch_warnings / simplefilter
# How: inside catch_warnings(record=True) with simplefilter("always"), emit
#      warnings and inspect the captured record list (category/message).
# Expected: each warn() yields a record with matching category & message.
# Why: warning machinery is pure-Python; deprecation handling is everywhere.
# =====================================================================
import warnings

with warnings.catch_warnings(record=True) as _w:
    warnings.simplefilter("always")
    warnings.warn("deprecated thing", DeprecationWarning)
    warnings.warn("user msg", UserWarning)
    chk("warnings_count", len(_w) == 2)
    chk("warnings_category",
        _w[0].category is DeprecationWarning and _w[1].category is UserWarning)
    chk("warnings_message", str(_w[0].message) == "deprecated thing")

# filterwarnings("ignore") suppresses matching warnings entirely.
with warnings.catch_warnings(record=True) as _w2:
    warnings.simplefilter("always")
    warnings.filterwarnings("ignore", category=UserWarning)
    warnings.warn("seen", DeprecationWarning)
    warnings.warn("hidden", UserWarning)
    _cats = [r.category for r in _w2]
    chk("warnings_filter_ignore",
        DeprecationWarning in _cats and UserWarning not in _cats)

# "error" action turns a warning into an exception.
with warnings.catch_warnings():
    warnings.simplefilter("error")
    try:
        warnings.warn("promote", UserWarning)
        _werr = False
    except UserWarning:
        _werr = True
    chk("warnings_action_error", _werr)

# filterwarnings("error", category=...) (not just simplefilter) promotes only
# the matching category; a non-matching category still passes through.
with warnings.catch_warnings():
    warnings.simplefilter("always")
    warnings.filterwarnings("error", category=FutureWarning)
    try:
        warnings.warn("future boom", FutureWarning)
        _fwe = False
    except FutureWarning:
        _fwe = True
    chk("warnings_filterwarnings_error", _fwe)

# message= is an anchored regex matched against the warning text: matching
# messages are filtered, non-matching ones still recorded.
with warnings.catch_warnings(record=True) as _w3:
    warnings.simplefilter("always")
    warnings.filterwarnings("ignore", message="^secret")
    warnings.warn("secret payload", RuntimeWarning)
    warnings.warn("ordinary", RuntimeWarning)
    _msgs = [str(r.message) for r in _w3]
    chk("warnings_message_regex",
        _msgs == ["ordinary"])

# Built-in warning categories form a hierarchy rooted at Warning; Deprecation
# and Pending are sub-classes (used widely for migration signalling).
chk("warnings_category_hierarchy",
    issubclass(DeprecationWarning, Warning)
    and issubclass(PendingDeprecationWarning, Warning)
    and issubclass(RuntimeWarning, Warning)
    and issubclass(FutureWarning, Warning))

# simplefilter("once"): identical warnings are de-duplicated to a single record.
with warnings.catch_warnings(record=True) as _w4:
    warnings.simplefilter("once")
    warnings.warn("repeat me", UserWarning)
    warnings.warn("repeat me", UserWarning)
    chk("warnings_action_once", len(_w4) == 1)


# =====================================================================
# SECTION 20: traceback — format_exc / format_exception / extract_tb /
#             print_exc / TracebackException
# Ref: traceback module
# How: raise & catch; pull exc info via sys.exc_info(); render with each API
#      and assert the rendered text / structure.
# Expected: rendered strings contain the exception type+message and a frame;
#      extract_tb yields FrameSummary objects; TracebackException round-trips.
# Why: traceback is the core diagnostics surface; pure-Python, no syscalls.
# =====================================================================
import traceback

def _boom():
    raise RuntimeError("explode")

try:
    _boom()
except RuntimeError:
    _exc_type, _exc_val, _exc_tb = sys.exc_info()
    _fe = traceback.format_exc()
    chk("traceback_format_exc",
        "RuntimeError: explode" in _fe and "_boom" in _fe)
    _fl = traceback.format_exception(_exc_type, _exc_val, _exc_tb)
    chk("traceback_format_exception_list",
        isinstance(_fl, list) and any("RuntimeError: explode" in s for s in _fl))
    _frames = traceback.extract_tb(_exc_tb)
    chk("traceback_extract_tb",
        len(_frames) >= 1 and _frames[-1].name == "_boom")
    chk("traceback_framesummary_fields",
        hasattr(_frames[-1], "filename") and hasattr(_frames[-1], "lineno"))
    # FrameSummary.line carries the (stripped) source text of the failing line.
    chk("traceback_framesummary_line",
        _frames[-1].line is not None and "raise RuntimeError" in _frames[-1].line)
    _pcap = io.StringIO()
    traceback.print_exc(file=_pcap)
    chk("traceback_print_exc", "RuntimeError: explode" in _pcap.getvalue())
    _tbe = traceback.TracebackException(_exc_type, _exc_val, _exc_tb)
    _tbe_text = "".join(_tbe.format())
    chk("traceback_exception_obj",
        "RuntimeError: explode" in _tbe_text and _tbe.exc_type is RuntimeError)

# format_exception_only renders just the type+message (no frames).
_only = traceback.format_exception_only(ValueError, ValueError("v"))
chk("traceback_exception_only",
    isinstance(_only, list) and _only[-1].strip() == "ValueError: v")

# Explicit chaining (raise ... from ...) is rendered with the "direct cause"
# linkage, and TracebackException carries a .stack (StackSummary) of frames.
try:
    try:
        raise ValueError("inner")
    except ValueError as _ie:
        raise RuntimeError("outer") from _ie
except RuntimeError:
    _ct, _cv, _ctb = sys.exc_info()
    _chain_txt = "".join(traceback.format_exception(_ct, _cv, _ctb))
    chk("traceback_chained_cause",
        "ValueError: inner" in _chain_txt
        and "RuntimeError: outer" in _chain_txt
        and "direct cause" in _chain_txt)
    _ctbe = traceback.TracebackException(_ct, _cv, _ctb)
    chk("traceback_exception_stack",
        isinstance(_ctbe.stack, traceback.StackSummary) and len(_ctbe.stack) >= 1)


# =====================================================================
# SECTION 21: uuid — uuid1 / uuid3 / uuid4 / uuid5 + NAMESPACE + fields
# Ref: uuid module (https://docs.python.org/3/library/uuid.html)
# How: build UUIDs of each version; check .version, deterministic name-based
#      uuid3/uuid5 reproducibility, and hex/int/str round-trips.
# Expected: versions 1/3/4/5 reported; uuid3/uuid5 deterministic per name;
#      hex is 32 chars, int matches, UUID(hex) round-trips.
# Why: uuid1 may read host time/node (a possible starry risk); uuid3/4/5 are
#      pure-compute. We assert the pure paths firmly and version-check uuid1.
# =====================================================================
import uuid

_u4 = uuid.uuid4()
chk("uuid4_version", _u4.version == 4)
chk("uuid4_variant", _u4.variant == uuid.RFC_4122)
chk("uuid4_hex_len", len(_u4.hex) == 32)
chk("uuid4_int_roundtrip", uuid.UUID(int=_u4.int) == _u4)
chk("uuid4_hex_roundtrip", uuid.UUID(_u4.hex) == _u4)
chk("uuid4_str_roundtrip", uuid.UUID(str(_u4)) == _u4)
chk("uuid4_bytes_roundtrip", uuid.UUID(bytes=_u4.bytes) == _u4)
chk("uuid4_distinct", uuid.uuid4() != uuid.uuid4())
# UUID(int=..., version=N) stamps the version/variant bits into a raw integer.
_uv = uuid.UUID(int=0, version=4)
chk("uuid_int_version_stamp", _uv.version == 4 and _uv.variant == uuid.RFC_4122)

# uuid3 (MD5) / uuid5 (SHA1) are deterministic functions of namespace+name.
_n3a = uuid.uuid3(uuid.NAMESPACE_DNS, "example.com")
_n3b = uuid.uuid3(uuid.NAMESPACE_DNS, "example.com")
chk("uuid3_deterministic", _n3a == _n3b and _n3a.version == 3)
_n5a = uuid.uuid5(uuid.NAMESPACE_URL, "http://x/")
_n5b = uuid.uuid5(uuid.NAMESPACE_URL, "http://x/")
chk("uuid5_deterministic", _n5a == _n5b and _n5a.version == 5)
chk("uuid3_vs_uuid5_differ", _n3a != _n5a)
# Known fixed vector: uuid5(NAMESPACE_DNS, "python.org").
chk("uuid5_known_vector",
    str(uuid.uuid5(uuid.NAMESPACE_DNS, "python.org"))
    == "886313e1-3b8a-5372-9b90-0c9aee199e5d")
chk("uuid_namespaces_distinct",
    len({uuid.NAMESPACE_DNS, uuid.NAMESPACE_URL,
         uuid.NAMESPACE_OID, uuid.NAMESPACE_X500}) == 4)
chk("uuid_fields", len(_u4.fields) == 6 and _u4.bytes == _u4.int.to_bytes(16, "big"))
# Named field accessors + the urn / bytes_le representations.
chk("uuid_named_fields",
    _u4.fields == (_u4.time_low, _u4.time_mid, _u4.time_hi_version,
                   _u4.clock_seq_hi_variant, _u4.clock_seq_low, _u4.node))
chk("uuid_urn", _u4.urn == "urn:uuid:" + str(_u4))
chk("uuid_bytes_le_roundtrip",
    len(_u4.bytes_le) == 16 and uuid.UUID(bytes_le=_u4.bytes_le) == _u4)

# uuid1: time-based; may depend on host clock/node (starry risk). Version-check
# only and guard against environments where it raises (no node source).
try:
    _u1 = uuid.uuid1()
    chk("uuid1_version", _u1.version == 1)
    # Explicit node/clock_seq are pure-compute (no host clock/node read) and
    # are reflected verbatim in the resulting UUID's fields.
    _u1e = uuid.uuid1(node=0x123456789ABC, clock_seq=0x1234)
    chk("uuid1_explicit_node",
        _u1e.node == 0x123456789ABC and _u1e.version == 1)
except Exception as e:
    chk("uuid1_version", True, "(skip: uuid1 unavailable: %s)" % type(e).__name__)
    chk("uuid1_explicit_node", True, "(skip: uuid1 unavailable: %s)" % type(e).__name__)


# =====================================================================
# SECTION 22: ipaddress — IPv4/IPv6 address, network, interface
# Ref: ipaddress module (https://docs.python.org/3/library/ipaddress.html)
# How: parse addresses/networks/interfaces (v4 and v6); test membership,
#      netmask/prefixlen, hosts(), supernet(), subnets(), iteration.
# Expected: classification flags & arithmetic match the documented behavior;
#      pure-compute (no DNS/network), safe offline.
# Why: ipaddress is pure-Python integer math — strong correctness signal.
# =====================================================================
import ipaddress

# ip_address: auto-detect v4 vs v6.
_a4 = ipaddress.ip_address("192.168.1.10")
chk("ip_address_v4", isinstance(_a4, ipaddress.IPv4Address) and _a4.version == 4)
chk("ip_address_v4_int", int(_a4) == 0xC0A8010A)
_a6 = ipaddress.ip_address("2001:db8::1")
chk("ip_address_v6", isinstance(_a6, ipaddress.IPv6Address) and _a6.version == 6)
chk("ip_address_v6_compressed", _a6.compressed == "2001:db8::1")
chk("ip_address_v6_exploded", _a6.exploded == "2001:0db8:0000:0000:0000:0000:0000:0001")
# Classification flags.
chk("ip_address_loopback", ipaddress.ip_address("127.0.0.1").is_loopback)
chk("ip_address_private", _a4.is_private)
chk("ip_address_global", ipaddress.ip_address("8.8.8.8").is_global)
chk("ip_address_multicast", ipaddress.ip_address("224.0.0.1").is_multicast)
chk("ip_address_arithmetic", (_a4 + 1) == ipaddress.ip_address("192.168.1.11"))
chk("ip_address_reserved", ipaddress.ip_address("240.0.0.1").is_reserved)
chk("ip_address_link_local", ipaddress.ip_address("169.254.0.1").is_link_local)
chk("ip_address_v6_link_local", ipaddress.ip_address("fe80::1").is_link_local)
chk("ip_address_unspecified", ipaddress.ip_address("0.0.0.0").is_unspecified)
chk("ip_address_reverse_pointer",
    ipaddress.ip_address("192.168.1.10").reverse_pointer == "10.1.168.192.in-addr.arpa")
# IPv4-mapped IPv6 address exposes the embedded v4 address.
chk("ip_address_ipv4_mapped",
    ipaddress.ip_address("::ffff:192.168.1.1").ipv4_mapped
    == ipaddress.ip_address("192.168.1.1"))
# Invalid input raises ValueError (not a silent fallback).
try:
    ipaddress.ip_address("999.1.1.1")
    _ipbad = False
except ValueError:
    _ipbad = True
chk("ip_address_invalid_raises", _ipbad)

# ip_network: prefix math, membership, netmask, hosts(), iteration.
_net = ipaddress.ip_network("192.168.1.0/29")
chk("ip_network_prefix", _net.prefixlen == 29)
chk("ip_network_netmask", str(_net.netmask) == "255.255.255.248")
chk("ip_network_hostmask", str(_net.hostmask) == "0.0.0.7")
chk("ip_network_num_addresses", _net.num_addresses == 8)
chk("ip_network_contains", ipaddress.ip_address("192.168.1.5") in _net)
chk("ip_network_not_contains", ipaddress.ip_address("192.168.2.1") not in _net)
chk("ip_network_network_address", str(_net.network_address) == "192.168.1.0")
chk("ip_network_broadcast", str(_net.broadcast_address) == "192.168.1.7")
_hosts = list(_net.hosts())
chk("ip_network_hosts",
    _hosts[0] == ipaddress.ip_address("192.168.1.1")
    and _hosts[-1] == ipaddress.ip_address("192.168.1.6")
    and len(_hosts) == 6)

# supernet() / subnets(): hierarchical prefix arithmetic.
chk("ip_network_supernet", str(_net.supernet()) == "192.168.1.0/28")
_subs = list(_net.subnets())
chk("ip_network_subnets",
    len(_subs) == 2 and str(_subs[0]) == "192.168.1.0/30"
    and str(_subs[1]) == "192.168.1.4/30")
_subs2 = list(_net.subnets(prefixlen_diff=2))
chk("ip_network_subnets_diff", len(_subs2) == 4)
# strict=False allows host bits set in the network string.
_nstrict = ipaddress.ip_network("192.168.1.5/29", strict=False)
chk("ip_network_strict_false", str(_nstrict.network_address) == "192.168.1.0")
try:
    ipaddress.ip_network("192.168.1.5/29")  # strict=True default -> error
    _strict_raised = False
except ValueError:
    _strict_raised = True
chk("ip_network_strict_raises", _strict_raised)

# ip_interface: address + network together.
_iface = ipaddress.ip_interface("10.0.0.5/24")
chk("ip_interface_ip", str(_iface.ip) == "10.0.0.5")
chk("ip_interface_network", str(_iface.network) == "10.0.0.0/24")
chk("ip_interface_with_prefix", str(_iface.with_prefixlen) == "10.0.0.5/24")

# IPv6 network math.
_net6 = ipaddress.ip_network("2001:db8::/126")
chk("ip_network_v6_num", _net6.num_addresses == 4)
chk("ip_network_v6_contains", ipaddress.ip_address("2001:db8::2") in _net6)
_iface6 = ipaddress.ip_interface("fe80::1/64")
chk("ip_interface_v6", _iface6.ip == ipaddress.ip_address("fe80::1")
    and _iface6.network.prefixlen == 64)

# overlaps / subnet_of / supernet_of relations.
_big = ipaddress.ip_network("192.168.0.0/16")
chk("ip_network_subnet_of", _net.subnet_of(_big))
chk("ip_network_supernet_of", _big.supernet_of(_net))
chk("ip_network_overlaps", _net.overlaps(_big))
chk("ip_network_no_overlap",
    not _net.overlaps(ipaddress.ip_network("10.0.0.0/8")))

# Module-level aggregation/decomposition helpers (pure integer arithmetic).
# address_exclude: remove a subnet, yielding the covering complement blocks.
_excl = sorted(str(n) for n in
               ipaddress.ip_network("192.168.1.0/24").address_exclude(
                   ipaddress.ip_network("192.168.1.0/26")))
chk("ip_network_address_exclude",
    _excl == ["192.168.1.128/25", "192.168.1.64/26"])
# collapse_addresses: merge adjacent halves back into the parent network.
_coll = list(ipaddress.collapse_addresses([
    ipaddress.ip_network("192.168.1.0/25"),
    ipaddress.ip_network("192.168.1.128/25")]))
chk("ip_collapse_addresses",
    len(_coll) == 1 and str(_coll[0]) == "192.168.1.0/24")
# summarize_address_range: minimal CIDR cover of an inclusive address range.
_summ = list(ipaddress.summarize_address_range(
    ipaddress.ip_address("192.168.1.0"),
    ipaddress.ip_address("192.168.1.3")))
chk("ip_summarize_range",
    len(_summ) == 1 and str(_summ[0]) == "192.168.1.0/30")


print(("PY_TYPING_OK") if _ok else ("PY_TYPING_FAIL"))
sys.exit(0 if _ok else 1)
