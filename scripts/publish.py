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
import io
import json
import math
import os
import re
import ssl
import subprocess
import sys
import tarfile
import time
import tomllib
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
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
RATE_LIMIT_MARKERS = [
    "status 429 Too Many Requests",
    "You have published too many new crates in a short period of time",
]
RATE_LIMIT_RETRY_AFTER_RE = re.compile(
    r"Please try again after (?P<retry_after>.+?)(?: and see|$)"
)
LOCKED_FAILURE_MARKERS = [
    "because --locked was passed",
]
PUBLISH_INTERVAL_SECONDS = 60
DEFAULT_RATE_LIMIT_WAIT_SECONDS = 10 * 60
RATE_LIMIT_WAIT_BUFFER_SECONDS = 5
CRATES_IO_RETRY_ATTEMPTS = 3
CRATES_IO_RETRY_DELAY_SECONDS = 2.0
EXCLUDED_SUBTREES = (
    Path("os/arceos/tools"),
)
EXCLUDED_DIR_NAMES = frozenset({"examples", "tools", "scripts"})


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    manifest_path: Path
    package_id: str
    workspace_root: Path
    publish: Any
    dependencies: list[dict[str, Any]]

    @property
    def crate_dir(self) -> Path:
        return self.manifest_path.parent

    @property
    def rel_dir(self) -> str:
        return os.path.relpath(self.crate_dir, Path.cwd())


@dataclass(frozen=True)
class CratesIoVersionStatus:
    exists: bool
    yanked: bool = False


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


def cargo_toml_data(manifest: Path) -> dict[str, Any]:
    with manifest.open("rb") as fh:
        return tomllib.load(fh)


def discover_cargo_manifests(search_root: Path) -> list[Path]:
    repo_root = Path.cwd().resolve()
    manifests: list[Path] = []
    for manifest in sorted(search_root.rglob("Cargo.toml")):
        if "target" in manifest.parts:
            continue
        if is_excluded_path(manifest, repo_root):
            continue
        manifests.append(manifest.resolve())
    return manifests


def normalize_path(path: str | Path) -> Path:
    return Path(path).resolve()


def is_path_under(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def is_excluded_path(path: Path, repo_root: Path) -> bool:
    resolved_path = path.resolve()
    if any(part in EXCLUDED_DIR_NAMES for part in resolved_path.parts):
        return True
    return any(
        is_path_under(resolved_path, (repo_root / subtree).resolve())
        for subtree in EXCLUDED_SUBTREES
    )


def package_workspace_root(manifest_path: Path) -> Path:
    workspace_manifest = find_enclosing_workspace_manifest(manifest_path)
    if workspace_manifest is None:
        return manifest_path.parent.resolve()
    return workspace_manifest.parent.resolve()


def discover_workspace_manifests(search_root: Path) -> list[Path]:
    manifests: list[Path] = []
    for manifest in discover_cargo_manifests(search_root):
        if "workspace" in cargo_toml_data(manifest):
            manifests.append(manifest)
    return manifests


def dependency_tables(data: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
    tables: list[tuple[str, dict[str, Any]]] = []
    for key in ("dependencies", "build-dependencies", "dev-dependencies"):
        table = data.get(key)
        if isinstance(table, dict):
            tables.append((key, table))

    target_tables = data.get("target")
    if not isinstance(target_tables, dict):
        return tables

    for target_data in target_tables.values():
        if not isinstance(target_data, dict):
            continue
        for key in ("dependencies", "build-dependencies", "dev-dependencies"):
            table = target_data.get(key)
            if isinstance(table, dict):
                tables.append((key, table))

    return tables


def find_enclosing_workspace_manifest(manifest_path: Path) -> Path | None:
    current = manifest_path.parent
    while True:
        candidate = current / "Cargo.toml"
        if candidate.exists():
            data = cargo_toml_data(candidate)
            if "workspace" in data:
                return candidate.resolve()
        if current == current.parent:
            return None
        current = current.parent


def workspace_table(
    workspace_data: dict[str, Any], section: str, name: str
) -> Any | None:
    workspace = workspace_data.get("workspace")
    if not isinstance(workspace, dict):
        return None
    table = workspace.get(section)
    if not isinstance(table, dict):
        return None
    return table.get(name)


def resolve_workspace_value(
    value: Any,
    *,
    workspace_data: dict[str, Any],
    section: str,
    name: str,
) -> Any:
    if isinstance(value, dict) and value.get("workspace") is True:
        inherited = workspace_table(workspace_data, section, name)
        if inherited is None:
            raise SystemExit(
                f"unable to resolve workspace {section} entry {name!r} from enclosing workspace"
            )
        return inherited
    return value


def dependency_kind(table_name: str) -> str | None:
    if table_name == "build-dependencies":
        return "build"
    if table_name == "dev-dependencies":
        return "dev"
    return None


def normalize_dependency_spec(name: str, spec: Any, kind: str | None) -> dict[str, Any]:
    if isinstance(spec, str):
        return {
            "name": name,
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "req": spec,
            "kind": kind,
            "rename": None,
            "optional": False,
            "uses_default_features": True,
            "features": [],
            "target": None,
            "registry": None,
        }

    if not isinstance(spec, dict):
        raise SystemExit(f"unsupported dependency specification for {name!r}: {spec!r}")

    dep_name = spec.get("package", name)
    path = spec.get("path")
    source = None if path else "registry+https://github.com/rust-lang/crates.io-index"
    return {
        "name": dep_name,
        "source": source,
        "req": spec.get("version", "*"),
        "kind": kind,
        "rename": None if dep_name == name else name,
        "optional": bool(spec.get("optional", False)),
        "uses_default_features": spec.get("default-features", True),
        "features": list(spec.get("features", [])),
        "target": None,
        "registry": spec.get("registry"),
        **({"path": path} if path else {}),
    }


def package_from_manifest(
    manifest_path: Path,
    *,
    workspace_manifest: Path | None,
) -> Package | None:
    data = cargo_toml_data(manifest_path)
    package_data = data.get("package")
    if not isinstance(package_data, dict):
        return None

    workspace_data = cargo_toml_data(workspace_manifest) if workspace_manifest else {}

    publish = resolve_workspace_value(
        package_data.get("publish"),
        workspace_data=workspace_data,
        section="package",
        name="publish",
    )
    if publish is False or publish == []:
        return None

    name = package_data.get("name")
    if not isinstance(name, str) or not name:
        raise SystemExit(f"package name missing in {manifest_path}")

    version_value = resolve_workspace_value(
        package_data.get("version"),
        workspace_data=workspace_data,
        section="package",
        name="version",
    )
    if not isinstance(version_value, str) or not version_value:
        raise SystemExit(f"package version missing in {manifest_path}")

    dependencies: list[dict[str, Any]] = []
    for table_name, table in dependency_tables(data):
        kind = dependency_kind(table_name)
        for dep_alias, raw_spec in table.items():
            spec = resolve_workspace_value(
                raw_spec,
                workspace_data=workspace_data,
                section="dependencies",
                name=dep_alias,
            )
            dependencies.append(normalize_dependency_spec(dep_alias, spec, kind))

    workspace_root = (
        workspace_manifest.parent.resolve() if workspace_manifest else manifest_path.parent.resolve()
    )
    return Package(
        name=name,
        version=version_value,
        manifest_path=manifest_path.resolve(),
        package_id=str(manifest_path.resolve()),
        workspace_root=workspace_root,
        publish=publish,
        dependencies=dependencies,
    )


def collect_packages(metadata_sets: list[dict[str, Any]], root: Path) -> dict[str, Package]:
    root = root.resolve()
    repo_root = Path.cwd().resolve()
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
            if is_excluded_path(crate_dir, repo_root):
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
                    workspace_root=package_workspace_root(manifest_path),
                    publish=publish,
                    dependencies=pkg.get("dependencies", []),
                ),
            )

    return selected


def metadata_manifest_paths(metadata_sets: list[dict[str, Any]]) -> set[Path]:
    manifests: set[Path] = set()
    for metadata in metadata_sets:
        for pkg in metadata["packages"]:
            manifests.add(normalize_path(pkg["manifest_path"]))
    return manifests


def collect_orphan_packages(
    root: Path,
    known_manifests: set[Path],
) -> dict[str, Package]:
    repo_root = Path.cwd().resolve()
    selected: dict[str, Package] = {}

    for manifest_path in discover_cargo_manifests(repo_root):
        if manifest_path in known_manifests:
            continue

        try:
            manifest_path.parent.relative_to(root)
        except ValueError:
            continue

        data = cargo_toml_data(manifest_path)
        if "package" not in data or "workspace" in data:
            continue

        workspace_manifest = find_enclosing_workspace_manifest(manifest_path)
        package = package_from_manifest(
            manifest_path,
            workspace_manifest=workspace_manifest,
        )
        if package is None:
            continue

        selected[str(package.manifest_path)] = package

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


def package_blockers(
    package_id: str,
    packages: dict[str, Package],
    graph: dict[str, set[str]],
    workspace_blockers: dict[Path, set[str]],
) -> set[str]:
    return graph[package_id]


def summarize_failure_detail(detail: str) -> str:
    lines = [line.strip() for line in detail.splitlines() if line.strip()]
    if not lines:
        return detail.strip() or "failed"
    return lines[-1]


def parse_rate_limit_retry_after(detail: str) -> datetime | None:
    match = RATE_LIMIT_RETRY_AFTER_RE.search(detail)
    if match is None:
        return None

    retry_after = match.group("retry_after").strip()
    if not retry_after:
        return None

    try:
        retry_at = parsedate_to_datetime(retry_after)
    except (TypeError, ValueError, IndexError):
        return None

    if retry_at.tzinfo is None:
        retry_at = retry_at.replace(tzinfo=timezone.utc)
    return retry_at.astimezone(timezone.utc)


def rate_limit_wait_seconds(detail: str) -> tuple[int, str | None]:
    retry_at = parse_rate_limit_retry_after(detail)
    if retry_at is None:
        return DEFAULT_RATE_LIMIT_WAIT_SECONDS, None

    wait_seconds = math.ceil(
        (retry_at - datetime.now(timezone.utc)).total_seconds()
        + RATE_LIMIT_WAIT_BUFFER_SECONDS
    )
    wait_seconds = max(wait_seconds, 1)
    return wait_seconds, retry_at.strftime("%Y-%m-%d %H:%M:%S %Z")


def format_wait_duration(wait_seconds: int) -> str:
    minutes, seconds = divmod(wait_seconds, 60)
    hours, minutes = divmod(minutes, 60)

    parts: list[str] = []
    if hours:
        parts.append(f"{hours}h")
    if minutes:
        parts.append(f"{minutes}m")
    if seconds or not parts:
        parts.append(f"{seconds}s")
    return " ".join(parts)


def canonical_crate_name(name: str) -> str:
    return name.replace("_", "-")


def pending_unmet_blockers(
    pending: list[str],
    ready_packages: set[str],
    packages: dict[str, Package],
    graph: dict[str, set[str]],
    workspace_blockers: dict[Path, set[str]],
) -> dict[str, set[str]]:
    unmet_by_pkg: dict[str, set[str]] = {}
    for package_id in pending:
        blockers = package_blockers(package_id, packages, graph, workspace_blockers)
        unmet_by_pkg[package_id] = {dep_id for dep_id in blockers if dep_id not in ready_packages}
    return unmet_by_pkg


def failed_blocking_roots(
    package_id: str,
    unmet_by_pkg: dict[str, set[str]],
    failed_packages: dict[str, str],
    memo: dict[str, set[str]],
    visiting: set[str],
) -> set[str]:
    cached = memo.get(package_id)
    if cached is not None:
        return cached
    if package_id in visiting:
        return set()

    visiting.add(package_id)
    roots: set[str] = set()
    for dep_id in unmet_by_pkg.get(package_id, set()):
        if dep_id in failed_packages:
            roots.add(dep_id)
        elif dep_id in unmet_by_pkg:
            roots |= failed_blocking_roots(dep_id, unmet_by_pkg, failed_packages, memo, visiting)
    visiting.remove(package_id)
    memo[package_id] = roots
    return roots


def pending_cycle_groups(unmet_by_pkg: dict[str, set[str]]) -> list[list[str]]:
    pending_set = set(unmet_by_pkg)
    adjacency = {
        package_id: [dep_id for dep_id in deps if dep_id in pending_set]
        for package_id, deps in unmet_by_pkg.items()
    }
    index = 0
    stack: list[str] = []
    on_stack: set[str] = set()
    indices: dict[str, int] = {}
    lowlinks: dict[str, int] = {}
    groups: list[list[str]] = []

    def strongconnect(node: str) -> None:
        nonlocal index
        indices[node] = index
        lowlinks[node] = index
        index += 1
        stack.append(node)
        on_stack.add(node)

        for neighbor in adjacency[node]:
            if neighbor not in indices:
                strongconnect(neighbor)
                lowlinks[node] = min(lowlinks[node], lowlinks[neighbor])
            elif neighbor in on_stack:
                lowlinks[node] = min(lowlinks[node], indices[neighbor])

        if lowlinks[node] != indices[node]:
            return

        component: list[str] = []
        while stack:
            member = stack.pop()
            on_stack.remove(member)
            component.append(member)
            if member == node:
                break
        if len(component) > 1:
            groups.append(sorted(component))
            return
        member = component[0]
        if member in adjacency[member]:
            groups.append(component)

    for node in adjacency:
        if node not in indices:
            strongconnect(node)

    return sorted(groups, key=lambda group: [len(group), [str(item) for item in group]])


def stall_message(
    pending: list[str],
    ready_packages: set[str],
    failed_packages: dict[str, str],
    packages: dict[str, Package],
    graph: dict[str, set[str]],
    workspace_blockers: dict[Path, set[str]],
) -> str:
    unmet_by_pkg = pending_unmet_blockers(
        pending, ready_packages, packages, graph, workspace_blockers
    )
    lines = ["unable to make publishing progress"]

    failed_impacts: dict[str, list[str]] = defaultdict(list)
    memo: dict[str, set[str]] = {}
    for package_id in pending:
        for root_id in failed_blocking_roots(
            package_id, unmet_by_pkg, failed_packages, memo, set()
        ):
            failed_impacts[root_id].append(package_id)

    if failed_impacts:
        lines.append("blocked by failed prerequisites:")
        for root_id in sorted(failed_impacts, key=lambda item: packages[item].name):
            dependents = sorted(
                {packages[package_id].name for package_id in failed_impacts[root_id]}
            )
            preview = ", ".join(dependents[:8])
            if len(dependents) > 8:
                preview += f", ... (+{len(dependents) - 8} more)"
            root_pkg = packages[root_id]
            lines.append(
                f"  - {root_pkg.name} ({root_pkg.rel_dir}): "
                f"{summarize_failure_detail(failed_packages[root_id])}; blocks {preview}"
            )

    cycle_groups = pending_cycle_groups(unmet_by_pkg)
    if cycle_groups:
        lines.append("remaining dependency cycles:")
        for group in cycle_groups[:10]:
            names = ", ".join(packages[package_id].name for package_id in group)
            lines.append(f"  - {names}")
        if len(cycle_groups) > 10:
            lines.append(f"  - ... ({len(cycle_groups) - 10} more cycle groups)")

    if len(lines) == 1:
        unresolved = ", ".join(
            f"{packages[package_id].name} ({packages[package_id].rel_dir})"
            for package_id in pending
        )
        lines.append(f"unresolved prerequisites for: {unresolved}")

    return "\n".join(lines)


def crates_io_version_status(
    crate: str, version: str, timeout: float = 15.0
) -> CratesIoVersionStatus:
    crate_q = urllib.parse.quote(crate, safe="")
    version_q = urllib.parse.quote(version, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}/{version_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for attempt in range(1, CRATES_IO_RETRY_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                data = json.load(resp)
                version_data = data.get("version", {})
                return CratesIoVersionStatus(
                    exists=200 <= resp.status < 300,
                    yanked=bool(version_data.get("yanked", False)),
                )
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return CratesIoVersionStatus(exists=False)
            raise
        except (urllib.error.URLError, TimeoutError, ssl.SSLError):
            if attempt == CRATES_IO_RETRY_ATTEMPTS:
                raise
            time.sleep(CRATES_IO_RETRY_DELAY_SECONDS)


def crates_io_has_crate(crate: str, timeout: float = 15.0) -> bool:
    crate_q = urllib.parse.quote(crate, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for attempt in range(1, CRATES_IO_RETRY_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return 200 <= resp.status < 300
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return False
            raise
        except (urllib.error.URLError, TimeoutError, ssl.SSLError):
            if attempt == CRATES_IO_RETRY_ATTEMPTS:
                raise
            time.sleep(CRATES_IO_RETRY_DELAY_SECONDS)


def crates_io_crate_data(crate: str, timeout: float = 15.0) -> dict[str, Any]:
    crate_q = urllib.parse.quote(crate, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for attempt in range(1, CRATES_IO_RETRY_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return json.load(resp)
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return {}
            raise
        except (urllib.error.URLError, TimeoutError, ssl.SSLError):
            if attempt == CRATES_IO_RETRY_ATTEMPTS:
                raise
            time.sleep(CRATES_IO_RETRY_DELAY_SECONDS)


def crates_io_latest_version(crate: str, timeout: float = 15.0) -> str | None:
    data = crates_io_crate_data(crate, timeout=timeout)
    crate_data = data.get("crate")
    if not isinstance(crate_data, dict):
        return None
    newest_version = crate_data.get("newest_version")
    if isinstance(newest_version, str) and newest_version:
        return newest_version
    return None


def download_crate_file(crate: str, version: str, timeout: float = 30.0) -> bytes:
    crate_q = urllib.parse.quote(crate, safe="")
    version_q = urllib.parse.quote(version, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}/{version_q}/download"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for attempt in range(1, CRATES_IO_RETRY_ATTEMPTS + 1):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read()
        except urllib.error.HTTPError:
            raise
        except (urllib.error.URLError, TimeoutError, ssl.SSLError):
            if attempt == CRATES_IO_RETRY_ATTEMPTS:
                raise
            time.sleep(CRATES_IO_RETRY_DELAY_SECONDS)


def published_manifest_data(crate: str, version: str) -> dict[str, Any]:
    archive = download_crate_file(crate, version)
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tf:
        for preferred_suffix in ("Cargo.toml.orig", "Cargo.toml"):
            for member in tf.getmembers():
                if member.name.endswith(preferred_suffix):
                    extracted = tf.extractfile(member)
                    if extracted is None:
                        break
                    return tomllib.loads(extracted.read().decode())
    raise RuntimeError(f"unable to locate Cargo.toml in published crate {crate} {version}")


def manifest_dependency_names(manifest_data: dict[str, Any]) -> set[str]:
    dependency_names: set[str] = set()
    for _, table in dependency_tables(manifest_data):
        for dep_alias, raw_spec in table.items():
            if isinstance(raw_spec, str):
                dependency_names.add(canonical_crate_name(dep_alias))
            elif isinstance(raw_spec, dict):
                dependency_names.add(
                    canonical_crate_name(raw_spec.get("package", dep_alias))
                )
    return dependency_names


def published_internal_dependency_mismatches(
    package_id: str,
    packages: dict[str, Package],
    graph: dict[str, set[str]],
    *,
    version: str,
) -> list[str]:
    pkg = packages[package_id]
    manifest_data = published_manifest_data(pkg.name, version)
    published_deps = manifest_dependency_names(manifest_data)
    expected = sorted(
        {
            canonical_crate_name(packages[dep_id].name)
            for dep_id in graph[package_id]
        }
    )
    return [dep_name for dep_name in expected if dep_name not in published_deps]


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


def run_publish_command(
    pkg: Package, *, locked: bool, allow_dirty: bool
) -> subprocess.CompletedProcess[str]:
    cmd = [
        "cargo",
        "publish",
        "-p",
        pkg.name,
    ]
    if locked:
        cmd.append("--locked")
    if allow_dirty:
        cmd.append("--allow-dirty")
    return run(cmd, check=False)


def publish_package(pkg: Package, dry_run: bool, *, allow_dirty: bool) -> tuple[str, str]:
    if dry_run:
        return "DRY-RUN", "not published"

    proc = run_publish_command(pkg, locked=True, allow_dirty=allow_dirty)
    retried_without_locked = False

    if proc.returncode != 0:
        detail = (proc.stderr.strip() or proc.stdout.strip() or f"exit code {proc.returncode}")
        if any(marker in detail for marker in LOCKED_FAILURE_MARKERS):
            proc = run_publish_command(pkg, locked=False, allow_dirty=allow_dirty)
            retried_without_locked = True

    if proc.returncode == 0:
        detail = "ok"
        if retried_without_locked:
            detail += " (retried without --locked)"
        return "PUBLISHED", detail

    stderr = proc.stderr.strip()
    stdout = proc.stdout.strip()
    detail = stderr or stdout or f"exit code {proc.returncode}"
    if retried_without_locked:
        detail = f"{detail}\n(retried without --locked after lockfile update was required)"
    if any(marker in detail for marker in RATE_LIMIT_MARKERS):
        return "RATE-LIMITED", detail
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
        "--allow-dirty",
        action="store_true",
        help=(
            "Pass --allow-dirty to cargo publish. Use this only when you intend "
            "to publish the current uncommitted workspace state."
        ),
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
    parser.add_argument(
        "--check-published-deps",
        action="store_true",
        help=(
            "Check whether already-published crate versions depend on the current "
            "internal crate names used in this repository. If the current local "
            "version is not published yet, inspect the latest published version."
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
    packages.update(
        collect_orphan_packages(root, metadata_manifest_paths(metadata_sets))
    )
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
    workspace_blockers: dict[Path, set[str]] = {}

    if args.check_published_deps:
        any_failed = False
        any_mismatch = False
        for package_id in order:
            pkg = packages[package_id]
            try:
                version_status = crates_io_version_status(pkg.name, pkg.version)
                inspected_version = pkg.version
                if not version_status.exists:
                    inspected_version = crates_io_latest_version(pkg.name) or ""
                if not inspected_version:
                    continue
                mismatches = published_internal_dependency_mismatches(
                    package_id, packages, graph, version=inspected_version
                )
            except Exception as exc:  # noqa: BLE001
                any_failed = True
                print(f"{pkg.name}\tFAILED\t{exc}")
                continue

            if mismatches:
                any_mismatch = True
                print(
                    f"{pkg.name}\tMISMATCH\tpublished {inspected_version} missing current internal deps: "
                    + ", ".join(mismatches)
                )
            else:
                print(f"{pkg.name}\tOK\tpublished {inspected_version}")

        return 1 if any_failed or any_mismatch else 0

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
    print("initial candidate order (packages may be deferred until prerequisites are published):")
    for index, package_id in enumerate(order, start=1):
        pkg = packages[package_id]
        print(f"  {index}. {pkg.name} {pkg.version} ({pkg.rel_dir})")
    print()

    any_failed = False
    ready_packages: set[str] = set()
    failed_packages: dict[str, str] = {}
    pending = list(order)
    completed_count = 0
    publish_attempts = 0

    while pending:
        next_pending: list[str] = []
        progress = False

        for package_id in pending:
            pkg = packages[package_id]
            blockers = graph[package_id]
            unmet = [dep_id for dep_id in blockers if dep_id not in ready_packages]
            if unmet:
                next_pending.append(package_id)
                continue

            completed_count += 1
            progress = True
            prefix = f"[{completed_count}/{len(order)}] {pkg.name} {pkg.version} ({pkg.rel_dir})"
            package_failed = False
            crate_exists = False
            version_status = CratesIoVersionStatus(exists=False)
            should_sync_owner = False
            try:
                if args.check:
                    crate_exists = crates_io_has_crate(pkg.name)
                version_status = crates_io_version_status(pkg.name, pkg.version)
                if version_status.exists and version_status.yanked:
                    any_failed = True
                    package_failed = True
                    detail = (
                        "version exists on crates.io but is yanked; "
                        "bump the version or unyank it before publishing dependents"
                    )
                    failed_packages[package_id] = detail
                    print(f"{prefix} -> FAILED {detail}")
                elif version_status.exists:
                    print(f"{prefix} -> SKIP already exists on crates.io")
                    ready_packages.add(package_id)
                elif args.check and crate_exists:
                    print(f"{prefix} -> CHECK INFO crate exists, current version not published")
                elif args.check:
                    print(f"{prefix} -> CHECK SKIP crate not published on crates.io")
                else:
                    if not args.dry_run and publish_attempts > 0:
                        print(
                            f"{prefix} -> WAIT {PUBLISH_INTERVAL_SECONDS}s before publish"
                        )
                        time.sleep(PUBLISH_INTERVAL_SECONDS)
                    while True:
                        status, detail = publish_package(
                            pkg,
                            args.dry_run,
                            allow_dirty=args.allow_dirty,
                        )
                        if not args.dry_run:
                            publish_attempts += 1
                        if status != "RATE-LIMITED":
                            break

                        print(f"{prefix} -> {status} {detail}")
                        wait_seconds, retry_after = rate_limit_wait_seconds(detail)
                        wait_detail = format_wait_duration(wait_seconds)
                        if retry_after is None:
                            print(
                                f"{prefix} -> WAIT {wait_detail} for crates.io rate limit reset "
                                "(default backoff)"
                            )
                        else:
                            print(
                                f"{prefix} -> WAIT {wait_detail} for crates.io rate limit reset "
                                f"(retry after {retry_after})"
                            )
                        time.sleep(wait_seconds)

                    if status == "FAILED":
                        any_failed = True
                        package_failed = True
                        failed_packages[package_id] = detail
                    elif status in {"PUBLISHED", "DRY-RUN"}:
                        should_sync_owner = True
                        ready_packages.add(package_id)
                    print(f"{prefix} -> {status} {detail}")
            except Exception as exc:  # noqa: BLE001
                any_failed = True
                package_failed = True
                failed_detail = f"crates.io check failed: {exc}"
                failed_packages[package_id] = failed_detail
                print(f"{prefix} -> FAILED {failed_detail}")
                continue

            if package_failed or not owners or not should_sync_owner:
                continue

            owner_status, owner_detail = sync_owners(pkg.name, owners, args.dry_run)
            if owner_status == "FAILED":
                any_failed = True
            print(f"{prefix} -> OWNER {owner_status} {owner_detail}")

        if not next_pending:
            break

        if not progress:
            raise SystemExit(
                stall_message(
                    next_pending,
                    ready_packages,
                    failed_packages,
                    packages,
                    graph,
                    workspace_blockers,
                )
            )

        pending = next_pending

    return 1 if any_failed else 0


if __name__ == "__main__":
    sys.exit(main())
