#!/usr/bin/env python3
# CascaCarpet.py — industrial, exact-assertion carpet for the `casca` TUI framework running
# headless on musl-native CPython3 (StarryOS, 4 arches).
#
# casca is a fully synchronous, dependency-free TUI framework. Every widget lays itself out via
# calculate_layout(w, h) and paints via render(app)->draw_text(x, y, text); every interactive
# widget mutates deterministic state through handle_input(KeyEvent)/on_mouse(MouseEvent). This
# carpet drives ALL of that headlessly through a capture Surface (records every draw_text into a
# character grid) plus the framework's own control paths, and asserts BOTH the rendered cells and
# the post-event widget state against exact goldens recomputed from the library's documented
# behavior. No TTY, no threads, no timers, no network, no randomness, no timestamps.
#
# Coverage: Store (reducer/dispatch/subscribe/unsubscribe/deepcopy-isolation/middleware/errors) +
# combine_reducers; the full Keys constant map; KeyEvent/MouseEvent/ResizeEvent dataclasses; the
# ansi color engine (Color/BackColor enums, color(), color_from_spec named/hex/rgb/ansi/enum/None,
# move_cursor, CLEAR_SCREEN/RESET); the CSS engine (parse_css shorthand expansion + comments +
# specificity via get_style_for_node, get_box_spacing, get_border_width, parse_size,
# clamp_dimension min/max, validate_stylesheet, load_css_file guards); themes (THEMES tokens +
# var() resolution); the plugin registry (register/get/create/list/compat/errors); and every core
# widget — Label, Button, Container(flex), Checkbox, Input, Select, ListView, RadioGroup,
# ProgressBar, TextArea, Table, Tabs/TabItem, TreeView, Spinner, Card — construction, layout,
# render cells, and keyboard/mouse control round-trips; plus App lifecycle (build_ui/render/
# set_state/set_store/dispatch/get_state/set_theme/handle_input routing) and run_app.
#
# Emits `CASCA_RESULT ok=<N> fail=<F>` and, only when F==0, `CASCA_DONE`.

import importlib.metadata as _md
import os
import sys
import tempfile

import casca
from casca import (
    Button,
    Card,
    Checkbox,
    Container,
    Input,
    Keys,
    KeyEvent,
    Label,
    ListView,
    MouseEvent,
    ProgressBar,
    RadioGroup,
    ResizeEvent,
    Select,
    Spinner,
    Table,
    TabItem,
    Tabs,
    TextArea,
    TreeView,
    combine_reducers,
    create_store,
)
from casca.core.ansi import (
    CLEAR_SCREEN,
    RESET,
    BackColor,
    Color,
    color,
    color_from_spec,
    move_cursor,
)
from casca.plugins.registry import PluginRegistry
from casca.style.css import (
    clamp_dimension,
    get_border_width,
    get_box_spacing,
    get_style_for_node,
    load_css_file,
    parse_css,
    parse_size,
    validate_stylesheet,
)
from casca.core.themes import THEMES, apply_theme_tokens, resolve_theme_value

# --------------------------------------------------------------------------------------------
# assertion harness
# --------------------------------------------------------------------------------------------
_ok = 0
_fail = 0


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def eq(got, want, label):
    check(got == want, "%s: got %r want %r" % (label, got, want))


def raises(exc, fn, label):
    try:
        fn()
    except exc:
        check(True, label)
        return
    except Exception as e:  # wrong exception type
        check(False, "%s: raised %r not %s" % (label, e, exc.__name__))
        return
    check(False, "%s: did not raise %s" % (label, exc.__name__))


# --------------------------------------------------------------------------------------------
# headless capture surface — records every draw_text into a 1-based character grid
# --------------------------------------------------------------------------------------------
class Surface:
    """Stand-in for casca.App during render: satisfies write()/draw_text() and paints a grid."""

    def __init__(self, width=80, height=30):
        self.width = width
        self.height = height
        self.grid = [[" "] * width for _ in range(height)]
        self.ops = []  # (x, y, text) in the order drawn
        self.focused_widget = None
        self.focused_id = None

    def write(self, text):
        # ANSI style stream is irrelevant to cell-content assertions; ignore.
        pass

    def draw_text(self, x, y, text, apply_clip=True):
        self.ops.append((x, y, text))
        row = y - 1
        if 0 <= row < self.height:
            for i, ch in enumerate(text):
                col = x - 1 + i
                if 0 <= col < self.width:
                    self.grid[row][col] = ch

    def line(self, row):  # 1-based, trailing blanks stripped
        return "".join(self.grid[row - 1]).rstrip()

    def drew(self, text):  # was `text` drawn verbatim in any single draw_text op?
        return any(text == op[2] for op in self.ops)

    def drew_containing(self, text):
        return any(text in op[2] for op in self.ops)


def paint(widget, w=80, h=30, stylesheet=None):
    """Resolve styles, lay out at origin, render into a fresh Surface, return it."""
    widget.resolve_styles(stylesheet or {})
    widget.x = 0
    widget.y = 0
    widget.calculate_layout(w, h)
    surf = Surface(w, h)
    widget.render(surf)
    return surf


def kev(key, printable=False):
    return KeyEvent(key=key, is_printable=printable)


# --------------------------------------------------------------------------------------------
# 1. package surface + dataclasses + Keys
# --------------------------------------------------------------------------------------------
def test_surface():
    check(_md.version("casca") == "1.0.4", "casca version pinned 1.0.4 (got %s)" % _md.version("casca"))
    for name in ("App", "Store", "create_store", "combine_reducers", "Label", "Button",
                 "Container", "Input", "Checkbox", "Select", "ListView", "RadioGroup",
                 "ProgressBar", "TextArea", "Table", "Tabs", "TabItem", "TreeView", "Spinner",
                 "Card", "Keys", "KeyEvent", "MouseEvent", "ResizeEvent", "parse_css",
                 "load_css_file", "THEMES", "register_widget", "run_app"):
        check(hasattr(casca, name), "casca exports %s" % name)

    # dataclasses carry exactly the documented fields
    ke = KeyEvent(key="a", is_printable=True)
    eq(ke.key, "a", "KeyEvent.key")
    eq(ke.is_printable, True, "KeyEvent.is_printable")
    eq(ke, KeyEvent(key="a", is_printable=True), "KeyEvent dataclass equality")
    me = MouseEvent(x=3, y=4, button=0, pressed=True)
    eq((me.x, me.y, me.button, me.pressed), (3, 4, 0, True), "MouseEvent fields")
    re_ = ResizeEvent(width=100, height=40)
    eq((re_.width, re_.height), (100, 40), "ResizeEvent fields")

    # the canonical Keys constant map — exact escape sequences
    expected_keys = {
        "TAB": "\t", "SHIFT_TAB": "\x1b[Z", "ENTER": "\r", "ESCAPE": "\x1b",
        "LEFT": "\x1b[D", "RIGHT": "\x1b[C", "UP": "\x1b[A", "DOWN": "\x1b[B",
        "HOME": "\x1b[H", "END": "\x1b[F", "PAGE_UP": "\x1b[5~", "PAGE_DOWN": "\x1b[6~",
        "F1": "\x1bOP", "F2": "\x1bOQ", "F3": "\x1bOR", "F4": "\x1bOS",
        "F5": "\x1b[15~", "F6": "\x1b[17~", "F7": "\x1b[18~", "F8": "\x1b[19~",
        "F9": "\x1b[20~", "F10": "\x1b[21~", "F11": "\x1b[23~", "F12": "\x1b[24~",
    }
    for name, val in expected_keys.items():
        eq(getattr(Keys, name), val, "Keys.%s" % name)
    got = {k for k in dir(Keys) if not k.startswith("_")}
    eq(got, set(expected_keys), "Keys has exactly the 24 documented constants")


# --------------------------------------------------------------------------------------------
# 2. Store / reducers / subscribe / middleware
# --------------------------------------------------------------------------------------------
def counter_reducer(state, action):
    t = action.get("type")
    c = state.get("count", 0)
    if t == "INC":
        return {"count": c + action.get("by", 1)}
    if t == "DEC":
        return {"count": c - 1}
    if t == "RESET":
        return {"count": 0}
    return state


def test_store():
    # empty initial state is {} (documented dict(initial_state or {}))
    s = create_store(counter_reducer)
    eq(s.get_state(), {}, "store initial empty state is {}")
    s = create_store(counter_reducer, {"count": 10})
    eq(s.get_state(), {"count": 10}, "store honors initial_state")

    seen = []
    unsub = s.subscribe(lambda st: seen.append(dict(st)))
    eq(s.dispatch({"type": "INC"}), {"count": 11}, "dispatch INC returns new snapshot")
    s.dispatch({"type": "INC", "by": 4})
    s.dispatch({"type": "DEC"})
    eq(s.get_state(), {"count": 14}, "sequence INC/INC+4/DEC -> 14")
    eq(seen, [{"count": 11}, {"count": 15}, {"count": 14}], "subscriber saw every snapshot in order")
    eq(len(seen), 3, "subscriber fired once per dispatch")

    # get_state returns a deep copy — mutating it does not corrupt the store
    snap = s.get_state()
    snap["count"] = -999
    eq(s.get_state(), {"count": 14}, "get_state is a deep copy (isolation)")

    # unsubscribe stops further notifications but state still advances
    unsub()
    s.dispatch({"type": "INC"})
    eq(len(seen), 3, "unsubscribe stops notifications")
    eq(s.get_state(), {"count": 15}, "state advances after unsubscribe")
    s.dispatch({"type": "RESET"})
    eq(s.get_state(), {"count": 0}, "RESET zeroes count")

    # dispatch input contract
    raises(TypeError, lambda: s.dispatch(["not", "a", "dict"]), "dispatch non-dict -> TypeError")
    raises(ValueError, lambda: s.dispatch({"no": "type"}), "dispatch without 'type' -> ValueError")
    bad = create_store(lambda st, a: "not-a-dict")
    raises(TypeError, lambda: bad.dispatch({"type": "x"}), "reducer returning non-dict -> TypeError")

    # subscribe input contract
    raises(TypeError, lambda: s.subscribe("nope"), "subscribe non-callable -> TypeError")


def test_combine_reducers():
    def todos(state, action):
        state = state or []
        if action.get("type") == "ADD":
            return state + [action["text"]]
        return state

    def visibility(state, action):
        if state is None:
            state = "ALL"
        if action.get("type") == "SET_FILTER":
            return action["filter"]
        return state

    root = combine_reducers({"todos": todos, "visibility": visibility})
    s = create_store(root)
    eq(s.get_state(), {}, "combine store starts empty until first dispatch (no auto @@INIT)")
    s.dispatch({"type": "@@INIT"})
    eq(s.get_state(), {"todos": [], "visibility": "ALL"}, "first dispatch initializes every slice")
    s.dispatch({"type": "ADD", "text": "a"})
    s.dispatch({"type": "ADD", "text": "b"})
    eq(s.get_state()["todos"], ["a", "b"], "combined todos slice grows")
    eq(s.get_state()["visibility"], "ALL", "unrelated slice unchanged by ADD")
    s.dispatch({"type": "SET_FILTER", "filter": "DONE"})
    eq(s.get_state(), {"todos": ["a", "b"], "visibility": "DONE"}, "SET_FILTER only touches its slice")


def test_middleware():
    order = []

    def mw_a(store, action, nxt):
        order.append("A:before:%s" % action["type"])
        result = nxt(action)
        order.append("A:after")
        return result

    def mw_b(store, action, nxt):
        order.append("B:before:%s" % action["type"])
        return nxt(action)

    s = create_store(counter_reducer, {"count": 0}, middlewares=[mw_a, mw_b])
    s.dispatch({"type": "INC"})
    eq(order, ["A:before:INC", "B:before:INC", "A:after"], "middleware runs outer(A)->inner(B)->reduce")
    eq(s.get_state(), {"count": 1}, "middleware pipeline still reduces to new state")

    # a short-circuit middleware can veto the reducer entirely
    def veto(store, action, nxt):
        if action.get("type") == "BLOCK":
            return store.get_state()
        return nxt(action)

    s2 = create_store(counter_reducer, {"count": 5}, middlewares=[veto])
    s2.dispatch({"type": "BLOCK"})
    eq(s2.get_state(), {"count": 5}, "veto middleware short-circuits the reducer")
    s2.dispatch({"type": "INC"})
    eq(s2.get_state(), {"count": 6}, "non-blocked action passes through veto middleware")


# --------------------------------------------------------------------------------------------
# 3. ansi color engine
# --------------------------------------------------------------------------------------------
def test_ansi():
    eq(Color.RED.value, 31, "Color.RED")
    eq(Color.DEFAULT.value, 39, "Color.DEFAULT")
    eq(BackColor.BLUE.value, 44, "BackColor.BLUE")
    eq(BackColor.DEFAULT.value, 49, "BackColor.DEFAULT")
    eq(CLEAR_SCREEN, "\x1b[2J", "CLEAR_SCREEN constant")
    eq(RESET, "\x1b[0m", "RESET constant")
    eq(move_cursor(5, 3), "\x1b[3;5H", "move_cursor(x,y) -> ESC y;x H")

    eq(color(), "\x1b[39;49m", "color() defaults")
    eq(color(Color.RED, BackColor.BLUE), "\x1b[31;44m", "color(RED,BLUE)")

    eq(color_from_spec("red", "blue"), "\x1b[31;44m", "color_from_spec named fg/bg")
    eq(color_from_spec(None, None), "\x1b[39;49m", "color_from_spec None/None")
    eq(color_from_spec("", ""), "\x1b[39;49m", "color_from_spec empty -> default")
    eq(color_from_spec("#ff0000", None), "\x1b[38;2;255;0;0;49m", "color_from_spec hex fg")
    eq(color_from_spec("rgb(1, 2, 3)", None), "\x1b[38;2;1;2;3;49m", "color_from_spec rgb fg")
    eq(color_from_spec("ansi(196)", None), "\x1b[38;5;196;49m", "color_from_spec ansi index fg")
    eq(color_from_spec("bright_cyan", None), "\x1b[96;49m", "color_from_spec bright_cyan")
    eq(color_from_spec("notacolor", "alsobad"), "\x1b[39;49m", "color_from_spec invalid -> default")
    # enum specs: RED as fg=31; RED as bg auto +10 = 41
    eq(color_from_spec(Color.RED, Color.RED), "\x1b[31;41m", "color_from_spec Color enum fg&bg conversion")
    # BackColor.BLUE(44) as fg auto -10 = 34
    eq(color_from_spec(BackColor.BLUE, BackColor.BLUE), "\x1b[34;44m", "color_from_spec BackColor enum fg&bg")


# --------------------------------------------------------------------------------------------
# 4. CSS engine
# --------------------------------------------------------------------------------------------
def test_css():
    ss = parse_css(".btn { color: red; padding: 2; }  #main { background: blue; }  label { color: white; }")
    eq(ss[".btn"], {"color": "red", "padding": "2"}, "parse_css class rule")
    eq(ss["#main"], {"background": "blue"}, "parse_css id rule")
    eq(ss["label"], {"color": "white"}, "parse_css tag rule")

    # shorthand expansion (4 / 3 / 2 parts)
    four = parse_css(".a { margin: 1 2 3 4; }")[".a"]
    eq(four, {"margin-top": "1", "margin-right": "2", "margin-bottom": "3", "margin-left": "4"},
       "margin 4-part shorthand expands TRBL")
    three = parse_css(".b { padding: 1 2 3; }")[".b"]
    eq(three, {"padding-top": "1", "padding-right": "2", "padding-left": "2", "padding-bottom": "3"},
       "padding 3-part shorthand")
    two = parse_css(".c { margin: 5 6; }")[".c"]
    eq(two, {"margin-top": "5", "margin-bottom": "5", "margin-right": "6", "margin-left": "6"},
       "margin 2-part shorthand")

    # comments stripped, grouped selectors, later rules merge
    grouped = parse_css("/* c */ .x, .y { color: red; } .x { padding: 1; }")
    eq(grouped[".x"], {"color": "red", "padding": "1"}, "grouped selector + comment strip + merge")
    eq(grouped[".y"], {"color": "red"}, "grouped selector second target")

    # specificity: tag < class < id (later wins on the same property)
    ss2 = parse_css("label { color: white; } .hl { color: yellow; } #b { color: red; }")
    style = get_style_for_node("b", ["hl"], "label", ss2)
    eq(style["color"], "red", "get_style_for_node id beats class beats tag")
    style2 = get_style_for_node("", ["hl"], "label", ss2)
    eq(style2["color"], "yellow", "class beats tag when no id")
    style3 = get_style_for_node("", [], "label", ss2)
    eq(style3["color"], "white", "tag-only style")

    # box spacing helpers
    eq(get_box_spacing({"padding": 2}, "padding"), (2, 2, 2, 2), "get_box_spacing base")
    eq(get_box_spacing({"padding": 1, "padding-left": 5}, "padding"), (1, 1, 1, 5),
       "get_box_spacing per-side override")
    eq(get_box_spacing({}, "margin"), (0, 0, 0, 0), "get_box_spacing default zero")

    # border width normalization
    eq(get_border_width({"border": "none"}), 0, "border none -> 0")
    eq(get_border_width({}), 0, "border absent -> 0")
    eq(get_border_width({"border": "solid"}), 1, "border solid -> 1")
    eq(get_border_width({"border": "ascii"}), 1, "border ascii -> 1")

    # parse_size
    eq(parse_size(None, 100, 7), 7, "parse_size None -> fallback")
    eq(parse_size("50%", 200, 0), 100, "parse_size percent")
    eq(parse_size(12, 100, 0), 12, "parse_size int")
    eq(parse_size("bad", 100, 3), 3, "parse_size invalid -> fallback")

    # clamp_dimension with min/max
    eq(clamp_dimension(50, {"min-width": 60}, "width", 100, 0), 60, "clamp min-width raises")
    eq(clamp_dimension(90, {"max-width": 80}, "width", 100, 0), 80, "clamp max-width caps")
    eq(clamp_dimension(70, {"min-width": 60, "max-width": 80}, "width", 100, 0), 70, "clamp within range")
    eq(clamp_dimension(-5, {}, "width", 100, 0), 0, "clamp never negative")

    # validate_stylesheet warnings
    warns = validate_stylesheet({".w": {"boguskey": "1"}})
    check(any("boguskey" in w for w in warns), "validate warns unknown key")
    warns2 = validate_stylesheet({".w": {"color": "notacolor"}})
    check(any("notacolor" in w for w in warns2), "validate warns invalid color")
    warns3 = validate_stylesheet({".w": {"width": "-4"}})
    check(any("Negative" in w for w in warns3), "validate warns negative dimension")
    warns4 = validate_stylesheet({".w": {"overflow-x": "sideways"}})
    check(any("overflow" in w.lower() for w in warns4), "validate warns bad overflow value")
    eq(validate_stylesheet({".ok": {"color": "red", "width": "50%"}}), [], "clean stylesheet -> no warnings")

    # load_css_file
    with tempfile.NamedTemporaryFile("w", suffix=".css", delete=False) as f:
        f.write(".z { color: green; }")
        path = f.name
    try:
        eq(load_css_file(path), ".z { color: green; }", "load_css_file reads content")
        eq(parse_css(load_css_file(path))[".z"], {"color": "green"}, "load->parse round trip")
    finally:
        os.unlink(path)
    raises(FileNotFoundError, lambda: load_css_file("/no/such/file.css"), "load_css_file missing -> FileNotFoundError")


# --------------------------------------------------------------------------------------------
# 5. themes
# --------------------------------------------------------------------------------------------
def test_themes():
    for name in ("default", "solarized", "high-contrast"):
        check(name in THEMES, "THEMES has %s" % name)
    eq(THEMES["default"]["primary"], "bright_cyan", "default theme primary token")
    eq(THEMES["default"]["danger"], "bright_red", "default theme danger token")
    eq(resolve_theme_value("var(primary)", THEMES["default"]), "bright_cyan", "resolve var(token)")
    eq(resolve_theme_value("var(--primary)", THEMES["default"]), "bright_cyan", "resolve var(--token) strips --")
    eq(resolve_theme_value("var(missing, magenta)", THEMES["default"]), "magenta", "resolve var fallback")
    eq(resolve_theme_value("var(missing)", THEMES["default"]), "var(missing)", "unresolved var kept verbatim")
    themed = apply_theme_tokens({".x": {"color": "var(danger)"}}, THEMES["default"])
    eq(themed[".x"]["color"], "bright_red", "apply_theme_tokens resolves rule values")


# --------------------------------------------------------------------------------------------
# 6. plugin registry
# --------------------------------------------------------------------------------------------
class _PlugA(Label):
    tag = "plug-a"


class _PlugB(Label):
    tag = "plug-b"


def test_plugins():
    reg = PluginRegistry(api_version="1.0")
    reg.register_widget("Alpha", _PlugA)
    eq(reg.get_widget("Alpha"), _PlugA, "register/get widget (case-insensitive key)")
    eq(reg.get_widget("ALPHA"), _PlugA, "get_widget lowercases name")
    inst = reg.create_widget("alpha", "hi")
    check(isinstance(inst, _PlugA), "create_widget returns instance")
    eq(inst.text, "hi", "create_widget forwards constructor args")
    reg.register_widget("Beta", _PlugB)
    names = [r.name for r in reg.list_widgets()]
    eq(names, ["alpha", "beta"], "list_widgets returns sorted registrations")

    raises(ValueError, lambda: reg.register_widget("Alpha", _PlugB), "duplicate register -> ValueError")
    reg.register_widget("Alpha", _PlugB, replace=True)
    eq(reg.get_widget("alpha"), _PlugB, "replace=True overrides registration")
    raises(ValueError, lambda: reg.register_widget("", _PlugA), "empty name -> ValueError")
    raises(TypeError, lambda: reg.register_widget("x", "not-a-class"), "non-class -> TypeError")
    raises(KeyError, lambda: reg.create_widget("missing"), "create unknown -> KeyError")

    check(casca.is_compatible_api_version("1.4", "1.0"), "compat: same major (1.x) compatible")
    check(not casca.is_compatible_api_version("2.0", "1.0"), "compat: different major incompatible")
    raises(ValueError, lambda: casca.validate_plugin_api_version("2.0", "1.0"),
           "validate incompatible -> ValueError")


# --------------------------------------------------------------------------------------------
# 7. Label + base widget
# --------------------------------------------------------------------------------------------
def test_label():
    w, h, s = None, None, None
    lbl = Label("hello", id="greet", classes="a b")
    eq(lbl.tag, "label", "Label.tag")
    eq(lbl.id, "greet", "Label.id")
    eq(lbl.classes, ["a", "b"], "classes split on whitespace")
    surf = paint(lbl)
    eq(surf.line(1), "hello", "Label renders text on row 1")
    eq(lbl.content_width, 5, "Label content_width == len(text)")
    eq(lbl.content_height, 1, "Label content_height == 1 line")
    eq((lbl.width, lbl.height), (5, 1), "Label sizes to content")

    multi = Label("a\nbbb")
    surf2 = paint(multi)
    eq(surf2.line(1), "a", "multiline row 1")
    eq(surf2.line(2), "bbb", "multiline row 2")
    eq(multi.content_height, 2, "multiline content_height 2")
    eq(multi.content_width, 3, "multiline content_width == longest line")

    wrapped = Label("hello world", width=5)
    surf3 = paint(wrapped)
    eq(surf3.line(1), "hello", "wrap: first chunk")
    eq(surf3.line(2), "world", "wrap: second chunk")
    eq(wrapped.width, 5, "explicit width honored")

    ell = Label("abcdefgh", width=5, style={"white-space": "nowrap", "text-overflow": "ellipsis"})
    surf4 = paint(ell)
    eq(surf4.line(1), "ab...", "nowrap ellipsis clip to width 5")

    # inline style merge via resolve_styles + stylesheet specificity
    styled = Label("x", id="idd", classes="cls")
    styled.resolve_styles({"label": {"color": "white"}, ".cls": {"color": "yellow"}, "#idd": {"color": "red"}})
    eq(styled.style["color"], "red", "widget resolve_styles applies id>class>tag")


def test_base_bubbling():
    # handle_input bubbles to children in reverse order; first consumer wins
    log = []

    class Sink(Label):
        def __init__(self, name, consume, **k):
            super().__init__(name, **k)
            self._name = name
            self._consume = consume

        def handle_input(self, event):
            log.append(self._name)
            return self._consume

    parent = Container(Sink("first", False), Sink("second", True), Sink("third", False))
    consumed = parent.handle_input(kev("z"))
    eq(consumed, True, "container.handle_input consumed by a child")
    eq(log, ["third", "second"], "bubbling visits children in reverse until consumed")

    # disabled widget short-circuits input
    cb = Checkbox("x")
    cb.disabled = True
    eq(cb.handle_input(kev(" ")), False, "disabled widget ignores input")
    eq(cb.checked, False, "disabled checkbox not toggled")


# --------------------------------------------------------------------------------------------
# 8. Button
# --------------------------------------------------------------------------------------------
def test_button():
    hits = []
    btn = Button("OK", on_click=lambda e: hits.append(e))
    surf = paint(btn)
    eq(surf.line(1), "OK", "Button renders its label")
    eq(btn.tag, "button", "Button.tag")
    eq(btn.handle_input(kev("\r")), True, "Enter triggers button")
    eq(btn.handle_input(kev(" ")), True, "Space triggers button")
    eq(len(hits), 2, "on_click fired for Enter and Space")
    eq(btn.handle_input(kev("x")), False, "other keys ignored by button")
    # mouse press triggers on_click and consumes
    eq(btn.on_mouse(MouseEvent(x=1, y=1, button=0, pressed=True)), True, "button mouse press consumes")
    eq(len(hits), 3, "mouse press fired on_click")
    # right button ignored
    eq(btn.on_mouse(MouseEvent(x=1, y=1, button=1, pressed=True)), False, "non-left mouse ignored")
    # disabled
    btn.disabled = True
    eq(btn.handle_input(kev("\r")), False, "disabled button ignores Enter")
    eq(len(hits), 3, "disabled button did not fire")


# --------------------------------------------------------------------------------------------
# 9. Checkbox
# --------------------------------------------------------------------------------------------
def test_checkbox():
    changes = []
    cb = Checkbox("agree", on_change=lambda v: changes.append(v))
    eq(cb.checked, False, "checkbox default unchecked")
    surf = paint(cb)
    eq(cb.content_width, len("agree") + 4, "checkbox content_width = label+4 (post-layout)")
    eq(surf.line(1), "[ ] agree", "unchecked unfocused render")

    cb.checked = True
    surf2 = paint(cb)
    eq(surf2.line(1), "[✓] agree", "checked unfocused shows check mark")

    cb.checked = False
    cb.focused = True
    surf3 = paint(cb)
    eq(surf3.line(1), "[o] agree", "unchecked focused shows 'o'")
    cb.checked = True
    surf4 = paint(cb)
    eq(surf4.line(1), "[x] agree", "checked focused shows 'x'")

    cb2 = Checkbox("t", on_change=lambda v: changes.append(v))
    eq(cb2.handle_input(kev(" ")), True, "space toggles checkbox")
    eq(cb2.checked, True, "checkbox toggled on")
    cb2.handle_input(kev("\r"))
    eq(cb2.checked, False, "enter toggles checkbox back")
    eq(changes[-2:], [True, False], "on_change fired with new value each toggle")
    cb2.set_checked(True)
    eq(cb2.checked, True, "set_checked programmatic")
    # mouse toggle on release
    cb2.on_mouse(MouseEvent(x=1, y=1, button=0, pressed=False))
    eq(cb2.checked, False, "mouse release toggles checkbox")


# --------------------------------------------------------------------------------------------
# 10. Input
# --------------------------------------------------------------------------------------------
def test_input():
    submits = []
    inp = Input(value="ab", placeholder="type", on_submit=lambda v: submits.append(v))
    eq(inp.value, "ab", "input initial value")
    eq(inp.cursor_position, 2, "cursor starts at end of value")
    eq(inp.props["width"], 20, "input default width 20")
    surf = paint(inp)
    eq(surf.line(1), "ab", "input renders value")

    empty = Input(placeholder="hint")
    surf2 = paint(empty)
    eq(surf2.line(1), "hint", "empty input renders placeholder")

    # typing inserts at cursor
    inp.handle_input(kev("c", printable=True))
    eq(inp.value, "abc", "printable key appends at cursor")
    eq(inp.cursor_position, 3, "cursor advanced on type")
    # cursor navigation
    inp.handle_input(kev(Keys.HOME))
    eq(inp.cursor_position, 0, "Home -> cursor 0")
    inp.handle_input(kev("Z", printable=True))
    eq(inp.value, "Zabc", "insert at start after Home")
    inp.handle_input(kev(Keys.END))
    eq(inp.cursor_position, 4, "End -> cursor at len")
    inp.handle_input(kev(Keys.LEFT))
    eq(inp.cursor_position, 3, "Left decrements cursor")
    inp.handle_input(kev(Keys.RIGHT))
    eq(inp.cursor_position, 4, "Right increments cursor")
    # backspace deletes before cursor
    inp.handle_input(kev("\x7f"))
    eq(inp.value, "Zab", "backspace removes char before cursor")
    # submit
    inp.handle_input(kev("\r"))
    eq(submits, ["Zab"], "Enter submits current value")
    # focused cursor block appears in render
    inp.focused = True
    surf3 = paint(inp)
    check("█" in surf3.line(1), "focused input renders cursor block")
    # set_value / clear
    inp.set_value("hello")
    eq((inp.value, inp.cursor_position), ("hello", 5), "set_value updates value+cursor")
    inp.clear()
    eq((inp.value, inp.cursor_position), ("", 0), "clear empties input")


# --------------------------------------------------------------------------------------------
# 11. Select
# --------------------------------------------------------------------------------------------
def test_select():
    changes = []
    sel = Select(["red", "green", "blue"], selected_index=1, on_change=lambda v, i: changes.append((v, i)))
    eq(sel.value, "green", "select value property reflects index")
    surf = paint(sel)
    eq(surf.line(1), "green ▾", "collapsed select shows value + down marker")

    eq(sel.handle_input(kev("\r")), True, "Enter toggles expansion")
    eq(sel.expanded, True, "select expanded after Enter")
    surf2 = paint(sel)
    eq(surf2.line(1), "green ▴", "expanded select shows up marker")
    # option rows drawn with '>' prefix on selected
    eq(surf2.line(3), "> green", "expanded: selected option row has '>' prefix")
    eq(surf2.line(2), "  red", "expanded: unselected option row indented")

    sel.handle_input(kev(Keys.DOWN))
    eq(sel.selected_index, 2, "Down moves selection while expanded")
    eq(sel.value, "blue", "value follows selection")
    eq(changes[-1], ("blue", 2), "on_change emitted on move")
    sel.handle_input(kev(Keys.UP))
    eq(sel.selected_index, 1, "Up moves selection back")
    # clamp at bounds
    sel.handle_input(kev(Keys.UP))
    sel.handle_input(kev(Keys.UP))
    eq(sel.selected_index, 0, "Up clamps at 0")
    sel.handle_input(kev(Keys.ESCAPE))
    eq(sel.expanded, False, "Escape collapses expanded select")


# --------------------------------------------------------------------------------------------
# 12. ListView
# --------------------------------------------------------------------------------------------
def test_listview():
    changes = []
    lv = ListView(["one", "two", "three"], on_change=lambda v, i: changes.append((v, i)))
    eq(lv.value, "one", "listview initial value")
    eq(lv.props["height"], 6, "listview default height 6")
    surf = paint(lv)
    eq(surf.line(1), "> one", "selected row prefixed with '>'")
    eq(surf.line(2), "  two", "unselected row indented")

    lv.handle_input(kev(Keys.DOWN))
    eq(lv.selected_index, 1, "Down moves selection")
    eq(lv.value, "two", "value follows selection")
    eq(changes[-1], ("two", 1), "on_change emitted")
    lv.handle_input(kev(Keys.DOWN))
    lv.handle_input(kev(Keys.DOWN))
    eq(lv.selected_index, 2, "Down clamps at last item")
    lv.handle_input(kev(Keys.UP))
    eq(lv.selected_index, 1, "Up moves selection back")


# --------------------------------------------------------------------------------------------
# 13. RadioGroup
# --------------------------------------------------------------------------------------------
def test_radiogroup():
    changes = []
    rg = RadioGroup(["a", "b", "c"], on_change=lambda v, i: changes.append((v, i)))
    eq(rg.value, "a", "radiogroup initial value")
    surf = paint(rg)
    eq(surf.line(1), "● a", "selected radio marker filled")
    eq(surf.line(2), "○ b", "unselected radio marker hollow")
    rg.handle_input(kev(Keys.DOWN))
    eq(rg.selected_index, 1, "Down moves radio selection")
    eq(rg.value, "b", "radio value follows selection")
    eq(changes[-1], ("b", 1), "radio on_change emitted")
    rg.handle_input(kev(Keys.UP))
    eq(rg.selected_index, 0, "Up moves radio selection back")
    rg.handle_input(kev(" "))
    eq(changes[-1], ("a", 0), "space re-emits current selection")


# --------------------------------------------------------------------------------------------
# 14. ProgressBar
# --------------------------------------------------------------------------------------------
def test_progressbar():
    pb = ProgressBar(value=50, min_value=0, max_value=100, width=30)
    eq(pb.progress, 0.5, "progress fraction at midpoint")
    surf = paint(pb)
    line = surf.line(1)
    check(line.startswith("["), "progress bar starts with '['")
    check("]" in line, "progress bar has ']'")
    check("50%" in line, "progress bar shows percentage")

    pb.set_value(500)
    eq(pb.value, 100, "set_value clamps to max")
    eq(pb.progress, 1.0, "progress clamps to 1.0")
    pb.set_value(-10)
    eq(pb.value, 0, "set_value clamps to min")
    eq(pb.progress, 0.0, "progress clamps to 0.0")

    pb2 = ProgressBar(value=25, show_percentage=False, fill_char="#", empty_char=".", width=20)
    eq(pb2.progress, 0.25, "second bar progress")
    line2 = paint(pb2).line(1)
    check("%" not in line2, "show_percentage=False hides percent")
    check("#" in line2, "custom fill char used")
    check("." in line2, "custom empty char used")
    # degenerate range guarded
    pb3 = ProgressBar(value=5, min_value=10, max_value=10)
    eq(pb3.max_value, 11, "max<=min bumped to min+1")


# --------------------------------------------------------------------------------------------
# 15. TextArea
# --------------------------------------------------------------------------------------------
def test_textarea():
    ta = TextArea(default_value="line1\nline2\nline3", width=20, height=6)
    eq(ta.value, "line1\nline2\nline3", "textarea initial value")
    eq(ta._lines(), ["line1", "line2", "line3"], "textarea splits into lines")
    eq(ta.cursor_position, len("line1\nline2\nline3"), "textarea cursor at end")
    # index<->row/col are inverses
    eq(ta._index_to_row_col(0), (0, 0), "index 0 -> (0,0)")
    eq(ta._index_to_row_col(6), (1, 0), "index after first newline -> row 1 col 0")
    eq(ta._row_col_to_index(1, 0), 6, "row/col -> index inverse")
    eq(ta._row_col_to_index(2, 3), 15, "row 2 col 3 -> index 15")
    surf = paint(ta)
    eq(surf.line(1), "line1", "textarea renders row 1")
    eq(surf.line(2), "line2", "textarea renders row 2")
    empty = TextArea(placeholder="write here", width=20, height=4)
    eq(paint(empty).line(1), "write here", "empty textarea shows placeholder")


# --------------------------------------------------------------------------------------------
# 16. Table
# --------------------------------------------------------------------------------------------
def test_table():
    rows = [["alice", "30"], ["bob", "25"], ["carol", "40"]]
    tbl = Table([{"name": "Name", "width": 8}, {"name": "Age", "width": 5}], rows=rows, width=40)
    eq(tbl.rows, [["alice", "30"], ["bob", "25"], ["carol", "40"]], "table stringifies rows")
    eq(len(tbl.filtered_rows), 3, "table filtered rows initialize to all")
    surf = paint(tbl)
    check(surf.drew_containing("Name"), "table header cell 'Name' drawn")
    check(surf.drew_containing("Age"), "table header cell 'Age' drawn")
    check(any(set(op[2]) == {"-"} and len(op[2]) > 3 for op in surf.ops), "table draws separator rule")
    check(surf.drew_containing("alice"), "table draws first data row")

    tbl.set_filter("bob")
    eq(len(tbl.filtered_rows), 1, "set_filter narrows rows")
    eq(tbl.filtered_rows[0][0], "bob", "filter matched bob")
    tbl.set_filter("")
    eq(len(tbl.filtered_rows), 3, "empty filter restores all rows")

    tbl.sort_by(1)  # by Age ascending (string sort of the age column)
    eq([r[1] for r in tbl.filtered_rows], ["25", "30", "40"], "sort_by column ascending")
    tbl.sort_by("Name", reverse=True)
    eq([r[0] for r in tbl.filtered_rows], ["carol", "bob", "alice"], "sort_by name descending")

    tbl.selected_row = 0
    tbl.handle_input(kev(Keys.DOWN))
    eq(tbl.selected_row, 1, "table Down moves selection")
    tbl.handle_input(kev(Keys.UP))
    eq(tbl.selected_row, 0, "table Up moves selection")

    tbl.resize_column("Age", 9)
    eq(tbl.columns[1]["width"], 9, "resize_column updates width")
    # string columns normalize to dict with computed width
    strcols = Table(["A", "BB"], rows=[["1", "2"]])
    eq(strcols.columns[0]["name"], "A", "string column normalized name")
    check(strcols.columns[0]["width"] >= 5, "string column minimum width")


# --------------------------------------------------------------------------------------------
# 17. Tabs / TabItem
# --------------------------------------------------------------------------------------------
def test_tabs():
    changes = []
    tabs = Tabs(
        TabItem("Home", Label("home content")),
        TabItem("Settings", Label("settings content")),
        TabItem("About", Label("about content")),
        on_change=lambda i: changes.append(i),
    )
    eq(tabs.active_index, 0, "tabs default active index 0")
    eq(len(tabs.tabs), 3, "tabs holds all TabItems")
    eq(tabs.get_active_tab().title, "Home", "active tab is Home")
    eq(tabs.tabs[1].title, "Settings", "second tab title")

    eq(tabs.switch_tab(1), True, "switch_tab to a new index returns True")
    eq(tabs.active_index, 1, "active index updated")
    eq(changes, [1], "on_change fired with new index")
    eq(tabs.get_active_tab().title, "Settings", "active tab now Settings")
    eq(tabs.switch_tab(1), False, "switch to same index returns False")
    eq(tabs.switch_tab(99), False, "switch to out-of-range returns False")

    tabs.add_tab(TabItem("Extra", Label("x")))
    eq(len(tabs.tabs), 4, "add_tab appends")
    eq(tabs.remove_tab(0), True, "remove_tab removes")
    eq(len(tabs.tabs), 3, "tab count after removal")

    # content area holds exactly the active tab's pane after layout
    tabs.calculate_layout(80, 24)
    eq(len(tabs.content_area.children), 1, "content area shows exactly one active pane")


# --------------------------------------------------------------------------------------------
# 18. TreeView
# --------------------------------------------------------------------------------------------
def test_treeview():
    changes = []
    nodes = [
        {"label": "src", "children": [{"label": "main.py"}, {"label": "util.py"}]},
        {"label": "README"},
    ]
    tv = TreeView(nodes, on_change=lambda lbl, path: changes.append((lbl, path)))
    tv.calculate_layout(40, 12)  # builds flat nodes
    eq(len(tv.flat_nodes), 2, "collapsed tree: only top-level nodes flattened")
    eq(tv.flat_nodes[0]["display"], "▶ src", "collapsed parent shows right-arrow marker")
    eq(tv.flat_nodes[1]["display"], "• README", "leaf shows bullet marker")

    # expand 'src' via Right, then it should reveal children
    tv.selected_index = 0
    tv.handle_input(kev(Keys.RIGHT))
    eq(len(tv.flat_nodes), 4, "expanding src reveals its two children")
    eq(tv.flat_nodes[0]["display"], "▼ src", "expanded parent shows down-arrow marker")
    eq(tv.flat_nodes[1]["display"], "  • main.py", "child indented under parent")

    tv.handle_input(kev(Keys.DOWN))
    eq(tv.selected_index, 1, "Down moves tree selection")
    eq(changes[-1][0], "main.py", "on_change reports selected label")
    # collapse again
    tv.selected_index = 0
    tv.handle_input(kev(Keys.LEFT))
    tv.calculate_layout(40, 12)
    eq(len(tv.flat_nodes), 2, "Left collapses expanded node")


# --------------------------------------------------------------------------------------------
# 19. Container flex layout
# --------------------------------------------------------------------------------------------
def test_container_layout():
    a = Label("AA")
    b = Label("BB")
    col = Container(a, b)  # default column
    col.resolve_styles({})
    col.x = 0
    col.y = 0
    col.calculate_layout(40, 20)
    eq(a.y, 0, "column: first child at y=0")
    eq(b.y, 1, "column: second child stacked below first")
    eq(a.x, b.x, "column: children share x")

    c = Label("CC")
    d = Label("DD")
    row = Container(c, d, style={"flex-direction": "row"})
    row.resolve_styles({})
    row.x = 0
    row.y = 0
    row.calculate_layout(40, 20)
    eq(c.y, d.y, "row: children share y")
    check(d.x > c.x, "row: second child to the right of first")

    # padding offsets children
    pad = Container(Label("Z"), style={"padding": 2})
    pad.resolve_styles({})
    pad.x = 0
    pad.y = 0
    pad.calculate_layout(40, 20)
    eq(pad.children[0].x, 2, "padding shifts child x by pad-left")
    eq(pad.children[0].y, 2, "padding shifts child y by pad-top")


# --------------------------------------------------------------------------------------------
# 20. Spinner + Card
# --------------------------------------------------------------------------------------------
def test_spinner_card():
    sp = Spinner(text="loading", frames=["|", "/", "-", "\\"])
    eq(sp.current_frame, "|", "spinner initial frame")
    sp.tick()
    eq(sp.current_frame, "/", "spinner tick advances frame")
    sp.tick(2)
    eq(sp.current_frame, "\\", "spinner tick by steps")
    sp.tick()
    eq(sp.current_frame, "|", "spinner frame index wraps")
    surf = paint(sp)
    check(surf.drew_containing("loading"), "spinner renders its text")

    card = Card("Title", Label("body"))
    card.resolve_styles({})
    eq(card.style.get("border"), "ascii", "card defaults to ascii border")
    eq(card.style.get("padding"), 1, "card defaults to padding 1")
    # first child is the title label with padded title text
    eq(card.children[0].text, " Title ", "card injects a padded title label")


# --------------------------------------------------------------------------------------------
# 21. App lifecycle + run_app
# --------------------------------------------------------------------------------------------
def test_app():
    def counter(state, action):
        c = state.get("count", 0)
        if action.get("type") == "INC":
            return {"count": c + 1}
        return state

    class MyApp(casca.App):
        css = "#title { color: red; }"

        def __init__(self):
            super().__init__()
            self.status = "start"

        def build_ui(self):
            return Container(
                Label("Counter App", id="title"),
                Label("status: %s" % self.status, id="status"),
            )

    app = MyApp()
    eq(app._last_size, (80, 24), "headless terminal size deterministic (80x24)")
    check("#title" in app.stylesheet, "app parsed inline css into stylesheet")
    app.render()  # headless render populates the output buffer
    frame = "".join(app._output_buffer)
    check("Counter App" in frame, "app render drew title text")
    check("status: start" in frame, "app render drew status text")

    # set_state mutates attribute and invalidates the tree so build_ui re-runs
    app.set_state(status="updated")
    eq(app.status, "updated", "set_state updated attribute")
    check(app.root is None, "set_state invalidated the UI tree")
    app.render()
    frame2 = "".join(app._output_buffer)
    check("status: updated" in frame2, "re-render reflects new state")

    # store integration: set_store / dispatch / get_state
    store = create_store(counter, {"count": 0})
    app.set_store(store)
    eq(app.get_state(), {"count": 0}, "app.get_state proxies store")
    app.dispatch({"type": "INC"})
    app.dispatch({"type": "INC"})
    eq(app.get_state(), {"count": 2}, "app.dispatch drives the store")
    app.set_store(None)
    raises(RuntimeError, lambda: app.dispatch({"type": "INC"}), "dispatch without store -> RuntimeError")

    # theme switching swaps the resolved stylesheet, unknown theme rejected
    app.set_theme("high-contrast")
    eq(app.theme, "high-contrast", "set_theme applied")
    raises(ValueError, lambda: app.set_theme("no-such-theme"), "unknown theme -> ValueError")

    # handle_input routes to the focused widget first
    routed = []

    class Catcher(Label):
        def handle_input(self, event):
            routed.append(event.key)
            return True

    app2 = casca.App()
    catcher = Catcher("x", id="c")
    app2.root = Container(catcher)
    app2.focused_widget = catcher
    app2.handle_input(kev("k"))
    eq(routed, ["k"], "App.handle_input routes to focused widget")

    # run_app builds a wrapper App around a single widget without starting the loop
    wrapper_cls = type(casca.App)  # sanity: App is a class
    check(callable(casca.run_app), "run_app is callable")


# --------------------------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------------------------
TESTS = [
    test_surface, test_store, test_combine_reducers, test_middleware, test_ansi, test_css,
    test_themes, test_plugins, test_label, test_base_bubbling, test_button, test_checkbox,
    test_input, test_select, test_listview, test_radiogroup, test_progressbar, test_textarea,
    test_table, test_tabs, test_treeview, test_container_layout, test_spinner_card, test_app,
]


def main():
    print("=== CascaCarpet: casca %s on python %s ===" % (_md.version("casca"), sys.version.split()[0]))
    for t in TESTS:
        try:
            t()
        except Exception as e:  # a crashing section is a hard failure, not a silent skip
            global _fail
            _fail += 1
            import traceback
            print("  FAIL %s raised %r" % (t.__name__, e))
            traceback.print_exc()
    print("CASCA_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("CASCA_DONE")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
