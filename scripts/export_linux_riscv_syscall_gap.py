#!/usr/bin/env python3
"""Compare Linux riscv64 `unistd_64.h` __NR_* names with StarryOS dispatch.json; write YAML + CSV.

Example:
  python3 scripts/export_linux_riscv_syscall_gap.py \\
    --linux-header /path/to/linux-6.18.20/arch/riscv/include/generated/uapi/asm/unistd_64.h \\
    --kernel-version v6.18.20
"""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path


def parse_linux_names(path: Path) -> tuple[dict[str, int], int | None]:
    text = path.read_text(encoding="utf-8", errors="replace")
    linux: dict[str, int] = {}
    nr_upper: int | None = None
    for m in re.finditer(r"#define (__NR_\w+)\s+(\d+)", text):
        name, num_s = m.group(1), m.group(2)
        num = int(num_s)
        if name == "__NR_syscalls":
            nr_upper = num
            continue
        sym = name.removeprefix("__NR_")
        linux[sym] = num
    return linux, nr_upper


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--linux-header",
        type=Path,
        required=True,
        help="generated uapi asm unistd_64.h (riscv64)",
    )
    ap.add_argument("--kernel-version", default="v6.18.20", help="label for metadata")
    ap.add_argument(
        "--dispatch",
        type=Path,
        default=Path("docs/starryos-syscall-dispatch.json"),
    )
    ap.add_argument(
        "--out-yaml",
        type=Path,
        default=Path("docs/starryos-linux-riscv64-syscall-gap-v6.18.20.yaml"),
    )
    ap.add_argument(
        "--out-csv",
        type=Path,
        default=Path("docs/starryos-linux-riscv64-syscall-gap-v6.18.20.csv"),
    )
    args = ap.parse_args()
    root = Path(__file__).resolve().parent.parent

    linux, nr_upper = parse_linux_names(args.linux_header.resolve())
    dispatch_path = args.dispatch
    if not dispatch_path.is_absolute():
        dispatch_path = root / dispatch_path
    data = json.loads(dispatch_path.read_text(encoding="utf-8"))
    starry = [e["syscall"] for e in data["syscalls"]]
    starry_set = set(starry)

    only_linux = sorted(set(linux.keys()) - starry_set)
    only_starry = sorted(starry_set - set(linux.keys()))

    out_yaml = args.out_yaml
    out_csv = args.out_csv
    if not out_yaml.is_absolute():
        out_yaml = root / out_yaml
    if not out_csv.is_absolute():
        out_csv = root / out_csv

    ver = args.kernel_version.lstrip("v") if args.kernel_version.startswith("v") else args.kernel_version
    lines = [
        "# StarryOS dispatch vs Linux riscv64 UAPI syscall names "
        f"({args.kernel_version}).",
        "# Regenerate: python3 scripts/export_linux_riscv_syscall_gap.py --linux-header <unistd_64.h>",
        "schema_version: 1",
        f"linux_kernel: {args.kernel_version}",
        "target_arch: riscv64",
        "source_header: arch/riscv/include/generated/uapi/asm/unistd_64.h",
        f"source_path: {args.linux_header.resolve().as_posix()}",
        "counts:",
        f"  linux_uapi_named_syscalls: {len(linux)}",
        f"  linux_nr_syscalls_upper_bound: {nr_upper}",
        f"  starry_dispatch_entries: {len(starry)}",
        f"  only_in_linux_uapi: {len(only_linux)}",
        f"  only_in_starry_dispatch: {len(only_starry)}",
        "",
        "only_in_linux_uapi:",
    ]
    for name in only_linux:
        lines.append(f"  - syscall: {name}")
        lines.append(f"    __NR: {linux[name]}")
    lines.append("")
    lines.append("only_in_starry_dispatch:")
    for name in only_starry:
        lines.append(f"  - syscall: {name}")

    out_yaml.parent.mkdir(parents=True, exist_ok=True)
    out_yaml.write_text("\n".join(lines) + "\n", encoding="utf-8")

    with out_csv.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["category", "syscall", "__NR_linux_riscv64"])
        for name in only_linux:
            w.writerow(["only_in_linux_uapi", name, linux[name]])
        for name in only_starry:
            w.writerow(["only_in_starry_dispatch", name, ""])

    print(f"Wrote {out_yaml.relative_to(root)}")
    print(f"Wrote {out_csv.relative_to(root)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
