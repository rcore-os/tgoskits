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
import time
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
RATE_LIMIT_MARKERS = [
    "status 429 Too Many Requests",
    "You have published too many new crates in a short period of time",
]
LOCKED_FAILURE_MARKERS = [
    "because --locked was passed",
]
PUBLISH_INTERVAL_SECONDS = 60
EXCLUDED_SUBTREES = (
    Path("os/arceos/tools"),
)


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
    return any(
        is_path_under(resolved_path, (repo_root / subtree).resolve())
        for subtree in EXCLUDED_SUBTREES
    )


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

    publish = package_data.get("publish")
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
                    workspace_root=normalize_path(metadata["workspace_root"]),
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


def workspace_external_registry_blockers(
    packages: dict[str, Package],
    repo_root: Path,
) -> dict[Path, set[str]]:
    name_to_ids = package_name_index(packages)
    blockers: dict[Path, set[str]] = defaultdict(set)

    for package_id, pkg in packages.items():
        if pkg.workspace_root == repo_root:
            continue
        for dep in pkg.dependencies:
            if dep.get("path"):
                continue
            matching_ids = name_to_ids.get(dep["name"], [])
            if len(matching_ids) != 1:
                continue
            dep_id = matching_ids[0]
            dep_pkg = packages[dep_id]
            if dep_pkg.workspace_root == pkg.workspace_root:
                continue
            blockers[pkg.workspace_root].add(dep_id)

    return blockers


def crates_io_version_status(
    crate: str, version: str, timeout: float = 15.0
) -> CratesIoVersionStatus:
    crate_q = urllib.parse.quote(crate, safe="")
    version_q = urllib.parse.quote(version, safe="")
    url = f"https://crates.io/api/v1/crates/{crate_q}/{version_q}"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
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


def run_publish_command(pkg: Package, *, locked: bool) -> subprocess.CompletedProcess[str]:
    cmd = [
        "cargo",
        "publish",
        "--manifest-path",
        str(pkg.manifest_path),
    ]
    if locked:
        cmd.append("--locked")
    return run(cmd, check=False)


def publish_package(pkg: Package, dry_run: bool) -> tuple[str, str]:
    if dry_run:
        return "DRY-RUN", "not published"

    proc = run_publish_command(pkg, locked=True)
    retried_without_locked = False

    if proc.returncode != 0:
        detail = (proc.stderr.strip() or proc.stdout.strip() or f"exit code {proc.returncode}")
        if any(marker in detail for marker in LOCKED_FAILURE_MARKERS):
            proc = run_publish_command(pkg, locked=False)
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
    workspace_blockers = workspace_external_registry_blockers(packages, Path.cwd().resolve())

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
    pending = list(order)
    completed_count = 0
    publish_attempts = 0

    while pending:
        next_pending: list[str] = []
        progress = False

        for package_id in pending:
            pkg = packages[package_id]
            blockers = graph[package_id] | workspace_blockers.get(pkg.workspace_root, set())
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
                    print(
                        f"{prefix} -> FAILED version exists on crates.io but is yanked; "
                        "bump the version or unyank it before publishing dependents"
                    )
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
                    status, detail = publish_package(pkg, args.dry_run)
                    if not args.dry_run:
                        publish_attempts += 1
                    if status == "FAILED":
                        any_failed = True
                        package_failed = True
                    elif status == "RATE-LIMITED":
                        print(f"{prefix} -> {status} {detail}")
                        raise SystemExit(
                            "crates.io rate limit reached; stop now and retry after the "
                            "timestamp reported above"
                        )
                    elif status in {"PUBLISHED", "DRY-RUN"}:
                        should_sync_owner = True
                        ready_packages.add(package_id)
                    print(f"{prefix} -> {status} {detail}")
            except Exception as exc:  # noqa: BLE001
                any_failed = True
                package_failed = True
                print(f"{prefix} -> FAILED crates.io check: {exc}")
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
            unresolved = ", ".join(
                f"{packages[package_id].name} ({packages[package_id].rel_dir})"
                for package_id in next_pending
            )
            raise SystemExit(
                "unable to make publishing progress; unresolved prerequisites for: "
                + unresolved
            )

        pending = next_pending

    return 1 if any_failed else 0


if __name__ == "__main__":
    sys.exit(main())
