#!/usr/bin/env python3
"""Text processing + regular expressions + binary struct — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False


# ======================================================================
# re — Regular expression operations (docs: "re" module).
# Carpet-cover module-level functions, compile flags, Pattern & Match
# objects, group machinery, and every regex construct. How: drive each
# documented function/method with inputs whose result is fully known;
# expected per the CPython "re" docs; why: re backs nearly all text
# parsing and the bytecode VM (sre) must behave identically on StarryOS.
# ======================================================================

import re

# re.match — anchored at start; returns Match or None.
chk("re_match", re.match(r"\d+", "123abc").group() == "123")
chk("re_match_none", re.match(r"\d+", "abc") is None)
# re.fullmatch — whole string must match.
chk("re_fullmatch", re.fullmatch(r"\d+", "123").group() == "123")
chk("re_fullmatch_none", re.fullmatch(r"\d+", "123a") is None)
# re.search — first match anywhere.
chk("re_search", re.search(r"\d+", "ab123cd").group() == "123")
chk("re_search_none", re.search(r"z", "abc") is None)
# re.findall — list of all non-overlapping matches (groups -> tuples).
chk("re_findall", re.findall(r"\d+", "a1b22c333") == ["1", "22", "333"])
chk("re_findall_groups", re.findall(r"(\w)(\d)", "a1b2") == [("a", "1"), ("b", "2")])
chk("re_findall_onegroup", re.findall(r"(\d)", "a1b2") == ["1", "2"])
# re.finditer — iterator of Match objects.
chk("re_finditer", [m.group() for m in re.finditer(r"\d+", "1 22 333")] == ["1", "22", "333"])
# re.sub — replace; count limits replacements; \N / \g<N> backrefs.
chk("re_sub", re.sub(r"\d+", "#", "a1b22c") == "a#b#c")
chk("re_sub_count", re.sub(r"\d", "#", "1234", count=2) == "##34")
chk("re_sub_backref", re.sub(r"(\w)(\w)", r"\2\1", "abcd") == "badc")
chk("re_sub_g_num", re.sub(r"(\d+)", r"[\g<1>]", "a12b") == "a[12]b")
chk("re_sub_g_name", re.sub(r"(?P<n>\d+)", r"<\g<n>>", "x9") == "x<9>")
# re.sub with a replacement *function* receiving each Match.
chk("re_sub_func", re.sub(r"\d", lambda m: str(int(m.group()) + 1), "1 2 3") == "2 3 4")
# re.subn — like sub but returns (new_string, number_of_subs).
chk("re_subn", re.subn(r"\d", "X", "a1b2") == ("aXbX", 2))
# re.split — split by pattern; captured groups are kept; maxsplit limits.
chk("re_split", re.split(r"\s*,\s*", "a, b ,c") == ["a", "b", "c"])
chk("re_split_group", re.split(r"(\d)", "a1b2") == ["a", "1", "b", "2", ""])
chk("re_split_max", re.split(r",", "a,b,c,d", maxsplit=2) == ["a", "b", "c,d"])
# re.escape — escape regex metacharacters in arbitrary text.
chk("re_escape", re.match(re.escape("a.b*c") + "$", "a.b*c") is not None)
# Since 3.7 re.escape only escapes regex-special chars: '+' is escaped, '=' is NOT.
chk("re_escape_literal", re.escape("1+1=2") == "1\\+1=2")

# Flags: re.I/IGNORECASE, re.M/MULTILINE, re.S/DOTALL, re.X/VERBOSE,
# re.A/ASCII. How: prove each flag changes matching as documented.
chk("flag_I", re.match(r"abc", "ABC", re.I) is not None)
chk("flag_I_alias", re.IGNORECASE is re.I)
chk("flag_M", re.findall(r"^\w", "ab\ncd", re.M) == ["a", "c"])
chk("flag_M_alias", re.MULTILINE is re.M)
chk("flag_S", re.match(r"a.c", "a\nc", re.S) is not None)
chk("flag_S_dotall_alias", re.DOTALL is re.S)
chk("flag_dot_no_S", re.match(r"a.c", "a\nc") is None)
chk("flag_X", re.match(r"(?x) a \d+ # comment", "a123") is not None)
chk("flag_X_alias", re.VERBOSE is re.X)
chk("flag_A", re.findall(r"\w+", "ab_é", re.A) == ["ab_"])
chk("flag_A_alias", re.ASCII is re.A)
# Flags combine with | ; inline (?i) at start equals re.I.
chk("flag_combine", re.match(r"a.c", "A\nC", re.I | re.S) is not None)
chk("flag_inline", re.match(r"(?i)abc", "ABC") is not None)

# Match object: group/groups/groupdict/start/end/span/expand/lastindex.
m = re.match(r"(?P<y>\d{4})-(?P<m>\d{2})-(\d{2})", "2026-06-13")
chk("match_group0", m.group() == "2026-06-13" and m.group(0) == "2026-06-13")
chk("match_group_n", m.group(1) == "2026" and m.group(2) == "06" and m.group(3) == "13")
chk("match_group_multi", m.group(1, 3) == ("2026", "13"))
chk("match_group_name", m.group("y") == "2026" and m.group("m") == "06")
chk("match_groups", m.groups() == ("2026", "06", "13"))
chk("match_groupdict", m.groupdict() == {"y": "2026", "m": "06"})
chk("match_start_end", m.start(1) == 0 and m.end(1) == 4)
chk("match_span", m.span(2) == (5, 7) and m.span() == (0, 10))
chk("match_subscript", m["y"] == "2026" and m[3] == "13")
chk("match_lastindex", m.lastindex == 3)
chk("match_re_string", m.re.pattern.startswith("(?P<y>") and m.string == "2026-06-13")
chk("match_pos_endpos", m.pos == 0 and m.endpos == 10)
chk("match_expand", m.expand(r"\g<y>/\2") == "2026/06")
# groups()/group() with a non-participating optional group -> default.
mo = re.match(r"(a)(b)?", "a")
chk("match_optional_none", mo.group(2) is None and mo.groups() == ("a", None))
chk("match_groups_default", re.match(r"(a)(b)?", "a").groups("Z") == ("a", "Z"))
# group() on missing name/index raises IndexError/error.
try:
    re.match(r"(a)", "a").group(5)
    _g = False
except IndexError:
    _g = True
chk("match_bad_group", _g)

# Pattern object: every method + attributes (groups/groupindex/pattern/flags).
p = re.compile(r"(?P<a>\d)(?P<b>[a-z])")
chk("pat_match", p.match("1x").group() == "1x")
chk("pat_fullmatch", p.fullmatch("1x") is not None and p.fullmatch("1xy") is None)
chk("pat_search", p.search("zz3q").group() == "3q")
chk("pat_findall", p.findall("1a2b") == [("1", "a"), ("2", "b")])
chk("pat_finditer", [mm.group() for mm in p.finditer("1a2b")] == ["1a", "2b"])
chk("pat_sub", p.sub("#", "1a2b") == "##")
chk("pat_subn", p.subn("#", "1a2b") == ("##", 2))
chk("pat_split", re.compile(r",").split("a,b") == ["a", "b"])
chk("pat_groups_attr", p.groups == 2)
chk("pat_groupindex", dict(p.groupindex) == {"a": 1, "b": 2})  # 1-based group numbers
chk("pat_pattern_attr", p.pattern == r"(?P<a>\d)(?P<b>[a-z])")
chk("pat_flags_attr", (re.compile("a", re.I).flags & re.I) == re.I)
# Pattern.match with pos/endpos arguments.
chk("pat_match_pos", re.compile(r"\d").match("a1b", 1).group() == "1")
chk("pat_search_endpos", re.compile(r"\d").search("ab12", 0, 3).group() == "1")

# Regex constructs: named groups + (?P=name) + numeric backref \1.
chk("named_backref", re.match(r"(?P<w>\w+)=(?P=w)", "ab=ab") is not None)
chk("named_backref_no", re.match(r"(?P<w>\w+)=(?P=w)", "ab=cd") is None)
chk("numeric_backref", re.match(r"(\w+) \1", "hi hi") is not None)
# Non-capturing group (?:...) groups without numbering.
chk("noncapture", re.match(r"(?:ab)+(\d)", "abab7").groups() == ("7",))
# Lookahead (?=...) and negative lookahead (?!...).
chk("lookahead", re.findall(r"\d+(?= USD)", "5 USD 6 EUR") == ["5"])
# "5"->whole digit-run is followed by " USD" so it (and its prefixes) are rejected; only "60" survives.
chk("neg_lookahead", re.findall(r"\d+(?! USD)", "5 USD 60 EUR") == ["60"])
# Lookbehind (?<=...) and negative lookbehind (?<!...).
chk("lookbehind", re.findall(r"(?<=\$)\d+", "$5 €6 $7") == ["5", "7"])
chk("neg_lookbehind", re.findall(r"(?<!\$)\b\d+", "$5 6 7") == ["6", "7"])
# Anchors: ^ $ \b \B \A \Z.
chk("anchor_caret_dollar", re.fullmatch(r"^abc$", "abc") is not None)
chk("anchor_word_b", re.findall(r"\bcat\b", "cat category cat") == ["cat", "cat"])
chk("anchor_nonword_B", re.search(r"\Bcat\B", "scatter") is not None)
chk("anchor_A_Z", re.match(r"\Aabc\Z", "abc") is not None)
# Quantifiers greedy vs lazy: * + ? {m,n} and ? suffix for non-greedy.
chk("quant_greedy", re.match(r"<.*>", "<a><b>").group() == "<a><b>")
chk("quant_lazy", re.match(r"<.*?>", "<a><b>").group() == "<a>")
chk("quant_plus_lazy", re.match(r"a+?", "aaa").group() == "a")
chk("quant_optional", re.fullmatch(r"colou?r", "color") is not None and
    re.fullmatch(r"colou?r", "colour") is not None)
chk("quant_repeat", re.fullmatch(r"\d{2,4}", "123") is not None and
    re.fullmatch(r"\d{2,4}", "1") is None)
chk("quant_exact", re.fullmatch(r"a{3}", "aaa") is not None and re.fullmatch(r"a{3}", "aa") is None)
# Alternation a|b and grouped alternation.
chk("alternation", re.findall(r"cat|dog", "cat dog cat") == ["cat", "dog", "cat"])
chk("alternation_group", re.fullmatch(r"(yes|no)", "yes") is not None)
# Character classes: [...], negation [^...], ranges, predefined \d\w\s.
chk("charclass", re.findall(r"[aeiou]", "hello world") == ["e", "o", "o"])
chk("charclass_neg", re.findall(r"[^aeiou ]", "a b c") == ["b", "c"])
chk("charclass_range", re.fullmatch(r"[a-f0-9]+", "abc123") is not None)
chk("charclass_predef", re.findall(r"\d", "a1b2") == ["1", "2"] and
    re.findall(r"\s", "a b\tc")[0] == " ")
# Compile error path: unbalanced parenthesis raises re.error.
try:
    re.compile(r"(")
    _ce = False
except re.error:
    _ce = True
chk("re_error", _ce)
# bytes patterns operate on bytes input.
chk("re_bytes", re.match(rb"\d+", b"123").group() == b"123")
# Conditional pattern (?(id)yes|no) — match yes-branch iff group id participated.
chk("cond_pattern_yes", re.fullmatch(r"(a)?(?(1)b|c)", "ab") is not None)
chk("cond_pattern_no", re.fullmatch(r"(a)?(?(1)b|c)", "c") is not None)
chk("cond_pattern_named", re.fullmatch(r"(?P<q>a)?(?(q)b|c)", "c") is not None and
    re.fullmatch(r"(?P<q>a)?(?(q)b|c)", "b") is None)
# Match.lastgroup — name of last matched named group (None if none / unnamed last).
chk("match_lastgroup", re.match(r"(\d)(?P<last>[a-z])", "5x").lastgroup == "last")
chk("match_lastgroup_none", re.match(r"(\d)", "5").lastgroup is None)
# Pattern.scanner — low-level scanner whose .match()/.search() advance position.
sc = re.compile(r"\d").scanner("1a2")
chk("pat_scanner", sc.match().group() == "1" and sc.search().group() == "2")
# re.purge — clears the internal compiled-pattern cache; returns None, idempotent.
chk("re_purge", re.purge() is None)


# ======================================================================
# string — Common string operations (docs: "string" module).
# Cover Template (substitute/safe_substitute), Formatter (format/vformat),
# capwords, and the constant tables. How: instantiate each helper and
# assert exact output; expected per docs; why: Template/Formatter back
# config & i18n substitution and must round-trip exactly.
# ======================================================================

import string

# string constants — fixed character tables.
chk("const_ascii_lowercase", string.ascii_lowercase == "abcdefghijklmnopqrstuvwxyz")
chk("const_ascii_uppercase", string.ascii_uppercase == "ABCDEFGHIJKLMNOPQRSTUVWXYZ")
chk("const_ascii_letters", string.ascii_letters == string.ascii_lowercase + string.ascii_uppercase)
chk("const_digits", string.digits == "0123456789")
chk("const_hexdigits", string.hexdigits == "0123456789abcdefABCDEF")
chk("const_octdigits", string.octdigits == "01234567")
chk("const_punctuation", "!" in string.punctuation and "," in string.punctuation)
chk("const_whitespace", " " in string.whitespace and "\t" in string.whitespace)
chk("const_printable", "a" in string.printable and "0" in string.printable)

# string.capwords(s[, sep]) — split, capitalize each, rejoin.
chk("capwords", string.capwords("hello   world") == "Hello World")
chk("capwords_sep", string.capwords("a-b-c", "-") == "A-B-C")

# string.Template — $-based substitution. substitute raises KeyError on miss;
# safe_substitute leaves unknown placeholders intact; $$ is a literal $; ${x}.
tpl = string.Template("$who likes ${what}, paid $$5")
chk("tmpl_substitute", tpl.substitute(who="I", what="py") == "I likes py, paid $5")
chk("tmpl_safe_substitute", tpl.safe_substitute(who="I") == "I likes ${what}, paid $5")
try:
    tpl.substitute(who="I")
    _ke = False
except KeyError:
    _ke = True
chk("tmpl_substitute_keyerror", _ke)
chk("tmpl_mapping", string.Template("$a$b").substitute({"a": "1", "b": "2"}) == "12")

# string.Formatter — programmable str.format engine.
fmt = string.Formatter()
chk("formatter_format", fmt.format("{0}-{name}", "x", name="y") == "x-y")
chk("formatter_vformat", fmt.vformat("{0} {k}", ("a",), {"k": "b"}) == "a b")
chk("formatter_spec", fmt.format("{0:>5}", "ab") == "   ab")
chk("formatter_get_value", fmt.get_value(0, ("v",), {}) == "v" and fmt.get_value("k", (), {"k": 9}) == 9)
# Formatter.parse — yields (literal_text, field_name, format_spec, conversion) tuples.
chk("formatter_parse", list(fmt.parse("a{0!r:>3}b{n}")) ==
    [("a", "0", ">3", "r"), ("b", "n", "", None)])
chk("formatter_parse_literal", list(fmt.parse("plain")) == [("plain", None, None, None)])
# Formatter.get_field — resolve dotted/indexed field name against args/kwargs -> (obj, used_key).
chk("formatter_get_field", fmt.get_field("0", ("v",), {}) == ("v", 0))
chk("formatter_get_field_attr", fmt.get_field("a.real", (), {"a": 5}) == (5, "a"))
# Formatter.convert_field — apply !s/!r/!a conversion (None = no conversion).
chk("formatter_convert_r", fmt.convert_field("a", "r") == "'a'")
chk("formatter_convert_s", fmt.convert_field(123, "s") == "123")
chk("formatter_convert_a", fmt.convert_field("é", "a") == "'\\xe9'")
chk("formatter_convert_none", fmt.convert_field("z", None) == "z")
# Formatter.format_field — apply a format spec to a value (== format()).
chk("formatter_format_field", fmt.format_field(3, ">5") == "    3")


# ======================================================================
# textwrap — Text wrapping and filling (docs: "textwrap" module).
# Cover wrap/fill/shorten/indent/dedent with width & options. How: feed
# known text and assert the exact wrapped structure; expected per docs;
# why: textwrap is the canonical paragraph reflow used by CLIs/help text.
# ======================================================================

import textwrap

# textwrap.wrap — returns list of lines, each <= width.
chk("wrap_basic", textwrap.wrap("aa bb cc dd", width=5) == ["aa bb", "cc dd"])
chk("wrap_long_word", textwrap.wrap("abcdefgh", width=4) == ["abcd", "efgh"])
chk("wrap_no_break_long", textwrap.wrap("abcdefgh", width=4, break_long_words=False) == ["abcdefgh"])
# textwrap.fill — wrap then join with newlines.
chk("fill", textwrap.fill("aa bb cc dd", width=5) == "aa bb\ncc dd")
# textwrap.shorten — collapse whitespace and truncate with placeholder.
chk("shorten", textwrap.shorten("Hello  world  foo", width=12, placeholder="...") == "Hello...")
chk("shorten_fits", textwrap.shorten("Hi there", width=20) == "Hi there")
# textwrap.indent — prefix selected lines.
chk("indent", textwrap.indent("a\nb\n", "> ") == "> a\n> b\n")
chk("indent_predicate", textwrap.indent("a\n\nb\n", "+ ") == "+ a\n\n+ b\n")
# textwrap.dedent — remove common leading whitespace.
chk("dedent", textwrap.dedent("    a\n    b") == "a\nb")
chk("dedent_mixed", textwrap.dedent("  a\n    b") == "a\n  b")
chk("dedent_blank", textwrap.dedent("\n  a\n  b") == "\na\nb")
# textwrap.TextWrapper — the reusable engine; initial/subsequent_indent prefix first vs rest.
tw = textwrap.TextWrapper(width=10, initial_indent=">>", subsequent_indent="..")
chk("textwrapper_indents", tw.wrap("aa bb cc dd ee") == [">>aa bb cc", "..dd ee"])
# break_on_hyphens — split at hyphens within words (default True).
chk("wrap_break_hyphens", textwrap.wrap("foo-bar-baz", width=5) == ["foo-", "bar-", "baz"])
chk("wrap_no_break_hyphens",
    textwrap.wrap("foo-bar-baz", width=5, break_on_hyphens=False) == ["foo-b", "ar-ba", "z"])
# expand_tabs (default True) turns tabs into spaces before wrapping.
chk("wrap_expand_tabs", textwrap.wrap("a\tb", width=20, expand_tabs=True)[0] == "a       b")
# drop_whitespace (default True) drops whitespace at start/end of wrapped lines.
chk("wrap_drop_whitespace", textwrap.wrap("a    b", width=4, drop_whitespace=True) == ["a", "b"])


# ======================================================================
# difflib — Helpers for computing deltas (docs: "difflib" module).
# Cover SequenceMatcher (ratio/get_matching_blocks/get_opcodes),
# unified_diff, ndiff, get_close_matches. How: compare known sequences
# and assert exact ratios/diffs; expected per docs; why: difflib powers
# diff tooling and assertion error rendering.
# ======================================================================

import difflib

# difflib.SequenceMatcher — similarity scoring & block/opcode extraction.
sm = difflib.SequenceMatcher(None, "abcd", "abxd")
chk("seqmatcher_ratio", sm.ratio() == 0.75)
chk("seqmatcher_quick_ratio", sm.quick_ratio() == 0.75)
chk("seqmatcher_blocks", sm.get_matching_blocks()[-1] == difflib.Match(4, 4, 0))
chk("seqmatcher_opcodes", any(op[0] == "replace" for op in sm.get_opcodes()))
chk("seqmatcher_identical", difflib.SequenceMatcher(None, "xyz", "xyz").ratio() == 1.0)
# difflib.get_close_matches(word, possibilities, n, cutoff).
chk("get_close_matches", difflib.get_close_matches("appel", ["apple", "ape", "xyz"]) == ["apple", "ape"])
chk("get_close_matches_n", difflib.get_close_matches("appel", ["apple", "ape", "apply"], n=1) == ["apply"])
chk("get_close_matches_empty", difflib.get_close_matches("zzz", ["apple"]) == [])
# difflib.unified_diff — standard unified diff between line lists.
# lineterm="" suppresses the header trailers but content lines keep their own "\n".
ud = list(difflib.unified_diff(["a\n", "b\n"], ["a\n", "c\n"], lineterm=""))
chk("unified_diff", "-b\n" in ud and "+c\n" in ud and any(x.startswith("@@") for x in ud))
# difflib.ndiff — line-by-line delta with +/-/space prefixes.
nd = list(difflib.ndiff(["one"], ["ore"]))
chk("ndiff", nd[0].startswith("- one") and any(x.startswith("+ ore") for x in nd))
# difflib.Differ — class behind ndiff; .compare() yields the same +/-/?/space deltas.
dr = list(difflib.Differ().compare(["one\n"], ["ore\n"]))
chk("differ_compare", dr[0] == "- one\n" and any(x.startswith("+ ore") for x in dr) and
    any(x.startswith("? ") for x in dr))
# difflib.context_diff — context-format diff with *** / --- file markers.
cd = list(difflib.context_diff(["a\n", "b\n"], ["a\n", "c\n"], lineterm=""))
chk("context_diff", any(x.startswith("***") for x in cd) and any(x.startswith("---") for x in cd) and
    any(x.startswith("! ") for x in cd))
# difflib.HtmlDiff — produces an HTML table diff; make_table yields a <table>.
chk("htmldiff_table", "<table" in difflib.HtmlDiff().make_table(["a"], ["b"]))
# difflib.IS_LINE_JUNK / IS_CHARACTER_JUNK — default junk predicates.
chk("is_line_junk", difflib.IS_LINE_JUNK("\n") is True and difflib.IS_LINE_JUNK("x") is False)
chk("is_character_junk", difflib.IS_CHARACTER_JUNK(" ") is True and
    difflib.IS_CHARACTER_JUNK("x") is False)
# SequenceMatcher.set_seqs — rebind both sequences and rescore.
sm2 = difflib.SequenceMatcher()
sm2.set_seqs("abcd", "abxd")
chk("seqmatcher_set_seqs", sm2.ratio() == 0.75)
chk("seqmatcher_find_longest",
    sm2.find_longest_match(0, 4, 0, 4) == difflib.Match(0, 0, 2))


# ======================================================================
# unicodedata — Unicode Database access (docs: "unicodedata" module).
# Cover name/lookup/category/numeric/decimal/digit + the four
# normalization forms NFC/NFD/NFKC/NFKD + combining/bidirectional.
# How: query known code points; expected per the UCD; why: correct
# Unicode handling is required for any non-ASCII text on StarryOS.
# ======================================================================

import unicodedata

# unicodedata.name / lookup — code point <-> canonical name (inverse pair).
chk("ucd_name", unicodedata.name("A") == "LATIN CAPITAL LETTER A")
chk("ucd_lookup", unicodedata.lookup("LATIN SMALL LETTER A") == "a")
chk("ucd_name_roundtrip", unicodedata.lookup(unicodedata.name("é")) == "é")
chk("ucd_name_default", unicodedata.name("\x00", "NONE") == "NONE")
# unicodedata.category — two-letter general category.
chk("ucd_category_Lu", unicodedata.category("A") == "Lu")
chk("ucd_category_Ll", unicodedata.category("a") == "Ll")
chk("ucd_category_Nd", unicodedata.category("5") == "Nd")
chk("ucd_category_Zs", unicodedata.category(" ") == "Zs")
# unicodedata.decimal / digit / numeric — numeric value extraction.
chk("ucd_decimal", unicodedata.decimal("7") == 7)
chk("ucd_digit", unicodedata.digit("9") == 9)
chk("ucd_numeric", unicodedata.numeric("½") == 0.5)  # ½ -> 0.5
chk("ucd_numeric_default", unicodedata.numeric("A", -1) == -1)
# unicodedata.combining / bidirectional / mirrored / east_asian_width.
chk("ucd_combining", unicodedata.combining("́") == 230 and unicodedata.combining("a") == 0)
chk("ucd_bidirectional", unicodedata.bidirectional("A") == "L")
chk("ucd_mirrored", unicodedata.mirrored("(") == 1 and unicodedata.mirrored("a") == 0)
chk("ucd_east_asian_width", unicodedata.east_asian_width("A") == "Na")
# unicodedata.normalize — NFC/NFD canonical, NFKC/NFKD compatibility.
chk("ucd_nfd", unicodedata.normalize("NFD", "é") == "é")        # é -> e + combining acute
chk("ucd_nfc", unicodedata.normalize("NFC", "é") == "é")        # recompose
chk("ucd_nfc_nfd_inverse", unicodedata.normalize("NFC", unicodedata.normalize("NFD", "é")) == "é")
chk("ucd_nfkc", unicodedata.normalize("NFKC", "①") == "1")            # ① -> 1
chk("ucd_nfkd_ligature", unicodedata.normalize("NFKD", "ﬁ") == "fi")  # ﬁ -> f i
chk("ucd_unidata_version", isinstance(unicodedata.unidata_version, str) and "." in unicodedata.unidata_version)
# unicodedata.is_normalized (3.8+) — fast check without producing the normalized form.
_nfc_e = unicodedata.normalize("NFC", "é")   # composed U+00E9
_nfd_e = unicodedata.normalize("NFD", "é")   # decomposed 'e' + U+0301
chk("ucd_is_normalized", unicodedata.is_normalized("NFC", _nfc_e) is True and
    unicodedata.is_normalized("NFC", _nfd_e) is False)
chk("ucd_is_normalized_nfd", unicodedata.is_normalized("NFD", _nfd_e) is True and
    unicodedata.is_normalized("NFD", _nfc_e) is False)


# ======================================================================
# struct — Interpret bytes as packed binary data (docs: "struct" module).
# Carpet-cover EVERY format character, byte-order/size/alignment prefix,
# and all module functions plus the Struct class. How: pack a known value
# then unpack and assert round-trip + exact byte widths; expected per the
# "Format Characters" / "Byte Order, Size, and Alignment" tables; why:
# struct underpins all binary protocols/file formats and is endian- and
# word-size-sensitive — exactly the surface a new arch/kernel can break.
# ======================================================================

import struct

# Format char 'x' — pad byte (no value); 'c' — single bytes char.
chk("fmt_x_pad", struct.pack("x") == b"\x00" and struct.calcsize("3x") == 3)
chk("fmt_c", struct.unpack("c", struct.pack("c", b"A")) == (b"A",))
# 'b'/'B' — signed/unsigned char (1 byte).
chk("fmt_b_B", struct.unpack("bB", struct.pack("bB", -1, 255)) == (-1, 255))
chk("fmt_b_size", struct.calcsize("b") == 1 and struct.calcsize("B") == 1)
# '?' — _Bool (1 byte).
chk("fmt_bool", struct.unpack("?", struct.pack("?", True)) == (True,) and
    struct.unpack("?", struct.pack("?", False)) == (False,))
# 'h'/'H' — short (standard 2 bytes).
chk("fmt_h_H", struct.unpack("=hH", struct.pack("=hH", -2, 5)) == (-2, 5))
chk("fmt_h_size", struct.calcsize("=h") == 2 and struct.calcsize("=H") == 2)
# 'i'/'I' — int (standard 4 bytes).
chk("fmt_i_I", struct.unpack("=iI", struct.pack("=iI", -3, 6)) == (-3, 6))
chk("fmt_i_size", struct.calcsize("=i") == 4 and struct.calcsize("=I") == 4)
# 'l'/'L' — long (standard 4 bytes).
chk("fmt_l_L", struct.unpack("=lL", struct.pack("=lL", -4, 7)) == (-4, 7))
chk("fmt_l_size", struct.calcsize("=l") == 4 and struct.calcsize("=L") == 4)
# 'q'/'Q' — long long (8 bytes).
chk("fmt_q_Q", struct.unpack("=qQ", struct.pack("=qQ", -5, 8)) == (-5, 8))
chk("fmt_q_size", struct.calcsize("=q") == 8 and struct.calcsize("=Q") == 8)
# 'n'/'N' — ssize_t/size_t — native-only (require '@' / no prefix).
chk("fmt_n_N", struct.unpack("@nN", struct.pack("@nN", -9, 10)) == (-9, 10))
try:
    struct.calcsize("=n")
    _n = False
except struct.error:
    _n = True
chk("fmt_n_native_only", _n)
# 'e'/'f'/'d' — half/single/double precision IEEE-754 floats.
chk("fmt_e_half", struct.unpack("e", struct.pack("e", 1.5)) == (1.5,) and struct.calcsize("e") == 2)
chk("fmt_f_float", struct.unpack("f", struct.pack("f", 1.5)) == (1.5,) and struct.calcsize("f") == 4)
chk("fmt_d_double", struct.unpack("d", struct.pack("d", 3.25)) == (3.25,) and struct.calcsize("d") == 8)
# 's' — fixed-width bytes (NUL/space padded); 'p' — Pascal string (len byte).
chk("fmt_s", struct.unpack("5s", struct.pack("5s", b"hi")) == (b"hi\x00\x00\x00",))
chk("fmt_s_truncate", struct.pack("2s", b"abcd") == b"ab")
chk("fmt_p_pascal", struct.unpack("5p", struct.pack("5p", b"hi")) == (b"hi",))
# 'P' — void* pointer — native-only.
chk("fmt_P", struct.unpack("@P", struct.pack("@P", 12345)) == (12345,))
try:
    struct.calcsize("=P")
    _p = False
except struct.error:
    _p = True
chk("fmt_P_native_only", _p)
# Repeat count prefix: "3h" == "hhh".
chk("fmt_repeat", struct.calcsize(">3h") == 6 and
    struct.unpack(">3h", struct.pack(">3h", 1, 2, 3)) == (1, 2, 3))

# Byte-order / size / alignment prefixes: @ = < > !.
# '<' little-endian, '>' big-endian, '!' network(= big-endian).
chk("order_big", struct.pack(">H", 1) == b"\x00\x01")
chk("order_little", struct.pack("<H", 1) == b"\x01\x00")
chk("order_network", struct.pack("!H", 1) == struct.pack(">H", 1))
chk("order_std_eq", struct.pack("=H", 1) in (b"\x00\x01", b"\x01\x00"))
chk("order_roundtrip", struct.unpack("<I", struct.pack(">I", 0x01020304)[::-1]) == (0x01020304,))
# '@' native uses native alignment (padding); '=' standard has no padding.
chk("align_native", struct.calcsize("@ci") >= struct.calcsize("=ci"))
chk("align_std_nopad", struct.calcsize("=ci") == 5)

# struct.pack / unpack — round trip a heterogeneous record.
packed = struct.pack(">HIf", 7, 70000, 1.5)
chk("pack_unpack", struct.unpack(">HIf", packed) == (7, 70000, 1.5))
chk("calcsize", struct.calcsize(">HIf") == 10 and struct.calcsize(">HIf") == len(packed))
# struct.pack_into / unpack_from — write into / read from a buffer at offset.
buf = bytearray(8)
struct.pack_into(">I", buf, 2, 0xAABBCCDD)
chk("pack_into", buf == b"\x00\x00\xaa\xbb\xcc\xdd\x00\x00")
chk("unpack_from", struct.unpack_from(">I", buf, 2) == (0xAABBCCDD,))
chk("unpack_from_default_offset", struct.unpack_from(">H", b"\x00\x05extra") == (5,))
# struct.iter_unpack — iterate fixed-size records from a buffer.
chk("iter_unpack", list(struct.iter_unpack(">H", struct.pack(">HHH", 1, 2, 3))) == [(1,), (2,), (3,)])
# struct.Struct class — precompiled format with size/format + same methods.
S = struct.Struct(">HI")
chk("Struct_attrs", S.size == 6 and S.format == ">HI")
chk("Struct_pack_unpack", S.unpack(S.pack(11, 22)) == (11, 22))
buf2 = bytearray(6)
S.pack_into(buf2, 0, 1, 2)
chk("Struct_pack_into", S.unpack_from(buf2, 0) == (1, 2))
chk("Struct_iter_unpack", list(S.iter_unpack(S.pack(1, 2) + S.pack(3, 4))) == [(1, 2), (3, 4)])

# Error paths: value out of range, wrong buffer length, bad format char.
try:
    struct.pack("B", 256)
    _r = False
except struct.error:
    _r = True
chk("struct_range_error", _r)
try:
    struct.unpack(">I", b"\x00")
    _b = False
except struct.error:
    _b = True
chk("struct_short_buffer", _b)
try:
    struct.calcsize("z")
    _z = False
except struct.error:
    _z = True
chk("struct_bad_char", _z)


# ======================================================================
# 3.14-only TEXT syntax (PEP 750 t-strings) — isolated in exec()'d source
# guarded by version so the file PARSES on 3.12. A t-string yields a
# string.templatelib.Template (NOT an interpolated str). How: build a
# t-string and assert the object type + captured value; expected per
# PEP 750; why: this is a 3.14 language addition central to text handling.
# ======================================================================

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


# PEP 750 (3.14): t'...' evaluates to a Template; .values holds interpolated parts.
_gated_syntax(
    "pep750_tstring", (3, 14),
    "name = 'world'\n"
    "tmpl = t'hi {name}!'\n"
    "R = (type(tmpl).__name__, tmpl.values[0], tmpl.strings)\n",
    lambda ns: ns["R"][0] == "Template" and ns["R"][1] == "world",
)

# Non-syntax 3.14 feature: string.templatelib module / Template (hasattr-guarded).
try:
    import string.templatelib as _tl  # noqa: F401
    chk("templatelib_module", hasattr(__import__("string.templatelib", fromlist=["Template"]), "Template"))
except ImportError:
    chk("templatelib_module", True, "(skip: needs 3.14 string.templatelib)")


print("PY_TEXT_OK" if _ok else "PY_TEXT_FAIL")
sys.exit(0 if _ok else 1)
