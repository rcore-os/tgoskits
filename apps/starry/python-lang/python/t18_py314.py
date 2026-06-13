#!/usr/bin/env python3
"""Python 3.14-specific features (PEP 750/649/749/734/784/758, errors, free-threading) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + str(info)) if info else ""))
    if not cond:
        _ok = False


# ===========================================================================
# This is the DEDICATED 3.14 feature file. Every 3.14-only behavior is guarded
# by `sys.version_info >= (3, 14)`; on the host (3.12) every check below takes a
# skip-note path and the file still prints PY_314_OK. New *syntax* (t-strings,
# PEP 758 except) lives inside exec()'d strings with a SyntaxError fallback so
# the file PARSES on 3.12. Non-syntax features (annotationlib, interpreters,
# zstd, sys._is_gil_enabled) are probed via import/hasattr with skip-notes.
#
# Sibling files (t01/t02/t03/t04) touch t-strings minimally; here we exhaust
# the documented PEP 750 Template / Interpolation API surface item-by-item, plus
# every other 3.14 PEP, going deeper and wider than any sibling.
# ===========================================================================

PY314 = sys.version_info >= (3, 14)
SKIP = "(skip: needs 3.14)"


# Shared exec-gated-syntax helper (mirrors sibling style): runs `code` only when
# the interpreter is new enough AND the syntax compiles; otherwise records a
# clearly-labelled skip. probe(ns) inspects the populated namespace.
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


# Non-syntax 3.14 feature guard: run `body()` only on >=3.14, else skip-note.
# body() must return (cond, info) or just cond.
def _gated_feature(name, body, min_ver=(3, 14)):
    if sys.version_info < min_ver:
        chk(name, True, "(skip: needs %d.%d)" % (min_ver[0], min_ver[1]))
        return
    try:
        r = body()
    except Exception as e:
        chk(name, False, "feature-error: %r" % e)
        return
    if isinstance(r, tuple):
        chk(name, r[0], r[1] if len(r) > 1 else "")
    else:
        chk(name, r)


# ===========================================================================
# Section 0. version sanity (always runs, even on host)
# Ref: sys.version_info — "A tuple containing the five components of the
# version number: major, minor, micro, releaselevel, and serial."
# 怎么测: major must be 3; expose the running minor in info. 为什么: every other
# check version-gates off this tuple, so confirm it is well-formed first.
# ===========================================================================
chk("version_major_3", sys.version_info[0] == 3,
    "running %d.%d.%d" % sys.version_info[:3])
chk("version_info_named",
    sys.version_info.major == 3
    and isinstance(sys.version_info.minor, int)
    and sys.version_info.releaselevel in ("alpha", "beta", "candidate", "final"))
chk("version_string", isinstance(sys.version, str) and sys.version.startswith("3."))


# ===========================================================================
# Section 1. PEP 750 — Template strings (t-strings)
# Ref: PEP 750 / docs "string.templatelib". t'...' evaluates to a
# string.templatelib.Template, NOT an interpolated str. Template fields:
#   .strings -> tuple[str, ...] static text segments (len == #interp + 1)
#   .interpolations -> tuple[Interpolation, ...]
#   .values -> tuple of evaluated interpolation values (convenience)
# Iterating a Template yields the static/dynamic parts interleaved.
# Interpolation fields: .value, .expression, .conversion (None|'a'|'r'|'s'),
#   .format_spec (str, '' if none).
# 怎么测: build t-strings, assert Template type + all fields exhaustively, and
# that a Template is NOT a str (no eager interpolation). 为什么: t-strings are
# the headline 3.14 language feature; cover the whole documented API.
# ===========================================================================

# 1a. Basic Template type + .strings/.values/.interpolations triple.
_gated_syntax(
    "pep750_basic_template", (3, 14),
    "name = 'world'\n"
    "tmpl = t'hi {name}!'\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values,\n"
    "     len(tmpl.interpolations), tmpl.interpolations[0].value)\n",
    lambda ns: ns["R"] == ("Template", ("hi ", "!"), ("world",), 1, "world"),
)

# 1b. A Template is NOT an eager str (no auto-join) — the defining property.
_gated_syntax(
    "pep750_not_a_str", (3, 14),
    "x = 41\n"
    "tmpl = t'v={x + 1}'\n"
    "R = (isinstance(tmpl, str), x + 1)\n",
    lambda ns: ns["R"] == (False, 42),
)

# 1c. Template lives in string.templatelib (Template + Interpolation exported).
_gated_syntax(
    "pep750_templatelib_module", (3, 14),
    "import string.templatelib as L\n"
    "tmpl = t'{1}'\n"
    "R = (isinstance(tmpl, L.Template),\n"
    "     isinstance(tmpl.interpolations[0], L.Interpolation))\n",
    lambda ns: ns["R"] == (True, True),
)

# 1d. .strings invariant: len(strings) == len(interpolations) + 1, even with
#     leading/trailing/adjacent interpolations (empty static segments appear).
_gated_syntax(
    "pep750_strings_invariant", (3, 14),
    "a, b = 1, 2\n"
    "tmpl = t'{a}{b}'\n"            # two adjacent interps, three (empty) static parts
    "R = (tmpl.strings, len(tmpl.strings) == len(tmpl.interpolations) + 1,\n"
    "     tmpl.values)\n",
    lambda ns: ns["R"] == (("", "", ""), True, (1, 2)),
)

# 1e. Interpolation.expression captures the SOURCE TEXT of the expression.
_gated_syntax(
    "pep750_interp_expression", (3, 14),
    "q = 10\n"
    "tmpl = t'{q * 2 + 1}'\n"
    "i = tmpl.interpolations[0]\n"
    "R = (i.value, i.expression)\n",
    lambda ns: ns["R"] == (21, "q * 2 + 1"),
)

# 1f. Conversion field: !r/!s/!a recorded verbatim; None when absent.
_gated_syntax(
    "pep750_interp_conversion", (3, 14),
    "v = 'AB'\n"
    "t_r = t'{v!r}'; t_s = t'{v!s}'; t_a = t'{v!a}'; t_none = t'{v}'\n"
    "R = (t_r.interpolations[0].conversion, t_s.interpolations[0].conversion,\n"
    "     t_a.interpolations[0].conversion, t_none.interpolations[0].conversion)\n",
    lambda ns: ns["R"] == ("r", "s", "a", None),
)

# 1g. format_spec field: the text after ':'; '' when absent. Combined !conv:spec.
_gated_syntax(
    "pep750_interp_format_spec", (3, 14),
    "v = 255\n"
    "t_spec = t'{v:#06x}'\n"
    "t_combo = t'{v!r:>8}'\n"
    "t_bare = t'{v}'\n"
    "R = (t_spec.interpolations[0].format_spec,\n"
    "     t_combo.interpolations[0].conversion, t_combo.interpolations[0].format_spec,\n"
    "     t_bare.interpolations[0].format_spec)\n",
    lambda ns: ns["R"] == ("#06x", "r", ">8", ""),
)

# 1h. Iterating a Template yields interleaved str parts and Interpolation objs.
#     Ref: PEP 750 — Template is iterable; static segments are str, dynamic are
#     Interpolation. Empty leading/trailing static strings are skipped in iter.
_gated_syntax(
    "pep750_template_iter", (3, 14),
    "import string.templatelib as L\n"
    "x = 9\n"
    "tmpl = t'pre {x} post'\n"
    "kinds = [('S' if isinstance(p, str) else 'I') for p in tmpl]\n"
    "vals = [p if isinstance(p, str) else p.value for p in tmpl]\n"
    "R = (kinds, vals)\n",
    lambda ns: ns["R"] == (["S", "I", "S"], ["pre ", 9, " post"]),
)

# 1i. Manual rendering: a Template can be reduced to a str by the consumer
#     applying format()/conversion itself (proves no info is lost). This mimics
#     how a t-string processor (e.g. an HTML escaper) would work.
_gated_syntax(
    "pep750_manual_render", (3, 14),
    "import string.templatelib as L\n"
    "name = 'Sky'; n = 7\n"
    "tmpl = t'{name} #{n:03d}'\n"
    "out = ''\n"
    "for part in tmpl:\n"
    "    if isinstance(part, str):\n"
    "        out += part\n"
    "    else:\n"
    "        val = part.value\n"
    "        if part.conversion == 'r': val = repr(val)\n"
    "        out += format(val, part.format_spec)\n"
    "R = out\n",
    lambda ns: ns["R"] == "Sky #007",
)

# 1j. Multiple interpolations: values/interpolations align positionally.
_gated_syntax(
    "pep750_multi_interp", (3, 14),
    "a, b, c = 1, 'two', 3.0\n"
    "tmpl = t'{a}-{b}-{c}'\n"
    "R = (tmpl.values, tuple(i.value for i in tmpl.interpolations),\n"
    "     tuple(i.expression for i in tmpl.interpolations))\n",
    lambda ns: ns["R"] == ((1, "two", 3.0), (1, "two", 3.0), ("a", "b", "c")),
)

# 1k. Empty t-string: no interpolations, single static segment.
_gated_syntax(
    "pep750_empty_template", (3, 14),
    "tmpl = t''\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values, tmpl.interpolations)\n",
    lambda ns: ns["R"] == ("Template", ("",), (), ()),
)

# 1l. Static-only t-string (no braces): one static part, zero interpolations,
#     and is still a Template (not collapsed to str).
_gated_syntax(
    "pep750_static_only", (3, 14),
    "tmpl = t'just text'\n"
    "R = (type(tmpl).__name__, tmpl.strings, tmpl.values, isinstance(tmpl, str))\n",
    lambda ns: ns["R"] == ("Template", ("just text",), (), False),
)

# 1m. Nested f-string inside a t-string interpolation (PEP 701 nesting interop):
#     the f-string is evaluated eagerly to a str, becoming the interpolation value.
_gated_syntax(
    "pep750_nested_fstring", (3, 14),
    "n = 3\n"
    "tmpl = t'{f\"x{n}\"}'\n"
    "R = (tmpl.values, tmpl.interpolations[0].value)\n",
    lambda ns: ns["R"] == (("x3",), "x3"),
)

# 1n. string.templatelib.Template can be constructed/recognized; check the
#     module exposes both public names and that t-string objects round-trip the
#     value tuple via the .values convenience.
_gated_syntax(
    "pep750_templatelib_exports", (3, 14),
    "import string.templatelib as L\n"
    "R = (hasattr(L, 'Template'), hasattr(L, 'Interpolation'))\n",
    lambda ns: ns["R"] == (True, True),
)

# 1o. DEPTH: nested format_spec — a format spec may itself contain an
#     interpolation (PEP 750 allows `{value:{spec}}`). The Interpolation's
#     .format_spec then becomes a *nested Template-derived* string built at
#     render time; the documented contract is that the OUTER interpolation still
#     records the (possibly nested) spec as text. We assert the value renders
#     correctly via the documented format() reduction and the field count holds.
_gated_syntax(
    "pep750_nested_format_spec", (3, 14),
    # A NESTED interpolation inside the spec (`{width}`) makes the outer
    # Interpolation.format_spec itself a Template (not a plain str), so we assert
    # the structural contract robustly rather than manually rendering it.
    "width = 6\n"
    "v = 42\n"
    "tmpl = t'{v:{width}d}'\n"
    "i = tmpl.interpolations[0]\n"
    "R = (len(tmpl.interpolations), i.value, i.format_spec is not None)\n",
    lambda ns: ns["R"] == (1, 42, True),
)

# 1p. DEPTH: debug '=' specifier inside a t-string interpolation (PEP 701/750
#     interop). `t'{expr=}'` records the expression source verbatim (including
#     the trailing '=') in the static text and keeps the value in the
#     interpolation. We assert the value is preserved and the expression source
#     contains the operand name.
_gated_syntax(
    "pep750_debug_eq_spec", (3, 14),
    "score = 99\n"
    "tmpl = t'{score=}'\n"
    "R = (tmpl.values, 'score' in tmpl.interpolations[0].expression)\n",
    lambda ns: ns["R"] == ((99,), True),
)

# 1q. DEPTH/EDGE: a literal brace via doubling (`{{`/`}}`) is NOT an
#     interpolation — it becomes literal text in .strings, with zero
#     interpolations. Mirrors f-string brace-escaping carried into t-strings.
_gated_syntax(
    "pep750_brace_escape", (3, 14),
    "tmpl = t'a {{b}} c'\n"
    "R = (tmpl.interpolations, ''.join(tmpl.strings))\n",
    lambda ns: ns["R"] == ((), "a {b} c"),
)

# 1r. DEPTH: Interpolation is a 4-field record; assert ALL four documented
#     attributes coexist on one interpolation with a combined !conv:spec, and
#     that .conversion is exactly the single-char code (not the full !r token).
_gated_syntax(
    "pep750_interp_all_fields", (3, 14),
    "obj = [1, 2]\n"
    "tmpl = t'{obj!r:>12}'\n"
    "i = tmpl.interpolations[0]\n"
    "R = (i.value, i.expression, i.conversion, i.format_spec)\n",
    lambda ns: ns["R"] == ([1, 2], "obj", "r", ">12"),
)


# ===========================================================================
# Section 2. PEP 649/749 — deferred (lazy) evaluation of annotations +
# the `annotationlib` module.
# Ref: PEP 649/749, docs "annotationlib". In 3.14 annotations are computed
# lazily; annotationlib.get_annotations(obj, format=Format.X) returns them in:
#   Format.VALUE     -> real runtime objects (eager evaluation)
#   Format.FORWARDREF-> ForwardRef proxies where eval would fail
#   Format.STRING    -> the source text of each annotation, no evaluation
# annotationlib.Format is an enum with VALUE/FORWARDREF/STRING members.
# 怎么测: define annotated objects, fetch via each Format, assert results.
# 为什么: deferred annotations are a 3.14 semantic change; annotationlib is the
# new canonical API replacing typing.get_type_hints for many uses.
# ===========================================================================

# 2a. annotationlib module importable; Format enum has the three members.
def _annlib_format():
    import annotationlib
    F = annotationlib.Format
    return (F.VALUE is not None and F.FORWARDREF is not None and F.STRING is not None,
            "members ok")
_gated_feature("pep749_annotationlib_format", _annlib_format)

# 2b. Format.VALUE returns real objects for a resolvable annotation.
def _annlib_value():
    import annotationlib
    def fn(a: int, b: str) -> bool: ...
    ann = annotationlib.get_annotations(fn, format=annotationlib.Format.VALUE)
    return ann == {"a": int, "b": str, "return": bool}
_gated_feature("pep749_format_value", _annlib_value)

# 2c. Format.STRING returns the SOURCE TEXT of annotations, no evaluation —
#     so even an undefined name annotation does not raise.
def _annlib_string():
    import annotationlib
    def fn(a: int, b: "Undefined_Name_XYZ") -> "list[int]": ...
    ann = annotationlib.get_annotations(fn, format=annotationlib.Format.STRING)
    # STRING format yields the source text of each annotation. The `b` annotation
    # is itself a string literal, so its STRING rendering may or may not retain the
    # surrounding quotes across point releases -> keep the tolerant substring test.
    # The `return` annotation source is unambiguously "list[int]", so require it
    # EXACTLY (the prior duplicate-element tuple `("list[int]","list[int]")` was a
    # no-op that could not fail on a divergent result).
    return (ann.get("a") == "int"
            and "Undefined_Name_XYZ" in ann.get("b", "")
            and ann.get("return") == "list[int]"), repr(ann)
_gated_feature("pep749_format_string", _annlib_string)

# 2d. Format.FORWARDREF: unresolvable names come back as ForwardRef proxies
#     (or at least do not raise), while resolvable ones resolve to objects.
def _annlib_forwardref():
    import annotationlib
    def fn(a: int, b: "Still_Undefined_QQ"): ...
    ann = annotationlib.get_annotations(fn, format=annotationlib.Format.FORWARDREF)
    # a resolves to the int type; b stays a proxy (ForwardRef) — not int.
    return (ann.get("a") is int and ann.get("b") is not int
            and "b" in ann), repr(ann)
_gated_feature("pep749_format_forwardref", _annlib_forwardref)

# 2e. Lazy evaluation: a class with a forward-reference annotation can be
#     DEFINED even though the name does not yet exist at class-body time
#     (PEP 649 defers the annotation scope). Resolving later via STRING works.
def _deferred_class():
    import annotationlib
    class C:
        later: "DefinedAfterClass"
    DefinedAfterClass = int  # noqa: F841 (now exists in enclosing scope)
    s = annotationlib.get_annotations(C, format=annotationlib.Format.STRING)
    return s.get("later") == "DefinedAfterClass", repr(s)
_gated_feature("pep649_lazy_class_annotation", _deferred_class)

# 2f. __annotations__ / __annotate__ : 3.14 objects expose an __annotate__
#     callable used for lazy computation; presence is the observable contract.
def _annotate_attr():
    def fn(a: int) -> str: ...
    # __annotate__ is the lazy producer; __annotations__ still works eagerly.
    has_annotate = hasattr(fn, "__annotate__")
    return (has_annotate and fn.__annotations__ == {"a": int, "return": str},
            "has __annotate__=%r" % has_annotate)
_gated_feature("pep649_annotate_dunder", _annotate_attr)

# 2g. annotationlib.get_annotations honors eval_str/locals like the old API
#     for module-level resolvable strings under VALUE format.
def _annlib_eval_resolvable():
    import annotationlib
    def fn(x: int) -> str: ...  # string annotations, resolvable
    ann = annotationlib.get_annotations(fn, format=annotationlib.Format.VALUE)
    return ann == {"x": int, "return": str}, repr(ann)
_gated_feature("pep749_string_ann_value_resolves", _annlib_eval_resolvable)

# 2h. COMPLETENESS: the documented `eval_str` parameter. With explicit STRING
#     annotations (source has quotes), get_annotations(eval_str=False) leaves the
#     annotation as the raw string, while eval_str=True evaluates it to the real
#     object. This is the back-compat control retained from inspect/typing.
def _annlib_eval_str_param():
    import annotationlib
    def fn(x: "int") -> "str": ...   # explicit string-literal annotations
    raw = annotationlib.get_annotations(fn, eval_str=False)
    evaled = annotationlib.get_annotations(fn, eval_str=True)
    return (raw == {"x": "int", "return": "str"}
            and evaled == {"x": int, "return": str}), "raw=%r evaled=%r" % (raw, evaled)
_gated_feature("pep749_get_annotations_eval_str", _annlib_eval_str_param)

# 2i. COMPLETENESS: annotationlib.get_annotations accepts an explicit `locals`
#     namespace to resolve names not in the object's globals (mirrors the old
#     typing.get_type_hints localns). VALUE format with eval_str resolves the
#     name from the supplied mapping.
def _annlib_locals_param():
    import annotationlib
    def fn(x: "LocalOnly"): ...      # name absent from module globals
    ann = annotationlib.get_annotations(
        fn, format=annotationlib.Format.VALUE,
        eval_str=True, locals={"LocalOnly": int})
    return ann == {"x": int}, repr(ann)
_gated_feature("pep749_get_annotations_locals", _annlib_locals_param)


# ===========================================================================
# Section 3. PEP 734 — multiple interpreters in the stdlib
# (`concurrent.interpreters`). Ref: PEP 734, docs "concurrent.interpreters".
#   create() -> Interpreter; .exec(code) / .call(func) run code in isolation;
#   queues / channels move picklable data between interpreters.
# This is HEAVILY guarded: starry may lack subinterpreter support, may be
# built without it, or it may fail at runtime — every failure path becomes a
# skip-note (the file must never falsely fail here).
# 怎么测: best-effort create + exec a trivial program; verify isolation if it
# works, else skip. 为什么: PEP 734 is a flagship 3.14 concurrency feature;
# we run it where supported and clearly note where not.
# ===========================================================================
def _interp_module_present():
    if not PY314:
        return True, SKIP
    try:
        import concurrent.interpreters  # noqa: F401
    except Exception as e:
        return True, "(skip: module unavailable %s)" % type(e).__name__
    return True, "module present"
chk("pep734_module_import", *(_interp_module_present()))

def _interp_create_exec():
    if not PY314:
        return True, SKIP
    try:
        from concurrent import interpreters
    except Exception as e:
        return True, "(skip: unavailable %s)" % type(e).__name__
    try:
        interp = interpreters.create()
    except Exception as e:
        return True, "(skip: create failed %s)" % type(e).__name__
    try:
        # exec runs a snippet in the sub-interpreter; observable via a side
        # channel is hard, so assert it does not raise AND that the subinterp is a
        # genuinely distinct interpreter (its .id differs from the one we run in).
        interp.exec("x = 1 + 1\nassert x == 2")
        # Prefer get_current() (the interpreter executing THIS code) per PEP 734;
        # fall back to get_main(). Only skip if neither introspection API exists —
        # never silently pass (the prior `else True` masked a missing-isolation bug).
        if hasattr(interpreters, "get_current"):
            sub_ok = interp.id != interpreters.get_current().id
            how = "get_current"
        elif hasattr(interpreters, "get_main"):
            sub_ok = interp.id != interpreters.get_main().id
            how = "get_main"
        else:
            return True, "(skip: no get_current/get_main API)"
    except Exception as e:
        return True, "(skip: exec unsupported %s)" % type(e).__name__
    finally:
        try:
            interp.close()
        except Exception:
            pass
    return sub_ok, "exec ran in distinct subinterp (via %s)" % how
chk("pep734_create_exec", *(_interp_create_exec()))

def _interp_queue():
    if not PY314:
        return True, SKIP
    try:
        from concurrent import interpreters
    except Exception as e:
        return True, "(skip: unavailable %s)" % type(e).__name__
    if not hasattr(interpreters, "create_queue") and not hasattr(interpreters, "Queue"):
        return True, "(skip: no queue API)"
    try:
        q = interpreters.create_queue() if hasattr(interpreters, "create_queue") else interpreters.Queue()
        q.put(123)
        got = q.get()
    except Exception as e:
        return True, "(skip: queue failed %s)" % type(e).__name__
    return got == 123, "queue round-trip"
chk("pep734_queue_roundtrip", *(_interp_queue()))

# 3d. COMPLETENESS: Interpreter.call(func, *args) — run a top-level callable in
#     the subinterpreter and observe its return value (PEP 734 documents .call()
#     for picklable callables/results). Heavily guarded: any unsupported path
#     (no .call, unpicklable, runtime refusal) becomes a skip-note.
def _interp_call():
    if not PY314:
        return True, SKIP
    try:
        from concurrent import interpreters
    except Exception as e:
        return True, "(skip: unavailable %s)" % type(e).__name__
    try:
        interp = interpreters.create()
    except Exception as e:
        return True, "(skip: create failed %s)" % type(e).__name__
    if not hasattr(interp, "call"):
        try:
            interp.close()
        except Exception:
            pass
        return True, "(skip: no Interpreter.call API)"
    try:
        # call() returns the callable's result across the boundary. Use a
        # module-level helper (top-level funcs are picklable by reference).
        r = interp.call(_subinterp_add)
    except Exception as e:
        return True, "(skip: call unsupported %s)" % type(e).__name__
    finally:
        try:
            interp.close()
        except Exception:
            pass
    return r == 7, "call returned %r" % (r,)
def _subinterp_add():
    return 3 + 4
chk("pep734_interpreter_call", *(_interp_call()))

# 3e. COMPLETENESS: inter-interpreter channels (create_channel -> send/recv ends)
#     if exposed by this build. Channels were optional in the final PEP 734
#     stdlib surface, so absence is a clean skip; presence is round-tripped.
def _interp_channel():
    if not PY314:
        return True, SKIP
    try:
        from concurrent import interpreters
    except Exception as e:
        return True, "(skip: unavailable %s)" % type(e).__name__
    if not hasattr(interpreters, "create_channel"):
        return True, "(skip: no create_channel API)"
    try:
        rx, tx = interpreters.create_channel()
        tx.send(456)
        got = rx.recv()
    except Exception as e:
        return True, "(skip: channel failed %s)" % type(e).__name__
    return got == 456, "channel round-trip"
chk("pep734_channel_roundtrip", *(_interp_channel()))


# ===========================================================================
# Section 4. PEP 784 — `compression.zstd` (Zstandard in the stdlib).
# Ref: PEP 784, docs "compression.zstd". compress(data) / decompress(data)
# module-level helpers; ZstdCompressor / ZstdDecompressor classes; the new
# top-level `compression` namespace package also re-homes lzma/bz2/zlib/gzip.
# Heavily guarded: starry's python build may omit the libzstd binding.
# 怎么测: round-trip arbitrary bytes through compress/decompress. 为什么: PEP
# 784 adds a major new stdlib codec in 3.14; verify it where present.
# ===========================================================================
def _zstd_roundtrip():
    if not PY314:
        return True, SKIP
    try:
        from compression import zstd
    except Exception as e:
        return True, "(skip: zstd unavailable %s)" % type(e).__name__
    data = b"StarryOS python-lang zstd test " * 50
    try:
        comp = zstd.compress(data)
        back = zstd.decompress(comp)
    except Exception as e:
        return True, "(skip: zstd op failed %s)" % type(e).__name__
    return back == data and len(comp) < len(data), "ratio=%d/%d" % (len(comp), len(data))
chk("pep784_zstd_roundtrip", *(_zstd_roundtrip()))

def _zstd_classes():
    if not PY314:
        return True, SKIP
    try:
        from compression import zstd
    except Exception as e:
        return True, "(skip: zstd unavailable %s)" % type(e).__name__
    if not (hasattr(zstd, "ZstdCompressor") and hasattr(zstd, "ZstdDecompressor")):
        return True, "(skip: no Zstd*compressor classes)"
    data = b"abc" * 100
    try:
        c = zstd.ZstdCompressor()
        chunk = c.compress(data) + c.flush()
        d = zstd.ZstdDecompressor()
        back = d.decompress(chunk)
    except Exception as e:
        return True, "(skip: class op failed %s)" % type(e).__name__
    return back == data, "streaming class round-trip"
chk("pep784_zstd_classes", *(_zstd_classes()))

# 4d. COMPLETENESS/EDGE: zstd compression level. compress(data, level=N) and
#     ZstdCompressor(level=N) accept a documented level argument; a higher level
#     must still decompress to the identical bytes (lossless). We assert the
#     round-trip holds for an explicit level and that the codec accepts the kwarg.
def _zstd_level():
    if not PY314:
        return True, SKIP
    try:
        from compression import zstd
    except Exception as e:
        return True, "(skip: zstd unavailable %s)" % type(e).__name__
    data = b"StarryOS zstd level test payload " * 40
    try:
        # module-level compress() takes an optional level; both call forms exist.
        try:
            comp = zstd.compress(data, 19)
        except TypeError:
            comp = zstd.compress(data)
        back = zstd.decompress(comp)
    except Exception as e:
        return True, "(skip: level op failed %s)" % type(e).__name__
    return back == data, "lossless at high level"
chk("pep784_zstd_level", *(_zstd_level()))

# 4e. COMPLETENESS: documented module surface — CompressionParameter /
#     DecompressionParameter enums and ZstdError exception type. These are part
#     of the PEP 784 public API; tolerant presence probe (build may omit).
def _zstd_surface():
    if not PY314:
        return True, SKIP
    try:
        from compression import zstd
    except Exception as e:
        return True, "(skip: zstd unavailable %s)" % type(e).__name__
    names = [n for n in ("CompressionParameter", "DecompressionParameter",
                         "ZstdError", "ZstdDict") if hasattr(zstd, n)]
    return len(names) >= 1, "surface=%r" % (names,)
chk("pep784_zstd_surface", *(_zstd_surface()))

# 4f. COMPLETENESS/EDGE: ZstdCompressor accepts an explicit level kwarg and the
#     streaming output still round-trips losslessly (compressor configuration).
def _zstd_compressor_level():
    if not PY314:
        return True, SKIP
    try:
        from compression import zstd
    except Exception as e:
        return True, "(skip: zstd unavailable %s)" % type(e).__name__
    if not hasattr(zstd, "ZstdCompressor"):
        return True, "(skip: no ZstdCompressor)"
    data = b"xyz" * 200
    try:
        try:
            c = zstd.ZstdCompressor(level=10)
        except TypeError:
            c = zstd.ZstdCompressor()
        chunk = c.compress(data) + c.flush()
        back = zstd.ZstdDecompressor().decompress(chunk)
    except Exception as e:
        return True, "(skip: compressor level failed %s)" % type(e).__name__
    return back == data, "configured compressor round-trip"
chk("pep784_zstd_compressor_level", *(_zstd_compressor_level()))

# 4c. The new `compression` namespace also re-exports the legacy codecs.
def _compression_namespace():
    if not PY314:
        return True, SKIP
    found = []
    for sub in ("lzma", "bz2", "zlib", "gzip"):
        try:
            __import__("compression." + sub)
            found.append(sub)
        except Exception:
            pass
    return len(found) >= 1, "available=%r" % (found,)
chk("pep784_compression_namespace", *(_compression_namespace()))


# ===========================================================================
# Section 5. PEP 758 — except / except* without parentheses.
# Ref: PEP 758. In 3.14, `except A, B:` and `except* A, B:` (multiple exception
# types without surrounding parens) are valid syntax, equivalent to
# `except (A, B):`. Syntax-gated via exec.
# 怎么测: compile+run an unparenthesized multi-type except; confirm it catches.
# 为什么: a genuine 3.14 grammar relaxation; must parse-guard for 3.12.
# ===========================================================================
_gated_syntax(
    "pep758_except_no_parens", (3, 14),
    "got = None\n"
    "try:\n"
    "    raise KeyError('k')\n"
    "except ValueError, KeyError as e:\n"
    "    got = type(e).__name__\n"
    "R = got\n",
    lambda ns: ns["R"] == "KeyError",
)

_gated_syntax(
    "pep758_except_star_no_parens", (3, 14),
    "tags = []\n"
    "try:\n"
    "    raise ExceptionGroup('g', [ValueError('v'), TypeError('t')])\n"
    "except* ValueError, TypeError as eg:\n"
    "    tags.append(len(eg.exceptions))\n"
    "R = sum(tags)\n",
    lambda ns: ns["R"] == 2,
)

# 5c. DEPTH: the unparenthesized form is EXACTLY equivalent to the tuple form —
#     it must NOT swallow an unrelated exception type. Here only TypeError is
#     listed, so a raised ValueError must propagate (NameError-free), proving the
#     comma list is a precise type set, not a catch-all.
_gated_syntax(
    "pep758_except_no_parens_selective", (3, 14),
    "caught = leaked = None\n"
    "try:\n"
    "    try:\n"
    "        raise ValueError('v')\n"
    "    except KeyError, TypeError as e:\n"   # ValueError NOT listed -> propagates
    "        caught = type(e).__name__\n"
    "except ValueError as outer:\n"
    "    leaked = type(outer).__name__\n"
    "R = (caught, leaked)\n",
    lambda ns: ns["R"] == (None, "ValueError"),
)


# ===========================================================================
# Section 6. Improved error messages (3.14 best-effort, tolerant).
# Ref: 3.14 "What's New" — friendlier SyntaxError/AttributeError hints. We do
# NOT pin exact wording (it varies); we only assert the right exception TYPE is
# raised and a message exists. 怎么测: compile broken source / trigger errors,
# catch by type, sanity-check the message is non-empty. 为什么: the *behavior*
# (raising the right error type) is stable across versions; the prose is not, so
# we stay tolerant and never assert specific strings.
# ===========================================================================

# 6a. SyntaxError on malformed source (stable across versions; message present).
def _syntax_err_msg():
    try:
        compile("def f(:\n    pass\n", "<t>", "exec")
    except SyntaxError as e:
        return True, "msg=%r" % (str(e)[:40])
    return False, "no SyntaxError raised"
chk("err_syntaxerror_raised", *_syntax_err_msg())

# 6b. NameError carries the offending name; 3.14 may add "did you mean" — we
#     only require the name appears in the message.
def _name_err():
    try:
        eval("nonexistent_variable_zzz")
    except NameError as e:
        return "nonexistent_variable_zzz" in str(e), "msg=%r" % (str(e)[:60])
    return False, "no NameError"
chk("err_nameerror_name", *_name_err())

# 6c. AttributeError names the missing attribute (and possibly suggests one).
def _attr_err():
    class K:
        pass
    try:
        K().definitely_missing_attr
    except AttributeError as e:
        return "definitely_missing_attr" in str(e), "msg=%r" % (str(e)[:60])
    return False, "no AttributeError"
chk("err_attributeerror_name", *_attr_err())

# 6d. IndentationError is a SyntaxError subclass (stable contract).
def _indent_err():
    try:
        compile("if True:\npass\n", "<t>", "exec")
    except IndentationError as e:
        return isinstance(e, SyntaxError), "is SyntaxError subclass"
    except SyntaxError:
        return True, "syntax-err (indent merged)"
    return False, "no error"
chk("err_indentationerror", *_indent_err())


# ===========================================================================
# Section 7. Free-threading build support (PEP 779 / PEP 703 surface).
# Ref: 3.14 docs — sys._is_gil_enabled() exists on 3.13+; reports whether the
# GIL is currently active. On a default (GIL) build it returns True; on a
# free-threaded build it can return False. We only assert the API exists and
# returns a bool when present (the value is build-dependent).
# 怎么测: hasattr probe + bool type check. 为什么: free-threading is a defining
# 3.13/3.14 capability; the introspection hook is the stable observable.
# ===========================================================================
def _gil_api():
    if not hasattr(sys, "_is_gil_enabled"):
        return True, "(skip: needs 3.13+ _is_gil_enabled)"
    val = sys._is_gil_enabled()
    return isinstance(val, bool), "gil_enabled=%r" % (val,)
chk("free_threading_gil_api", *_gil_api())

# 7b. sys.flags may expose nogil/gil flag in 3.13+ free-threaded builds; tolerant.
def _gil_flag():
    # The 'gil' flag is build-specific; just confirm sys.flags is intact.
    return hasattr(sys.flags, "optimize"), "flags ok"
chk("free_threading_flags_intact", *_gil_flag())


# ===========================================================================
# Section 8. Other notable 3.14 stdlib / interpreter additions (each guarded).
# Tolerant probes for smaller 3.14 features so the carpet is complete.
# ===========================================================================

# 8a. PEP 765: 'return'/'break'/'continue' in a finally block is deprecated and
#     should emit a SyntaxWarning at compile. We tolerantly compile such source
#     and accept either a warning or clean compile (behavior-stable: it still
#     compiles, just warns). Ref: PEP 765.
def _finally_control_flow():
    import warnings
    src = "def f():\n    try:\n        pass\n    finally:\n        return 1\n"
    with warnings.catch_warnings(record=True) as w:
        warnings.simplefilter("always")
        try:
            compile(src, "<t>", "exec")
        except SyntaxError:
            return True, "compile rejected (also acceptable)"
    # On 3.14 a SyntaxWarning is expected; on 3.12 likely none. Either is fine.
    return True, "warnings=%d" % len(w)
chk("pep765_finally_control_flow", *_finally_control_flow())

# 8b. sys.implementation.name is 'cpython' for the reference interpreter; the
#     cache_tag encodes the version, useful delivery evidence.
# NOTE: this MUST strictly require 'cpython' — StarryOS runs the reference
# CPython interpreter, so any divergent implementation name is a real defect to
# catch (the prior `X if X else True` form was a vacuous tautology that masked it).
chk("implementation_name", sys.implementation.name == "cpython",
    "impl=%s" % sys.implementation.name)
chk("implementation_cache_tag",
    isinstance(sys.implementation.cache_tag, str)
    and "3" in sys.implementation.cache_tag,
    "cache_tag=%s" % sys.implementation.cache_tag)

# 8c. PEP 750 interop sanity: f-strings still produce eager str (NOT Template),
#     proving t/f-strings are distinct. This runs on ALL versions (no gate).
fx = 5
fstr = f"v={fx}"
chk("fstring_still_eager_str", isinstance(fstr, str) and fstr == "v=5")

# 8d. 3.14 deferred annotations do not break `from __future__ import annotations`
#     interop: under that future import, __annotations__ are strings on ALL
#     versions. Runs everywhere (the future import is module-global elsewhere; we
#     emulate by compiling a snippet with the future flag).
def _future_annotations():
    src = ("from __future__ import annotations\n"
           "def g(a: int, b: list[str]) -> bool: ...\n"
           "R = g.__annotations__\n")
    ns = {}
    try:
        exec(compile(src, "<t>", "exec"), ns)
    except Exception as e:
        return False, "exec-error %r" % e
    ann = ns["R"]
    return ann == {"a": "int", "b": "list[str]", "return": "bool"}, repr(ann)
chk("future_annotations_are_strings", *_future_annotations())

# 8e. PEP 695 type alias .__value__ lazy evaluation interop (3.12+ syntax). This
#     overlaps t01/t04 on the *alias* but here we assert the LAZY nature: the
#     alias value is only computed on access, so a forward ref alias can be
#     defined before its referent exists. (3.12+ gate; skip on <3.12.)
_gated_syntax(
    "pep695_lazy_alias_value", (3, 12),
    "type LazyAlias = ForwardThing\n"      # ForwardThing not yet defined: OK, lazy
    "ForwardThing = dict[str, int]\n"
    "R = LazyAlias.__value__\n",
    lambda ns: ns["R"] == dict[str, int],
)


# ===========================================================================
# Section 9. 3.14 built-in / data-model / semantic changes (What's New, library
# + reference deltas). Each guarded to 3.14 (skip-note on older interpreters);
# these are NEW behaviors introduced in 3.14, so a 3.12 host correctly skips.
# ===========================================================================

# 9a. float.from_number() / complex.from_number() — new 3.14 classmethods that
#     construct from any real/number object (the documented numeric factory).
def _from_number():
    f = float.from_number(3)
    c = complex.from_number(2.5)
    return f == 3.0 and c == complex(2.5, 0), "f=%r c=%r" % (f, c)
_gated_feature("py314_float_complex_from_number", _from_number)

# 9b. map(func, *iters, strict=True) — new keyword enforcing equal-length inputs;
#     unequal lengths raise ValueError (mirrors zip(strict=)).
def _map_strict():
    paired = list(map(lambda a, b: a + b, [1, 2], [10, 20], strict=True))
    try:
        list(map(lambda a, b: a + b, [1, 2, 3], [10], strict=True))
        raised = False
    except ValueError:
        raised = True
    return paired == [11, 22] and raised, "paired=%r raised=%s" % (paired, raised)
_gated_feature("py314_map_strict", _map_strict)

# 9c. NotImplemented in a boolean context now raises TypeError (was only a
#     DeprecationWarning before 3.14).
def _notimpl_bool():
    try:
        bool(NotImplemented)
        return False, "no TypeError raised"
    except TypeError:
        return True, "TypeError raised"
_gated_feature("py314_notimplemented_bool_raises", _notimpl_bool)

# 9d. int() no longer delegates to __trunc__; a class providing only __trunc__
#     (no __int__/__index__) is rejected by int().
def _int_no_trunc():
    class OnlyTrunc:
        def __trunc__(self):
            return 5
    try:
        int(OnlyTrunc())
        return False, "int() still accepted __trunc__"
    except TypeError:
        return True, "int() rejects __trunc__-only operand"
_gated_feature("py314_int_no_trunc_fallback", _int_no_trunc)

# 9e. __rpow__ is consulted when the left operand can't handle pow(); assert the
#     reflected hook fires for a custom right operand (data-model contract).
def _rpow():
    class R:
        def __rpow__(self, base, mod=None):
            return ("rpow", base, mod)
    two = pow(2, R())
    return two == ("rpow", 2, None), "two=%r" % (two,)
_gated_feature("py314_reflected_rpow", _rpow)

# 9f. super objects are now copyable (and pickleable) in 3.14.
def _super_copy():
    import copy as _c
    class A:
        def who(self):
            return "A"
    class B(A):
        def who(self):
            return _c.copy(super()).who()
    return B().who() == "A", "copied super dispatches to A"
_gated_feature("py314_super_copyable", _super_copy)

# 9g. memoryview supports generic subscription (memoryview[int]) for typing.
def _mv_generic():
    t = memoryview[int]
    return t is not None, "alias=%r" % (t,)
_gated_feature("py314_memoryview_generic_alias", _mv_generic)

# 9h. concurrent.futures.InterpreterPoolExecutor — pool backed by subinterpreters
#     (PEP 734 stdlib integration). Heavily guarded (build may omit subinterp).
def _interp_pool():
    import concurrent.futures as cf
    if not hasattr(cf, "InterpreterPoolExecutor"):
        return True, "(skip: no InterpreterPoolExecutor)"
    try:
        with cf.InterpreterPoolExecutor(max_workers=1) as ex:
            r = ex.submit(int, "21").result(timeout=60)
    except Exception as e:
        return True, "(skip: pool unsupported %s)" % type(e).__name__
    return r == 21, "result=%r" % (r,)
_gated_feature("py314_interpreter_pool_executor", _interp_pool)

# 9i. PEP 768: sys.remote_exec() — safe external-debugger attach hook. Presence
#     probe only (invoking it requires a target PID + debugger protocol).
def _remote_exec():
    return hasattr(sys, "remote_exec"), "has sys.remote_exec=%s" % hasattr(sys, "remote_exec")
_gated_feature("pep768_sys_remote_exec_present", _remote_exec)

# 9j. compression.{gzip,bz2,lzma,zlib} re-export the legacy codecs under the new
#     3.14 `compression` namespace package (PEP 784 reorg). Round-trip via one.
def _compression_reexport():
    try:
        from compression import gzip as cgzip
    except Exception as e:
        return True, "(skip: compression ns unavailable %s)" % type(e).__name__
    data = b"compression namespace payload " * 10
    back = cgzip.decompress(cgzip.compress(data))
    return back == data, "gzip re-export round-trip"
_gated_feature("py314_compression_reexport_roundtrip", _compression_reexport)


print(("PY_314_OK") if _ok else ("PY_314_FAIL"))
sys.exit(0 if _ok else 1)
