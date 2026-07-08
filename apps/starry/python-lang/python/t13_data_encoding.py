#!/usr/bin/env python3
"""Serialization, encoding, hashing & compression — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

import io

# =====================================================================
# json  (docs: json — JSON encoder and decoder)
# how: round-trip every JSON-mappable type; exercise every documented
#      dumps/loads keyword; expected: bit-exact strings / restored objects;
#      why: JSON is the workhorse interchange format, must be byte-faithful.
# =====================================================================
import json

# json.dumps: default serialization of all JSON types (object/array/str/
# number/true/false/null). dict keys become strings, tuples become arrays.
_obj = {"i": 1, "f": 2.5, "s": "x", "b": True, "n": None, "arr": [1, 2, 3]}
chk("json_dumps_loads_roundtrip", json.loads(json.dumps(_obj)) == _obj)
chk("json_dumps_types",
    json.dumps([1, 2.5, "x", True, False, None]) == '[1, 2.5, "x", true, false, null]')
chk("json_tuple_to_array", json.loads(json.dumps((1, 2))) == [1, 2])
chk("json_int_keys_become_str", json.loads(json.dumps({1: "a"})) == {"1": "a"})

# json round-trip preserves exact Python types: int stays int (not float),
# float stays float, bool stays bool (distinct from int), None stays None.
_typed = json.loads(json.dumps({"i": 7, "f": 7.0, "b": True, "n": None}))
chk("json_type_fidelity",
    type(_typed["i"]) is int and type(_typed["f"]) is float
    and _typed["b"] is True and _typed["n"] is None)

# json.dumps(sort_keys=True): keys emitted in sorted order.
chk("json_sort_keys",
    json.dumps({"b": 1, "a": 2}, sort_keys=True) == '{"a": 2, "b": 1}')

# json.dumps(indent=N): pretty-print; newlines + N-space indent per level.
chk("json_indent",
    json.dumps({"a": 1}, indent=2) == '{\n  "a": 1\n}')

# json.dumps(separators=(item, key)): override default ", "/": " separators.
chk("json_separators",
    json.dumps({"a": 1, "b": 2}, separators=(",", ":")) == '{"a":1,"b":2}')

# json.dumps(ensure_ascii): True (default) escapes non-ASCII to \uXXXX;
# False emits raw UTF-8 text.
chk("json_ensure_ascii_true", json.dumps("é") == '"\\u00e9"')
chk("json_ensure_ascii_false", json.dumps("é", ensure_ascii=False) == '"é"')

# json.dumps(default=fn): fallback serializer for otherwise-unserializable
# objects; raises TypeError when no default given.
chk("json_default_callback",
    json.dumps({1, 2, 3}, default=lambda o: sorted(o)) == "[1, 2, 3]")
try:
    json.dumps({1, 2})
    _de = False
except TypeError:
    _de = True
chk("json_unserializable_raises_typeerror", _de)

# json special floats: NaN/Infinity emitted by default; allow_nan=False -> ValueError.
chk("json_nan_inf",
    json.dumps([float("nan"), float("inf"), float("-inf")]) == "[NaN, Infinity, -Infinity]")
try:
    json.dumps(float("nan"), allow_nan=False)
    _na = False
except ValueError:
    _na = True
chk("json_allow_nan_false_raises", _na)

# json.JSONEncoder subclass: override default() to serialize custom types.
class _SetEnc(json.JSONEncoder):
    def default(self, o):
        if isinstance(o, set):
            return {"__set__": sorted(o)}
        return super().default(o)
chk("json_encoder_subclass",
    json.loads(_SetEnc().encode({1, 2})) == {"__set__": [1, 2]})

# json.loads(object_hook=fn): called for every decoded JSON object (dict).
chk("json_object_hook",
    json.loads('{"a": 1}', object_hook=lambda d: ("HOOK", d)) == ("HOOK", {"a": 1}))

# json.loads(object_pairs_hook=fn): receives ordered list of (k,v) pairs
# (takes priority over object_hook); preserves order / duplicate keys.
chk("json_object_pairs_hook",
    json.loads('{"b": 1, "a": 2}', object_pairs_hook=list) == [("b", 1), ("a", 2)])

# json.loads(parse_float / parse_int): custom number constructors.
chk("json_parse_float",
    json.loads('[1.5]', parse_float=lambda s: ("F", s)) == [("F", "1.5")])
chk("json_parse_int",
    json.loads('[42]', parse_int=lambda s: ("I", s)) == [("I", "42")])

# json.JSONDecoder: explicit decoder object; .decode() and .raw_decode().
_dec = json.JSONDecoder()
chk("json_decoder_decode", _dec.decode('{"x": 1}') == {"x": 1})
# raw_decode does not skip leading whitespace; idx points past the value.
_val, _end = json.JSONDecoder().raw_decode('{"a": 1}  rest')
chk("json_raw_decode", _val == {"a": 1} and _end == 8)

# json.load / json.dump: file-object (stream) variants via io.StringIO.
_sio = io.StringIO()
json.dump({"k": [1, 2]}, _sio)
_sio.seek(0)
chk("json_dump_load_stream", json.load(_sio) == {"k": [1, 2]})

# json.JSONDecodeError: malformed input raises with msg/pos/lineno/colno.
try:
    json.loads("{bad}")
    _je = False
except json.JSONDecodeError as e:
    _je = isinstance(e, ValueError) and hasattr(e, "pos") and hasattr(e, "lineno")
chk("json_decode_error", _je)

# json escaping of control chars and reverse decode.
chk("json_escape_control",
    json.dumps("a\tb\nc\"d\\e") == '"a\\tb\\nc\\"d\\\\e"')
chk("json_unicode_decode", json.loads('"\\u4e2d"') == "中")

# json.JSONEncoder.iterencode: streaming chunk generator; concatenation
# equals one-shot encode() (exercises the generator code path).
chk("json_iterencode",
    "".join(json.JSONEncoder().iterencode({"a": 1, "b": [2, 3]}))
    == json.dumps({"a": 1, "b": [2, 3]}))

# json.dumps(skipkeys=True): silently drop dict keys of non-basic type
# (e.g. tuple) instead of raising TypeError.
chk("json_skipkeys", json.dumps({1: "a", (1, 2): "b"}, skipkeys=True) == '{"1": "a"}')
try:
    json.dumps({(1, 2): "b"})
    _sk = False
except TypeError:
    _sk = True
chk("json_skipkeys_default_raises", _sk)

# json.loads(parse_constant=fn): custom handler for NaN/Infinity/-Infinity tokens.
chk("json_parse_constant",
    json.loads("[NaN, Infinity]", parse_constant=lambda s: ("C", s))
    == [("C", "NaN"), ("C", "Infinity")])

# json.dumps(indent) with item separator default loses trailing space.
chk("json_indent_separators",
    json.dumps([1, 2], indent=2) == "[\n  1,\n  2\n]")

# =====================================================================
# csv  (docs: csv — CSV File Reading and Writing)
# how: write & re-read rows through io.StringIO; vary dialect knobs and
#      QUOTE_* constants; expected: faithful field reconstruction;
#      why: CSV quoting rules are subtle (embedded delimiters/quotes/newlines).
# =====================================================================
import csv

# csv.writer / csv.reader: basic row round-trip (default excel dialect).
_buf = io.StringIO()
csv.writer(_buf).writerows([["a", "b"], ["1", "2"]])
chk("csv_writer_reader",
    list(csv.reader(io.StringIO(_buf.getvalue()))) == [["a", "b"], ["1", "2"]])

# csv quoting of embedded delimiter/quote: field with comma gets quoted,
# embedded quote is doubled.
_buf = io.StringIO()
csv.writer(_buf).writerow(["a,b", 'he said "hi"'])
chk("csv_quote_embedded",
    _buf.getvalue() == '"a,b","he said ""hi"""\r\n')
chk("csv_reader_unquote",
    list(csv.reader(io.StringIO('"a,b","he said ""hi"""')))
    == [["a,b", 'he said "hi"']])

# csv writer delimiter / quotechar options.
_buf = io.StringIO()
csv.writer(_buf, delimiter=";", quotechar="'").writerow(["x;y", "z"])
chk("csv_delimiter_quotechar", _buf.getvalue() == "'x;y';z\r\n")

# csv.QUOTE_ALL: quote every field.
_buf = io.StringIO()
csv.writer(_buf, quoting=csv.QUOTE_ALL).writerow(["a", "1"])
chk("csv_quote_all", _buf.getvalue() == '"a","1"\r\n')

# csv.QUOTE_NONNUMERIC: quote non-numbers; reader converts numbers to float.
_buf = io.StringIO()
csv.writer(_buf, quoting=csv.QUOTE_NONNUMERIC).writerow(["a", 1])
chk("csv_quote_nonnumeric_write", _buf.getvalue() == '"a",1\r\n')
chk("csv_quote_nonnumeric_read",
    list(csv.reader(io.StringIO('"a",1'), quoting=csv.QUOTE_NONNUMERIC))
    == [["a", 1.0]])

# csv.QUOTE_MINIMAL (default) vs QUOTE_NONE (no quoting; needs escapechar).
_buf = io.StringIO()
csv.writer(_buf, quoting=csv.QUOTE_NONE, escapechar="\\").writerow(["a,b"])
chk("csv_quote_none", _buf.getvalue() == "a\\,b\r\n")

# csv.DictWriter / csv.DictReader: header-keyed rows.
_buf = io.StringIO()
_dw = csv.DictWriter(_buf, fieldnames=["name", "age"])
_dw.writeheader()
_dw.writerow({"name": "x", "age": 3})
chk("csv_dictwriter", _buf.getvalue() == "name,age\r\nx,3\r\n")
_dr = list(csv.DictReader(io.StringIO("name,age\r\nx,3\r\n")))
chk("csv_dictreader", _dr == [{"name": "x", "age": "3"}])

# csv.register_dialect / get_dialect / list_dialects.
csv.register_dialect("pipe", delimiter="|")
chk("csv_register_dialect", "pipe" in csv.list_dialects())
_buf = io.StringIO()
csv.writer(_buf, dialect="pipe").writerow(["a", "b"])
chk("csv_use_dialect", _buf.getvalue() == "a|b\r\n")
csv.unregister_dialect("pipe")

# csv.get_dialect: retrieve a registered Dialect object by name.
csv.register_dialect("pipe2", delimiter="|", quoting=csv.QUOTE_ALL)
chk("csv_get_dialect",
    csv.get_dialect("pipe2").delimiter == "|"
    and csv.get_dialect("pipe2").quoting == csv.QUOTE_ALL)
csv.unregister_dialect("pipe2")
# csv.get_dialect on an unknown name raises csv.Error.
try:
    csv.get_dialect("no_such_dialect")
    _gde = False
except csv.Error:
    _gde = True
chk("csv_get_dialect_error", _gde)

# csv.field_size_limit: get/set the max field size; setter returns old value
# and the new limit is enforced (oversized field -> csv.Error).
_old_limit = csv.field_size_limit()
_prev = csv.field_size_limit(8)
chk("csv_field_size_limit_setget",
    _prev == _old_limit and csv.field_size_limit() == 8)
try:
    list(csv.reader(io.StringIO("x" * 50 + "\n")))
    _fsle = False
except csv.Error:
    _fsle = True
csv.field_size_limit(_old_limit)
chk("csv_field_size_limit_enforced", _fsle)

# csv.Sniffer: deduce delimiter and presence of a header.
_sample = "a;b;c\n1;2;3\n"
chk("csv_sniffer_delimiter", csv.Sniffer().sniff(_sample).delimiter == ";")
chk("csv_sniffer_has_header", csv.Sniffer().has_header("name;v\nx;1\n2;3\n") is True)

# =====================================================================
# pickle  (docs: pickle — Python object serialization)
# how: round-trip nested structures across every protocol 0..HIGHEST;
#      exercise Pickler/Unpickler streams and __reduce__/__get/setstate__;
#      expected: equal reconstructed objects; why: pickle is the canonical
#      binary object persistence; protocol coverage proves the marshaller.
# =====================================================================
import pickle

# pickle.HIGHEST_PROTOCOL / DEFAULT_PROTOCOL: documented module constants.
chk("pickle_protocol_constants",
    pickle.HIGHEST_PROTOCOL >= 5 and pickle.DEFAULT_PROTOCOL >= 0)

# pickle.dumps/loads across ALL protocols on a deeply nested mixed object.
_nested = {"a": [1, (2, 3)], "b": {"c": {4, 5}}, "t": ("x", None, True), "f": 1.25}
_allp = all(
    pickle.loads(pickle.dumps(_nested, protocol=p)) == _nested
    for p in range(0, pickle.HIGHEST_PROTOCOL + 1))
chk("pickle_all_protocols", _allp)

# pickle.Pickler / pickle.Unpickler: stream-based serialization to BytesIO.
_bio = io.BytesIO()
pickle.Pickler(_bio, protocol=4).dump({"k": [1, 2, 3]})
_bio.seek(0)
chk("pickle_pickler_unpickler", pickle.Unpickler(_bio).load() == {"k": [1, 2, 3]})

# pickle custom __reduce__: object defines how it is reconstructed.
class _RedObj:
    def __init__(self, v):
        self.v = v
    def __reduce__(self):
        return (self.__class__, (self.v,))
    def __eq__(self, o):
        return isinstance(o, _RedObj) and self.v == o.v
chk("pickle_reduce", pickle.loads(pickle.dumps(_RedObj(99))) == _RedObj(99))

# pickle __getstate__/__setstate__: customize the pickled state dict.
class _StateObj:
    def __init__(self, a, b):
        self.a, self.b = a, b
        self.transient = "DROP"
    def __getstate__(self):
        return {"a": self.a, "b": self.b}
    def __setstate__(self, st):
        self.a = st["a"]
        self.b = st["b"]
        self.transient = "RESTORED"
_rt = pickle.loads(pickle.dumps(_StateObj(1, 2)))
chk("pickle_get_set_state",
    _rt.a == 1 and _rt.b == 2 and _rt.transient == "RESTORED")

# pickle preserves shared references (memoization): two refs stay identical.
_shared = [1, 2]
_pk = pickle.loads(pickle.dumps([_shared, _shared]))
chk("pickle_shared_refs", _pk[0] is _pk[1])

# pickle.UnpicklingError on corrupt data: documented exception type must
# match exactly (truncated proto-5 stream -> pickle.UnpicklingError), not
# merely "some exception" — catching the specific type detects divergence.
try:
    pickle.loads(b"\x80\x05corrupt-not-a-pickle")
    _pe = False
except pickle.UnpicklingError:
    _pe = True
chk("pickle_corrupt_raises", _pe)

# pickle.UnpicklingError is a subclass of pickle.PickleError (documented hierarchy).
chk("pickle_error_hierarchy",
    issubclass(pickle.UnpicklingError, pickle.PickleError)
    and issubclass(pickle.PicklingError, pickle.PickleError))

# pickle __reduce_ex__(protocol): protocol-aware reconstruction hook
# (takes precedence and receives the active protocol number).
class _RedExObj:
    def __init__(self, v):
        self.v = v
    def __reduce_ex__(self, proto):
        return (self.__class__, (self.v,))
    def __eq__(self, o):
        return isinstance(o, _RedExObj) and self.v == o.v
chk("pickle_reduce_ex", pickle.loads(pickle.dumps(_RedExObj(13))) == _RedExObj(13))

# pickle protocol byte: a protocol>=2 stream begins with the PROTO opcode
# (0x80) followed by the protocol number.
chk("pickle_proto_header",
    pickle.dumps(0, protocol=4)[:2] == b"\x80\x04")

# =====================================================================
# copy  (docs: copy — Shallow and deep copy operations)
# how: compare shallow vs deep on nested containers; expected: shallow
#      shares inner objects, deep is fully independent; why: silent aliasing
#      bugs come from confusing the two.
# =====================================================================
import copy

# copy.copy: shallow — top-level new, inner objects shared.
_orig = [[1, 2], [3, 4]]
_sh = copy.copy(_orig)
_sh[0].append(99)
chk("copy_shallow_shares_inner", _orig[0] == [1, 2, 99] and _sh is not _orig)

# copy.deepcopy: recursively independent.
_orig = [[1, 2], [3, 4]]
_dp = copy.deepcopy(_orig)
_dp[0].append(99)
chk("copy_deep_independent", _orig[0] == [1, 2] and _dp[0] == [1, 2, 99])

# copy.deepcopy handles cyclic references without infinite recursion.
_cyc = []
_cyc.append(_cyc)
_dc = copy.deepcopy(_cyc)
chk("copy_deep_cycle", _dc[0] is _dc)

# copy.deepcopy(x, memo): a user-supplied memo dict is populated with the
# objects already copied (keyed by id) and reuses entries for shared refs.
_memo = {}
_inner = [1, 2]
_src = [_inner, _inner]
_cp = copy.deepcopy(_src, _memo)
chk("copy_deepcopy_memo",
    len(_memo) > 0 and _cp[0] is _cp[1] and _cp[0] is not _inner)

# copy honors __copy__ / __deepcopy__ hooks.
class _CopyHook:
    def __copy__(self):
        return "SHALLOW"
    def __deepcopy__(self, memo):
        return "DEEP"
chk("copy_hooks", copy.copy(_CopyHook()) == "SHALLOW"
    and copy.deepcopy(_CopyHook()) == "DEEP")

# copy.replace (3.13+): returns a copy with named attrs replaced (NamedTuple/dataclass).
if hasattr(copy, "replace"):
    from collections import namedtuple as _nt
    _P = _nt("_P", "x y")
    chk("copy_replace", copy.replace(_P(1, 2), y=9) == _P(1, 9))
else:
    chk("copy_replace", True, "(skip: needs 3.13)")

# =====================================================================
# base64  (docs: base64 — Base16, Base32, Base64, Base85 Data Encodings)
# how: encode/decode against RFC 4648 / known vectors; expected: exact
#      strings & inverse round-trip; why: transport encoding for binary blobs.
# =====================================================================
import base64

# base64.b64encode/b64decode (RFC 4648 standard alphabet).
chk("base64_b64", base64.b64encode(b"foobar") == b"Zm9vYmFy"
    and base64.b64decode(b"Zm9vYmFy") == b"foobar")
# urlsafe variant: '+/' -> '-_'.
chk("base64_urlsafe",
    base64.urlsafe_b64encode(b"\xfb\xff") == b"-_8="
    and base64.urlsafe_b64decode(b"-_8=") == b"\xfb\xff")
# b32encode/b32decode (RFC 4648 base32).
chk("base64_b32", base64.b32encode(b"foobar") == b"MZXW6YTBOI======"
    and base64.b32decode(b"MZXW6YTBOI======") == b"foobar")
# b32hexencode/b32hexdecode (extended-hex base32).
chk("base64_b32hex", base64.b32hexencode(b"foobar") == b"CPNMUOJ1E8======"
    and base64.b32hexdecode(b"CPNMUOJ1E8======") == b"foobar")
# b16encode/b16decode (uppercase hex).
chk("base64_b16", base64.b16encode(b"foobar") == b"666F6F626172"
    and base64.b16decode(b"666F6F626172") == b"foobar")
# b85encode/b85decode (RFC-1924-ish base85).
chk("base64_b85", base64.b85encode(b"foobar") == b"W^Zp|VR8"
    and base64.b85decode(b"W^Zp|VR8") == b"foobar")
# a85encode/a85decode (Ascii85).
chk("base64_a85", base64.a85encode(b"foobar") == b"AoDTs@<)"
    and base64.a85decode(b"AoDTs@<)") == b"foobar")
# Standard encode/decode (file-stream-style functions operate on bytes too).
chk("base64_standard_b64",
    base64.standard_b64encode(b"x") == b"eA==" and base64.standard_b64decode(b"eA==") == b"x")
# b64encode/b64decode(altchars=...): custom 62/63 chars; '-_' reproduces urlsafe.
chk("base64_altchars",
    base64.b64encode(b"\xfb\xff", altchars=b"-_") == b"-_8="
    and base64.b64decode(b"-_8=", altchars=b"-_") == b"\xfb\xff")
# b32decode(casefold=True): accept lowercase input.
chk("base64_b32_casefold",
    base64.b32decode(b"mzxw6ytboi======", casefold=True) == b"foobar")
# b16decode(casefold=True): accept lowercase hex.
chk("base64_b16_casefold", base64.b16decode(b"666f6f626172", casefold=True) == b"foobar")
# invalid base64 with validate=True raises binascii.Error (a ValueError
# subclass) — catch the documented type so a wrong/silent codec is caught.
try:
    base64.b64decode(b"@@@@", validate=True)
    _b64e = False
except base64.binascii.Error:
    _b64e = True
chk("base64_validate_raises", _b64e)

# =====================================================================
# binascii  (docs: binascii — Convert between binary and ASCII)
# how: hexlify/unhexlify, crc32, hqx, base64 helpers; expected: known
#      vectors; why: low-level codecs underpin base64/zlib & protocols.
# =====================================================================
import binascii

# binascii.hexlify / unhexlify (a2b_hex / b2a_hex aliases).
chk("binascii_hexlify",
    binascii.hexlify(b"AB") == b"4142" and binascii.unhexlify(b"4142") == b"AB")
chk("binascii_hex_aliases",
    binascii.b2a_hex(b"\x0f") == b"0f" and binascii.a2b_hex(b"0f") == b"\x0f")
# hexlify with separator (3.8+).
chk("binascii_hexlify_sep", binascii.hexlify(b"ABC", "-") == b"41-42-43")
# binascii.crc32: known vector for b"abc".
chk("binascii_crc32", binascii.crc32(b"abc") == 0x352441C2)
# crc32 incremental: value = crc32(b2, crc32(b1)).
chk("binascii_crc32_incremental",
    binascii.crc32(b"def", binascii.crc32(b"abc")) == binascii.crc32(b"abcdef"))
# binascii.crc_hqx (CRC-CCITT, XModem): known vector for b"abc" with seed 0,
# plus the incremental (seed-chaining) property — value-exact, not type-only.
chk("binascii_crc_hqx",
    binascii.crc_hqx(b"abc", 0) == 0x9DD6
    and binascii.crc_hqx(b"bc", binascii.crc_hqx(b"a", 0)) == binascii.crc_hqx(b"abc", 0))
# binascii.b2a_base64 / a2b_base64 (newline-terminated base64).
chk("binascii_b2a_base64",
    binascii.b2a_base64(b"x") == b"eA==\n" and binascii.a2b_base64(b"eA==\n") == b"x")
# b2a_base64(newline=False) suppresses trailing newline (3.6+).
chk("binascii_b2a_base64_nonl", binascii.b2a_base64(b"x", newline=False) == b"eA==")
# binascii.b2a_qp / a2b_qp: quoted-printable codec; '=' and trailing
# whitespace are escaped as =XX.
chk("binascii_qp",
    binascii.b2a_qp(b"a b=") == b"a b=3D"
    and binascii.a2b_qp(b"a=3Db") == b"a=b")
# binascii.b2a_uu / a2b_uu: uuencode line codec round-trips losslessly.
chk("binascii_uu",
    binascii.a2b_uu(binascii.b2a_uu(b"foobar")) == b"foobar"
    and binascii.b2a_uu(b"abc").endswith(b"\n"))
# binascii.Error on bad hex (odd length / non-hex).
try:
    binascii.unhexlify(b"xyz")
    _bae = False
except binascii.Error:
    _bae = True
chk("binascii_error", _bae)

# =====================================================================
# hashlib  (docs: hashlib — Secure hashes and message digests)
# how: hexdigest of b"abc" against published vectors for every algorithm;
#      incremental update; .copy(); pbkdf2; new(); file_digest; expected:
#      exact hex strings; why: cryptographic correctness is non-negotiable.
# =====================================================================
import hashlib

# Guaranteed algorithms: published digests of b"abc".
chk("hashlib_md5",
    hashlib.md5(b"abc").hexdigest() == "900150983cd24fb0d6963f7d28e17f72")
chk("hashlib_sha1",
    hashlib.sha1(b"abc").hexdigest() == "a9993e364706816aba3e25717850c26c9cd0d89d")
chk("hashlib_sha224",
    hashlib.sha224(b"abc").hexdigest()
    == "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7")
chk("hashlib_sha256",
    hashlib.sha256(b"abc").hexdigest()
    == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
chk("hashlib_sha384",
    hashlib.sha384(b"abc").hexdigest()
    == "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed"
       "8086072ba1e7cc2358baeca134c825a7")
chk("hashlib_sha512",
    hashlib.sha512(b"abc").hexdigest()
    == "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a"
       "2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
chk("hashlib_sha3_256",
    hashlib.sha3_256(b"abc").hexdigest()
    == "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")
chk("hashlib_sha3_512",
    hashlib.sha3_512(b"abc").hexdigest()
    == "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e"
       "10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0")
chk("hashlib_blake2b",
    hashlib.blake2b(b"abc").hexdigest()
    == "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1"
       "7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923")
chk("hashlib_blake2s",
    hashlib.blake2s(b"abc").hexdigest()
    == "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982")
# SHAKE (extendable-output): explicit length argument.
chk("hashlib_shake128",
    hashlib.shake_128(b"abc").hexdigest(16) == "5881092dd818bf5cf8a3ddb793fbcba7")
chk("hashlib_shake256",
    hashlib.shake_256(b"abc").hexdigest(16) == "483366601360a8771c6863080cc4114d")
# SHAKE variable output length: requesting more bytes is a prefix-extension,
# and the requested length is honored exactly.
_sk32 = hashlib.shake_128(b"abc").hexdigest(32)
chk("hashlib_shake_varlen",
    len(_sk32) == 64 and _sk32.startswith("5881092dd818bf5cf8a3ddb793fbcba7"))

# Incremental update equals one-shot.
_h = hashlib.sha256()
_h.update(b"ab")
_h.update(b"c")
chk("hashlib_incremental_update",
    _h.hexdigest() == hashlib.sha256(b"abc").hexdigest())

# .copy(): forks digest state independently.
_h1 = hashlib.sha256(b"a")
_h2 = _h1.copy()
_h2.update(b"bc")
chk("hashlib_copy",
    _h1.hexdigest() == hashlib.sha256(b"a").hexdigest()
    and _h2.hexdigest() == hashlib.sha256(b"abc").hexdigest())

# digest_size / block_size / name attributes.
chk("hashlib_attributes",
    hashlib.sha256().digest_size == 32 and hashlib.sha256().name == "sha256"
    and hashlib.sha256().block_size == 64)

# .digest() returns bytes equal to bytes.fromhex(hexdigest).
chk("hashlib_digest_bytes",
    hashlib.sha256(b"abc").digest() == bytes.fromhex(hashlib.sha256(b"abc").hexdigest()))

# blake2 keyed hashing (MAC mode) differs from unkeyed.
chk("hashlib_blake2_keyed",
    hashlib.blake2b(b"msg", key=b"k").hexdigest()
    != hashlib.blake2b(b"msg").hexdigest())

# blake2 personalization (person) and salt parameters each change the digest
# and are independent from key and from each other.
_b0 = hashlib.blake2b(b"x").hexdigest()
chk("hashlib_blake2_person",
    hashlib.blake2b(b"x", person=b"p").hexdigest() != _b0)
chk("hashlib_blake2_salt",
    hashlib.blake2b(b"x", salt=b"s").hexdigest() != _b0
    and hashlib.blake2b(b"x", salt=b"s").hexdigest()
        != hashlib.blake2b(b"x", person=b"s").hexdigest())

# blake2 custom digest_size: truncated output of the requested length.
chk("hashlib_blake2_digest_size",
    hashlib.blake2b(b"x", digest_size=16).digest_size == 16
    and len(hashlib.blake2b(b"x", digest_size=16).digest()) == 16)

# hashlib.new(name): construct by algorithm name.
chk("hashlib_new",
    hashlib.new("sha256", b"abc").hexdigest() == hashlib.sha256(b"abc").hexdigest())

# hashlib.algorithms_available / algorithms_guaranteed: documented sets.
chk("hashlib_algorithms_guaranteed",
    {"md5", "sha1", "sha256", "sha512"} <= hashlib.algorithms_guaranteed
    and hashlib.algorithms_guaranteed <= hashlib.algorithms_available)

# hashlib.pbkdf2_hmac: known RFC-6070-style vector (sha256, 1 iter).
chk("hashlib_pbkdf2",
    hashlib.pbkdf2_hmac("sha256", b"password", b"salt", 1).hex()
    == "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b")

# hashlib.scrypt: RFC 7914 vector (N=16, r=1, p=1, empty pw/salt).
# Guarded — scrypt requires an OpenSSL build that exposes EVP_PBE_scrypt.
if hasattr(hashlib, "scrypt"):
    try:
        chk("hashlib_scrypt",
            hashlib.scrypt(b"", salt=b"", n=16, r=1, p=1, dklen=64).hex()
            == "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442"
               "fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906")
    except (ValueError, NotImplementedError):
        chk("hashlib_scrypt", True, "(skip: scrypt unsupported by OpenSSL build)")
else:
    chk("hashlib_scrypt", True, "(skip: no scrypt)")

# hashlib.file_digest (3.11+): hash a binary file object.
if hasattr(hashlib, "file_digest"):
    chk("hashlib_file_digest",
        hashlib.file_digest(io.BytesIO(b"abc"), "sha256").hexdigest()
        == hashlib.sha256(b"abc").hexdigest())
else:
    chk("hashlib_file_digest", True, "(skip: needs 3.11)")

# =====================================================================
# hmac  (docs: hmac — Keyed-Hashing for Message Authentication)
# how: HMAC against RFC 2104/2202 vectors; expected exact hex; why: message
#      authentication primitive, must match other implementations exactly.
# =====================================================================
import hmac

# hmac.new(...).hexdigest(): RFC 2202 HMAC-MD5 test case 1.
chk("hmac_md5",
    hmac.new(b"\x0b" * 16, b"Hi There", "md5").hexdigest()
    == "9294727a3638bb1c13f48ef8158bfc9d")
# HMAC-SHA256 known vector.
chk("hmac_sha256",
    hmac.new(b"key", b"The quick brown fox jumps over the lazy dog", "sha256").hexdigest()
    == "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
# hmac.digest() one-shot equals .new().digest().
chk("hmac_digest_oneshot",
    hmac.digest(b"key", b"msg", "sha256")
    == hmac.new(b"key", b"msg", "sha256").digest())
# hmac.compare_digest: constant-time equality (True/False).
chk("hmac_compare_digest",
    hmac.compare_digest(b"abc", b"abc") and not hmac.compare_digest(b"abc", b"abd"))
# hmac incremental update equals one-shot.
_hm = hmac.new(b"key", digestmod="sha256")
_hm.update(b"ab")
_hm.update(b"c")
chk("hmac_incremental",
    _hm.hexdigest() == hmac.new(b"key", b"abc", "sha256").hexdigest())

# hmac digestmod accepts a callable/module form equivalent to the name form.
chk("hmac_digestmod_callable",
    hmac.new(b"key", b"abc", hashlib.sha256).hexdigest()
    == hmac.new(b"key", b"abc", "sha256").hexdigest())

# hmac object .copy(): fork the MAC state; further updates diverge.
_hmc = hmac.new(b"key", b"a", "sha256")
_hmc2 = _hmc.copy()
_hmc2.update(b"bc")
chk("hmac_copy",
    _hmc.hexdigest() == hmac.new(b"key", b"a", "sha256").hexdigest()
    and _hmc2.hexdigest() == hmac.new(b"key", b"abc", "sha256").hexdigest())

# hmac object exposes digest_size / block_size / name attributes.
chk("hmac_attributes",
    hmac.new(b"k", b"m", "sha256").digest_size == 32
    and hmac.new(b"k", b"m", "sha256").name == "hmac-sha256")

# =====================================================================
# secrets  (docs: secrets — Generate secure random numbers)
# how: check lengths/ranges/membership of generated tokens; expected:
#      correct sizes and bounds (values random); why: secure RNG for tokens.
# =====================================================================
import secrets

# secrets.token_bytes(n): exactly n random bytes.
chk("secrets_token_bytes",
    isinstance(secrets.token_bytes(16), bytes) and len(secrets.token_bytes(16)) == 16)
# secrets.token_hex(n): 2n hex chars.
chk("secrets_token_hex",
    len(secrets.token_hex(16)) == 32 and all(c in "0123456789abcdef" for c in secrets.token_hex(8)))
# secrets.token_urlsafe(n): URL-safe text, ~ceil(4n/3) chars (no padding).
chk("secrets_token_urlsafe", isinstance(secrets.token_urlsafe(16), str)
    and len(secrets.token_urlsafe(16)) >= 16)
# secrets.randbelow(n): 0 <= x < n.
chk("secrets_randbelow", all(0 <= secrets.randbelow(10) < 10 for _ in range(50)))
# secrets.choice(seq): element drawn from the sequence.
chk("secrets_choice", secrets.choice(["a", "b", "c"]) in ("a", "b", "c"))
# secrets.compare_digest: constant-time equality (alias of hmac's).
chk("secrets_compare_digest",
    secrets.compare_digest("xy", "xy") and not secrets.compare_digest("xy", "xz"))
# secrets.randbits(k): integer with at most k bits.
chk("secrets_randbits", 0 <= secrets.randbits(8) < 256)
# secrets.token_hex/token_bytes length is byte-exact, and token_urlsafe is
# unpadded url-safe text (no '+' '/' '=').
chk("secrets_token_lengths",
    len(secrets.token_bytes(7)) == 7 and len(secrets.token_hex(7)) == 14)
_tu = secrets.token_urlsafe(24)
chk("secrets_token_urlsafe_alphabet",
    all(c not in "+/=" for c in _tu))
# secrets.SystemRandom: CSPRNG generator; randrange/randint stay in bounds.
_sr = secrets.SystemRandom()
chk("secrets_systemrandom",
    isinstance(_sr, secrets.SystemRandom)
    and all(0 <= _sr.randrange(10) < 10 for _ in range(30))
    and all(1 <= _sr.randint(1, 3) <= 3 for _ in range(30)))

# =====================================================================
# zlib  (docs: zlib — Compression compatible with gzip)
# how: compress/decompress round-trip; checksums vs vectors; streaming
#      compressobj/decompressobj; expected: lossless + known crc/adler;
#      why: DEFLATE underlies gzip/zip; checksums verify integrity.
# =====================================================================
import zlib

_payload = b"the quick brown fox " * 50
# zlib.compress / zlib.decompress: lossless round-trip; output smaller.
chk("zlib_roundtrip",
    zlib.decompress(zlib.compress(_payload)) == _payload
    and len(zlib.compress(_payload)) < len(_payload))
# zlib.compress(level): 0..9 all decompress back to the original.
chk("zlib_levels",
    all(zlib.decompress(zlib.compress(_payload, lv)) == _payload for lv in range(0, 10)))
# zlib.crc32: known vector for b"abc".
chk("zlib_crc32", zlib.crc32(b"abc") & 0xffffffff == 0x352441C2)
# zlib.adler32: known vector for b"abc".
chk("zlib_adler32", zlib.adler32(b"abc") & 0xffffffff == 0x024D0127)
# zlib streaming: compressobj/decompressobj with .compress()+.flush().
_co = zlib.compressobj()
_chunk = _co.compress(_payload) + _co.flush()
_do = zlib.decompressobj()
chk("zlib_stream_objs",
    _do.decompress(_chunk) + _do.flush() == _payload)
# zlib raw deflate via wbits=-15 (no zlib header/trailer): compressobj +
# decompressobj must agree on wbits to round-trip.
_rco = zlib.compressobj(wbits=-15)
_raw = _rco.compress(_payload) + _rco.flush()
_rdo = zlib.decompressobj(wbits=-15)
chk("zlib_wbits_raw", _rdo.decompress(_raw) + _rdo.flush() == _payload)

# zlib decompressobj.decompress(data, max_length): bounded output; remaining
# input is stashed in unconsumed_tail for a follow-up call.
_mdo = zlib.decompressobj()
_full = zlib.compress(_payload)
_part = _mdo.decompress(_full, 10)
chk("zlib_max_length",
    len(_part) == 10 and len(_mdo.unconsumed_tail) > 0
    and _part + _mdo.decompress(_mdo.unconsumed_tail) + _mdo.flush() == _payload)

# zlib gzip-wrapped output via wbits=31 carries the gzip magic header and
# round-trips through a wbits=31 decompressor.
_gzwrap = zlib.compress(b"hi", wbits=31)
chk("zlib_wbits_gzip",
    _gzwrap[:2] == b"\x1f\x8b"
    and zlib.decompress(_gzwrap, wbits=31) == b"hi")

# zlib.error on garbage input.
try:
    zlib.decompress(b"not-compressed-garbage")
    _ze = False
except zlib.error:
    _ze = True
chk("zlib_error", _ze)

# =====================================================================
# gzip  (docs: gzip — Support for gzip files)
# how: gzip.compress/decompress one-shot + GzipFile stream over BytesIO;
#      expected: lossless; why: standard .gz format with header/CRC.
# =====================================================================
import gzip

# gzip.compress / gzip.decompress: one-shot lossless round-trip.
chk("gzip_oneshot", gzip.decompress(gzip.compress(_payload)) == _payload)
# gzip.GzipFile: write then read back through a BytesIO stream.
_gb = io.BytesIO()
with gzip.GzipFile(fileobj=_gb, mode="wb") as _gf:
    _gf.write(_payload)
_gb.seek(0)
with gzip.GzipFile(fileobj=_gb, mode="rb") as _gf:
    _read = _gf.read()
chk("gzip_filelike", _read == _payload)
# gzip output begins with the magic header 0x1f 0x8b.
chk("gzip_magic", gzip.compress(b"x")[:2] == b"\x1f\x8b")
# gzip.compress(mtime=0): deterministic/reproducible output (mtime field zeroed).
chk("gzip_mtime_deterministic",
    gzip.compress(_payload, mtime=0) == gzip.compress(_payload, mtime=0))
# gzip.compress(compresslevel): all 0..9 levels decompress back losslessly.
chk("gzip_compresslevel",
    all(gzip.decompress(gzip.compress(_payload, compresslevel=lv)) == _payload
        for lv in range(0, 10)))
# GzipFile records the embedded mtime and exposes peek() without consuming.
_gmb = io.BytesIO()
with gzip.GzipFile(fileobj=_gmb, mode="wb", mtime=4242) as _gf:
    _gf.write(_payload)
_gmb.seek(0)
with gzip.GzipFile(fileobj=_gmb, mode="rb") as _gf:
    _peeked = _gf.peek(4)
    _allread = _gf.read()
    _mt = _gf.mtime
chk("gzip_mtime_peek",
    _allread == _payload and _allread.startswith(_peeked) and _mt == 4242)

# =====================================================================
# bz2  (docs: bz2 — Support for bzip2 compression)
# how: bz2.compress/decompress + BZ2Compressor/Decompressor streaming;
#      expected: lossless; why: bzip2 is a common archive codec.
# =====================================================================
import bz2

# bz2.compress / bz2.decompress: one-shot round-trip.
chk("bz2_oneshot", bz2.decompress(bz2.compress(_payload)) == _payload)
# bz2.BZ2Compressor / BZ2Decompressor: streaming round-trip.
_bc = bz2.BZ2Compressor()
_bdata = _bc.compress(_payload) + _bc.flush()
chk("bz2_stream", bz2.BZ2Decompressor().decompress(_bdata) == _payload)
# bz2.compress(compresslevel): levels 1..9 all round-trip losslessly.
chk("bz2_compresslevel",
    all(bz2.decompress(bz2.compress(_payload, lv)) == _payload for lv in range(1, 10)))
# bz2.BZ2File: file-object interface over a BytesIO stream.
_bzb = io.BytesIO()
with bz2.BZ2File(_bzb, mode="wb") as _bf:
    _bf.write(_payload)
_bzb.seek(0)
with bz2.BZ2File(_bzb, mode="rb") as _bf:
    chk("bz2_bz2file", _bf.read() == _payload)

# =====================================================================
# lzma  (docs: lzma — Compression using the LZMA algorithm)
# how: lzma.compress/decompress with both xz and alone formats; streaming;
#      expected: lossless; why: xz/lzma is the highest-ratio stdlib codec.
# =====================================================================
import lzma

# lzma.compress / lzma.decompress: default .xz format round-trip.
chk("lzma_oneshot", lzma.decompress(lzma.compress(_payload)) == _payload)
# lzma FORMAT_ALONE (legacy .lzma) round-trip.
_alone = lzma.compress(_payload, format=lzma.FORMAT_ALONE)
chk("lzma_format_alone",
    lzma.decompress(_alone, format=lzma.FORMAT_ALONE) == _payload)
# lzma.LZMACompressor / LZMADecompressor: streaming round-trip.
_lc = lzma.LZMACompressor()
_ldata = _lc.compress(_payload) + _lc.flush()
chk("lzma_stream", lzma.LZMADecompressor().decompress(_ldata) == _payload)
# lzma FORMAT_RAW with an explicit filter chain: same filters required to
# decode (no container/header self-describes the stream).
_filters = [{"id": lzma.FILTER_LZMA2, "preset": 6}]
_rawx = lzma.compress(_payload, format=lzma.FORMAT_RAW, filters=_filters)
chk("lzma_format_raw",
    lzma.decompress(_rawx, format=lzma.FORMAT_RAW, filters=_filters) == _payload)
# lzma.compress(preset): 0..9 presets all round-trip on the default xz format.
chk("lzma_preset",
    all(lzma.decompress(lzma.compress(_payload, preset=p)) == _payload
        for p in range(0, 10)))
# lzma integrity check selection (CHECK_CRC32) still round-trips; constant exposed.
chk("lzma_check",
    lzma.decompress(lzma.compress(_payload, check=lzma.CHECK_CRC32)) == _payload
    and lzma.CHECK_NONE == 0)
# lzma.LZMAFile: file-object interface over a BytesIO stream.
_lzb = io.BytesIO()
with lzma.LZMAFile(_lzb, mode="wb") as _lf:
    _lf.write(_payload)
_lzb.seek(0)
with lzma.LZMAFile(_lzb, mode="rb") as _lf:
    chk("lzma_lzmafile", _lf.read() == _payload)
# lzma.LZMAError on corrupt xz input (documented exception type).
try:
    lzma.decompress(b"\xfd7zXZ\x00garbage")
    _le = False
except lzma.LZMAError:
    _le = True
chk("lzma_error", _le)

# =====================================================================
# compression.zstd  (3.14, PEP 784: Zstandard in the stdlib)
# how: if the module exists, round-trip via compress/decompress; else note
#      a guarded skip; why: zstd is the new stdlib codec on 3.14.
# =====================================================================
if sys.version_info >= (3, 14):
    try:
        from compression import zstd as _zstd
        chk("compression_zstd",
            _zstd.decompress(_zstd.compress(_payload)) == _payload)
        # level argument: positive levels compress, all round-trip losslessly.
        chk("compression_zstd_level",
            _zstd.decompress(_zstd.compress(_payload, level=10)) == _payload)
        # ZstdCompressor / ZstdDecompressor: streaming round-trip.
        _zc = _zstd.ZstdCompressor()
        _zdata = _zc.compress(_payload) + _zc.flush()
        chk("compression_zstd_stream",
            _zstd.ZstdDecompressor().decompress(_zdata) == _payload)
        # ZstdFile: file-object interface over a BytesIO stream.
        _zsb = io.BytesIO()
        with _zstd.ZstdFile(_zsb, mode="wb") as _zf:
            _zf.write(_payload)
        _zsb.seek(0)
        with _zstd.ZstdFile(_zsb, mode="rb") as _zf:
            chk("compression_zstd_file", _zf.read() == _payload)
        # ZstdError on corrupt input (documented exception type).
        try:
            _zstd.decompress(b"not-a-zstd-frame-at-all")
            _zse = False
        except _zstd.ZstdError:
            _zse = True
        chk("compression_zstd_error", _zse)
    except ImportError:
        chk("compression_zstd", True, "(skip: zstd build optional)")
        chk("compression_zstd_level", True, "(skip: zstd build optional)")
        chk("compression_zstd_stream", True, "(skip: zstd build optional)")
        chk("compression_zstd_file", True, "(skip: zstd build optional)")
        chk("compression_zstd_error", True, "(skip: zstd build optional)")
else:
    chk("compression_zstd", True, "(skip: needs 3.14)")
    chk("compression_zstd_level", True, "(skip: needs 3.14)")
    chk("compression_zstd_stream", True, "(skip: needs 3.14)")
    chk("compression_zstd_file", True, "(skip: needs 3.14)")
    chk("compression_zstd_error", True, "(skip: needs 3.14)")

print(("PY_ENCODING_OK") if _ok else ("PY_ENCODING_FAIL"))
sys.exit(0 if _ok else 1)
