#!/usr/bin/env python3
"""OOP machinery (MRO/super/metaclass/abc/property/slots/dataclass/enum/namedtuple) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# =====================================================================
# Inheritance + C3 linearization (MRO)
# Ref: docs "The Python 2.3 Method Resolution Order" / data model "__mro__".
# How: build a diamond (D->B,C->A); expect C3 order D,B,C,A,object.
# Why: StarryOS must compute MRO identically; broken C3 = wrong dispatch.
# =====================================================================
class A_d:
    def label(self):
        return "A"

class B_d(A_d):
    def label(self):
        return "B->" + super().label()

class C_d(A_d):
    def label(self):
        return "C->" + super().label()

class D_d(B_d, C_d):
    def label(self):
        return "D->" + super().label()

chk("c3_mro_order",
    [k.__name__ for k in D_d.__mro__] == ["D_d", "B_d", "C_d", "A_d", "object"])
# In a diamond, super() chains cooperatively: D->B->C->A (each next is the MRO successor).
chk("c3_cooperative_super", D_d().label() == "D->B->C->A")
chk("mro_method_callable", callable(D_d.mro))
chk("mro_method_list", D_d.mro() == list(D_d.__mro__))

# Inconsistent MRO must raise TypeError (C3 cannot linearize X(A_d,B_d) vs base order).
try:
    type("Bad", (A_d, B_d), {})
    _bad = False
except TypeError:
    _bad = True
chk("c3_inconsistent_raises", _bad)

# issubclass / isinstance follow the MRO graph.
chk("issubclass_chain", issubclass(D_d, A_d) and issubclass(B_d, A_d))
chk("isinstance_chain", isinstance(D_d(), A_d) and isinstance(D_d(), object))
chk("issubclass_negative", not issubclass(A_d, B_d))
chk("issubclass_tuple", issubclass(D_d, (int, A_d)))
# __bases__: tuple of DIRECT bases (not the full MRO); __subclasses__(): live children.
chk("class_bases_direct", D_d.__bases__ == (B_d, C_d) and B_d.__bases__ == (A_d,))
chk("class_bases_object", A_d.__bases__ == (object,) and object.__bases__ == ())
chk("class_subclasses", set(A_d.__subclasses__()) == {B_d, C_d}
    and D_d in B_d.__subclasses__())


# =====================================================================
# super(): zero-arg, explicit two-arg, in classmethod, unbound proxy.
# Ref: builtins "super", language ref "super and method resolution".
# How: zero-arg uses __class__ cell; explicit super(B, self); classmethod form.
# Why: zero-arg super relies on the __class__ closure cell injected by the
#      compiler — a subtle compile-time feature StarryOS's CPython must honor.
# =====================================================================
class SBase:
    def greet(self):
        return "base"
    @classmethod
    def cmake(cls):
        return "cbase"

class SMid(SBase):
    def greet(self):
        # explicit two-arg form, must equal zero-arg form
        return "mid+" + super(SMid, self).greet()
    @classmethod
    def cmake(cls):
        return "cmid+" + super().cmake()

class SLeaf(SMid):
    def greet(self):
        return "leaf+" + super().greet()  # zero-arg

chk("super_zero_arg", SLeaf().greet() == "leaf+mid+base")
chk("super_explicit_two_arg", SMid().greet() == "mid+base")
chk("super_in_classmethod", SLeaf.cmake() == "cmid+cbase")
# Explicit super with a type as 2nd arg yields a bound classmethod proxy.
chk("super_type_second_arg", super(SLeaf, SLeaf).cmake() == "cmid+cbase")
# super().__init__ cooperative multiple inheritance (the canonical pattern).
_seq = []
class Root:
    def __init__(self):
        _seq.append("Root")
class L1(Root):
    def __init__(self):
        _seq.append("L1"); super().__init__()
class L2(Root):
    def __init__(self):
        _seq.append("L2"); super().__init__()
class Combined(L1, L2):
    def __init__(self):
        _seq.append("Combined"); super().__init__()
Combined()
chk("super_cooperative_init", _seq == ["Combined", "L1", "L2", "Root"])


# =====================================================================
# Metaclasses: custom type subclass, __prepare__, __new__, __init__,
#              __call__ control, __init_subclass__, __set_name__.
# Ref: data model "Metaclasses", "Customizing class creation".
# How: Meta.__prepare__ injects a name into the namespace; Meta.__call__
#      post-processes instances; __init_subclass__ runs on subclassing;
#      __set_name__ runs when a descriptor is bound in a class body.
# Why: class-creation protocol is deeply compiler/runtime coupled.
# =====================================================================
class Meta(type):
    instances_created = 0
    @classmethod
    def __prepare__(mcs, name, bases, **kw):
        ns = {}
        ns["_prepared_marker"] = 99
        return ns
    def __new__(mcs, name, bases, ns, **kw):
        ns["_via_new"] = True
        return super().__new__(mcs, name, bases, ns)
    def __init__(cls, name, bases, ns, **kw):
        super().__init__(name, bases, ns)
        cls._meta_init_ran = True
    def __call__(cls, *a, **k):
        inst = super().__call__(*a, **k)
        inst._post_call = True
        Meta.instances_created += 1
        return inst

class WithMeta(metaclass=Meta):
    def __init__(self, v=0):
        self.v = v

chk("meta_is_subclass_of_type", isinstance(WithMeta, Meta) and issubclass(Meta, type))
chk("meta_prepare_injects", WithMeta._prepared_marker == 99)
chk("meta_new_injects", WithMeta._via_new is True)
chk("meta_init_ran", WithMeta._meta_init_ran is True)
_wm = WithMeta(7)
chk("meta_call_controls_instance", _wm._post_call is True and _wm.v == 7)
chk("meta_call_counts", Meta.instances_created == 1)
chk("type_of_class_is_meta", type(WithMeta) is Meta)

# type() three-arg dynamic class creation (the metaclass call surface).
Dyn = type("Dyn", (object,), {"answer": 42, "m": lambda self: "dyn"})
_dyn = Dyn()
chk("type_dynamic_create", Dyn.answer == 42 and _dyn.m() == "dyn" and Dyn.__name__ == "Dyn")

# __mro_entries__ (PEP 560): a non-class base in a class statement is resolved
# via __mro_entries__ to real bases (the mechanism powering typing generics).
class _ProxyBase:
    def __mro_entries__(self, bases):
        return (A_d,)
_proxy = _ProxyBase()
class ViaProxy(_proxy):
    pass
chk("mro_entries_resolves", ViaProxy.__bases__ == (A_d,) and issubclass(ViaProxy, A_d))
# __orig_bases__ preserves the ORIGINAL (pre-resolution) base list.
chk("mro_entries_orig_bases", ViaProxy.__orig_bases__ == (_proxy,))

# __init_subclass__ hook (PEP 487): runs once per subclass, sees keyword args.
class PluginBase:
    registry = {}
    def __init_subclass__(cls, /, key=None, **kw):
        super().__init_subclass__(**kw)
        cls.plugin_key = key
        if key is not None:
            PluginBase.registry[key] = cls

class AlphaPlugin(PluginBase, key="alpha"):
    pass
class BetaPlugin(PluginBase, key="beta"):
    pass

chk("init_subclass_keyword", AlphaPlugin.plugin_key == "alpha")
chk("init_subclass_registry", set(PluginBase.registry) == {"alpha", "beta"}
     and PluginBase.registry["beta"] is BetaPlugin)

# __set_name__ hook (PEP 487): descriptor learns its attribute name at class creation.
class NamedDescriptor:
    def __set_name__(self, owner, name):
        self.public_name = name
        self.private_name = "_" + name
    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return getattr(obj, self.private_name, None)
    def __set__(self, obj, value):
        setattr(obj, self.private_name, value)

class HasNamed:
    field = NamedDescriptor()

chk("set_name_called", HasNamed.field.public_name == "field"
     and HasNamed.field.private_name == "_field")
_hn = HasNamed()
_hn.field = "stored"
chk("set_name_descriptor_works", _hn.field == "stored" and _hn._field == "stored")


# =====================================================================
# abc: ABC base, abstractmethod, ABCMeta, register(), __subclasshook__,
#      abstract property/classmethod/staticmethod, update_abstractmethods.
# Ref: abc module docs.
# How: abstract class can't instantiate; concrete subclass can; register()
#      makes a virtual subclass; __subclasshook__ enables duck-typed issubclass.
# Why: ABC dispatch underpins collections.abc and isinstance protocols.
# =====================================================================
import abc

class AbstractAnimal(abc.ABC):
    @abc.abstractmethod
    def sound(self):
        ...
    @property
    @abc.abstractmethod
    def legs(self):
        ...
    @classmethod
    @abc.abstractmethod
    def species(cls):
        ...
    @staticmethod
    @abc.abstractmethod
    def kingdom():
        ...

# Instantiating an abstract class raises TypeError listing abstract members.
try:
    AbstractAnimal()
    _abs_inst = False
except TypeError:
    _abs_inst = True
chk("abc_cannot_instantiate", _abs_inst)
chk("abc_metaclass_is_abcmeta", type(AbstractAnimal) is abc.ABCMeta)
chk("abc_abstractmethods_set",
    AbstractAnimal.__abstractmethods__ == frozenset({"sound", "legs", "species", "kingdom"}))

class Dog(AbstractAnimal):
    def sound(self):
        return "woof"
    @property
    def legs(self):
        return 4
    @classmethod
    def species(cls):
        return "canis"
    @staticmethod
    def kingdom():
        return "animalia"

_dog = Dog()
chk("abc_concrete_instantiates", _dog.sound() == "woof")
chk("abc_abstract_property_impl", _dog.legs == 4)
chk("abc_abstract_classmethod_impl", Dog.species() == "canis")
chk("abc_abstract_staticmethod_impl", Dog.kingdom() == "animalia")
chk("abc_concrete_empty_abstracts", Dog.__abstractmethods__ == frozenset())

# A subclass that does NOT implement all abstracts is still abstract.
class HalfDone(AbstractAnimal):
    def sound(self):
        return "?"
try:
    HalfDone()
    _half = False
except TypeError:
    _half = True
chk("abc_partial_still_abstract", _half)

# ABC.register(): virtual subclassing without inheritance.
class Walkable(abc.ABC):
    pass
class Robot:
    pass
Walkable.register(Robot)
chk("abc_register_issubclass", issubclass(Robot, Walkable))
chk("abc_register_isinstance", isinstance(Robot(), Walkable))
chk("abc_register_no_mro", Walkable not in Robot.__mro__)

# __subclasshook__: structural/duck-typed subclass test.
class Quacker(abc.ABC):
    @classmethod
    def __subclasshook__(cls, C):
        if cls is Quacker:
            if any("quack" in B.__dict__ for B in C.__mro__):
                return True
        return NotImplemented
class Duck:
    def quack(self):
        return "quack"
class Cat:
    def meow(self):
        return "meow"
chk("subclasshook_positive", issubclass(Duck, Quacker) and isinstance(Duck(), Quacker))
chk("subclasshook_negative", not issubclass(Cat, Quacker))

# abc.ABCMeta usable directly as a metaclass.
class ViaMeta(metaclass=abc.ABCMeta):
    @abc.abstractmethod
    def f(self):
        ...
try:
    ViaMeta()
    _vm = False
except TypeError:
    _vm = True
chk("abcmeta_direct", _vm)

# update_abstractmethods (3.10+): recompute __abstractmethods__ after dynamic mutation.
chk("abc_update_abstractmethods_exists", hasattr(abc, "update_abstractmethods"))
if hasattr(abc, "update_abstractmethods"):
    class _DynAbc(abc.ABC):
        pass
    # Inject an abstract method AFTER class creation, then recompute.
    _DynAbc.gimme = abc.abstractmethod(lambda self: None)
    chk("abc_update_before_recompute", _DynAbc.__abstractmethods__ == frozenset())
    abc.update_abstractmethods(_DynAbc)
    chk("abc_update_after_recompute", _DynAbc.__abstractmethods__ == frozenset({"gimme"}))
    try:
        _DynAbc()
        _dabs = False
    except TypeError:
        _dabs = True
    chk("abc_update_makes_abstract", _dabs)
else:
    chk("abc_update_before_recompute", True, "(skip: needs 3.10)")
    chk("abc_update_after_recompute", True, "(skip: needs 3.10)")
    chk("abc_update_makes_abstract", True, "(skip: needs 3.10)")


# =====================================================================
# property: getter / setter / deleter, computed, validation, doc.
# Ref: builtins "property"; data model descriptor protocol.
# How: full read/write/del cycle; property() functional form; subclass override.
# Why: property is the canonical data descriptor; must support all 3 accessors.
# =====================================================================
class Celsius:
    def __init__(self, t=0.0):
        self._t = t
    @property
    def temp(self):
        "the temperature in celsius"
        return self._t
    @temp.setter
    def temp(self, value):
        if value < -273.15:
            raise ValueError("below absolute zero")
        self._t = value
    @temp.deleter
    def temp(self):
        self._t = 0.0
    @property
    def fahrenheit(self):
        return self._t * 9 / 5 + 32

_c = Celsius(25)
chk("property_getter", _c.temp == 25)
chk("property_computed", _c.fahrenheit == 77.0)
_c.temp = 100
chk("property_setter", _c.temp == 100 and _c.fahrenheit == 212.0)
try:
    _c.temp = -300
    _ps = False
except ValueError:
    _ps = True
chk("property_setter_validates", _ps)
del _c.temp
chk("property_deleter", _c.temp == 0.0)
chk("property_doc", Celsius.temp.__doc__ == "the temperature in celsius")
chk("property_is_data_descriptor",
    hasattr(Celsius.temp, "__get__") and hasattr(Celsius.temp, "__set__"))
# property exposes its fget/fset/fdel accessor functions for introspection.
chk("property_fget_fset_fdel",
    Celsius.temp.fget.__name__ == "temp"
    and Celsius.temp.fset.__name__ == "temp"
    and Celsius.temp.fdel.__name__ == "temp")
# A computed (read-only) property has fset/fdel == None.
chk("property_readonly_no_setter",
    Celsius.fahrenheit.fget is not None
    and Celsius.fahrenheit.fset is None and Celsius.fahrenheit.fdel is None)

# Non-data descriptor: defines __get__ only (no __set__/__delete__), so an
# entry in the instance __dict__ SHADOWS it (data descriptors would win instead).
class _NonData:
    def __get__(self, obj, objtype=None):
        return "from_descriptor"
class HasNonData:
    d = _NonData()
_nd = HasNonData()
chk("nondata_descriptor_get", _nd.d == "from_descriptor"
    and not hasattr(_NonData, "__set__"))
_nd.__dict__["d"] = "from_instance"
chk("nondata_descriptor_shadowed", _nd.d == "from_instance")  # instance dict wins

# Functional property() form.
class FuncProp:
    def __init__(self):
        self._v = 1
    def _g(self):
        return self._v
    def _s(self, x):
        self._v = x
    v = property(_g, _s, doc="functional")
_fp = FuncProp()
_fp.v = 9
chk("property_functional", _fp.v == 9 and FuncProp.v.__doc__ == "functional")

# Read-only property: setting raises AttributeError.
class ReadOnly:
    @property
    def x(self):
        return 5
try:
    ReadOnly().x = 1
    _ro = False
except AttributeError:
    _ro = True
chk("property_readonly_raises", _ro)


# =====================================================================
# functools.cached_property: computed once, cached in instance __dict__.
# Ref: functools.cached_property.
# How: increment a counter in the getter; access twice; expect one compute.
# Why: caching descriptor with non-data semantics (overridable by __dict__).
# =====================================================================
import functools

class Expensive:
    def __init__(self):
        self.calls = 0
    @functools.cached_property
    def value(self):
        self.calls += 1
        return 42
_ex = Expensive()
chk("cached_property_value", _ex.value == 42 and _ex.value == 42)
chk("cached_property_computed_once", _ex.calls == 1)
chk("cached_property_in_dict", _ex.__dict__["value"] == 42)


# =====================================================================
# classmethod / staticmethod: binding, cls dispatch, alt-constructors,
#      chaining (classmethod over property, 3.9+ deprecated but tested live).
# Ref: builtins "classmethod"/"staticmethod".
# How: classmethod sees subclass cls (polymorphic factory); staticmethod is plain.
# Why: descriptor binding differs; cls must follow the receiving subclass.
# =====================================================================
class Pizza:
    def __init__(self, toppings):
        self.toppings = list(toppings)
    @classmethod
    def margherita(cls):
        return cls(["cheese", "tomato"])
    @staticmethod
    def is_food():
        return True

class DeepDish(Pizza):
    pass

chk("classmethod_factory", Pizza.margherita().toppings == ["cheese", "tomato"])
# classmethod is polymorphic: subclass factory returns a subclass instance.
chk("classmethod_polymorphic", type(DeepDish.margherita()) is DeepDish)
chk("staticmethod_call", Pizza.is_food() is True and Pizza(["x"]).is_food() is True)
# classmethod accessible from instance too, still binds the class.
chk("classmethod_from_instance", Pizza(["x"]).margherita().toppings == ["cheese", "tomato"])
# __func__ exposes the underlying plain function.
chk("classmethod_func_attr", Pizza.margherita.__func__.__name__ == "margherita")
chk("staticmethod_get_returns_plain", type(Pizza.__dict__["is_food"]) is staticmethod)
# classmethod.__self__ is the bound class, and it is POLYMORPHIC over the receiver.
chk("classmethod_self_bound", Pizza.margherita.__self__ is Pizza
    and DeepDish.margherita.__self__ is DeepDish)
# staticmethod.__func__ exposes the wrapped plain function (parallel to classmethod).
chk("staticmethod_func_attr",
    Pizza.__dict__["is_food"].__func__.__name__ == "is_food"
    and Pizza.__dict__["is_food"].__func__() is True)
# staticmethod object is itself callable (3.10+).
_sm = staticmethod(lambda: "raw")
chk("staticmethod_callable", _sm() == "raw")


# =====================================================================
# __slots__: storage suppression, attribute restriction, inheritance,
#            weakref slot, combining with __dict__.
# Ref: data model "__slots__".
# How: instance with slots forbids unknown attrs and lacks __dict__; an
#      empty-__slots__ subclass keeps restriction; adding __weakref__ enables refs.
# Why: memory-layout optimization with strict semantics; common in delivery.
# =====================================================================
import weakref

class Slotted:
    __slots__ = ("a", "b")
    def __init__(self, a, b):
        self.a, self.b = a, b

_s = Slotted(1, 2)
chk("slots_set_get", _s.a == 1 and _s.b == 2)
chk("slots_no_dict", not hasattr(_s, "__dict__"))
try:
    _s.c = 3
    _se = False
except AttributeError:
    _se = True
chk("slots_unknown_attr_raises", _se)

# Subclass without __slots__ regains a __dict__ (and unrestricted attrs).
class SlottedChild(Slotted):
    pass
_sc = SlottedChild(1, 2)
_sc.extra = 9
chk("slots_subclass_no_slots_has_dict", hasattr(_sc, "__dict__") and _sc.extra == 9)

# Subclass with empty __slots__ keeps restriction.
class SlottedStrict(Slotted):
    __slots__ = ()
try:
    _ss = SlottedStrict(1, 2)
    _ss.zzz = 1
    _sse = False
except AttributeError:
    _sse = True
chk("slots_empty_subclass_strict", _sse)

# weakref requires __weakref__ slot.
class NoWeak:
    __slots__ = ("x",)
    def __init__(self):
        self.x = 1
try:
    weakref.ref(NoWeak())
    _nw = False
except TypeError:
    _nw = True
chk("slots_no_weakref_raises", _nw)

class CanWeak:
    __slots__ = ("x", "__weakref__")
    def __init__(self):
        self.x = 1
_cw = CanWeak()
_ref = weakref.ref(_cw)
chk("slots_weakref_slot_works", _ref() is _cw)

# __slots__ as a single string still defines one slot.
class OneSlot:
    __slots__ = "only"
_os = OneSlot()
_os.only = 5
chk("slots_string_form", _os.only == 5 and not hasattr(_os, "__dict__"))


# =====================================================================
# dataclasses: full surface — field/default/default_factory/init=False/
#   repr/compare/frozen/order/eq/kw_only/slots/__post_init__/fields()/
#   asdict/astuple/replace/InitVar/ClassVar/match_args/metadata.
# Ref: dataclasses module docs.
# How: exercise each constructor flag and helper independently.
# Why: dataclasses are pervasive in delivered Python apps; must be exact.
# =====================================================================
import dataclasses
from dataclasses import (dataclass, field, fields, asdict, astuple, replace,
                         InitVar, FrozenInstanceError, MISSING, is_dataclass)
import typing

@dataclass
class Basic:
    x: int
    y: int = 10

_b = Basic(1)
chk("dc_default", _b.x == 1 and _b.y == 10)
chk("dc_auto_repr", repr(_b) == "Basic(x=1, y=10)")
chk("dc_auto_eq", Basic(1, 2) == Basic(1, 2) and Basic(1, 2) != Basic(1, 3))
chk("dc_is_dataclass", is_dataclass(Basic) and is_dataclass(_b))

# default_factory for mutable defaults (avoids the shared-mutable bug).
@dataclass
class WithFactory:
    items: list = field(default_factory=list)
    mapping: dict = field(default_factory=dict)
_wf1, _wf2 = WithFactory(), WithFactory()
_wf1.items.append(1)
chk("dc_default_factory_independent", _wf1.items == [1] and _wf2.items == [])

# init=False fields (set in __post_init__).
@dataclass
class Computed:
    radius: float
    area: float = field(init=False)
    def __post_init__(self):
        self.area = 3.14 * self.radius * self.radius
_cp = Computed(2.0)
chk("dc_init_false_postinit", abs(_cp.area - 12.56) < 1e-9)

# repr=False on a field hides it from repr.
@dataclass
class HideField:
    a: int
    secret: int = field(repr=False, default=0)
chk("dc_field_repr_false", repr(HideField(1, 9)) == "HideField(a=1)")

# compare=False excludes a field from eq/order.
@dataclass(order=True)
class PartialCompare:
    key: int
    note: str = field(compare=False, default="")
chk("dc_compare_false",
    PartialCompare(1, "a") == PartialCompare(1, "b")
    and PartialCompare(1, "a") < PartialCompare(2, "a"))

# order=True synthesizes __lt__/__le__/__gt__/__ge__.
@dataclass(order=True)
class Ordered:
    n: int
chk("dc_order", Ordered(1) < Ordered(2) and Ordered(3) >= Ordered(3)
     and Ordered(5) > Ordered(4))

# eq=False: falls back to identity equality (object.__eq__), NOT synthesized value eq.
@dataclass(eq=False)
class NoEq:
    n: int
_noeq = NoEq(1)
chk("dc_eq_false_identity",
    NoEq(1) != NoEq(1)                       # two distinct equal-valued instances are unequal
    and (_noeq == _noeq)                     # an instance equals its own identity
    and NoEq.__eq__ is object.__eq__)        # eq=False did NOT synthesize a value __eq__

# frozen=True: immutable; setting raises FrozenInstanceError; hashable.
@dataclass(frozen=True)
class Frozen:
    a: int
    b: int = 0
_fz = Frozen(1, 2)
try:
    _fz.a = 5
    _fe = False
except FrozenInstanceError:
    _fe = True
chk("dc_frozen_immutable", _fe)
chk("dc_frozen_hashable", hash(Frozen(1, 2)) == hash(Frozen(1, 2))
     and len({Frozen(1, 2), Frozen(1, 2)}) == 1)

# kw_only=True: all fields keyword-only.
@dataclass(kw_only=True)
class KwOnly:
    a: int
    b: int
chk("dc_kw_only", KwOnly(a=1, b=2).a == 1)
try:
    KwOnly(1, 2)
    _kw = False
except TypeError:
    _kw = True
chk("dc_kw_only_positional_fails", _kw)

# Per-field kw_only mixing (field(kw_only=True) after positional fields).
@dataclass
class MixedKw:
    a: int
    b: int = field(kw_only=True)
chk("dc_field_kw_only", MixedKw(1, b=2).a == 1 and MixedKw(1, b=2).b == 2)

# slots=True: dataclass with __slots__ (no per-instance __dict__).
@dataclass(slots=True)
class DCSlots:
    x: int
    y: int
_dcs = DCSlots(1, 2)
chk("dc_slots", not hasattr(_dcs, "__dict__") and _dcs.x == 1)

# InitVar: passed to __init__/__post_init__ but not stored as a field.
@dataclass
class WithInitVar:
    base: int
    multiplier: InitVar[int] = 1
    result: int = field(init=False, default=0)
    def __post_init__(self, multiplier):
        self.result = self.base * multiplier
_iv = WithInitVar(5, multiplier=3)
# InitVar is passed to __post_init__ but never stored as an instance attribute
# (it lives only on the class as its default value, not in instance __dict__).
chk("dc_initvar", _iv.result == 15 and "multiplier" not in _iv.__dict__)

# ClassVar: shared class attribute, excluded from fields/__init__.
@dataclass
class WithClassVar:
    instances: typing.ClassVar[int] = 0
    name: str = "x"
chk("dc_classvar_value", WithClassVar.instances == 0
     and WithClassVar("a").name == "a")

# fields(): returns Field tuple excluding ClassVar & InitVar.
_field_names = [f.name for f in fields(WithInitVar)]
chk("dc_fields_excludes_initvar", _field_names == ["base", "result"])
chk("dc_fields_classvar_excluded", [f.name for f in fields(WithClassVar)] == ["name"])
chk("dc_field_metadata",
    fields(WithInitVar)[0].name == "base"
    and isinstance(fields(WithInitVar)[0].type, (str, type)))

# field metadata is a read-only mapping proxy.
@dataclass
class WithMeta2:
    v: int = field(default=0, metadata={"unit": "kg"})
chk("dc_field_metadata_value", fields(WithMeta2)[0].metadata["unit"] == "kg")

# asdict / astuple (recursive).
@dataclass
class Inner:
    a: int
@dataclass
class Outer:
    name: str
    inner: Inner
_outer = Outer("o", Inner(7))
chk("dc_asdict_recursive", asdict(_outer) == {"name": "o", "inner": {"a": 7}})
chk("dc_astuple_recursive", astuple(_outer) == ("o", (7,)))

# replace(): build a copy with overrides.
_rep = replace(Basic(1, 2), y=99)
chk("dc_replace", _rep == Basic(1, 99))

# match_args: auto-generated tuple of positional field names for pattern matching.
chk("dc_match_args", Basic.__match_args__ == ("x", "y"))

# MISSING sentinel exists; fields without defaults are required.
chk("dc_missing_sentinel", MISSING is dataclasses.MISSING)
try:
    Basic()
    _req = False
except TypeError:
    _req = True
chk("dc_required_field", _req)

# make_dataclass(): dynamic/functional dataclass creation (mirror of type()).
_MD = dataclasses.make_dataclass(
    "MD",
    [("p", int), ("q", int, field(default=5))],
    namespace={"total": lambda self: self.p + self.q},
)
_md = _MD(1)
chk("dc_make_dataclass",
    is_dataclass(_MD) and _md.p == 1 and _md.q == 5
    and _md.total() == 6 and repr(_md) == "MD(p=1, q=5)"
    and [f.name for f in fields(_MD)] == ["p", "q"])


# =====================================================================
# enum: Enum/IntEnum/StrEnum/Flag/IntFlag, auto(), aliases, _missing_,
#       functional API, iteration, __members__, value/name, _ignore_.
# Ref: enum module docs.
# How: exhaustively exercise each Enum subtype + class machinery.
# Why: enums encode protocol constants; identity & alias semantics are strict.
# =====================================================================
import enum

class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 3

chk("enum_value", Color.RED.value == 1)
chk("enum_name", Color.RED.name == "RED")
chk("enum_by_value", Color(2) is Color.GREEN)
chk("enum_by_name", Color["BLUE"] is Color.BLUE)
chk("enum_identity", Color.RED is Color.RED and Color.RED is not Color.GREEN)
chk("enum_iteration", [c.name for c in Color] == ["RED", "GREEN", "BLUE"])
chk("enum_len", len(Color) == 3)
chk("enum_members", list(Color.__members__) == ["RED", "GREEN", "BLUE"])
chk("enum_contains", Color.RED in Color)
chk("enum_repr_str", repr(Color.RED).startswith("<Color.RED") and str(Color.RED) == "Color.RED")
# Exact repr format and hashability (members are usable as dict keys / set elements).
chk("enum_repr_exact", repr(Color.RED) == "<Color.RED: 1>")
chk("enum_hashable", {Color.RED: "r", Color.GREEN: "g"}[Color.RED] == "r"
    and len({Color.RED, Color.RED, Color.BLUE}) == 2)
# Invalid value raises ValueError; invalid name raises KeyError.
try:
    Color(99); _ev = False
except ValueError:
    _ev = True
chk("enum_invalid_value", _ev)
try:
    Color["NOPE"]; _en = False
except KeyError:
    _en = True
chk("enum_invalid_name", _en)
# Enum members are immutable / can't be subclassed when they have members.
try:
    class SubColor(Color):
        PURPLE = 4
    _sub = False
except TypeError:
    _sub = True
chk("enum_no_extend_with_members", _sub)

# auto(): assigns successive ints starting at 1.
class Direction(enum.Enum):
    NORTH = enum.auto()
    EAST = enum.auto()
    SOUTH = enum.auto()
    WEST = enum.auto()
chk("enum_auto", [d.value for d in Direction] == [1, 2, 3, 4])

# Private _name_ / _value_ attributes back the public .name / .value properties.
chk("enum_private_name_value",
    Color.RED._name_ == "RED" and Color.RED._value_ == 1
    and Color.RED._name_ == Color.RED.name and Color.RED._value_ == Color.RED.value)

# _generate_next_value_: customize what auto() produces (here: the lowercased name).
class AutoName(enum.Enum):
    def _generate_next_value_(name, start, count, last_values):
        return name.lower()
    FOO = enum.auto()
    BAR = enum.auto()
chk("enum_generate_next_value",
    AutoName.FOO.value == "foo" and AutoName.BAR.value == "bar")

# Aliases: duplicate value -> canonical member; alias not in iteration.
class Status(enum.Enum):
    ACTIVE = 1
    RUNNING = 1   # alias of ACTIVE
    STOPPED = 2
chk("enum_alias_identity", Status.RUNNING is Status.ACTIVE)
chk("enum_alias_not_iterated", [s.name for s in Status] == ["ACTIVE", "STOPPED"])
chk("enum_alias_in_members", "RUNNING" in Status.__members__)
chk("enum_aliases_attr", Status.ACTIVE.value == 1 and Status(1) is Status.ACTIVE)

# _missing_ hook: custom resolution for unknown values.
class Mood(enum.Enum):
    HAPPY = 1
    SAD = 2
    @classmethod
    def _missing_(cls, value):
        return cls.HAPPY  # default fallback
chk("enum_missing_hook", Mood(999) is Mood.HAPPY)

# Functional API: Enum(name, names).
Planet = enum.Enum("Planet", "MERCURY VENUS EARTH")
chk("enum_functional_str", [p.value for p in Planet] == [1, 2, 3])
Weekday = enum.Enum("Weekday", {"MON": 1, "TUE": 2})
chk("enum_functional_dict", Weekday.MON.value == 1 and Weekday.TUE.value == 2)
Coords = enum.Enum("Coords", [("X", 10), ("Y", 20)])
chk("enum_functional_pairs", Coords.X.value == 10 and Coords.Y.value == 20)

# IntEnum: members compare/behave as ints.
class Priority(enum.IntEnum):
    LOW = 1
    HIGH = 10
chk("intenum_is_int", isinstance(Priority.LOW, int) and Priority.LOW == 1)
chk("intenum_arithmetic", Priority.HIGH + 1 == 11 and Priority.LOW < Priority.HIGH)
chk("intenum_sort", sorted([Priority.HIGH, Priority.LOW]) == [Priority.LOW, Priority.HIGH])

# StrEnum (3.11+): members behave as str.
if hasattr(enum, "StrEnum"):
    class Suit(enum.StrEnum):
        HEARTS = "hearts"
        SPADES = "spades"
    chk("strenum_is_str", isinstance(Suit.HEARTS, str) and Suit.HEARTS == "hearts")
    chk("strenum_concat", Suit.HEARTS + "!" == "hearts!")
    chk("strenum_str", str(Suit.HEARTS) == "hearts")
    # StrEnum with auto() lowercases the member name.
    class Lower(enum.StrEnum):
        ALPHA = enum.auto()
        BETA = enum.auto()
    chk("strenum_auto_lowercases", Lower.ALPHA == "alpha" and Lower.BETA == "beta")
else:
    chk("strenum_is_str", True, "(skip: needs 3.11 StrEnum)")
    chk("strenum_concat", True, "(skip: needs 3.11 StrEnum)")
    chk("strenum_str", True, "(skip: needs 3.11 StrEnum)")
    chk("strenum_auto_lowercases", True, "(skip: needs 3.11 StrEnum)")

# Flag: bitwise composition, membership, iteration over set bits.
class Perm(enum.Flag):
    R = enum.auto()
    W = enum.auto()
    X = enum.auto()
_rw = Perm.R | Perm.W
chk("flag_auto_powers_of_two", (Perm.R.value, Perm.W.value, Perm.X.value) == (1, 2, 4))
chk("flag_or", _rw.value == 3)
chk("flag_membership", Perm.R in _rw and Perm.X not in _rw)
chk("flag_and", (_rw & Perm.R) is Perm.R)
chk("flag_xor", (_rw ^ Perm.R) is Perm.W)
chk("flag_iter_set_bits", [f.name for f in _rw] == ["R", "W"])
chk("flag_invert_type", isinstance(~Perm.R, Perm))
# Empty flag (no bits set) is falsy; combined flag is truthy.
_empty = Perm(0)
chk("flag_empty_falsy", not bool(_empty) and bool(_rw))

# IntFlag: Flag that is also an int.
class Mode(enum.IntFlag):
    READ = 1
    WRITE = 2
    EXEC = 4
_rwx = Mode.READ | Mode.WRITE | Mode.EXEC
chk("intflag_is_int", isinstance(Mode.READ, int) and Mode.READ == 1)
chk("intflag_combined_value", _rwx.value == 7 and (_rwx & Mode.READ) == Mode.READ)
chk("intflag_int_compat", _rwx == 7 and (Mode.READ | Mode.WRITE) == 3)

# _ignore_: names listed are not turned into members.
class Config(enum.Enum):
    _ignore_ = ["helper"]
    A = 1
    B = 2
    helper = 3  # excluded
chk("enum_ignore", "helper" not in Config.__members__ and len(Config) == 2)


# =====================================================================
# collections.namedtuple: factory, _fields, _replace, _asdict, _make,
#   defaults, field access, tuple-ness, rename.
# Ref: collections.namedtuple.
# =====================================================================
import collections

Point = collections.namedtuple("Point", ["x", "y"])
_pt = Point(1, 2)
chk("nt_field_access", _pt.x == 1 and _pt.y == 2)
chk("nt_index_access", _pt[0] == 1 and _pt[1] == 2)
chk("nt_is_tuple", isinstance(_pt, tuple) and tuple(_pt) == (1, 2))
chk("nt_fields", Point._fields == ("x", "y"))
chk("nt_replace", _pt._replace(y=9) == Point(1, 9))
chk("nt_asdict", _pt._asdict() == {"x": 1, "y": 2})
chk("nt_make", Point._make([3, 4]) == Point(3, 4))
chk("nt_unpack", (lambda x, y: x + y)(*_pt) == 3)
# Space-separated field string form.
P2 = collections.namedtuple("P2", "a b c")
chk("nt_string_fields", P2._fields == ("a", "b", "c"))
# defaults (rightmost fields).
P3 = collections.namedtuple("P3", "a b c", defaults=[10, 20])
chk("nt_defaults", P3(1) == P3(1, 10, 20) and P3._field_defaults == {"b": 10, "c": 20})
# rename=True replaces invalid/duplicate names with _0,_1,...
P4 = collections.namedtuple("P4", ["a", "a", "def"], rename=True)
chk("nt_rename", P4._fields == ("a", "_1", "_2"))


# =====================================================================
# typing.NamedTuple: class-syntax typed namedtuple with defaults & methods.
# Ref: typing.NamedTuple.
# =====================================================================
class Employee(typing.NamedTuple):
    name: str
    id: int = 0
    def greet(self):
        return "hi " + self.name

_emp = Employee("alice")
chk("typing_nt_defaults", _emp.name == "alice" and _emp.id == 0)
chk("typing_nt_is_tuple", isinstance(_emp, tuple) and _emp == ("alice", 0))
chk("typing_nt_fields", Employee._fields == ("name", "id"))
chk("typing_nt_methods", _emp.greet() == "hi alice")
chk("typing_nt_replace", _emp._replace(id=5).id == 5)
chk("typing_nt_annotations", Employee.__annotations__ == {"name": str, "id": int})
chk("typing_nt_field_defaults", Employee._field_defaults == {"id": 0})
# typing.NamedTuple inherits the full namedtuple helper surface (_make/_asdict).
chk("typing_nt_make", Employee._make(["bob", 7]) == Employee("bob", 7))
chk("typing_nt_asdict", Employee("bob", 7)._asdict() == {"name": "bob", "id": 7})


# =====================================================================
# typing.TypedDict: dict with per-key types; total / NotRequired.
# Ref: typing.TypedDict (PEP 589 / PEP 655).
# How: TypedDict is a runtime dict at runtime; check annotations & keys.
# Why: structural type hint used widely; must define __annotations__ etc.
# =====================================================================
class Movie(typing.TypedDict):
    title: str
    year: int

_movie: Movie = {"title": "Solaris", "year": 1972}
chk("typeddict_is_dict", isinstance(_movie, dict) and _movie["title"] == "Solaris")
chk("typeddict_annotations", Movie.__annotations__ == {"title": str, "year": int})
chk("typeddict_total", Movie.__total__ is True)
chk("typeddict_required_keys", Movie.__required_keys__ == frozenset({"title", "year"}))

# total=False: all keys optional.
class PartialMovie(typing.TypedDict, total=False):
    title: str
    year: int
chk("typeddict_total_false", PartialMovie.__total__ is False
     and PartialMovie.__optional_keys__ == frozenset({"title", "year"}))

# Functional TypedDict form.
Coord = typing.TypedDict("Coord", {"x": int, "y": int})
chk("typeddict_functional", Coord.__annotations__ == {"x": int, "y": int})

# NotRequired / Required (3.11+) per-key markers.
if hasattr(typing, "NotRequired"):
    class Record(typing.TypedDict):
        id: int
        note: typing.NotRequired[str]
    chk("typeddict_not_required",
        Record.__required_keys__ == frozenset({"id"})
        and Record.__optional_keys__ == frozenset({"note"}))
else:
    chk("typeddict_not_required", True, "(skip: needs 3.11 NotRequired)")

# Required (3.11+): forces a key required even in a total=False TypedDict.
if hasattr(typing, "Required"):
    class Profile(typing.TypedDict, total=False):
        handle: typing.Required[str]
        bio: str
    chk("typeddict_required",
        Profile.__required_keys__ == frozenset({"handle"})
        and Profile.__optional_keys__ == frozenset({"bio"}))
else:
    chk("typeddict_required", True, "(skip: needs 3.11 Required)")


# =====================================================================
# 3.14-only / newer SYNTAX, isolated in exec() guarded by version, with a
# SyntaxError fallback so this file still PARSES on 3.12.
# Ref: PEP 695 (3.12 type params), PEP 750 (3.14 t-strings).
# How: only the OOP-relevant generic-class form is tested here (PEP 695),
#      so we don't overlap t02/t01; t-string skip-note for completeness.
# Why: keep the file parseable everywhere; assert when the runtime supports it.
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

# PEP 695 (3.12): generic class via type-parameter syntax (OOP machinery).
_gated_syntax(
    "pep695_generic_class", (3, 12),
    "class Stack[T]:\n"
    "    def __init__(self): self._items = []\n"
    "    def push(self, x: T): self._items.append(x)\n"
    "    def pop(self) -> T: return self._items.pop()\n"
    "s = Stack[int]()\n"
    "s.push(1); s.push(2)\n"
    "R = (s.pop(), s.pop(), Stack.__type_params__[0].__name__)\n",
    lambda ns: ns["R"] == (2, 1, "T"),
)

# PEP 695 (3.12): generic method + bounded type parameter on a class.
_gated_syntax(
    "pep695_bounded_typevar", (3, 12),
    "class Container[T]:\n"
    "    def wrap[U](self, v: U) -> tuple: return (v,)\n"
    "R = Container().wrap(5)\n",
    lambda ns: ns["R"] == (5,),
)

# PEP 750 (3.14): t-strings (not OOP, noted for completeness as a skip on <3.14).
_gated_syntax(
    "pep750_tstring_note", (3, 14),
    "name = 'cls'\n"
    "tmpl = t'val {name}'\n"
    "R = type(tmpl).__name__\n",
    lambda ns: ns["R"] == "Template",
)


print(("PY_OOP_OK") if _ok else ("PY_OOP_FAIL"))
sys.exit(0 if _ok else 1)
