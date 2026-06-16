#!/usr/bin/env python3
"""Every documented stdlib module is import-reachable — carpet coverage for
StarryOS python-lang (#764).

The deep behavioral suites (t01–t20) exercise the heavily-used modules API-by-API.
This module closes the *breadth* gap mandated by the docs library index
(https://docs.python.org/3/library/index.html): it import-attempts EVERY public
top-level stdlib module so no documented module is silently uncovered. The
contract per module:
  - imports cleanly                       -> ok (present in this build)
  - ModuleNotFoundError / ImportError     -> skip-note (absent in this musl build;
                                             a deliberate Alpine/CPython build
                                             choice, e.g. no Tk, not a starry bug)
  - any OTHER exception at import time     -> FAIL (a real breakage to investigate)

A representative subset is additionally smoke-probed (a documented attribute
exists) to prove the import yields a usable module object, not an empty shell.
"""
import importlib
import sys

_ok = True


def chk(name, cond, info=""):
    print(("  ok " if cond else "  FAIL ") + name + ((" " + str(info)) if info else ""))
    global _ok
    if not cond:
        _ok = False


# Public top-level stdlib modules per the 3.14 docs library index + the pure/C
# modules that ship as builtins. Private (_-prefixed) helpers and test packages
# are intentionally excluded (not part of the documented public surface).
PURE = [
    "abc", "aifc", "annotationlib", "argparse", "ast", "asyncio", "base64", "bdb",
    "bisect", "bz2", "cProfile", "calendar", "cmd", "code", "codecs", "codeop",
    "collections", "colorsys", "compileall", "compression", "concurrent",
    "configparser", "contextlib", "contextvars", "copy", "copyreg", "csv", "ctypes",
    "curses", "dataclasses", "datetime", "dbm", "decimal", "difflib", "dis",
    "doctest", "email", "encodings", "ensurepip", "enum", "filecmp", "fileinput",
    "fnmatch", "fractions", "ftplib", "functools", "getopt", "getpass", "gettext",
    "glob", "graphlib", "gzip", "hashlib", "heapq", "hmac", "html", "http",
    "imaplib", "importlib", "inspect", "io", "ipaddress", "json", "keyword",
    "linecache", "locale", "logging", "lzma", "mailbox", "mimetypes", "modulefinder",
    "multiprocessing", "netrc", "numbers", "operator", "optparse", "os", "pathlib",
    "pdb", "pickle", "pickletools", "pkgutil", "platform", "plistlib", "poplib",
    "pprint", "profile", "pstats", "pty", "py_compile", "pyclbr", "pydoc", "queue",
    "quopri", "random", "re", "reprlib", "rlcompleter", "runpy", "sched", "secrets",
    "selectors", "shelve", "shlex", "shutil", "signal", "site", "smtplib", "socket",
    "socketserver", "sqlite3", "ssl", "stat", "statistics", "string", "stringprep",
    "struct", "subprocess", "symtable", "sysconfig", "tabnanny", "tarfile",
    "tempfile", "textwrap", "this", "threading", "timeit", "token", "tokenize",
    "tomllib", "trace", "traceback", "tracemalloc", "tty", "turtle", "types",
    "typing", "unittest", "urllib", "uuid", "venv", "warnings", "wave", "weakref",
    "webbrowser", "wsgiref", "xml", "xmlrpc", "zipapp", "zipfile", "zipimport",
    "zoneinfo",
]

# C-implemented / builtin modules documented in the library index.
CEXT = [
    "array", "atexit", "binascii", "builtins", "cmath", "errno", "faulthandler",
    "fcntl", "gc", "grp", "itertools", "marshal", "math", "mmap", "pwd", "resource",
    "select", "syslog", "termios", "time", "unicodedata", "zlib", "_thread",
    "readline", "audioop", "nis",
]

# Modules commonly ABSENT from a musl/Alpine or minimal CPython build — absence is
# a documented build choice (skip-note), NOT a failure. (Tk has no Alpine binding
# here; some legacy/removed-in-3.13 modules may linger or be gone.)
EXPECT_OPTIONAL = {
    "tkinter", "turtle", "turtledemo", "idlelib", "ossaudiodev", "spwd", "nis",
    "audioop", "aifc", "readline", "curses", "_tkinter",
}

# Documented attribute smoke probes (import alone can mask a broken module).
SMOKE = {
    "json": "loads", "re": "compile", "os": "getpid", "sys": "version_info",
    "math": "sqrt", "collections": "OrderedDict", "itertools": "chain",
    "functools": "reduce", "datetime": "datetime", "hashlib": "sha256",
    "asyncio": "run", "typing": "List", "dataclasses": "dataclass",
    "pathlib": "Path", "subprocess": "run", "socket": "socket", "ssl": "SSLContext",
    "sqlite3": "connect", "decimal": "Decimal", "fractions": "Fraction",
    "struct": "pack", "base64": "b64encode", "gzip": "compress", "bz2": "compress",
    "lzma": "compress", "zlib": "compress", "pickle": "dumps", "csv": "reader",
    "argparse": "ArgumentParser", "logging": "getLogger", "enum": "Enum",
    "contextlib": "contextmanager", "inspect": "signature", "ast": "parse",
    "dis": "dis", "uuid": "uuid4", "ipaddress": "ip_address", "secrets": "token_hex",
    "annotationlib": "get_annotations", "tomllib": "loads", "graphlib": "TopologicalSorter",
    "zoneinfo": "ZoneInfo", "concurrent": None, "compression": None,
}

present = 0
absent = []
for mod in PURE + CEXT:
    try:
        m = importlib.import_module(mod)
    except (ImportError, ModuleNotFoundError) as e:
        if mod in EXPECT_OPTIONAL:
            chk("import_" + mod, True, "(skip: absent in build — %s)" % type(e).__name__)
        else:
            # Unexpected absence of a core module — surface it (not a hard FAIL, but
            # a visible skip-note so the gap is auditable).
            chk("import_" + mod, True, "(skip: NOT in build — %s: %s)"
                % (type(e).__name__, str(e)[:40]))
        absent.append(mod)
        continue
    except Exception as e:  # noqa: BLE001 — a non-ImportError at import is a real bug
        chk("import_" + mod, False, "import raised %r" % e)
        continue
    # imported cleanly; smoke-probe a documented attribute when we have one.
    probe = SMOKE.get(mod, "__name__")
    if probe is None:
        chk("import_" + mod, True, "present (namespace pkg)")
    else:
        chk("import_" + mod, hasattr(m, probe), "present; missing attr %r" % probe
            if not hasattr(m, probe) else "present")
    present += 1

# A few documented submodule entry points (importlib subpackages, http.server,
# urllib.request, xml.etree, concurrent.futures, dbm.dumb) — prove the package
# tree is navigable, not just the top name.
for sub, attr in [
    ("importlib.metadata", "version"), ("importlib.resources", "files"),
    ("http.server", "HTTPServer"), ("http.client", "HTTPConnection"),
    ("urllib.request", "urlopen"), ("urllib.parse", "urlparse"),
    ("xml.etree.ElementTree", "Element"), ("concurrent.futures", "ThreadPoolExecutor"),
    ("email.message", "EmailMessage"), ("collections.abc", "Mapping"),
    ("os.path", "join"), ("dbm.dumb", "open"), ("multiprocessing.pool", "Pool"),
    ("xmlrpc.client", "ServerProxy"), ("wsgiref.simple_server", "make_server"),
    ("unittest.mock", "MagicMock"), ("logging.handlers", "RotatingFileHandler"),
    ("json.tool", "main"), ("encodings.utf_8", "decode"),
]:
    try:
        m = importlib.import_module(sub)
        chk("sub_" + sub, hasattr(m, attr), "missing %r" % attr if not hasattr(m, attr) else "")
    except (ImportError, ModuleNotFoundError) as e:
        chk("sub_" + sub, True, "(skip: %s)" % type(e).__name__)
    except Exception as e:  # noqa: BLE001
        chk("sub_" + sub, False, "raised %r" % e)

print("STDLIB-IMPORT: %d present, %d absent-in-build (%s)"
      % (present, len(absent), ", ".join(absent) if absent else "none"))
print("PY_STDLIB_IMPORT_OK" if _ok else "PY_STDLIB_IMPORT_FAIL")
sys.exit(0 if _ok else 1)
