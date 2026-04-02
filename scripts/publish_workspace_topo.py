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
import datetime as dt
import json
import os
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from collections import defaultdict, deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any


USER_AGENT = "tgoskits-publish-workspace-topo/1.0"
DEFAULT_PUBLISH_DELAY_SECONDS = 3.0


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    manifest_path: Path
    workspace_root: Path
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


def find_nearest_workspace_root(manifest_path: Path, fallback_root: Path) -> Path:
    current = manifest_path.parent
    while True:
        workspace_manifest = current / "Cargo.toml"
        if workspace_manifest.exists():
            try:
                data = tomllib.loads(workspace_manifest.read_text(encoding="utf-8"))
            except tomllib.TOMLDecodeError:
                data = {}
            if isinstance(data.get("workspace"), dict):
                return current.resolve()
        if current == fallback_root or current.parent == current:
            return fallback_root
        current = current.parent


def select_packages(metadata: dict[str, Any], root: Path) -> dict[str, Package]:
    root = root.resolve()
    workspace_members = set(metadata["workspace_members"])
    metadata_workspace_root = normalize_path(metadata["workspace_root"])
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
        if isinstance(publish, list) and not publish:
            continue

        selected[package_id] = Package(
            name=pkg["name"],
            version=pkg["version"],
            manifest_path=manifest_path,
            workspace_root=find_nearest_workspace_root(manifest_path, metadata_workspace_root),
            package_id=package_id,
            publish=publish,
            dependencies=pkg.get("dependencies", []),
        )

    return selected


def merge_packages(
    selected: dict[str, Package],
    additions: dict[str, Package],
) -> dict[str, Package]:
    merged = dict(selected)
    seen_manifests = {pkg.manifest_path for pkg in merged.values()}
    seen_names = {pkg.name for pkg in merged.values()}
    for package_id, pkg in additions.items():
        if pkg.manifest_path in seen_manifests:
            continue
        if pkg.name in seen_names:
            continue
        merged[package_id] = pkg
        seen_manifests.add(pkg.manifest_path)
        seen_names.add(pkg.name)
    return merged


def iter_patch_manifest_paths(workspace_manifest: Path) -> list[Path]:
    data = tomllib.loads(workspace_manifest.read_text(encoding="utf-8"))
    patch_crates_io = data.get("patch", {}).get("crates-io", {})
    if not isinstance(patch_crates_io, dict):
        return []

    manifests: list[Path] = []
    for value in patch_crates_io.values():
        if not isinstance(value, dict):
            continue
        rel_path = value.get("path")
        if not isinstance(rel_path, str):
            continue
        manifest_path = (workspace_manifest.parent / rel_path / "Cargo.toml").resolve()
        if manifest_path.exists():
            manifests.append(manifest_path)
    return manifests


def load_extra_patch_packages(
    workspace_manifest: Path,
    root: Path,
    selected: dict[str, Package],
) -> dict[str, Package]:
    extras: dict[str, Package] = {}
    selected_manifests = {pkg.manifest_path for pkg in selected.values()}
    selected_names = {pkg.name for pkg in selected.values()}

    for manifest_path in iter_patch_manifest_paths(workspace_manifest):
        if manifest_path in selected_manifests:
            continue
        try:
            metadata = load_metadata(manifest_path)
        except subprocess.CalledProcessError:
            continue
        filtered = {
            package_id: pkg
            for package_id, pkg in select_packages(metadata, root).items()
            if pkg.name not in selected_names
        }
        extras = merge_packages(extras, filtered)

    return extras


def expand_internal_dependency_closure(
    selected: dict[str, Package],
    candidates: dict[str, Package],
) -> dict[str, Package]:
    remaining_by_name: dict[str, tuple[str, Package]] = {}
    for package_id, pkg in candidates.items():
        remaining_by_name[pkg.name] = (package_id, pkg)

    expanded = dict(selected)
    changed = True
    while changed:
        changed = False
        needed_names = {dep["name"] for pkg in expanded.values() for dep in pkg.dependencies}
        for name in sorted(needed_names):
            candidate = remaining_by_name.pop(name, None)
            if candidate is None:
                continue
            package_id, pkg = candidate
            expanded[package_id] = pkg
            changed = True

    return expanded


def package_name_index(packages: dict[str, Package]) -> dict[str, str]:
    index: dict[str, str] = {}
    for package_id, pkg in packages.items():
        if pkg.name in index:
            raise SystemExit(f"duplicate package name in selection: {pkg.name}")
        index[pkg.name] = package_id
    return index


def build_dependency_graph(
    packages: dict[str, Package],
    primary_workspace_root: Path,
) -> dict[str, set[str]]:
    name_to_id = package_name_index(packages)
    graph: dict[str, set[str]] = {package_id: set() for package_id in packages}
    workspace_members: dict[Path, list[str]] = defaultdict(list)

    for package_id, pkg in packages.items():
        workspace_members[pkg.workspace_root].append(package_id)

    for package_id, pkg in packages.items():
        for dep in pkg.dependencies:
            dep_name = dep["name"]
            if dep_name not in name_to_id:
                continue
            dep_id = name_to_id[dep_name]
            if dep_id == package_id:
                continue
            graph[package_id].add(dep_id)

    for workspace_root, member_ids in workspace_members.items():
        if workspace_root == primary_workspace_root:
            continue
        member_id_set = set(member_ids)
        workspace_dep_ids: set[str] = set()
        for package_id in member_ids:
            for dep in packages[package_id].dependencies:
                dep_name = dep["name"]
                dep_id = name_to_id.get(dep_name)
                if dep_id is not None and dep_id not in member_id_set:
                    if graph[dep_id] & member_id_set:
                        continue
                    workspace_dep_ids.add(dep_id)

        for package_id in member_ids:
            graph[package_id].update(dep_id for dep_id in workspace_dep_ids if dep_id != package_id)

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


def is_not_owner_failure(detail: str) -> bool:
    return (
        "403 Forbidden" in detail
        and "don't seem to be an owner" in detail
    )


def parse_retry_after_timestamp(detail: str) -> dt.datetime | None:
    marker = "Please try again after "
    start = detail.find(marker)
    if start == -1:
        return None
    start += len(marker)
    end = detail.find(" and see ", start)
    if end == -1:
        return None

    timestamp_text = detail[start:end].strip()
    try:
        retry_at = dt.datetime.strptime(timestamp_text, "%a, %d %b %Y %H:%M:%S %Z")
    except ValueError:
        return None
    return retry_at.replace(tzinfo=dt.timezone.utc)


def is_rate_limit_failure(detail: str) -> bool:
    return "429 Too Many Requests" in detail


def run_publish_command(pkg: Package, *, locked: bool) -> subprocess.CompletedProcess[str]:
    cmd = [
        "cargo",
        "publish",
        "--manifest-path",
        str(pkg.manifest_path),
        "--allow-dirty",
    ]
    if locked:
        cmd.append("--locked")
    return run(cmd, check=False)


def publish_package(pkg: Package, dry_run: bool) -> tuple[str, str]:
    if dry_run:
        return "DRY-RUN", "not published"

    locked = True
    rate_limit_retried = False
    while True:
        proc = run_publish_command(pkg, locked=locked)
        stderr = proc.stderr.strip()
        if proc.returncode != 0 and "cannot update the lock file" in stderr and "--locked was passed" in stderr:
            locked = False
            continue
        if proc.returncode == 0:
            return "PUBLISHED", "ok"

        stdout = proc.stdout.strip()
        detail = stderr or stdout or f"exit code {proc.returncode}"
        if is_not_owner_failure(detail):
            return "SKIP", "not an owner on crates.io"
        if is_rate_limit_failure(detail) and not rate_limit_retried:
            retry_at = parse_retry_after_timestamp(detail)
            if retry_at is not None:
                sleep_seconds = max(
                    0.0,
                    (retry_at - dt.datetime.now(dt.timezone.utc)).total_seconds() + 1.0,
                )
                if sleep_seconds > 0:
                    print(
                        f"    rate limited by crates.io, sleeping {sleep_seconds:.1f}s before retrying {pkg.name}",
                        flush=True,
                    )
                    time.sleep(sleep_seconds)
                rate_limit_retried = True
                continue
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
    parser.add_argument(
        "--publish-delay",
        type=float,
        default=DEFAULT_PUBLISH_DELAY_SECONDS,
        help=(
            "Seconds to sleep between publish attempts to reduce crates.io rate-limit pressure. "
            f"Default: {DEFAULT_PUBLISH_DELAY_SECONDS}."
        ),
    )
    args = parser.parse_args()

    root = normalize_path(args.root)
    manifest_path = normalize_path(args.manifest_path) if args.manifest_path else None

    metadata = load_metadata(manifest_path)
    packages = select_packages(metadata, root)

    workspace_manifest = manifest_path or (Path.cwd() / "Cargo.toml").resolve()
    extra_packages = load_extra_patch_packages(workspace_manifest, root, packages)
    packages = expand_internal_dependency_closure(packages, extra_packages)

    if not packages:
        print(f"no publishable workspace packages found under {root}")
        return 0

    primary_workspace_root = normalize_path(metadata["workspace_root"])
    graph = build_dependency_graph(packages, primary_workspace_root)
    order = topo_sort(graph)

    print(f"selected {len(order)} package(s) under {root}")
    print("publish order:")
    for index, package_id in enumerate(order, start=1):
        pkg = packages[package_id]
        print(f"  {index}. {pkg.name} {pkg.version} ({pkg.rel_dir})")
    print()

    any_failed = False
    blocked: dict[str, str] = {}
    crates_io_available: dict[str, bool] = {}
    last_publish_attempt_at: float | None = None
    for index, package_id in enumerate(order, start=1):
        pkg = packages[package_id]
        prefix = f"[{index}/{len(order)}] {pkg.name} {pkg.version} ({pkg.rel_dir})"
        if args.dry_run:
            status, detail = publish_package(pkg, dry_run=True)
            print(f"{prefix} -> {status} {detail}")
            continue

        missing_blockers = [
            packages[dep_id].name
            for dep_id in sorted(graph[package_id])
            if dep_id in blocked and not crates_io_available.get(dep_id, False)
        ]
        if missing_blockers:
            blocked[package_id] = f"blocked by unavailable dependency: {', '.join(missing_blockers)}"
            print(f"{prefix} -> SKIP {blocked[package_id]}")
            continue

        try:
            crates_io_available[package_id] = crates_io_has_version(pkg.name, pkg.version)
            if crates_io_available[package_id]:
                print(f"{prefix} -> SKIP already exists on crates.io")
                continue
        except Exception as exc:  # noqa: BLE001
            any_failed = True
            print(f"{prefix} -> FAILED crates.io check: {exc}")
            continue

        if last_publish_attempt_at is not None and args.publish_delay > 0:
            elapsed = time.monotonic() - last_publish_attempt_at
            remaining = args.publish_delay - elapsed
            if remaining > 0:
                print(f"    sleeping {remaining:.1f}s before publishing {pkg.name}", flush=True)
                time.sleep(remaining)

        last_publish_attempt_at = time.monotonic()
        status, detail = publish_package(pkg, args.dry_run)
        if status == "FAILED":
            any_failed = True
            blocked[package_id] = detail
            print(f"{prefix} -> {status} {detail}")
        else:
            if status == "SKIP" and detail == "not an owner on crates.io":
                blocked[package_id] = detail
            print(f"{prefix} -> {status} {detail}")

    return 1 if any_failed else 0


if __name__ == "__main__":
    sys.exit(main())
