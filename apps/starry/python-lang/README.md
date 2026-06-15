# Starry python-lang App — CPython 3.14 language-level carpet suite

This app runs an industrial, carpet-coverage **CPython 3.14** language + standard
library test suite inside StarryOS QEMU, across `x86_64 / aarch64 / riscv64 /
loongarch64`. It validates the *language layer* (interpreter, syntax, data model,
stdlib APIs, concurrency, the `python3` CLI) — not third-party/native packages
(numpy/pyarrow/… are separate cases).

## Python 3.14

The base Alpine image ships Python 3.12, so `prebuild.sh` provisions CPython 3.14
portably (mirrors the merged `pip` app): it extracts the base rootfs to a staging
tree, `apk add python3` from **Alpine edge** into it via `qemu-<arch>-static` (so
it works for every target arch on an x86 build host), enforces a hard ≥3.14
version gate, then copies the `python3` binary, its shared-library closure, and
the full standard library into the app overlay alongside the test modules. No
prebuilt images and no host-absolute paths — only the registered base rootfs and
the app's own `python/` sources. A configured `prebuild.sh` makes the app runner
skip the managed-image ensure, so the overlay-provisioned 3.14 is what boots.

## Layout

```text
apps/starry/python-lang/
  prebuild.sh                  # install CPython 3.14 + stage modules (overlay)
  build-<target>.toml          # StarryOS build config (4 targets)
  qemu-<arch>.toml             # QEMU run config (4 arches)
  python/
    run_all.py                 # aggregator: runs each module, prints TEST PASSED iff all pass
    t01_syntax.py              # all syntax/operators/comprehensions/match/decorators
    t02_datamodel.py           # every special/dunder method
    t03_oop.py                 # MRO/super/metaclass/abc/dataclass/enum/property/slots
    t04_builtin_types.py       # every builtin type + all methods
    t05_builtin_funcs.py       # every builtin function
    t06_generators_itertools.py# generators + full itertools
    t07_functools_operator.py  # full functools + operator
    t08_async.py               # asyncio / coroutines / async gen / TaskGroup
    t09_threads.py             # threading / queue / ThreadPoolExecutor
    t10_multiprocessing.py     # multiprocessing / ProcessPoolExecutor
    t11_introspection.py       # inspect / ast / dis / gc / weakref / sys
    t12_text_re_struct.py      # re / string / textwrap / unicodedata / struct
    t13_data_encoding.py       # json / csv / pickle / base64 / hashlib / zlib…
    t14_numeric_collections.py # math / decimal / fractions / statistics / random / collections / heapq…
    t15_os_fs_io.py            # os / pathlib / io / tempfile / shutil / subprocess / signal
    t16_datetime_contextlib.py # datetime / time / calendar / contextlib / signal / subprocess
    t17_typing_argparse_logging.py # typing / argparse / logging / warnings / uuid / ipaddress
    t18_py314.py               # 3.14 features: t-strings / annotationlib / concurrent.interpreters / zstd / except-no-parens / from_number / map(strict) / memoryview[] / compression ns
    t19_cli.py                 # the `python3` CLI: every --help option + -m stdlib + REPL (-i) + multi-entry + PYTHON* env + exit codes
    t20_dash_m.py              # every `python3 -m` stdlib CLI tool (json.tool/base64/dis/tokenize/pydoc/zipfile/tarfile/gzip/timeit/cProfile/unittest/…)
    t21_stdlib_import.py       # docs library-index breadth: every public stdlib module import-reachable + subpackage entry points
    test_lang.py               # cross-cutting smoke + `python3 -m venv`
```

Each module is self-contained: it prints `  ok <name>` / `  FAIL <name>` per
documented behavior and exits non-zero on any failure. Version-specific *syntax*
(3.14-only) is isolated in `exec()` guarded by `sys.version_info`, so every module
also parses and runs on older interpreters (3.14-only checks take a noted skip).

## Run

```bash
source /home/heke/rcore/tgoskits/.starry-env.sh   # qemu-10
cargo xtask starry app qemu -t python-lang --arch aarch64
cargo xtask starry app qemu -t python-lang --arch riscv64
cargo xtask starry app qemu -t python-lang --arch loongarch64
cargo xtask starry app qemu -t python-lang --arch x86_64
```

Success criterion: `run_all.py` prints `TEST PASSED` on its final line iff every
module exits 0 (`success_regex = (?m)^TEST PASSED\s*$`); any module failure prints
`TEST FAILED` (a `fail_regex`, so the run fails fast).

> x86_64 boots only via OVMF/UEFI, which the StarryOS app-qemu path validates in
> CI; the local app-qemu path (`-kernel`, no PVH note) cannot boot it, so x86_64
> is validated through CI like the other prebuild apps. aarch64/riscv64/loongarch64
> boot locally.
