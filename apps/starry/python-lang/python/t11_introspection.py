#!/usr/bin/env python3
"""Introspection & reflection — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# A module-level docstring sentinel used by inspect/getdoc/getsource checks below.
def documented(a, b=10, *args, c, d=20, **kw):
    """Sample callable.

    Second line of the docstring.
    """
    return (a, b, args, c, d, kw)


class Animal:
    """An animal base class."""
    kingdom = "animalia"

    def __init__(self, name):
        self.name = name

    def speak(self):
        return "..."


class Dog(Animal):
    """A dog."""

    def speak(self):
        return "woof"


# =====================================================================
# builtins: getattr / setattr / delattr / hasattr (with defaults)
# docs (Built-in Functions): getattr(object, name[, default]) returns the
# attribute; if absent and a default is given, returns default, else raises
# AttributeError. setattr/delattr mutate; hasattr returns bool, swallowing
# AttributeError only. how: drive every documented branch; expected per docs.
# (t05 covers the trivial cases; here we exercise the rarer branches/types.)
# =====================================================================
d = Dog("Rex")
chk("getattr_inherited", getattr(d, "kingdom") == "animalia")
chk("getattr_method_bound", getattr(d, "speak")() == "woof")
chk("getattr_default_present", getattr(d, "name", "X") == "Rex")
chk("getattr_default_absent", getattr(d, "missing", 99) == 99)
chk("getattr_default_none", getattr(d, "missing", None) is None)
try:
    getattr(d, "missing")
    _e = None
except AttributeError as ex:
    _e = ex
chk("getattr_raises", isinstance(_e, AttributeError))
try:
    getattr(d, 123)
    _e = None
except TypeError as ex:
    _e = ex
chk("getattr_nonstr_name", isinstance(_e, TypeError))
setattr(d, "color", "brown")
chk("setattr_then_get", getattr(d, "color") == "brown")
chk("hasattr_true", hasattr(d, "color") is True)
delattr(d, "color")
chk("delattr_removes", hasattr(d, "color") is False)
try:
    delattr(d, "color")
    _e = None
except AttributeError as ex:
    _e = ex
chk("delattr_missing_raises", isinstance(_e, AttributeError))
# hasattr only suppresses AttributeError; other errors propagate.
class _Boom:
    @property
    def p(self):
        raise ValueError("boom")
try:
    hasattr(_Boom(), "p")
    _e = None
except ValueError as ex:
    _e = ex
chk("hasattr_propagates_nonattrerror", isinstance(_e, ValueError))

# =====================================================================
# builtins: vars / dir
# docs: vars([object]) returns __dict__ of a module/class/instance (or
# locals() with no arg). dir([object]) returns a sorted name list; __dir__
# can customize it. how: confirm sorted, membership, and __dir__ override.
# =====================================================================
chk("vars_instance", vars(d) == {"name": "Rex"})
chk("vars_class_has_method", "speak" in vars(Dog))
try:
    vars(42)
    _e = None
except TypeError as ex:
    _e = ex
chk("vars_no_dict_raises", isinstance(_e, TypeError))
_dl = dir(d)
chk("dir_sorted", _dl == sorted(_dl))
chk("dir_has_inherited", "kingdom" in _dl and "speak" in _dl and "name" in _dl)
class _DirOverride:
    def __dir__(self):
        return ["z", "a", "m"]
chk("dir_custom_dunder", dir(_DirOverride()) == ["a", "m", "z"])  # dir() sorts result

# =====================================================================
# builtins: type() — 1-arg introspection & 3-arg dynamic class creation
# docs: type(object) returns the type; type(name, bases, dict) creates a new
# type object (the metaclass machinery). how: build a class at runtime and
# verify identity, bases, naming, and that instances behave.
# =====================================================================
chk("type_one_arg", type(d) is Dog and type(Dog) is type)
chk("type_object_is_type", type(object) is type and type(type) is type)
Made = type("Made", (Animal,), {"speak": lambda self: "made", "legs": 4})
m_inst = Made("M")
chk("type_three_arg_name", Made.__name__ == "Made")
chk("type_three_arg_bases", Made.__bases__ == (Animal,))
chk("type_three_arg_attr", Made("x").legs == 4 and m_inst.speak() == "made")
chk("type_three_arg_isinstance", isinstance(m_inst, Animal) and type(m_inst) is Made)

# =====================================================================
# builtins: isinstance / issubclass with ABCs and __instancecheck__
# docs (abc): a metaclass defining __instancecheck__/__subclasscheck__
# customizes isinstance/issubclass; abc.ABCMeta supports register() for
# virtual subclasses. how: verify duck-typed virtual subclass + custom hook.
# =====================================================================
import abc

class MyABC(abc.ABC):
    @abc.abstractmethod
    def go(self):
        ...

@MyABC.register
class Virtual:
    pass

chk("abc_virtual_isinstance", isinstance(Virtual(), MyABC) is True)
chk("abc_virtual_issubclass", issubclass(Virtual, MyABC) is True)
chk("abc_not_registered", issubclass(int, MyABC) is False)

# collections.abc structural protocols via __subclasshook__
import collections.abc as cabc
class _Sized:
    def __len__(self):
        return 0
chk("abc_subclasshook_sized", issubclass(_Sized, cabc.Sized) is True)
chk("abc_iterable_proto", isinstance([], cabc.Iterable) and isinstance("", cabc.Sequence))
chk("abc_mapping_proto", isinstance({}, cabc.Mapping) and isinstance({}, cabc.MutableMapping))

# custom metaclass __instancecheck__
class EvenMeta(type):
    def __instancecheck__(cls, obj):
        return isinstance(obj, int) and obj % 2 == 0
class Even(metaclass=EvenMeta):
    pass
chk("custom_instancecheck_true", isinstance(4, Even) is True)
chk("custom_instancecheck_false", isinstance(3, Even) is False)

# __subclasscheck__ is a distinct hook from __instancecheck__: it backs
# issubclass(), receives the candidate *class*, and must not affect isinstance.
class IntSubMeta(type):
    def __subclasscheck__(cls, sub):
        return sub is int
class IntLike(metaclass=IntSubMeta):
    pass
chk("custom_subclasscheck_true", issubclass(int, IntLike) is True)
chk("custom_subclasscheck_false", issubclass(str, IntLike) is False)

# abc.ABCMeta.register returns the registered class (usable as a decorator) and
# the registration is reflected by issubclass against the ABC, not the concrete.
@MyABC.register
class _Virt2:
    def go(self):
        return 1
chk("abc_register_returns_class", isinstance(_Virt2, type) and issubclass(_Virt2, MyABC))

# Deprecated-but-documented abc helpers still exist for back-compat introspection.
chk("abc_legacy_decorators",
    hasattr(abc, "abstractclassmethod")
    and hasattr(abc, "abstractstaticmethod")
    and hasattr(abc, "abstractproperty"))
# A concrete class missing an @abstractmethod cannot be instantiated.
try:
    MyABC()
    _e = None
except TypeError as ex:
    _e = ex
chk("abc_abstract_blocks_instantiation", isinstance(_e, TypeError))

# =====================================================================
# Object internals: __dict__ / __class__ / __bases__ / __mro__ /
# __qualname__ / __name__ / __module__ / __doc__ / __annotations__
# docs (Data model): these special attributes expose the object model.
# how: read each on a known class/instance/function and assert exact values.
# =====================================================================
chk("dunder_class", d.__class__ is Dog)
chk("dunder_dict_instance", d.__dict__ == {"name": "Rex"})
chk("dunder_bases", Dog.__bases__ == (Animal,) and Animal.__bases__ == (object,))
chk("dunder_mro", Dog.__mro__ == (Dog, Animal, object))
chk("dunder_mro_method", Dog.mro() == [Dog, Animal, object])
chk("dunder_name", Dog.__name__ == "Dog")
chk("dunder_qualname_class", Dog.__qualname__ == "Dog")
chk("dunder_module", Dog.__module__ == __name__)
chk("dunder_doc_class", Dog.__doc__ == "A dog.")
chk("dunder_doc_func", documented.__doc__.startswith("Sample callable."))

def _outer():
    def _inner():
        pass
    return _inner
chk("dunder_qualname_nested", _outer().__qualname__ == "_outer.<locals>._inner")

def _anno(a: int, b: str = "x") -> bool:
    return True
chk("dunder_annotations_func", _anno.__annotations__ == {"a": int, "b": str, "return": bool})

class _Ann:
    x: int
    y: str = "s"
# __annotations__ on a class reflects annotated names (PEP 563-independent values).
chk("dunder_annotations_class", _Ann.__annotations__ == {"x": int, "y": str})

# function-object internals
chk("func_defaults", documented.__defaults__ == (10,))
chk("func_kwdefaults", documented.__kwdefaults__ == {"d": 20})
chk("func_code_argcount", documented.__code__.co_argcount == 2)  # a, b (before *args)
chk("func_globals_is_module_globals", documented.__globals__ is globals())

# code-object internals beyond co_argcount: co_name/co_varnames/co_consts/
# co_nlocals/co_kwonlyargcount/co_filename are all documented introspection hooks.
def _codefn(a, b=1):
    c = a + b
    return c
_co = _codefn.__code__
chk("code_co_name", _co.co_name == "_codefn")
chk("code_co_varnames", _co.co_varnames == ("a", "b", "c"))
chk("code_co_nlocals", _co.co_nlocals == 3)
chk("code_co_consts_tuple", isinstance(_co.co_consts, tuple) and None in _co.co_consts)
chk("code_co_argcount_kw",
    documented.__code__.co_kwonlyargcount == 2)  # c, d are keyword-only
chk("code_co_filename", _co.co_filename.endswith("t11_introspection.py"))
chk("code_co_flags_is_int", isinstance(_co.co_flags, int) and _co.co_flags > 0)

# __closure__ exposes the free-variable cells of a closure; cell_contents reads
# the captured value. A function with no free variables has __closure__ == None.
def _make_closure():
    _captured = 42
    def _reader():
        return _captured
    return _reader
_clo = _make_closure()
chk("func_closure_present", _clo.__closure__ is not None and len(_clo.__closure__) == 1)
chk("func_closure_cell_contents", _clo.__closure__[0].cell_contents == 42)
chk("func_closure_freevars", _clo.__code__.co_freevars == ("_captured",))
chk("func_closure_none", documented.__closure__ is None)

# type.__subclasses__() enumerates the *immediate* subclasses of a class.
chk("type_subclasses", Dog in Animal.__subclasses__() and Made in Animal.__subclasses__())
chk("type_subclasses_not_transitive", Dog not in object.__subclasses__())

# =====================================================================
# inspect: signature / Parameter (kind, default, annotation) / bind
# docs (inspect.signature): returns a Signature; .parameters is an ordered
# mapping of Parameter objects with .kind/.default/.annotation. .bind maps
# args to params. how: validate every Parameter kind & a successful bind.
# =====================================================================
import inspect

sig = inspect.signature(documented)
params = list(sig.parameters.values())
chk("sig_param_names", [p.name for p in params] == ["a", "b", "args", "c", "d", "kw"])
kinds = [p.kind for p in params]
chk("sig_kind_positional_or_kw",
    kinds[0] == inspect.Parameter.POSITIONAL_OR_KEYWORD)
chk("sig_kind_var_positional", kinds[2] == inspect.Parameter.VAR_POSITIONAL)
chk("sig_kind_keyword_only", kinds[3] == inspect.Parameter.KEYWORD_ONLY)
chk("sig_kind_var_keyword", kinds[5] == inspect.Parameter.VAR_KEYWORD)
chk("sig_param_default", sig.parameters["b"].default == 10)
chk("sig_param_no_default", sig.parameters["a"].default is inspect.Parameter.empty)

def _annfn(x: int, y: "str" = "z") -> bool:
    return True
sig2 = inspect.signature(_annfn)
chk("sig_param_annotation", sig2.parameters["x"].annotation is int)
chk("sig_return_annotation", sig2.return_annotation is bool)

# positional-only kind
def _po(a, b, /, c):
    return (a, b, c)
chk("sig_kind_positional_only",
    inspect.signature(_po).parameters["a"].kind == inspect.Parameter.POSITIONAL_ONLY)

# Signature.bind / bind_partial / apply_defaults
bound = sig.bind(1, 2, 3, 4, c=5, extra=6)
bound.apply_defaults()
chk("sig_bind_args", bound.arguments["a"] == 1 and bound.arguments["b"] == 2)
chk("sig_bind_varargs", bound.arguments["args"] == (3, 4))
chk("sig_bind_kwonly", bound.arguments["c"] == 5 and bound.arguments["d"] == 20)
chk("sig_bind_varkw", bound.arguments["kw"] == {"extra": 6})
try:
    sig.bind(1)  # missing required keyword-only 'c'
    _e = None
except TypeError as ex:
    _e = ex
chk("sig_bind_missing_raises", isinstance(_e, TypeError))
bp = sig.bind_partial(1)
chk("sig_bind_partial", bp.arguments == {"a": 1})
# apply_defaults() injects parameter defaults in-place on a partial binding too.
bp2 = sig.bind_partial(1, c=5)
bp2.apply_defaults()
chk("sig_bind_partial_apply_defaults",
    bp2.arguments == {"a": 1, "b": 10, "args": (), "c": 5, "d": 20, "kw": {}})
# Positional-only parameters cannot be supplied by keyword (PEP 570).
try:
    inspect.signature(_po).bind(1, 2, a=3)  # 'a' is positional-only
    _e = None
except TypeError as ex:
    _e = ex
chk("sig_positional_only_rejects_kw", isinstance(_e, TypeError))

# manual Signature/Parameter construction
np = inspect.Parameter("q", inspect.Parameter.POSITIONAL_OR_KEYWORD, default=7)
man_sig = inspect.Signature([np])
chk("sig_manual_construct", str(man_sig) == "(q=7)" and man_sig.parameters["q"].default == 7)

# =====================================================================
# inspect: getmembers / getmro / classify_class_attrs
# docs: getmembers(object[, predicate]) -> sorted (name, value) pairs;
# getmro(cls) -> the MRO tuple. how: filter members by a predicate.
# =====================================================================
methods = inspect.getmembers(Dog, inspect.isfunction)
chk("getmembers_methods", "speak" in dict(methods))
chk("getmembers_sorted", [n for n, _ in methods] == sorted(n for n, _ in methods))
chk("getmro", inspect.getmro(Dog) == (Dog, Animal, object))

# classify_class_attrs(cls) -> list of Attribute(name, kind, defining_class,
# object). 'kind' is one of 'method'/'class method'/'static method'/
# 'property'/'data'. how: build a class with one of each and assert the kinds.
class _Kinds:
    datum = 7
    def meth(self):
        ...
    @classmethod
    def cm(cls):
        ...
    @staticmethod
    def sm():
        ...
    @property
    def prop(self):
        return 1
_cca = {a.name: a for a in inspect.classify_class_attrs(_Kinds)}
chk("classify_method", _cca["meth"].kind == "method")
chk("classify_classmethod", _cca["cm"].kind == "class method")
chk("classify_staticmethod", _cca["sm"].kind == "static method")
chk("classify_property", _cca["prop"].kind == "property")
chk("classify_data", _cca["datum"].kind == "data" and _cca["datum"].object == 7)
chk("classify_defining_class", _cca["meth"].defining_class is _Kinds)

# Signature.replace(...) returns a *new* Signature with selected fields changed
# (parameters / return_annotation); the original is left untouched (immutable).
_rep = sig2.replace(return_annotation=int)
chk("sig_replace_return", _rep.return_annotation is int and sig2.return_annotation is bool)
_rep2 = man_sig.replace(parameters=[inspect.Parameter("z", inspect.Parameter.POSITIONAL_OR_KEYWORD)])
chk("sig_replace_params", list(_rep2.parameters) == ["z"] and list(man_sig.parameters) == ["q"])

# =====================================================================
# inspect: getfullargspec / getcallargs
# docs: getfullargspec(func) -> FullArgSpec(args, varargs, varkw, defaults,
# kwonlyargs, kwonlydefaults, annotations). getcallargs maps a call to a dict.
# =====================================================================
fas = inspect.getfullargspec(documented)
chk("getfullargspec_args", fas.args == ["a", "b"])
chk("getfullargspec_varargs", fas.varargs == "args" and fas.varkw == "kw")
chk("getfullargspec_kwonly", fas.kwonlyargs == ["c", "d"])
chk("getfullargspec_defaults", fas.defaults == (10,) and fas.kwonlydefaults == {"d": 20})
ca = inspect.getcallargs(documented, 1, 2, 3, c=9)
chk("getcallargs", ca["a"] == 1 and ca["b"] == 2 and ca["args"] == (3,) and ca["c"] == 9)

# =====================================================================
# inspect: is* predicates
# docs: isfunction/ismethod/isclass/ismodule/isbuiltin/isroutine/
# isgeneratorfunction/isgenerator/iscoroutinefunction/iscoroutine.
# how: feed each predicate the matching and a non-matching object.
# =====================================================================
chk("isfunction", inspect.isfunction(documented) and not inspect.isfunction(len))
chk("ismethod", inspect.ismethod(d.speak) and not inspect.ismethod(documented))
chk("isclass", inspect.isclass(Dog) and not inspect.isclass(d))
chk("ismodule", inspect.ismodule(inspect) and not inspect.ismodule(Dog))
chk("isbuiltin", inspect.isbuiltin(len))
chk("isroutine", inspect.isroutine(documented) and inspect.isroutine(len))

def _genf():
    yield 1
    yield 2
chk("isgeneratorfunction", inspect.isgeneratorfunction(_genf))
gobj = _genf()
chk("isgenerator", inspect.isgenerator(gobj) and not inspect.isgenerator(_genf))
# getgeneratorstate tracks the lifecycle: CREATED -> SUSPENDED -> CLOSED.
chk("getgeneratorstate_created",
    inspect.getgeneratorstate(gobj) == inspect.GEN_CREATED)
next(gobj)
chk("getgeneratorstate_suspended",
    inspect.getgeneratorstate(gobj) == inspect.GEN_SUSPENDED)
gobj.close()
chk("getgeneratorstate_closed",
    inspect.getgeneratorstate(gobj) == inspect.GEN_CLOSED)

# getclosurevars decomposes a function's referenced names into nonlocals /
# globals / builtins / unbound.
def _cv_outer():
    _z = 5
    def _cv_inner():
        return _z + len([])  # _z nonlocal, len builtin
    return _cv_inner
_cvars = inspect.getclosurevars(_cv_outer())
chk("getclosurevars_nonlocals", _cvars.nonlocals == {"_z": 5})
chk("getclosurevars_builtins", "len" in _cvars.builtins)

async def _coro():
    return 1
chk("iscoroutinefunction", inspect.iscoroutinefunction(_coro)
    and not inspect.iscoroutinefunction(documented))
_cobj = _coro()
chk("iscoroutine", inspect.iscoroutine(_cobj))
_cobj.close()

async def _agenf():
    yield 1
chk("isasyncgenfunction", inspect.isasyncgenfunction(_agenf))

# =====================================================================
# inspect: getsource / getsourcelines / getfile (on this very module)
# docs: getsource(object) returns the text of the source; getsourcelines
# returns (lines, lineno); getfile returns the file. how: read this file's
# own source for a known function and assert content & file path.
# =====================================================================
try:
    src = inspect.getsource(documented)
    src_ok = "def documented(" in src and src.rstrip().endswith("return (a, b, args, c, d, kw)")
except (OSError, TypeError):
    src_ok = False
chk("getsource_self", src_ok)
try:
    lines, lineno = inspect.getsourcelines(Dog)
    lines_ok = lines[0].startswith("class Dog") and lineno > 0
except (OSError, TypeError):
    lines_ok = False
chk("getsourcelines_self", lines_ok)
try:
    f = inspect.getfile(documented)
    file_ok = f.endswith("t11_introspection.py")
except TypeError:
    file_ok = False
chk("getfile_self", file_ok)

# =====================================================================
# inspect: getdoc / cleandoc / unwrap
# docs: getdoc returns the (inherited) cleaned docstring; cleandoc strips
# uniform leading whitespace; unwrap follows __wrapped__ chains.
# =====================================================================
chk("getdoc_func", inspect.getdoc(documented) == "Sample callable.\n\nSecond line of the docstring.")
# getdoc inherits from the MRO when the subclass has no own docstring.
class _NoDoc(Animal):
    pass
chk("getdoc_inherited", inspect.getdoc(_NoDoc) == "An animal base class.")
chk("cleandoc", inspect.cleandoc("  a\n    b\n  c") == "a\n  b\nc")

import functools
@functools.wraps(documented)
def _wrapper(*a, **k):
    return documented(*a, **k)
chk("unwrap", inspect.unwrap(_wrapper) is documented)
chk("wraps_sets_wrapped", _wrapper.__wrapped__ is documented)

# =====================================================================
# inspect: currentframe / stack / getframeinfo
# docs: currentframe() returns the caller's frame; stack() returns the
# FrameInfo list; getframeinfo extracts (filename, lineno, function, ...).
# how: from a nested helper, assert frame function names & file.
# =====================================================================
def _frame_probe():
    fr = inspect.currentframe()
    info = inspect.getframeinfo(fr)
    stk = inspect.stack()
    return fr, info, stk

_fr, _info, _stk = _frame_probe()
chk("currentframe", _fr is not None and _fr.f_code.co_name == "_frame_probe")
chk("getframeinfo_func", _info.function == "_frame_probe")
chk("getframeinfo_file", _info.filename.endswith("t11_introspection.py"))
chk("stack_has_frames", len(_stk) >= 1 and _stk[0].function == "_frame_probe")
del _fr, _stk  # drop frame refs promptly

# =====================================================================
# ast: parse / dump / walk / NodeVisitor / literal_eval / compile
# docs (ast): parse(src) -> Module AST; dump -> repr; walk -> all nodes;
# NodeVisitor dispatches visit_<Type>; literal_eval safely evals literals;
# compile(tree, ...) yields a code object. how: parse, count nodes, eval.
# =====================================================================
import ast

tree = ast.parse("x = 1 + 2 * 3")
chk("ast_parse_module", isinstance(tree, ast.Module))
chk("ast_dump_contains", "BinOp" in ast.dump(tree))
node_types = {type(n).__name__ for n in ast.walk(tree)}
chk("ast_walk_nodes", {"Assign", "BinOp", "Constant", "Name"} <= node_types)

class _Counter(ast.NodeVisitor):
    def __init__(self):
        self.consts = 0
    def visit_Constant(self, node):
        self.consts += 1
        self.generic_visit(node)
cv = _Counter()
cv.visit(tree)
chk("ast_nodevisitor", cv.consts == 3)  # 1, 2, 3

# visit() dispatches to visit_<Type> and returns its value; nodes without a
# matching visitor fall through to generic_visit, which returns None per docs.
class _Ret(ast.NodeVisitor):
    def visit_Constant(self, node):
        return node.value * 10
_rv = _Ret()
_const_node = ast.parse("7", mode="eval").body
chk("ast_visit_returns_visitor_value", _rv.visit(_const_node) == 70)
chk("ast_generic_visit_returns_none",
    ast.NodeVisitor().generic_visit(_const_node) is None)
# A node with no specific visit_* method routes through generic_visit -> None.
chk("ast_visit_unhandled_is_none", _rv.visit(ast.parse("1 + 2").body[0]) is None)

chk("ast_literal_eval_list", ast.literal_eval("[1, 2, {'a': (3, 4)}]") == [1, 2, {"a": (3, 4)}])
chk("ast_literal_eval_num", ast.literal_eval("0x10") == 16)
try:
    ast.literal_eval("__import__('os')")  # call node, not a literal -> ValueError
    _e = None
except (ValueError, SyntaxError) as ex:
    _e = ex
# Per the docs a non-literal (here, a call) raises ValueError specifically.
chk("ast_literal_eval_rejects_call", isinstance(_e, ValueError))
# A name reference is likewise rejected with ValueError.
try:
    ast.literal_eval("undefined_name")
    _e = None
except (ValueError, SyntaxError) as ex:
    _e = ex
chk("ast_literal_eval_rejects_name", isinstance(_e, ValueError))

expr = ast.parse("21 * 2", mode="eval")
code_obj = compile(expr, "<ast>", "eval")
chk("ast_compile_eval", eval(code_obj) == 42)

# ast.get_docstring(node) extracts the docstring of a Module/Class/Function node
# (or None when absent); clean=True (default) dedents it like inspect.cleandoc.
_mod_doc = ast.parse('"module doc"\nx = 1')
chk("ast_get_docstring_module", ast.get_docstring(_mod_doc) == "module doc")
_fn_doc = ast.parse('def f():\n    "fn doc"\n    return 1').body[0]
chk("ast_get_docstring_func", ast.get_docstring(_fn_doc) == "fn doc")
_nodoc = ast.parse("def g():\n    return 1").body[0]
chk("ast_get_docstring_none", ast.get_docstring(_nodoc) is None)

# ast.fix_missing_locations fills in lineno/col_offset on a hand-built tree so it
# can be compiled; without it, compile() of a synthetic node raises.
_synth = ast.Expression(body=ast.BinOp(left=ast.Constant(value=6),
                                       op=ast.Mult(),
                                       right=ast.Constant(value=7)))
ast.fix_missing_locations(_synth)
chk("ast_fix_missing_locations", eval(compile(_synth, "<synth>", "eval")) == 42)

# ast.copy_location copies position fields from an existing node onto a new one.
_src_node = ast.parse("1", mode="eval").body
_dst_node = ast.Constant(value=2)
ast.copy_location(_dst_node, _src_node)
chk("ast_copy_location", _dst_node.lineno == _src_node.lineno
    and _dst_node.col_offset == _src_node.col_offset)

# ast.increment_lineno shifts the lineno of a node (and descendants) by n.
_inc = ast.parse("y = 1")
_before = _inc.body[0].lineno
ast.increment_lineno(_inc, 10)
chk("ast_increment_lineno", _inc.body[0].lineno == _before + 10)

# =====================================================================
# dis: dis() / get_instructions / Bytecode
# docs (dis): get_instructions(x) yields Instruction objects with .opname;
# Bytecode(x) wraps them and .dis() renders text. how: confirm a known
# opcode appears and Bytecode is iterable.
# =====================================================================
import dis
import io

def _adder(a, b):
    return a + b

opnames = {ins.opname for ins in dis.get_instructions(_adder)}
chk("dis_get_instructions", "RETURN_VALUE" in opnames)
bc = dis.Bytecode(_adder)
chk("dis_bytecode_iter", any(i.opname.startswith("LOAD") for i in bc))
chk("dis_bytecode_dis_text", "RESUME" in bc.dis() or "RETURN_VALUE" in bc.dis())
buf = io.StringIO()
dis.dis(_adder, file=buf)
chk("dis_dis_to_file", "RETURN_VALUE" in buf.getvalue())
# code_info gives a human-readable summary including the arg count.
chk("dis_code_info", "Argument count:" in dis.code_info(_adder))

# dis.dis recurses into nested code objects by default ("Disassembly of"),
# but depth=0 suppresses that recursion (documented depth parameter).
def _nest():
    def _inner():
        return 42
    return _inner
_buf_full = io.StringIO()
dis.dis(_nest, file=_buf_full)
chk("dis_dis_recurses_nested", "Disassembly of" in _buf_full.getvalue())
_buf_d0 = io.StringIO()
dis.dis(_nest, file=_buf_d0, depth=0)
chk("dis_dis_depth_zero_no_recurse", "Disassembly of" not in _buf_d0.getvalue())

# Bytecode optional args: current_offset marks the active instr with '-->';
# documented attributes codeobj/first_line expose the wrapped code.
bc2 = dis.Bytecode(_adder, current_offset=0)
chk("dis_bytecode_current_offset", "-->" in bc2.dis())
chk("dis_bytecode_codeobj", bc2.codeobj is _adder.__code__)
chk("dis_bytecode_first_line", isinstance(bc2.first_line, int) and bc2.first_line > 0)

# dis.disassemble(co, *, file) disassembles a *code object* directly (lower-level
# than dis.dis which also accepts functions/source); RESUME leads every 3.11+ co.
_buf_da = io.StringIO()
dis.disassemble(_adder.__code__, file=_buf_da)
chk("dis_disassemble_codeobj", "RESUME" in _buf_da.getvalue())

# code_info embeds the precise argument count for the analyzed callable.
_ci = dis.code_info(_adder)
chk("dis_code_info_argcount", "Argument count:" in _ci and "_adder" in _ci)

# Bytecode.from_traceback reconstructs a Bytecode positioned at the failing
# instruction of a traceback (current_offset set to the crash site).
try:
    raise ValueError("x")
except ValueError:
    _tb = sys.exc_info()[2]
_bc_tb = dis.Bytecode.from_traceback(_tb)
chk("dis_bytecode_from_traceback",
    _bc_tb.current_offset is not None and _bc_tb.current_offset >= 0)

# =====================================================================
# gc: collect / get_count / disable / enable / is_tracked / get_referrers
# docs (gc): the cyclic garbage collector. collect() runs a collection and
# returns # of unreachable objects found; is_tracked tells if an object is
# tracked; get_referrers finds referring objects. how: build a cycle, drop
# it, force a collection; verify enable/disable toggle isenabled().
# =====================================================================
import gc

gc.collect()  # baseline
was_enabled = gc.isenabled()
gc.disable()
chk("gc_disable", gc.isenabled() is False)
gc.enable()
chk("gc_enable", gc.isenabled() is True)
if not was_enabled:
    gc.disable()
chk("gc_get_count_tuple", isinstance(gc.get_count(), tuple) and len(gc.get_count()) == 3)

# Containers are tracked; atomic immutables are not.
chk("gc_is_tracked_list", gc.is_tracked([]) is True)
chk("gc_is_tracked_int", gc.is_tracked(1) is False)

class _Node:
    pass
_n1 = _Node()
_n2 = _Node()
_n1.peer = _n2
_n2.peer = _n1  # reference cycle
del _n1, _n2
collected = gc.collect()
chk("gc_collect_returns_int", isinstance(collected, int) and collected >= 0)

_holder = []
_target = object()  # not container-tracked, but get_referrers still works on the list
_holder.append(_target)
refs = gc.get_referrers(_target)
chk("gc_get_referrers", _holder in refs)
del _holder, _target

# gc.get_objects() returns the list of all tracked objects; a freshly-created
# tracked container must appear in it.
_sentinel_list = ["gc_get_objects_sentinel"]
_all_objs = gc.get_objects()
chk("gc_get_objects", isinstance(_all_objs, list) and _sentinel_list in _all_objs)
del _all_objs, _sentinel_list

# gc.get_stats() -> a list of 3 per-generation dicts with documented keys.
_stats = gc.get_stats()
chk("gc_get_stats",
    isinstance(_stats, list) and len(_stats) == 3
    and all("collections" in s and "collected" in s for s in _stats))

# gc.get_referents is the inverse of get_referrers: the objects a container
# directly refers to.
_cont = ["only_member"]
chk("gc_get_referents", "only_member" in gc.get_referents(_cont))

# gc.get_threshold() returns the (t0, t1, t2) collection thresholds.
_thr = gc.get_threshold()
chk("gc_get_threshold", isinstance(_thr, tuple) and len(_thr) == 3)

# gc.garbage is the (normally empty) list of uncollectable objects.
chk("gc_garbage_is_list", isinstance(gc.garbage, list))

# get_referrers(*objs) accepts multiple objects; each holder must appear.
_ha = []
_hb = []
_ta = object()
_tb = object()
_ha.append(_ta)
_hb.append(_tb)
_multi = gc.get_referrers(_ta, _tb)
chk("gc_get_referrers_multi", _ha in _multi and _hb in _multi)
del _ha, _hb, _ta, _tb, _multi

# =====================================================================
# weakref: ref / proxy / WeakValueDictionary / WeakKeyDictionary /
# WeakSet / finalize
# docs (weakref): weak references don't keep objects alive. ref(o)() yields
# the object or None after it's collected; proxy mirrors it; finalize sets a
# callback fired at finalization. how: hold weakly, drop strong ref, verify.
# =====================================================================
import weakref

class _Ref:
    def __init__(self, v):
        self.v = v

obj = _Ref(5)
r = weakref.ref(obj)
chk("weakref_ref_alive", r() is obj and r().v == 5)
p = weakref.proxy(obj)
chk("weakref_proxy", p.v == 5)
chk("weakref_getweakrefcount", weakref.getweakrefcount(obj) >= 1)
# getweakrefs(obj) returns the list of all live weakrefs pointing at obj.
chk("weakref_getweakrefs", r in weakref.getweakrefs(obj)
    and weakref.getweakrefcount(obj) == len(weakref.getweakrefs(obj)))

callback_fired = []
r2 = weakref.ref(obj, lambda ref: callback_fired.append(True))
del obj
gc.collect()
chk("weakref_dead", r() is None)
chk("weakref_callback", callback_fired == [True])
try:
    p.v  # proxy to dead referent -> ReferenceError
    _e = None
except ReferenceError as ex:
    _e = ex
chk("weakref_proxy_dead", isinstance(_e, ReferenceError))

wvd = weakref.WeakValueDictionary()
val = _Ref(1)
wvd["k"] = val
chk("weakvaluedict_present", wvd["k"] is val and list(wvd.keys()) == ["k"])
# Mapping protocol on WeakValueDictionary: get / setdefault / pop / copy.
chk("weakvaluedict_get_default", wvd.get("absent", "DFLT") == "DFLT")
chk("weakvaluedict_setdefault", wvd.setdefault("k", val) is val)
val2 = _Ref(11)
wvd["k2"] = val2
chk("weakvaluedict_pop", wvd.pop("k2") is val2 and "k2" not in wvd)
del val2
chk("weakvaluedict_copy", isinstance(wvd.copy(), weakref.WeakValueDictionary)
    and wvd.copy()["k"] is val)
del val
gc.collect()
chk("weakvaluedict_evicts", "k" not in wvd and len(wvd) == 0)

wkd = weakref.WeakKeyDictionary()
key = _Ref(2)
wkd[key] = "data"
chk("weakkeydict_present", wkd[key] == "data")
chk("weakkeydict_get_present", wkd.get(key) == "data")
chk("weakkeydict_get_default", wkd.get(_Ref(99), "DFLT") == "DFLT")
del key
gc.collect()
chk("weakkeydict_evicts", len(wkd) == 0)

ws = weakref.WeakSet()
e1 = _Ref(3)
ws.add(e1)
chk("weakset_present", e1 in ws and len(ws) == 1)
del e1
gc.collect()
chk("weakset_evicts", len(ws) == 0)

fin_log = []
fobj = _Ref(9)
fin = weakref.finalize(fobj, lambda: fin_log.append("done"))
chk("finalize_alive", fin.alive is True)
del fobj
gc.collect()
chk("finalize_fired", fin_log == ["done"] and fin.alive is False)

# WeakMethod weakly references a *bound method* (a plain weakref.ref would die
# immediately because bound methods are recreated on each attribute access).
class _Owner:
    def greet(self):
        return "hi"
_own = _Owner()
_wm = weakref.WeakMethod(_own.greet)
chk("weakmethod_alive", _wm()() == "hi")
del _own
gc.collect()
chk("weakmethod_dead", _wm() is None)

# =====================================================================
# sys: getrefcount / getsizeof / intern / _getframe / recursionlimit /
# exc_info / maxsize / byteorder / float_info / int_info / getdefaultencoding
# docs (sys): runtime/interpreter introspection hooks. how: assert each
# returns the documented type/relationship without mutating global state
# destructively (restore recursionlimit afterwards).
# =====================================================================
_sx = []
rc = sys.getrefcount(_sx)
chk("sys_getrefcount", isinstance(rc, int) and rc >= 2)  # +1 for arg passing

chk("sys_getsizeof", isinstance(sys.getsizeof(0), int) and sys.getsizeof([]) > 0)
# object() has a valid __sizeof__, so the default is never used: getsizeof must
# return the real size (a positive int), never the sentinel default.
_gsz = sys.getsizeof(object(), 999)
chk("sys_getsizeof_default", isinstance(_gsz, int) and _gsz != 999 and _gsz > 0)
# The default IS returned when __sizeof__ is unavailable/raises.
class _NoSizeof:
    __slots__ = ()
    def __sizeof__(self):
        raise TypeError("no size")
chk("sys_getsizeof_default_used", sys.getsizeof(_NoSizeof(), 777) == 777)

# intern returns an interned (identity-shared) string for equal contents.
s_a = sys.intern("a_unique_intern_token_" + "xyz")
s_b = sys.intern("a_unique_intern_token_" + "xyz")
chk("sys_intern_identity", s_a is s_b)

def _depth():
    return sys._getframe().f_code.co_name
chk("sys_getframe", _depth() == "_depth")
chk("sys_getframe_back", sys._getframe(0) is not None)

old_limit = sys.getrecursionlimit()
chk("sys_getrecursionlimit", isinstance(old_limit, int) and old_limit > 0)
sys.setrecursionlimit(old_limit + 100)
chk("sys_setrecursionlimit", sys.getrecursionlimit() == old_limit + 100)
sys.setrecursionlimit(old_limit)  # restore

# exc_info reports the exception being handled (and (None,None,None) outside).
try:
    raise ValueError("probe")
except ValueError:
    et, ev, tb = sys.exc_info()
    exc_ok = et is ValueError and str(ev) == "probe" and tb is not None
chk("sys_exc_info", exc_ok)
chk("sys_exc_info_outside", sys.exc_info() == (None, None, None))

# Explicit chaining ('raise ... from ...') sets __cause__; the live exception
# from exc_info() carries the full chain.
try:
    try:
        raise ValueError("root")
    except ValueError as _rootexc:
        raise RuntimeError("wrapped") from _rootexc
except RuntimeError:
    _, _ev, _tb = sys.exc_info()
    chain_ok = (isinstance(_ev, RuntimeError)
                and isinstance(_ev.__cause__, ValueError)
                and str(_ev.__cause__) == "root"
                and _ev.__context__ is _ev.__cause__
                and _tb is not None)
chk("sys_exc_info_explicit_chain", chain_ok)

# Implicit chaining (no 'from') sets __context__ but leaves __cause__ None.
try:
    try:
        raise KeyError("ctx")
    except KeyError:
        raise IndexError("next")
except IndexError:
    _, _ev2, _ = sys.exc_info()
    implicit_ok = (isinstance(_ev2.__context__, KeyError)
                   and _ev2.__cause__ is None)
chk("sys_exc_info_implicit_context", implicit_ok)

chk("sys_maxsize", sys.maxsize >= 2 ** 31 - 1)
chk("sys_byteorder", sys.byteorder in ("little", "big"))
fi = sys.float_info
chk("sys_float_info", fi.dig > 0 and fi.max > 0 and hasattr(fi, "epsilon"))
ii = sys.int_info
chk("sys_int_info", ii.bits_per_digit > 0 and ii.sizeof_digit > 0)
chk("sys_getdefaultencoding", sys.getdefaultencoding() == "utf-8")

# Bonus sys introspection surface (all documented):
chk("sys_version_info_type", isinstance(sys.version_info[:3], tuple))
chk("sys_implementation_name", sys.implementation.name == "cpython")
chk("sys_modules_has_self", "sys" in sys.modules and "inspect" in sys.modules)
# hexversion encodes version_info as a single int; its high bytes match.
chk("sys_hexversion",
    isinstance(sys.hexversion, int)
    and (sys.hexversion >> 24) == sys.version_info[0]
    and ((sys.hexversion >> 16) & 0xFF) == sys.version_info[1])
# abiflags is the build's ABI flag string (often "" on CPython release builds).
chk("sys_abiflags", isinstance(sys.abiflags, str))
# api_version is the C-API version integer.
chk("sys_api_version", isinstance(sys.api_version, int) and sys.api_version > 0)
# version_info is a named tuple with a documented 'releaselevel'.
chk("sys_version_info_fields",
    sys.version_info.releaselevel in ("alpha", "beta", "candidate", "final")
    and isinstance(sys.version_info.serial, int))
# implementation.version is itself a version_info-like tuple.
chk("sys_implementation_version",
    sys.implementation.version[:2] == sys.version_info[:2])
# sys.flags exposes interpreter command-line flags as named ints.
chk("sys_flags", isinstance(sys.flags.optimize, int) and isinstance(sys.flags.debug, int))

# =====================================================================
# 3.14-only syntax: PEP 695 type-param syntax (3.12+) and PEP 750 t-strings
# (3.14+). Newer SYNTAX must live in an exec()'d string guarded by a version
# check + SyntaxError fallback so this file still parses on 3.12.
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

# PEP 695 (3.12): generic class/function type-param syntax — introspect via
# __type_params__, which exposes the declared TypeVars.
_gated_syntax(
    "pep695_type_params_introspect", (3, 12),
    "class C[T]:\n"
    "    pass\n"
    "def fn[U](x: U) -> U: return x\n"
    "R = (C.__type_params__[0].__name__, fn.__type_params__[0].__name__)\n",
    lambda ns: ns["R"] == ("T", "U"),
)

# PEP 750 (3.14): t-strings yield a Template whose .interpolations expose the
# introspectable evaluated parts (Interpolation objects with .value/.expression).
_gated_syntax(
    "pep750_tstring_introspect", (3, 14),
    "x = 5\n"
    "tmpl = t'val={x}'\n"
    "interp = tmpl.interpolations[0]\n"
    "R = (type(tmpl).__name__, interp.value, interp.expression)\n",
    lambda ns: ns["R"] == ("Template", 5, "x"),
)

print(("PY_INTROSPECT_OK") if _ok else ("PY_INTROSPECT_FAIL"))
sys.exit(0 if _ok else 1)
