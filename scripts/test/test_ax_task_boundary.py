#!/usr/bin/env python3
"""Repository-boundary checks for the OS-independent ax-task component."""

from __future__ import annotations

import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
NEW_CRATE = ROOT / "components" / "ax-task"
OLD_CRATE = ROOT / "os" / "arceos" / "modules" / "axtask"
RUNTIME_FACADE_CONSUMERS = (
    ROOT / "os" / "arceos" / "api" / "arceos_api" / "Cargo.toml",
    ROOT / "os" / "arceos" / "api" / "arceos_posix_api" / "Cargo.toml",
)
FORBIDDEN_DEPENDENCIES = {
    "ax-hal",
    "ax-ipi",
    "ax-kspin",
    "ax-mm",
    "ax-percpu",
    "ax-runtime",
    "ax-sched",
    "ax-sync",
    "lock_api",
    "spin",
}


def fail(message: str) -> None:
    print(f"ax-task boundary violation: {message}", file=sys.stderr)
    raise SystemExit(1)


def workspace_manifest() -> str:
    return (ROOT / "Cargo.toml").read_text(encoding="utf-8")


def crate_manifest() -> str:
    manifest = NEW_CRATE / "Cargo.toml"
    if not manifest.is_file():
        fail("components/ax-task/Cargo.toml does not exist")
    return manifest.read_text(encoding="utf-8")


def check_workspace_boundary() -> None:
    manifest = workspace_manifest()
    if '"components/ax-task"' not in manifest:
        fail("components/ax-task is not a workspace member")
    if 'path = "components/ax-task"' not in manifest:
        fail("the ax-task workspace dependency does not use components/ax-task")
    if "os/arceos/modules/axtask" in manifest:
        fail("the workspace still references the removed axtask directory")
    if OLD_CRATE.exists():
        fail("os/arceos/modules/axtask still exists")


def dependency_names(manifest: str) -> set[str]:
    names: set[str] = set()
    in_dependency_table = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_dependency_table = "dependencies" in stripped
            continue
        if not in_dependency_table or not stripped or stripped.startswith("#"):
            continue
        match = re.match(r"([A-Za-z0-9_-]+)\s*=", stripped)
        if match:
            names.add(match.group(1))
    return names


def check_crate_dependencies() -> None:
    dependencies = dependency_names(crate_manifest())
    forbidden = sorted(dependencies & FORBIDDEN_DEPENDENCIES)
    if forbidden:
        fail(f"forbidden dependencies found: {', '.join(forbidden)}")


def check_runtime_facade_consumers() -> None:
    for manifest_path in RUNTIME_FACADE_CONSUMERS:
        manifest = manifest_path.read_text(encoding="utf-8")
        if "ax-task" in dependency_names(manifest):
            fail(
                f"{manifest_path.relative_to(ROOT)} must use the ax-runtime task facade"
            )


def check_no_global_scheduler_state() -> None:
    source_root = NEW_CRATE / "src"
    if not source_root.is_dir():
        fail("components/ax-task/src does not exist")
    forbidden_patterns = {
        "per-CPU declaration": re.compile(r"#\s*\[\s*def_percpu"),
        "mutable static": re.compile(r"(?<!')\bstatic\s+mut\b"),
        "global scheduler object": re.compile(
            r"\bstatic\s+(?:TASK_SYSTEM|CPU_LOCAL|RUN_QUEUE|SCHEDULER)\b"
        ),
    }
    for source in source_root.rglob("*.rs"):
        text = source.read_text(encoding="utf-8")
        for description, pattern in forbidden_patterns.items():
            if pattern.search(text):
                fail(f"{description} found in {source.relative_to(ROOT)}")


def main() -> None:
    check_workspace_boundary()
    check_crate_dependencies()
    check_runtime_facade_consumers()
    check_no_global_scheduler_state()
    print("ax-task repository boundary: ok")


if __name__ == "__main__":
    main()
