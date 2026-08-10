#!/usr/bin/env python3
"""Reject direct OS/runtime dependencies from the portable rsext4 core."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys


FORBIDDEN_DEPENDENCIES = {
    "ax-kspin",
    "ax-sync",
    "ax-task",
    "ax-errno",
    "rdif-block",
    "log",
}

FORBIDDEN_SOURCE_TOKENS = {
    "ax_kspin": "OS-specific lock primitive",
    "ax_sync": "OS-specific synchronization primitive",
    "ax_task": "OS task runtime",
    "ax_errno": "Linux/ArceOS errno boundary",
    "rdif_block": "project block runtime must stay in the adapter",
    "starry_": "Starry integration must stay outside the core",
}


def workspace_root() -> pathlib.Path:
    return pathlib.Path(__file__).resolve().parents[2]


def package_metadata(root: pathlib.Path) -> dict:
    output = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=root,
        text=True,
    )
    metadata = json.loads(output)
    return next(pkg for pkg in metadata["packages"] if pkg["name"] == "rsext4")


def main() -> int:
    root = workspace_root()
    package = package_metadata(root)
    failures: list[str] = []

    direct_dependencies = {dependency["name"] for dependency in package["dependencies"]}
    for dependency in sorted(direct_dependencies & FORBIDDEN_DEPENDENCIES):
        failures.append(f"forbidden direct dependency: {dependency}")

    source_root = root / "fs" / "rsext4" / "src"
    for path in sorted(source_root.rglob("*.rs")):
        if path.name == "axtest.rs":
            continue
        text = path.read_text(encoding="utf-8")
        for token, reason in FORBIDDEN_SOURCE_TOKENS.items():
            if token in text:
                relative = path.relative_to(root)
                failures.append(f"{relative}: contains {token!r}: {reason}")

    lib_source = (source_root / "lib.rs").read_text(encoding="utf-8")
    if "#![no_std]" not in lib_source:
        failures.append("fs/rsext4/src/lib.rs: missing #![no_std]")

    if failures:
        print("RSEXT4_BOUNDARY_FAILED")
        for failure in failures:
            print(f"- {failure}")
        return 1

    print("RSEXT4_BOUNDARY_PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
