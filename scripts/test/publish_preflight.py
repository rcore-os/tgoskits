#!/usr/bin/env python3
"""Dry-run publishing while resolving workspace crates from this checkout."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any


def load_workspace_metadata(cwd: Path) -> dict[str, Any]:
    command = ["cargo", "metadata", "--format-version", "1", "--no-deps"]
    completed = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)


def crates_io_publishable(package: dict[str, Any]) -> bool:
    registries = package.get("publish")
    return registries is None or "crates-io" in registries


def workspace_patch_paths(metadata: dict[str, Any]) -> dict[str, Path]:
    members = set(metadata["workspace_members"])
    patches: dict[str, Path] = {}
    for package in metadata["packages"]:
        if package["id"] not in members or not crates_io_publishable(package):
            continue
        name = package["name"]
        if name in patches:
            raise ValueError(f"duplicate publishable workspace package name: {name}")
        patches[name] = Path(package["manifest_path"]).resolve().parent
    return patches


def render_patch_config(patches: dict[str, Path]) -> str:
    lines = ["[patch.crates-io]"]
    for name, path in sorted(patches.items()):
        lines.append(
            f"{json.dumps(name)} = {{ path = {json.dumps(str(path))} }}"
        )
    lines.append("")
    return "\n".join(lines)


def publish_dependency_graph(
    metadata: dict[str, Any], patches: dict[str, Path]
) -> dict[str, set[str]]:
    graph = {name: set() for name in patches}
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        name = package["name"]
        if package["id"] not in members or name not in graph:
            continue
        for dependency in package["dependencies"]:
            dependency_name = dependency["name"]
            if dependency_name not in graph:
                continue
            if dependency["kind"] == "dev" and dependency["req"] == "*":
                continue
            graph[name].add(dependency_name)
    return graph


def cyclic_packages(graph: dict[str, set[str]]) -> set[str]:
    next_index = 0
    indexes: dict[str, int] = {}
    low_links: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    cyclic: set[str] = set()

    def visit(package: str) -> None:
        nonlocal next_index
        indexes[package] = next_index
        low_links[package] = next_index
        next_index += 1
        stack.append(package)
        on_stack.add(package)

        for dependency in graph[package]:
            if dependency not in indexes:
                visit(dependency)
                low_links[package] = min(low_links[package], low_links[dependency])
            elif dependency in on_stack:
                low_links[package] = min(low_links[package], indexes[dependency])

        if low_links[package] != indexes[package]:
            return
        component: list[str] = []
        while True:
            member = stack.pop()
            on_stack.remove(member)
            component.append(member)
            if member == package:
                break
        if len(component) > 1 or package in graph[package]:
            cyclic.update(component)

    for package in graph:
        if package not in indexes:
            visit(package)
    return cyclic


def publish_command(
    config_path: Path,
    allow_dirty: bool,
    *,
    package: str | None = None,
    exclude: set[str] | None = None,
) -> list[str]:
    command = [
        "cargo",
        "publish",
        "--dry-run",
        "--no-verify",
        "--quiet",
        "--config",
        str(config_path),
    ]
    if package is None:
        command.append("--workspace")
        for excluded_package in sorted(exclude or set()):
            command.extend(["--exclude", excluded_package])
    else:
        command.extend(["--package", package])
    if allow_dirty:
        command.append("--allow-dirty")
    return command


def run_command(command: list[str], workspace_root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=workspace_root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def report_failure(label: str, completed: subprocess.CompletedProcess[str]) -> None:
    print(f"publish preflight failed for {label}")
    if completed.stdout:
        print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, end="")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "validate every publishable workspace package while treating the current "
            "workspace as the source of not-yet-published dependency versions"
        )
    )
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="forward --allow-dirty to cargo publish for local validation",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    metadata = load_workspace_metadata(Path.cwd())
    workspace_root = Path(metadata["workspace_root"])
    patches = workspace_patch_paths(metadata)
    if not patches:
        raise RuntimeError("workspace contains no crates.io-publishable packages")

    with tempfile.TemporaryDirectory(prefix="tgoskits-publish-preflight-") as temp_dir:
        config_path = Path(temp_dir) / "workspace-patches.toml"
        config_path.write_text(render_patch_config(patches), encoding="utf-8")
        print(
            f"validating workspace publish transaction with {len(patches)} local crate patches"
        )
        graph = publish_dependency_graph(metadata, patches)
        cyclic = cyclic_packages(graph)
        batch = run_command(
            publish_command(config_path, args.allow_dirty, exclude=cyclic),
            workspace_root,
        )
        if batch.returncode != 0:
            report_failure("acyclic workspace batch", batch)
            return batch.returncode

        print(
            f"validating {len(cyclic)} dev-dependency cycle members as individual packages"
        )
        failures: list[tuple[str, subprocess.CompletedProcess[str]]] = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            pending = {
                executor.submit(
                    run_command,
                    publish_command(config_path, args.allow_dirty, package=package),
                    workspace_root,
                ): package
                for package in cyclic
            }
            for future in concurrent.futures.as_completed(pending):
                package = pending[future]
                completed = future.result()
                if completed.returncode != 0:
                    failures.append((package, completed))

        for package, completed in sorted(failures):
            report_failure(package, completed)
        return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
