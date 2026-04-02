#!/usr/bin/env python3
"""Generate probe skeleton C files from docs/starryos-syscall-catalog.yaml.

Division of labor (see docs/starryos-syscall-testing-method.md):
- ``contract_write_zero`` / ``contract_read_zero``: small self-contained templates that
  compile and mirror the hand-written contracts for write/read.
- ``contract_execve_enoent`` / ``contract_wait4_echild``: same for the errno probes
  listed in catalog ``tests:``; hand-written sources in ``probes/contract/`` remain
  the oracle source of truth — regenerate to refresh ``generated/*.c`` after catalog edits.
- ``contract_errno`` / ``contract_stub`` / other unknown templates: ``emit_stub`` placeholders
  only; replace with hand-written ``contract/*.c`` before relying on oracle lines.
- **futex** / **ppoll**: keep ``contract_stub``; do not add fixed ``expected/*.line`` until
  dedicated SMP-safe scenarios exist.
"""

from __future__ import annotations

import argparse
from pathlib import Path

import yaml


def emit_write_zero(syscall: str, note: str) -> str:
    return f"""/* GENERATED — {syscall} — template contract_write_zero */
#include <stdio.h>
#include <unistd.h>

int main(void) {{
  ssize_t n = write(1, "", 0);
  dprintf(1, "CASE {syscall}.write_zero ret=%zd errno=0 note={note}\\n", n);
  return 0;
}}
"""


def emit_read_zero(syscall: str, note: str) -> str:
    return f"""/* GENERATED — {syscall} — template contract_read_zero */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {{
  errno = 0;
  ssize_t n = read(0, NULL, 0);
  dprintf(1, "CASE {syscall}.zero_count ret=%zd errno=%d note={note}\\n", n, errno);
  return 0;
}}
"""


def emit_execve_enoent(syscall: str, note: str) -> str:
    return f"""/* GENERATED — {syscall} — template contract_execve_enoent */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {{
  char *argv[] = {{ "/__starryos_probe__/execve_no_such", NULL }};
  char *envp[] = {{ NULL }};
  errno = 0;
  int r = execve("/__starryos_probe__/execve_no_such", argv, envp);
  int e = errno;
  dprintf(1, "CASE execve.enoent ret=%d errno=%d note={note}\\n", r, e);
  return 0;
}}
"""


def emit_wait4_echild(syscall: str, note: str) -> str:
    return f"""/* GENERATED — {syscall} — template contract_wait4_echild */
#include <errno.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {{
  errno = 0;
  pid_t r = wait4(999999, NULL, 0, NULL);
  int e = errno;
  dprintf(1, "CASE wait4.nochld ret=%d errno=%d note={note}\\n", (int)r, e);
  return 0;
}}
"""


def emit_stub(syscall: str, template: str) -> str:
    return f"""/* GENERATED — {syscall} — template {template} (stub) */
#include <stdio.h>
int main(void) {{
  puts("CASE {syscall}.stub ret=-1 errno=999 note=fill_me");
  return 1;
}}
"""


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--catalog", type=Path, default=Path("docs/starryos-syscall-catalog.yaml"))
    ap.add_argument(
        "--out-dir",
        type=Path,
        default=Path("test-suit/starryos/probes/generated"),
    )
    args = ap.parse_args()

    data = yaml.safe_load(args.catalog.read_text(encoding="utf-8"))
    entries = data.get("syscalls") or []
    args.out_dir.mkdir(parents=True, exist_ok=True)
    written = 0
    for e in entries:
        if not isinstance(e, dict):
            continue
        hints = e.get("generator_hints") or {}
        tpl = hints.get("template")
        if not tpl:
            continue
        name = str(e["syscall"])
        if tpl == "contract_write_zero":
            body = emit_write_zero(name, "generated-from-catalog")
        elif tpl == "contract_read_zero":
            body = emit_read_zero(name, "generated-from-catalog")
        elif tpl == "contract_execve_enoent":
            body = emit_execve_enoent(name, "generated-from-catalog")
        elif tpl == "contract_wait4_echild":
            body = emit_wait4_echild(name, "generated-from-catalog")
        else:
            body = emit_stub(name, tpl)
        out = args.out_dir / f"{name}_generated.c"
        out.write_text(body, encoding="utf-8")
        written += 1
    print(f"Wrote {written} skeleton(s) to {args.out_dir}")


if __name__ == "__main__":
    main()
