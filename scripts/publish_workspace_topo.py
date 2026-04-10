#!/usr/bin/env python3
"""
Publish workspace crates in topological order from lower-level dependencies upward.

Behavior:
- discovers workspace members via `cargo metadata`
- limits packages to those whose manifest paths are under `--root` (default: cwd)
- builds an internal dependency DAG among the selected packages
- checks crates.io for an existing identical version before publishing
- skips already-published versions and continues
- prints a result line for every package

Typical usage:
    python3 scripts/publish_workspace_topo.py
    python3 scripts/publish_workspace_topo.py --root components
    python3 scripts/publish_workspace_topo.py --dry-run
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from collections import defaultdict, deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any


USER_AGENT = "tgoskits-publish-workspace-topo/1.0"


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    manifest_path: Path
    package_id: str
    publish: Any
    dependencies: list[dict[str, Any]]

    @property
    def crate_dir(self) -> Path:
        return self.manifest_path.parent

    @property
    def rel_dir(self) -> str:
        return os.path.relpath(self.crate_dir, Path.cwd())


def run(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        check=check,
        text=True,
        capture_output=capture,
    )


def load_metadata(manifest_path: Path | None) -> dict[str, Any]:
    cmd = ["cargo", "metadata", "--format-version", "1", "--no-deps"]
    if manifest_path is not None:
        cmd.extend(["--manifest-path", str(manifest_path)])
    proc = run(cmd)
    return json.loads(proc.stdout)


def normalize_path(path: str | Path) -> Path:
    return Path(path).resolve()


def select_packages(metadata: dict[str, Any], root: Path) -> dict[str, Package]:
    root = root.resolve()
    workspace_members = set(metadata["workspace_members"])
    selected: dict[str, Package] = {}

    for pkg in metadata["packages"]:
        package_id = pkg["id"]
        if package_id not in workspace_members:
            continue

        manifest_path = normalize_path(pkg["manifest_path"])
        crate_dir = manifest_path.parent
        try:
            crate_dir.relative_to(root)
        except ValueError:
            continue

        publish = pkg.get("publish")
        if publish is False:
            continue

        selected[package_id] = Package(
            name=pkg["name"],
            version=pkg["version"],
            manifest_path=manifest_path,
            package_id=package_id,
            publish=publish,
            dependencies=pkg.get("dependencies", []),
        )

    return selected


def package_name_index(packages: dict[str, Package]) -> dict[str, str]:
    index: dict[str, str] = {}
    for package_id, pkg in packages.items():
        if pkg.name in index:
            raise SystemExit(f"duplicate package name in selection: {pkg.name}")
        index[pkg.name] = package_id
    return index


def build_dependency_graph(packages: dict[str, Package]) -> dict[str, set[str]]:
    name_to_id = package_name_index(packages)
    graph: dict[str, set[str]] = {package_id: set() for package_id in packages}

    for package_id, pkg in packages.items():
        for dep in pkg.dependencies:
            dep_name = dep["name"]
            if dep_name not in name_to_id:
                continue
            dep_id = name_to_id[dep_name]
            if dep_id == package_id:
                continue
            graph[package_id].add(dep_id)

    return graph


def topo_sort(graph: dict[str, set[str]]) -> list[str]:
    reverse: dict[str, set[str]] = defaultdict(set)
    indegree = {node: len(deps) for node, deps in graph.items()}

    for node, deps in graph.items():
        for dep in deps:
            reverse[dep].add(node)

    queue = deque(sorted(node for node, degree in indegree.items() if degree == 0))
    order: list[str] = []

    while queue:
        node = queue.popleft()
        order.append(node)
        for parent in sorted(reverse[node]):
            indegree[parent] -= 1
            if indegree[parent] == 0:
                queue.append(parent)

    if len(order) != len(graph):
        unresolved = sorted(node for node, degree in indegree.items() if degree > 0)
        raise SystemExit(f"cyclic internal dependency graph detected: {unresolved}")

    return order


def crates_io_has_version(crate: str, version: str, timeout: float = 15.0) -> bool:
    crate_q = urllib.parse.quote(crate, safe="")
    version_q = urllib.parse.quote(version, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}/{version_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return 200 <= resp.status < 300
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return False
        raise


def publish_package(pkg: Package, dry_run: bool) -> tuple[str, str]:
    if dry_run:
        return "DRY-RUN", "not published"

    cmd = [
        "cargo",
        "publish",
        "--manifest-path",
        str(pkg.manifest_path),
        "--locked",
    ]
    proc = run(cmd, check=False)
    if proc.returncode == 0:
        return "PUBLISHED", "ok"

    stderr = proc.stderr.strip()
    stdout = proc.stdout.strip()
    detail = stderr or stdout or f"exit code {proc.returncode}"
    return "FAILED", detail


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Publish workspace crates from lower-level dependencies upward."
    )
    parser.add_argument(
        "--root",
        default=".",
        help="Only include workspace crates under this directory. Default: current directory.",
    )
    parser.add_argument(
        "--manifest-path",
        default=None,
        help="Optional workspace manifest path to pass to cargo metadata.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Only print the publish plan and skip cargo publish.",
    )
    args = parser.parse_args()

    root = normalize_path(args.root)
    manifest_path = normalize_path(args.manifest_path) if args.manifest_path else None

    metadata = load_metadata(manifest_path)
    packages = select_packages(metadata, root)
    if not packages:
        print(f"no publishable workspace packages found under {root}")
        return 0

    graph = build_dependency_graph(packages)
    order = topo_sort(graph)

    print(f"selected {len(order)} package(s) under {root}")
    print("publish order:")
    for index, package_id in enumerate(order, start=1):
        pkg = packages[package_id]
        print(f"  {index}. {pkg.name} {pkg.version} ({pkg.rel_dir})")
    print()

    any_failed = False
    for index, package_id in enumerate(order, start=1):
        pkg = packages[package_id]
        prefix = f"[{index}/{len(order)}] {pkg.name} {pkg.version} ({pkg.rel_dir})"
        try:
            if crates_io_has_version(pkg.name, pkg.version):
                print(f"{prefix} -> SKIP already exists on crates.io")
                continue
        except Exception as exc:  # noqa: BLE001
            any_failed = True
            print(f"{prefix} -> FAILED crates.io check: {exc}")
            continue

        status, detail = publish_package(pkg, args.dry_run)
        if status == "FAILED":
            any_failed = True
            print(f"{prefix} -> {status} {detail}")
        else:
            print(f"{prefix} -> {status} {detail}")

    return 1 if any_failed else 0


if __name__ == "__main__":
    sys.exit(main())
