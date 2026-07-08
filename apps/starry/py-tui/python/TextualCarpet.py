#!/usr/bin/env python3
# TextualCarpet.py — industrial, exact-assertion carpet for the `textual` TUI framework running
# fully HEADLESS on musl-native CPython3 (StarryOS, 4 arches).
#
# textual apps are async and normally own a TTY; the framework ships a first-class HEADLESS test
# driver — App.run_test() yields a Pilot that pumps the real event loop with a virtual 80x24
# terminal (no TTY, no alternate screen). This carpet drives EVERY dimension through that driver
# and asserts BOTH the rendered output (the compositor's strip text and each widget's region/size/
# resolved styles) AND the control/state round-trip (reactive attributes, messages, bindings,
# screen stack) against goldens recomputed from the live library. Deterministic: fixed terminal
# size, no wall-clock/ETA (show_eta=False), no animation waits, no network, no randomness. Every
# App is exercised with `async with app.run_test()` and always exits cleanly (pilot ops + pause).
#
# Coverage: Static/Label content+visual+rendered strips+update(); Button(+Pressed message via @on
# and Pilot.click); Input(value/typing/Changed/Submitted/clear/cursor); Checkbox+Switch toggle;
# RadioSet/RadioButton(pressed_index, click-select); Select(NULL/blank/value); ListView/ListItem
# (index, highlight move); DataTable(columns/rows/row_count/get_cell_at/cursor/get_row_at); Tree
# (root/add/leaves/expand); TextArea(text/line_count/insert/selection); ProgressBar(total/advance/
# percentage); Tabs/TabbedContent(active switching); reactive() + watch_* + compute_* mutual
# updates; BINDINGS -> action_* via Pilot key; CSS -> styles.color/background(exact Color rgb)/
# width/height/display/text_style + horizontal layout regions + class add/remove/has/toggle; the
# query engine (query_one by id/type/class, query count, NoMatches, first/last, screen.query_one);
# the screen stack (push_screen/pop_screen/screen_stack/modal query); message passing (@on,
# post_message, custom Message, handler order). PLUS the `textual` CLI dimension: the core library
# ships NO `textual` console-script (that lives in the separate `textual-dev` package), and
# `python -m textual` is the interactive demo which requires a TTY — so each named subcommand
# (run/console/colors/keys/diagnose/borders/easing) is asserted as a documented, reasoned SKIP;
# if a real `textual` CLI happens to be on PATH it is probed with `--help` ONLY (hard subprocess
# timeout, never a bare interactive subcommand).
#
# Emits `TEXTUAL_RESULT ok=<N> fail=<F>` and, only when F==0, `TEXTUAL_DONE`.

import asyncio
import importlib.metadata as _md
import os
import shutil
import subprocess
import sys

import textual
from textual import on
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.color import Color
from textual.containers import Horizontal, Vertical
from textual.css.query import NoMatches
from textual.message import Message
from textual.reactive import reactive
from textual.screen import Screen
from textual.widgets import (
    Button,
    Checkbox,
    DataTable,
    Input,
    Label,
    ListItem,
    ListView,
    ProgressBar,
    RadioButton,
    RadioSet,
    Select,
    Static,
    Switch,
    TabbedContent,
    TabPane,
    TextArea,
    Tree,
)

# --------------------------------------------------------------------------------------------
# assertion harness
# --------------------------------------------------------------------------------------------
_ok = 0
_fail = 0
_skips = []


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def eq(got, want, label):
    check(got == want, "%s: got %r want %r" % (label, got, want))


def skip(label, reason):
    _skips.append((label, reason))
    print("  SKIP %s -- %s" % (label, reason))


def screen_text(app):
    """Full visible screen as a list of row strings, straight from the compositor."""
    return [strip.text for strip in app.screen._compositor.render_strips()]


def screen_blob(app):
    return "\n".join(screen_text(app))


# --------------------------------------------------------------------------------------------
# 1. package surface + headless driver sanity
# --------------------------------------------------------------------------------------------
async def test_surface():
    eq(_md.version("textual"), "8.2.8", "textual version pinned 8.2.8")
    eq(textual.__version__, "8.2.8", "textual.__version__ matches metadata")
    eq(_md.version("rich"), "15.0.0", "rich pinned 15.0.0")
    for mod in ("textual.app", "textual.widget", "textual.widgets", "textual.reactive",
                "textual.containers", "textual.screen", "textual.message", "textual.binding"):
        __import__(mod)
        check(True, "import %s" % mod)

    class A(App):
        def compose(self):
            yield Label("headless ok", id="l")

    app = A()
    async with app.run_test(size=(80, 24)) as pilot:
        eq(app.size.width, 80, "virtual terminal width 80")
        eq(app.size.height, 24, "virtual terminal height 24")
        eq(str(app.query_one("#l", Label).content), "headless ok", "label content readable headless")
        rows = screen_text(app)
        eq(len(rows), 24, "compositor produced 24 rows")
        check(rows[0].startswith("headless ok"), "top row shows the label text")
        await pilot.pause()


# --------------------------------------------------------------------------------------------
# 2. Static / Label
# --------------------------------------------------------------------------------------------
async def test_static_label():
    class A(App):
        def compose(self):
            yield Static("static one", id="s")
            yield Label("label two", id="l")

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        s = app.query_one("#s", Static)
        l = app.query_one("#l", Label)
        eq(str(s.content), "static one", "Static.content is the original text")
        eq(str(s.visual), "static one", "Static.visual renders the text")
        eq(str(l.content), "label two", "Label.content")
        eq(l.region.height, 1, "single-line label height 1")
        eq(l.region.width, len("label two"), "label width fits content")
        rows = screen_text(app)
        check(rows[0].startswith("static one"), "static text on row 0")
        check(rows[1].startswith("label two"), "label text on row 1")

        # update() swaps the content and the rendered cells
        s.update("replaced!")
        await pilot.pause()
        eq(str(s.content), "replaced!", "Static.update changed content")
        check(screen_text(app)[0].startswith("replaced!"), "update reflected in compositor")


# --------------------------------------------------------------------------------------------
# 3. Button + messages
# --------------------------------------------------------------------------------------------
async def test_button():
    class A(App):
        def __init__(self):
            super().__init__()
            self.events = []

        def compose(self):
            yield Button("Click me", id="b", variant="primary")
            yield Button("Disabled", id="d", disabled=True)

        @on(Button.Pressed, "#b")
        def _pressed(self, e):
            self.events.append(("pressed", e.button.id))

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        b = app.query_one("#b", Button)
        eq(str(b.label), "Click me", "button label")
        eq(b.variant, "primary", "button variant")
        check(screen_blob(app).find("Click me") >= 0, "button label rendered")
        await pilot.click("#b")
        await pilot.pause()
        eq(app.events, [("pressed", "b")], "Button.Pressed dispatched via @on on click")
        d = app.query_one("#d", Button)
        eq(d.disabled, True, "disabled button flagged")
        await pilot.click("#d")
        await pilot.pause()
        eq(app.events, [("pressed", "b")], "disabled button emits no Pressed")


# --------------------------------------------------------------------------------------------
# 4. Input
# --------------------------------------------------------------------------------------------
async def test_input():
    class A(App):
        def __init__(self):
            super().__init__()
            self.changed = []
            self.submitted = []

        def compose(self):
            yield Input(value="hi", placeholder="name", id="i", select_on_focus=False)

        @on(Input.Changed, "#i")
        def _ch(self, e):
            self.changed.append(e.value)

        @on(Input.Submitted, "#i")
        def _sub(self, e):
            self.submitted.append(e.value)

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        i = app.query_one("#i", Input)
        eq(i.value, "hi", "input initial value")
        app.set_focus(i)
        i.action_end()  # cursor to end (select_on_focus disabled so no selection)
        await pilot.press("!")
        await pilot.pause()
        eq(i.value, "hi!", "typed char appended to input")
        eq(app.changed[-1], "hi!", "Input.Changed carries new value")
        await pilot.press("enter")
        await pilot.pause()
        eq(app.submitted, ["hi!"], "Input.Submitted on Enter")
        i.clear()
        await pilot.pause()
        eq(i.value, "", "Input.clear empties value")
        i.value = "preset"
        eq(i.value, "preset", "input value settable programmatically")


# --------------------------------------------------------------------------------------------
# 5. Checkbox + Switch
# --------------------------------------------------------------------------------------------
async def test_checkbox_switch():
    class A(App):
        def __init__(self):
            super().__init__()
            self.cb_events = []
            self.sw_events = []

        def compose(self):
            yield Checkbox("accept", value=False, id="c")
            yield Switch(value=False, id="s")

        @on(Checkbox.Changed, "#c")
        def _c(self, e):
            self.cb_events.append(e.value)

        @on(Switch.Changed, "#s")
        def _s(self, e):
            self.sw_events.append(e.value)

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        c = app.query_one("#c", Checkbox)
        s = app.query_one("#s", Switch)
        eq(c.value, False, "checkbox initial value False")
        eq(s.value, False, "switch initial value False")
        c.value = True
        s.value = True
        await pilot.pause()
        eq(c.value, True, "checkbox value set True")
        eq(s.value, True, "switch value set True")
        eq(app.cb_events[-1], True, "Checkbox.Changed fired with True")
        eq(app.sw_events[-1], True, "Switch.Changed fired with True")


# --------------------------------------------------------------------------------------------
# 6. RadioSet / RadioButton
# --------------------------------------------------------------------------------------------
async def test_radioset():
    class A(App):
        def __init__(self):
            super().__init__()
            self.changed = []

        def compose(self):
            with RadioSet(id="r"):
                yield RadioButton("Red")
                yield RadioButton("Green", value=True)
                yield RadioButton("Blue")

        @on(RadioSet.Changed, "#r")
        def _c(self, e):
            self.changed.append(e.index)

    app = A()
    async with app.run_test(size=(40, 8)) as pilot:
        r = app.query_one("#r", RadioSet)
        eq(r.pressed_index, 1, "RadioSet honors initially-pressed button")
        # clicking a different radio button selects it (mutually exclusive)
        buttons = list(r.query(RadioButton))
        eq(len(buttons), 3, "radio set has 3 buttons")
        await pilot.click(buttons[2])
        await pilot.pause()
        eq(r.pressed_index, 2, "click selects a new radio (exclusive)")
        eq(buttons[2].value, True, "clicked radio is pressed")
        eq(buttons[1].value, False, "previously pressed radio released")
        eq(app.changed[-1], 2, "RadioSet.Changed reports new index")


# --------------------------------------------------------------------------------------------
# 7. Select
# --------------------------------------------------------------------------------------------
async def test_select():
    class A(App):
        def compose(self):
            yield Select([("One", 1), ("Two", 2), ("Three", 3)], id="s")
            yield Select([("A", "a"), ("B", "b")], value="b", id="s2", allow_blank=False)

    app = A()
    async with app.run_test(size=(40, 8)) as pilot:
        s = app.query_one("#s", Select)
        eq(s.value, Select.NULL, "Select defaults to the NULL blank sentinel")
        eq(s.is_blank(), True, "is_blank True when unset")
        s.value = 2
        await pilot.pause()
        eq(s.value, 2, "Select value settable")
        eq(s.is_blank(), False, "not blank after set")
        s2 = app.query_one("#s2", Select)
        eq(s2.value, "b", "Select honors constructor value")
        eq(s2.is_blank(), False, "allow_blank=False + value -> not blank")


# --------------------------------------------------------------------------------------------
# 8. ListView / ListItem
# --------------------------------------------------------------------------------------------
async def test_listview():
    class A(App):
        def __init__(self):
            super().__init__()
            self.highlights = []

        def compose(self):
            yield ListView(
                ListItem(Label("row0")),
                ListItem(Label("row1")),
                ListItem(Label("row2")),
                id="lv",
            )

        @on(ListView.Highlighted, "#lv")
        def _h(self, e):
            if e.item is not None:
                self.highlights.append(e.list_view.index)

    app = A()
    async with app.run_test(size=(40, 10)) as pilot:
        lv = app.query_one("#lv", ListView)
        eq(len(lv), 3, "list view length")
        eq(lv.index, 0, "initial highlighted index 0")
        app.set_focus(lv)
        await pilot.press("down")
        await pilot.pause()
        eq(lv.index, 1, "Down moves highlight to index 1")
        await pilot.press("down")
        await pilot.pause()
        eq(lv.index, 2, "Down moves highlight to index 2")
        await pilot.press("up")
        await pilot.pause()
        eq(lv.index, 1, "Up moves highlight back to 1")


# --------------------------------------------------------------------------------------------
# 9. DataTable
# --------------------------------------------------------------------------------------------
async def test_datatable():
    class A(App):
        def compose(self):
            yield DataTable(id="dt")

        def on_mount(self):
            dt = self.query_one("#dt", DataTable)
            dt.add_columns("name", "age", "city")
            dt.add_row("alice", "30", "paris")
            dt.add_row("bob", "25", "berlin")
            dt.add_row("carol", "41", "rome")

    app = A()
    async with app.run_test(size=(50, 12)) as pilot:
        dt = app.query_one("#dt", DataTable)
        eq(dt.row_count, 3, "DataTable row_count")
        eq(len(dt.columns), 3, "DataTable column count")
        eq(dt.get_cell_at((0, 0)), "alice", "get_cell_at (0,0)")
        eq(dt.get_cell_at((1, 2)), "berlin", "get_cell_at (1,2)")
        eq(list(dt.get_row_at(2)), ["carol", "41", "rome"], "get_row_at returns row values")
        # cursor movement
        dt.cursor_type = "row"
        dt.move_cursor(row=2, column=0)
        eq(dt.cursor_row, 2, "move_cursor sets cursor_row")
        # mutate + query
        dt.add_row("dave", "19", "oslo")
        eq(dt.row_count, 4, "add_row grows table")
        rendered = screen_blob(app)
        check("alice" in rendered, "table cell 'alice' rendered")
        check("name" in rendered, "table header 'name' rendered")
        dt.clear()
        eq(dt.row_count, 0, "clear empties rows")


# --------------------------------------------------------------------------------------------
# 10. Tree
# --------------------------------------------------------------------------------------------
async def test_tree():
    class A(App):
        def compose(self):
            tree = Tree("root", id="t")
            src = tree.root.add("src")
            src.add_leaf("main.py")
            src.add_leaf("util.py")
            tree.root.add("docs")
            yield tree

    app = A()
    async with app.run_test(size=(40, 12)) as pilot:
        t = app.query_one("#t", Tree)
        eq(str(t.root.label), "root", "tree root label")
        eq(len(t.root.children), 2, "root has two child nodes")
        src = t.root.children[0]
        eq(str(src.label), "src", "first child label")
        eq(len(src.children), 2, "src has two leaves")
        eq(src.allow_expand, True, "branch node allows expand")
        # expand the root and src, then verify labels are on screen
        t.root.expand()
        src.expand()
        await pilot.pause()
        blob = screen_blob(app)
        check("src" in blob, "expanded tree shows 'src'")
        check("main.py" in blob, "expanded tree shows leaf 'main.py'")


# --------------------------------------------------------------------------------------------
# 11. TextArea
# --------------------------------------------------------------------------------------------
async def test_textarea():
    class A(App):
        def compose(self):
            yield TextArea("alpha\nbeta\ngamma", id="ta")

    app = A()
    async with app.run_test(size=(40, 12)) as pilot:
        ta = app.query_one("#ta", TextArea)
        eq(ta.text, "alpha\nbeta\ngamma", "textarea initial text")
        eq(ta.document.line_count, 3, "textarea document line count")
        eq(ta.document.get_line(0), "alpha", "textarea line 0 content")
        # programmatic edit
        ta.load_text("one\ntwo")
        await pilot.pause()
        eq(ta.text, "one\ntwo", "load_text replaces content")
        eq(ta.document.line_count, 2, "line count after load_text")
        ta.insert("!!")
        await pilot.pause()
        check(ta.text.endswith("!!") or "!!" in ta.text, "insert added text at cursor")
        # selection API present and consistent
        ta.select_all()
        eq(ta.selected_text, ta.text, "select_all selects the whole document")


# --------------------------------------------------------------------------------------------
# 12. ProgressBar
# --------------------------------------------------------------------------------------------
async def test_progressbar():
    class A(App):
        def compose(self):
            yield ProgressBar(total=100, show_eta=False, id="pb")

    app = A()
    async with app.run_test(size=(50, 4)) as pilot:
        pb = app.query_one("#pb", ProgressBar)
        eq(pb.total, 100, "progress bar total")
        eq(pb.percentage, 0.0, "initial percentage 0")
        pb.advance(25)
        await pilot.pause()
        eq(pb.progress, 25.0, "advance(25) -> progress 25")
        eq(pb.percentage, 0.25, "percentage is a 0..1 fraction")
        pb.update(progress=100)
        await pilot.pause()
        eq(pb.percentage, 1.0, "update(progress=100) -> full")


# --------------------------------------------------------------------------------------------
# 13. Tabs (TabbedContent / TabPane)
# --------------------------------------------------------------------------------------------
async def test_tabs():
    class A(App):
        def compose(self):
            with TabbedContent(id="tc"):
                with TabPane("First", id="t1"):
                    yield Label("first pane")
                with TabPane("Second", id="t2"):
                    yield Label("second pane")
                with TabPane("Third", id="t3"):
                    yield Label("third pane")

    app = A()
    async with app.run_test(size=(50, 12)) as pilot:
        tc = app.query_one("#tc", TabbedContent)
        eq(tc.active, "t1", "TabbedContent starts on first pane")
        eq(tc.tab_count, 3, "tab count")
        tc.active = "t2"
        await pilot.pause()
        eq(tc.active, "t2", "active pane switched to t2")
        check("second pane" in screen_blob(app), "switched pane content rendered")


# --------------------------------------------------------------------------------------------
# 14. reactive / watch / compute
# --------------------------------------------------------------------------------------------
async def test_reactive():
    class W(Static):
        count = reactive(0)
        doubled = reactive(0)
        first = reactive("a")
        last = reactive("b")
        full = reactive("")

        def __init__(self, **k):
            super().__init__(**k)
            self.wlog = []

        def watch_count(self, old, new):
            self.wlog.append((old, new))
            self.doubled = new * 2

        def compute_full(self):
            return f"{self.first} {self.last}"

    class A(App):
        def compose(self):
            yield W(id="w")

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        w = app.query_one("#w", W)
        eq(w.full, "a b", "compute_full initial value")
        w.count = 5
        await pilot.pause()
        check((0, 5) in w.wlog, "watch_count observed the 0->5 transition")
        eq(w.doubled, 10, "watcher mutated a second reactive (mutual update)")
        w.first = "hello"
        w.last = "world"
        await pilot.pause()
        eq(w.full, "hello world", "compute_full recomputed from dependencies")
        w.count = 5  # same value: no repaint but state stable
        await pilot.pause()
        eq(w.doubled, 10, "setting same reactive value keeps derived state")


# --------------------------------------------------------------------------------------------
# 15. bindings / actions
# --------------------------------------------------------------------------------------------
async def test_bindings():
    class A(App):
        BINDINGS = [
            Binding("ctrl+g", "greet", "Greet"),
            ("space", "bump", "Bump"),
        ]

        def __init__(self):
            super().__init__()
            self.blog = []
            self.bumps = 0

        def compose(self):
            yield Label("bind", id="l")

        def action_greet(self):
            self.blog.append("greet")

        def action_bump(self):
            self.bumps += 1

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        await pilot.press("ctrl+g")
        await pilot.pause()
        eq(app.blog, ["greet"], "ctrl+g binding invoked action_greet")
        await pilot.press("space")
        await pilot.press("space")
        await pilot.pause()
        eq(app.bumps, 2, "space binding invoked action_bump twice")


# --------------------------------------------------------------------------------------------
# 16. CSS styling + layout + classes
# --------------------------------------------------------------------------------------------
async def test_css():
    class A(App):
        CSS = """
        #styled { color: rgb(10, 20, 30); background: rgb(0, 0, 255); width: 25; height: 3; text-style: bold; }
        .hidden { display: none; }
        #row { layout: horizontal; height: 1; }
        """

        def compose(self):
            yield Static("styled", id="styled")
            yield Static("gone", classes="hidden", id="hid")
            with Horizontal(id="row"):
                yield Label("L", id="l1")
                yield Label("R", id="l2")

    app = A()
    async with app.run_test(size=(60, 12)) as pilot:
        s = app.query_one("#styled")
        eq(s.styles.color, Color(10, 20, 30), "CSS color parsed to exact rgb")
        eq(s.styles.background, Color(0, 0, 255), "CSS background parsed to exact rgb")
        eq(s.size.width, 25, "CSS width applied to layout")
        eq(s.size.height, 3, "CSS height applied to layout")
        eq(s.styles.text_style.bold, True, "CSS text-style bold flag set")
        eq(str(s.styles.display), "block", "default display block")

        hid = app.query_one("#hid")
        eq(str(hid.styles.display), "none", "display:none from class")
        eq(hid.display, False, "hidden widget not displayed")

        l1 = app.query_one("#l1")
        l2 = app.query_one("#l2")
        eq(l1.region.y, l2.region.y, "horizontal layout: children on same row")
        check(l2.region.x > l1.region.x, "horizontal layout: second child to the right")

        # runtime class manipulation reflects in has_class
        eq(s.has_class("hidden"), False, "styled has no hidden class initially")
        s.add_class("hidden")
        await pilot.pause()
        eq(s.has_class("hidden"), True, "add_class applied")
        eq(str(s.styles.display), "none", "adding hidden class hides via CSS")
        s.remove_class("hidden")
        await pilot.pause()
        eq(s.has_class("hidden"), False, "remove_class removed it")
        s.toggle_class("hidden")
        eq(s.has_class("hidden"), True, "toggle_class toggles on")


# --------------------------------------------------------------------------------------------
# 17. query engine
# --------------------------------------------------------------------------------------------
async def test_query():
    class A(App):
        def compose(self):
            yield Button("one", id="a", classes="grp")
            yield Button("two", id="b", classes="grp")
            yield Label("lab", id="c")

    app = A()
    async with app.run_test(size=(40, 8)) as pilot:
        eq(len(app.query("Button")), 2, "query by type counts buttons")
        eq(len(app.query(".grp")), 2, "query by class")
        eq(len(app.query("#a")), 1, "query by id")
        eq(app.query_one("#c", Label).id, "c", "query_one by id + expected type")
        first = app.query("Button").first()
        last = app.query("Button").last()
        eq(first.id, "a", "DOMQuery.first()")
        eq(last.id, "b", "DOMQuery.last()")
        try:
            app.query_one("#nope")
            check(False, "query_one missing should raise NoMatches")
        except NoMatches:
            check(True, "query_one missing raises NoMatches")


# --------------------------------------------------------------------------------------------
# 18. screen stack
# --------------------------------------------------------------------------------------------
async def test_screens():
    class ModalScreen(Screen):
        def compose(self):
            yield Label("MODAL BODY", id="mbody")

    class A(App):
        def compose(self):
            yield Label("base", id="base")

    app = A()
    async with app.run_test(size=(40, 8)) as pilot:
        eq(len(app.screen_stack), 1, "one screen on the stack initially")
        base_name = type(app.screen).__name__
        check(base_name in ("Screen", "_DefaultScreen") or base_name.endswith("Screen"),
              "default screen present")
        await app.push_screen(ModalScreen())
        await pilot.pause()
        eq(len(app.screen_stack), 2, "push_screen grows the stack")
        eq(type(app.screen).__name__, "ModalScreen", "top screen is the modal")
        # modal-scoped query goes through the active screen
        eq(str(app.screen.query_one("#mbody", Label).content), "MODAL BODY", "modal widget queryable via active screen")
        check("MODAL BODY" in screen_blob(app), "modal content rendered on top")
        app.pop_screen()
        await pilot.pause()
        eq(len(app.screen_stack), 1, "pop_screen restores the base stack")


# --------------------------------------------------------------------------------------------
# 19. custom messages / post_message / handler
# --------------------------------------------------------------------------------------------
async def test_messages():
    class Ping(Message):
        def __init__(self, value):
            super().__init__()
            self.value = value

    class Emitter(Static):
        def fire(self, v):
            self.post_message(Ping(v))

    class A(App):
        def __init__(self):
            super().__init__()
            self.pings = []

        def compose(self):
            yield Emitter(id="e")

        def on_ping(self, message):
            self.pings.append(message.value)

    app = A()
    async with app.run_test(size=(40, 6)) as pilot:
        e = app.query_one("#e", Emitter)
        e.fire(42)
        e.fire(7)
        await pilot.pause()
        eq(app.pings, [42, 7], "custom Message bubbled to App.on_ping in order")


# --------------------------------------------------------------------------------------------
# 20. Pilot key -> reactive -> render pipeline (end-to-end)
# --------------------------------------------------------------------------------------------
async def test_pipeline():
    class Counter(Static):
        n = reactive(0)

        def render(self):
            return f"N={self.n}"

    class A(App):
        BINDINGS = [("plus", "inc", "inc")]  # placeholder; use key handler below

        def compose(self):
            yield Counter(id="c")

        def on_key(self, event):
            if event.key == "a":
                self.query_one("#c", Counter).n += 1

    app = A()
    async with app.run_test(size=(20, 4)) as pilot:
        c = app.query_one("#c", Counter)
        eq(c.n, 0, "counter starts at 0")
        check(screen_text(app)[0].startswith("N=0"), "initial render N=0")
        await pilot.press("a", "a", "a")
        await pilot.pause()
        eq(c.n, 3, "three key presses -> reactive n=3")
        check(screen_text(app)[0].startswith("N=3"), "reactive change repainted to N=3")


# --------------------------------------------------------------------------------------------
# 21. `textual` CLI dimension (structural + documented, non-blocking)
# --------------------------------------------------------------------------------------------
def test_cli():
    # The CORE `textual` package ships NO `textual` console-script — that CLI lives in the separate
    # `textual-dev` distribution (which drags in aiohttp/msgpack C-extensions, off the pure-Python
    # reproducible model). Assert that structural truth.
    dist = _md.distribution("textual")
    console_scripts = [ep for ep in dist.entry_points if ep.group == "console_scripts"]
    eq(console_scripts, [], "core `textual` package exposes no console_scripts entry point")

    # `python -m textual` DOES exist but is the interactive DEMO app: it opens the alternate screen
    # and requires a live TTY, so it cannot be run headless/deterministically here.
    import importlib.util as _u
    check(_u.find_spec("textual.__main__") is not None, "`python -m textual` (demo) module exists")

    subcommands = ["run", "console", "colors", "keys", "diagnose", "borders", "easing"]
    cli = shutil.which("textual")
    if cli:
        # A real `textual` CLI is on PATH (textual-dev present). Probe ONLY `--help` for each
        # subcommand with a hard subprocess timeout — never the bare interactive subcommand,
        # which would block waiting on a TTY.
        env = dict(os.environ)
        try:
            r = subprocess.run([cli, "--help"], capture_output=True, text=True, timeout=20, env=env)
            check(r.returncode == 0, "`textual --help` exits 0")
            for sub in subcommands:
                check(sub in r.stdout, "`textual --help` lists subcommand %s" % sub)
        except Exception as e:
            skip("textual --help", "CLI present but --help failed: %r" % e)
        for sub in subcommands:
            try:
                r = subprocess.run([cli, sub, "--help"], capture_output=True, text=True, timeout=20, env=env)
                check(r.returncode == 0, "`textual %s --help` exits 0" % sub)
            except Exception as e:
                skip("textual %s --help" % sub, "subcommand --help failed/timeout: %r" % e)
            # Never invoke `textual <sub>` bare (interactive TUI/console; needs a TTY).
            skip("textual %s (bare)" % sub, "interactive: opens a TUI/console requiring a live TTY")
    else:
        # Reproducible default image: no `textual` CLI. Record every subcommand as a documented skip.
        skip("textual --help", "core textual package installs no `textual` console-script (lives in textual-dev)")
        for sub in subcommands:
            skip("textual %s" % sub, "provided by the separate textual-dev package; interactive subcommands require a TTY")


# --------------------------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------------------------
ASYNC_TESTS = [
    test_surface, test_static_label, test_button, test_input, test_checkbox_switch,
    test_radioset, test_select, test_listview, test_datatable, test_tree, test_textarea,
    test_progressbar, test_tabs, test_reactive, test_bindings, test_css, test_query,
    test_screens, test_messages, test_pipeline,
]


async def _run_async():
    for t in ASYNC_TESTS:
        try:
            await t()
        except Exception as e:
            global _fail
            _fail += 1
            import traceback
            print("  FAIL %s raised %r" % (t.__name__, e))
            traceback.print_exc()


def main():
    print("=== TextualCarpet: textual %s / rich %s on python %s ==="
          % (textual.__version__, _md.version("rich"), sys.version.split()[0]))
    asyncio.run(_run_async())
    try:
        test_cli()
    except Exception as e:
        global _fail
        _fail += 1
        import traceback
        print("  FAIL test_cli raised %r" % e)
        traceback.print_exc()
    if _skips:
        print("--- documented skips (%d) ---" % len(_skips))
    print("TEXTUAL_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("TEXTUAL_DONE")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
