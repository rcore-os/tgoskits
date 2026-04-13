#!/usr/bin/env python3
"""
Publish workspace crates in topological order from lower-level dependencies upward.

Behavior:
- discovers publishable packages from all valid Cargo workspaces in the repository
- limits packages to those whose manifest paths are under `--root` (default: cwd)
- builds an internal dependency DAG among the selected packages
- checks crates.io for an existing identical version before publishing
- skips already-published versions and continues
- prints a result line for every package

Typical usage:
    python3 scripts/publish.py
    python3 scripts/publish.py --root components
    python3 scripts/publish.py --dry-run
    python3 scripts/publish.py --sync-owner github:rcore-os:crates-io
    python3 scripts/publish.py --sync-owner https://github.com/orgs/rcore-os/teams/crates-io
    python3 scripts/publish.py --check
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tomllib
from urllib.parse import urlparse
import urllib.error
import urllib.parse
import urllib.request
from collections import defaultdict, deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any


USER_AGENT = "tgoskits-publish-workspace-topo/1.0"
DEFAULT_CHECK_OWNERS = [
    "github:arceos-org:core",
    "equation314",
]


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


def discover_workspace_manifests(search_root: Path) -> list[Path]:
    manifests: list[Path] = []
    for manifest in sorted(search_root.rglob("Cargo.toml")):
        if "target" in manifest.parts:
            continue
        with manifest.open("rb") as fh:
            data = tomllib.load(fh)
        if "workspace" in data:
            manifests.append(manifest.resolve())
    return manifests


def normalize_path(path: str | Path) -> Path:
    return Path(path).resolve()


def collect_packages(metadata_sets: list[dict[str, Any]], root: Path) -> dict[str, Package]:
    root = root.resolve()
    selected: dict[str, Package] = {}

    for metadata in metadata_sets:
        workspace_members = set(metadata["workspace_members"])
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
            # `cargo metadata` reports `publish = false` as an empty list.
            if publish is False or publish == []:
                continue

            package_key = str(manifest_path)
            selected.setdefault(
                package_key,
                Package(
                    name=pkg["name"],
                    version=pkg["version"],
                    manifest_path=manifest_path,
                    package_id=package_key,
                    publish=publish,
                    dependencies=pkg.get("dependencies", []),
                ),
            )

    return selected


def package_name_index(packages: dict[str, Package]) -> dict[str, list[str]]:
    index: dict[str, list[str]] = defaultdict(list)
    for package_id, pkg in packages.items():
        index[pkg.name].append(package_id)
    return index


def build_dependency_graph(packages: dict[str, Package]) -> dict[str, set[str]]:
    name_to_ids = package_name_index(packages)
    path_to_id = {
        str(pkg.manifest_path.parent.resolve()): package_id
        for package_id, pkg in packages.items()
    }
    graph: dict[str, set[str]] = {package_id: set() for package_id in packages}

    for package_id, pkg in packages.items():
        for dep in pkg.dependencies:
            dep_id = None
            dep_path = dep.get("path")
            if dep_path:
                dep_id = path_to_id.get(str(normalize_path(dep_path)))

            if dep_id is None:
                dep_name = dep["name"]
                matching_ids = name_to_ids.get(dep_name, [])
                if not matching_ids:
                    continue
                if len(matching_ids) > 1:
                    raise SystemExit(
                        f"ambiguous internal dependency {dep_name!r} for package {pkg.name}; "
                        "multiple selected crates share this name and the dependency has no path"
                    )
                dep_id = matching_ids[0]

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


def crates_io_has_crate(crate: str, timeout: float = 15.0) -> bool:
    crate_q = urllib.parse.quote(crate, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return 200 <= resp.status < 300
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return False
        raise


def normalize_owner(owner: str) -> str:
    owner = owner.strip()
    if owner.startswith("github:"):
        return owner

    parsed = urlparse(owner)
    if parsed.scheme in {"http", "https"} and parsed.netloc == "crates.io":
        parts = [part for part in parsed.path.split("/") if part]
        if len(parts) == 2 and parts[0] == "teams":
            return parts[1]
        if len(parts) == 2 and parts[0] == "users":
            return parts[1]
        raise SystemExit(
            "unsupported crates.io owner URL. Expected "
            "https://crates.io/teams/<team> or https://crates.io/users/<user>"
        )

    if parsed.scheme in {"http", "https"} and parsed.netloc == "github.com":
        parts = [part for part in parsed.path.split("/") if part]
        if len(parts) == 4 and parts[0] == "orgs" and parts[2] == "teams":
            return f"github:{parts[1]}:{parts[3]}"
        raise SystemExit(
            "unsupported GitHub owner URL. Expected "
            "https://github.com/orgs/<org>/teams/<team>"
        )

    return owner


def list_owners(crate: str) -> set[str]:
    proc = run(["cargo", "owner", "--list", crate], check=False)
    if proc.returncode != 0:
        stderr = proc.stderr.strip()
        stdout = proc.stdout.strip()
        detail = stderr or stdout or f"exit code {proc.returncode}"
        raise RuntimeError(detail)

    owners: set[str] = set()
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        owners.add(line.split()[0])
    return owners


def sync_owners(crate: str, owners: list[str], dry_run: bool) -> tuple[str, str]:
    if not owners:
        return "SKIP", "no owners requested"

    try:
        existing = list_owners(crate)
    except Exception as exc:  # noqa: BLE001
        return "FAILED", f"owner list failed: {exc}"

    missing = [owner for owner in owners if owner not in existing]
    if not missing:
        return "SKIP", "owners already present"

    if dry_run:
        return "DRY-RUN", f"would add owners: {', '.join(missing)}"

    added: list[str] = []
    for owner in missing:
        proc = run(["cargo", "owner", "--add", owner, crate], check=False)
        if proc.returncode != 0:
            stderr = proc.stderr.strip()
            stdout = proc.stdout.strip()
            detail = stderr or stdout or f"exit code {proc.returncode}"
            return "FAILED", f"owner add failed for {owner}: {detail}"
        added.append(owner)

    return "SYNCED", f"added owners: {', '.join(added)}"


def check_owners(crate: str, expected_owners: list[str]) -> tuple[str, str]:
    try:
        existing = list_owners(crate)
    except Exception as exc:  # noqa: BLE001
        return "FAILED", f"owner list failed: {exc}"

    present = [owner for owner in expected_owners if owner in existing]
    if present:
        return "OK", f"matched owners: {', '.join(present)}"

    return "MISSING", f"expected one of: {', '.join(expected_owners)}"


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
    parser.add_argument(
        "--sync-owner",
        action="append",
        default=[],
        metavar="OWNER",
        help=(
            "Sync an owner for every selected crate after publish/skip. "
            "Accepts cargo owner syntax such as github:org:team or a GitHub team URL."
        ),
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help=(
            "Print only crates that already exist on crates.io and have at least one "
            "expected owner. Defaults to github:arceos-org:core or equation314."
        ),
    )
    args = parser.parse_args()

    root = normalize_path(args.root)
    manifest_path = normalize_path(args.manifest_path) if args.manifest_path else None
    owners = [normalize_owner(owner) for owner in args.sync_owner]
    check_owners_list = [normalize_owner(owner) for owner in DEFAULT_CHECK_OWNERS]

    if manifest_path is not None:
        metadata_sets = [load_metadata(manifest_path)]
        skipped_workspaces: list[Path] = []
    else:
        metadata_sets = []
        skipped_workspaces = []
        for workspace_manifest in discover_workspace_manifests(Path.cwd()):
            try:
                metadata_sets.append(load_metadata(workspace_manifest))
            except subprocess.CalledProcessError:
                skipped_workspaces.append(workspace_manifest)

    packages = collect_packages(metadata_sets, root)
    if not packages:
        print(f"no publishable workspace packages found under {root}")
        return 0

    if skipped_workspaces:
        print(
            "warning: skipped invalid workspaces: "
            + ", ".join(str(path.relative_to(Path.cwd())) for path in skipped_workspaces),
            file=sys.stderr,
        )

    graph = build_dependency_graph(packages)
    order = topo_sort(graph)

    if args.check:
        any_failed = False
        any_matched = False
        for package_id in order:
            pkg = packages[package_id]
            try:
                if not crates_io_has_crate(pkg.name):
                    continue
                owner_status, owner_detail = check_owners(pkg.name, check_owners_list)
            except Exception as exc:  # noqa: BLE001
                any_failed = True
                print(f"{pkg.name}\tFAILED\t{exc}")
                continue

            if owner_status == "FAILED":
                any_failed = True
                print(f"{pkg.name}\tFAILED\t{owner_detail}")
                continue

            if owner_status == "OK":
                any_matched = True
                print(f"{pkg.name}\t{owner_detail}")

        return 1 if any_failed else 0

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
        package_failed = False
        crate_exists = False
        has_version = False
        try:
            if args.check:
                crate_exists = crates_io_has_crate(pkg.name)
            has_version = crates_io_has_version(pkg.name, pkg.version)
            if has_version:
                print(f"{prefix} -> SKIP already exists on crates.io")
            elif args.check and crate_exists:
                print(f"{prefix} -> CHECK INFO crate exists, current version not published")
            elif args.check:
                print(f"{prefix} -> CHECK SKIP crate not published on crates.io")
            else:
                status, detail = publish_package(pkg, args.dry_run)
                if status == "FAILED":
                    any_failed = True
                    package_failed = True
                print(f"{prefix} -> {status} {detail}")
        except Exception as exc:  # noqa: BLE001
            any_failed = True
            package_failed = True
            print(f"{prefix} -> FAILED crates.io check: {exc}")
            continue

        if package_failed or not owners or not has_version:
            continue

        owner_status, owner_detail = sync_owners(pkg.name, owners, args.dry_run)
        if owner_status == "FAILED":
            any_failed = True
        print(f"{prefix} -> OWNER {owner_status} {owner_detail}")

    return 1 if any_failed else 0


if __name__ == "__main__":
    sys.exit(main())
