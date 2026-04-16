#!/usr/bin/env python3
"""Bump publishable internal crate versions using the minimal crates.io-safe version."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
ROOT_MANIFEST = (REPO_ROOT / "Cargo.toml").resolve()
IGNORED_PATH_PARTS = {".git", "node_modules", "target"}
AUXILIARY_DIR_NAMES = {"examples", "scripts", "test-suit", "tools"}
AUXILIARY_PUBLISH_FALSE_EXCLUDES = {
    (REPO_ROOT / "scripts/axbuild/Cargo.toml").resolve(),
    (REPO_ROOT / "xtask/Cargo.toml").resolve(),
}
CRATES_IO_API = "https://crates.io/api/v1/crates/{name}"

PACKAGE_LINE_RE = re.compile(r'^(\s*version\s*=\s*")([^"]+)(".*)$')
INLINE_VERSION_RE = re.compile(r'(\bversion\s*=\s*")([^"]+)(")')
INLINE_PACKAGE_RE = re.compile(r'\bpackage\s*=\s*"([^"]+)"')
PUBLISH_LINE_RE = re.compile(r"^(\s*publish\s*=\s*)(.+?)(\s*(?:#.*)?)$")
STRING_DEP_RE = re.compile(r'^(\s*)([A-Za-z0-9_.-]+)\s*=\s*"([^"]+)"(\s*(?:#.*)?)$')
SECTION_RE = re.compile(r"^\s*\[(.+)]\s*$")
SEMVER_CORE_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$")


@dataclass(frozen=True)
class PackageSpec:
    name: str
    old: str
    manifest: Path
    version_source: Path


@dataclass(frozen=True)
class PackageVersion:
    name: str
    old: str
    new: str
    manifest: Path
    version_source: Path
    published: str | None


@dataclass(frozen=True)
class MetadataPackage:
    manifest: Path
    version: str
    workspace_root: Path


def parse_semver(version: str) -> tuple[int, int, int]:
    match = SEMVER_CORE_RE.match(version)
    if match is None:
        raise ValueError(f"unsupported semver version: {version}")
    return tuple(int(part) for part in match.groups())


def compare_versions(left: str, right: str) -> int:
    left_parts = parse_semver(left)
    right_parts = parse_semver(right)
    if left_parts < right_parts:
        return -1
    if left_parts > right_parts:
        return 1
    return 0


def max_version(left: str, right: str) -> str:
    return left if compare_versions(left, right) >= 0 else right


def bump_patch(version: str) -> str:
    major, minor, patch = parse_semver(version)
    return f"{major}.{minor}.{patch + 1}"


def dependency_requirement(version: str) -> str:
    major, minor, _patch = parse_semver(version)
    return f"{major}.{minor}"


def is_ignored_manifest(manifest: Path) -> bool:
    relative = manifest.resolve().relative_to(REPO_ROOT)
    return any(part in IGNORED_PATH_PARTS for part in relative.parts)


def should_force_publish_false(manifest: Path) -> bool:
    manifest = manifest.resolve()
    if manifest in AUXILIARY_PUBLISH_FALSE_EXCLUDES:
        return False
    relative = manifest.relative_to(REPO_ROOT)
    return any(part in AUXILIARY_DIR_NAMES for part in relative.parts)


def load_workspace_metadata() -> dict[Path, MetadataPackage]:
    cmd = ["cargo", "metadata", "--format-version", "1", "--no-deps"]
    result = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    data = json.loads(result.stdout)
    workspace_root = Path(data["workspace_root"]).resolve()

    packages: dict[Path, MetadataPackage] = {}
    for package in data["packages"]:
        manifest = Path(package["manifest_path"]).resolve()
        version = package.get("version")
        if isinstance(version, str):
            packages[manifest] = MetadataPackage(
                manifest=manifest,
                version=version,
                workspace_root=workspace_root,
            )
    return packages


def resolve_workspace_package_field(manifest: Path, key: str) -> Any | None:
    current = manifest.parent.resolve()

    while True:
        workspace_manifest = current / "Cargo.toml"
        if workspace_manifest.exists():
            data = tomllib.loads(workspace_manifest.read_text(encoding="utf-8"))
            workspace_package = data.get("workspace", {}).get("package")
            if isinstance(workspace_package, dict) and key in workspace_package:
                return workspace_package[key]

        if current == REPO_ROOT or current.parent == current:
            return None
        current = current.parent


def resolve_workspace_package_version(manifest: Path) -> tuple[Path, str] | None:
    version = resolve_workspace_package_field(manifest, "version")
    if isinstance(version, str):
        current = manifest.parent.resolve()
        while True:
            workspace_manifest = current / "Cargo.toml"
            if workspace_manifest.exists():
                data = tomllib.loads(workspace_manifest.read_text(encoding="utf-8"))
                workspace_package = data.get("workspace", {}).get("package")
                if isinstance(workspace_package, dict) and workspace_package.get("version") == version:
                    return workspace_manifest.resolve(), version
            if current == REPO_ROOT or current.parent == current:
                break
            current = current.parent
    return None


def resolve_publishable(manifest: Path, raw_text: str, data: dict[str, Any]) -> bool:
    if should_force_publish_false(manifest):
        return False

    package = data.get("package")
    if not isinstance(package, dict):
        return False

    publish = package.get("publish")
    if isinstance(publish, bool):
        return publish
    if isinstance(publish, list):
        return True
    if isinstance(publish, dict) and publish.get("workspace") is True:
        workspace_publish = resolve_workspace_package_field(manifest, "publish")
        if isinstance(workspace_publish, bool):
            return workspace_publish
        if isinstance(workspace_publish, list):
            return True

    if re.search(r"^\s*publish\.workspace\s*=\s*true\s*$", raw_text, re.MULTILINE):
        workspace_publish = resolve_workspace_package_field(manifest, "publish")
        if isinstance(workspace_publish, bool):
            return workspace_publish
        if isinstance(workspace_publish, list):
            return True

    return True


def is_dependency_section(section: str | None) -> bool:
    if section is None:
        return False
    if section == "workspace.dependencies":
        return True
    return section.split(".")[-1] in {
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
    }


def discover_publishable_package_specs(manifests: list[Path]) -> list[PackageSpec]:
    specs: list[PackageSpec] = []
    metadata_packages = load_workspace_metadata()

    for manifest in manifests:
        manifest = manifest.resolve()
        raw_text = manifest.read_text(encoding="utf-8")
        data = tomllib.loads(raw_text)
        package = data.get("package")
        if not isinstance(package, dict):
            continue
        if not resolve_publishable(manifest, raw_text, data):
            continue

        name = package.get("name")
        version = package.get("version")
        version_source = manifest
        if not isinstance(name, str):
            continue
        if not isinstance(version, str):
            if re.search(r"^\s*version\.workspace\s*=\s*true\s*$", raw_text, re.MULTILINE):
                metadata_package = metadata_packages.get(manifest)
                if metadata_package is not None:
                    version = metadata_package.version
                    version_source = metadata_package.workspace_root / "Cargo.toml"
                else:
                    resolved = resolve_workspace_package_version(manifest)
                    if resolved is not None:
                        version_source, version = resolved
        if not isinstance(version, str):
            continue

        specs.append(
            PackageSpec(
                name=name,
                old=version,
                manifest=manifest,
                version_source=version_source.resolve(),
            )
        )

    return specs


@lru_cache(maxsize=None)
def fetch_crates_io_latest_version(crate_name: str) -> str | None:
    url = CRATES_IO_API.format(name=urllib.parse.quote(crate_name, safe=""))
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/json",
            "User-Agent": "tgoskits-version-bump/1.0",
        },
    )

    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise

    versions = payload.get("versions")
    if not isinstance(versions, list):
        return None

    latest: str | None = None
    for version_info in versions:
        if not isinstance(version_info, dict):
            continue
        if version_info.get("yanked") is True:
            continue
        version = version_info.get("num")
        if not isinstance(version, str):
            continue
        try:
            parse_semver(version)
        except ValueError:
            continue
        latest = version if latest is None else max_version(latest, version)
    return latest


def build_package_versions(specs: list[PackageSpec]) -> dict[str, PackageVersion]:
    desired_by_package: dict[str, tuple[str, str | None]] = {}
    desired_by_source: dict[Path, str] = {}

    for spec in specs:
        published = fetch_crates_io_latest_version(spec.name)
        base_version = spec.old if published is None else max_version(spec.old, published)
        desired = bump_patch(base_version)
        desired_by_package[spec.name] = (desired, published)
        current = desired_by_source.get(spec.version_source)
        desired_by_source[spec.version_source] = (
            desired if current is None else max_version(current, desired)
        )

    versions: dict[str, PackageVersion] = {}
    for spec in specs:
        desired, published = desired_by_package[spec.name]
        versions[spec.name] = PackageVersion(
            name=spec.name,
            old=spec.old,
            new=desired_by_source[spec.version_source],
            manifest=spec.manifest,
            version_source=spec.version_source,
            published=published,
        )
    return versions


def update_package_version(lines: list[str], package: PackageVersion) -> bool:
    changed = False
    section: str | None = None

    for idx, line in enumerate(lines):
        match = SECTION_RE.match(line)
        if match:
            section = match.group(1).strip()
            continue
        if section != "package":
            continue
        version_match = PACKAGE_LINE_RE.match(line)
        if version_match and version_match.group(2) == package.old:
            lines[idx] = f'{version_match.group(1)}{package.new}{version_match.group(3)}\n'
            changed = True
            break

    return changed


def update_workspace_package_version(
    lines: list[str],
    package_versions: dict[str, PackageVersion],
    manifest: Path,
) -> bool:
    targets = [
        package for package in package_versions.values() if package.version_source == manifest.resolve()
    ]
    if not targets:
        return False

    target_old = targets[0].old
    target_new = targets[0].new
    if any(package.old != target_old or package.new != target_new for package in targets[1:]):
        raise ValueError(
            f"inconsistent workspace package versions in {manifest.relative_to(REPO_ROOT)}"
        )

    changed = False
    section: str | None = None
    for idx, line in enumerate(lines):
        match = SECTION_RE.match(line)
        if match:
            section = match.group(1).strip()
            continue
        if section != "workspace.package":
            continue
        version_match = PACKAGE_LINE_RE.match(line)
        if version_match and version_match.group(2) == target_old:
            lines[idx] = f'{version_match.group(1)}{target_new}{version_match.group(3)}\n'
            changed = True
            break

    return changed


def update_package_publish_false(lines: list[str], manifest: Path) -> bool:
    if not should_force_publish_false(manifest.resolve()):
        return False

    original = list(lines)
    section: str | None = None
    package_section_started = False
    package_insert_at: int | None = None
    package_publish_idx: int | None = None
    package_body_end = 0

    for idx, line in enumerate(lines):
        match = SECTION_RE.match(line)
        if match:
            next_section = match.group(1).strip()
            if package_section_started:
                package_insert_at = idx
                break
            section = next_section
            package_section_started = next_section == "package"
            continue

        if section != "package":
            continue

        publish_match = PUBLISH_LINE_RE.match(line.rstrip("\n"))
        if publish_match is not None:
            package_publish_idx = idx
            if publish_match.group(2).strip() != "false":
                lines[idx] = f"{publish_match.group(1)}false{publish_match.group(3)}\n"
            continue

        stripped = line.strip()
        if stripped and not stripped.startswith("#"):
            package_body_end = idx + 1

    if not package_section_started:
        return False
    if package_insert_at is None:
        package_insert_at = len(lines)
    desired_insert_at = package_body_end

    changed = False
    if package_publish_idx is not None:
        publish_line = lines.pop(package_publish_idx)
        changed = True
        if package_publish_idx < desired_insert_at:
            desired_insert_at -= 1
    else:
        publish_line = "publish = false\n"
        changed = True

    if desired_insert_at < len(lines) and lines[desired_insert_at].strip():
        insertion = [publish_line, "\n"]
    else:
        insertion = [publish_line]
    lines[desired_insert_at:desired_insert_at] = insertion

    return lines != original


def collect_used_dependency_keys(manifests: list[Path]) -> set[str]:
    used: set[str] = set()
    pattern = re.compile(r'^\s*([A-Za-z0-9_.-]+)\s*=\s*(?:"|\{)')
    for manifest in manifests:
        section: str | None = None
        for line in manifest.read_text(encoding="utf-8").splitlines():
            section_match = SECTION_RE.match(line)
            if section_match:
                section = section_match.group(1).strip()
                continue
            if not is_dependency_section(section):
                continue
            match = pattern.match(line)
            if match:
                used.add(match.group(1))
    return used


def update_root_patch_crates_io(
    lines: list[str],
    manifest: Path,
    package_versions: dict[str, PackageVersion],
    manifests: list[Path],
) -> bool:
    if manifest.resolve() != ROOT_MANIFEST:
        return False

    data = tomllib.loads("".join(lines))
    patch_crates_io = data.get("patch", {}).get("crates-io", {})
    if not isinstance(patch_crates_io, dict):
        return False

    used_dependency_keys = collect_used_dependency_keys(manifests)
    missing_entries: list[tuple[str, str]] = []
    for name, package in package_versions.items():
        if name in patch_crates_io:
            continue
        if name not in used_dependency_keys:
            continue
        missing_entries.append((name, str(package.manifest.parent.relative_to(REPO_ROOT))))

    if not missing_entries:
        return False

    patch_start = None
    patch_end = len(lines)
    for idx, line in enumerate(lines):
        match = SECTION_RE.match(line)
        if not match:
            continue
        section = match.group(1).strip()
        if patch_start is None:
            if section == "patch.crates-io":
                patch_start = idx
            continue
        patch_end = idx
        break

    if patch_start is None:
        return False

    insertion: list[str] = []
    if patch_end > patch_start + 1 and lines[patch_end - 1].strip():
        insertion.append("\n")
    for name, path in sorted(missing_entries):
        insertion.append(f'{name} = {{ path = "{path}" }}\n')

    lines[patch_end:patch_end] = insertion
    return True


def update_inline_dependency_block(
    block_lines: list[str],
    package_versions: dict[str, PackageVersion],
) -> tuple[list[str], bool]:
    block = "".join(block_lines)
    header = block_lines[0]
    head_match = re.match(r"^(\s*)([A-Za-z0-9_.-]+)\s*=\s*\{", header)
    if head_match is None:
        return block_lines, False

    dep_key = head_match.group(2)
    package_name = dep_key
    package_match = INLINE_PACKAGE_RE.search(block)
    if package_match:
        package_name = package_match.group(1)

    package = package_versions.get(package_name)
    if package is None:
        return block_lines, False

    version_match = INLINE_VERSION_RE.search(block)
    if version_match is None:
        return block_lines, False

    old_req = version_match.group(2)
    new_req = dependency_requirement(package.new)
    if old_req == new_req:
        return block_lines, False

    updated = "".join(
        [
            block[: version_match.start(2)],
            new_req,
            block[version_match.end(2) :],
        ]
    )
    return updated.splitlines(keepends=True), True


def update_string_dependency_line(
    line: str,
    package_versions: dict[str, PackageVersion],
) -> tuple[str, bool]:
    match = STRING_DEP_RE.match(line.rstrip("\n"))
    if match is None:
        return line, False

    package = package_versions.get(match.group(2))
    if package is None:
        return line, False

    old_req = match.group(3)
    new_req = dependency_requirement(package.new)
    if old_req == new_req:
        return line, False

    updated = f'{match.group(1)}{match.group(2)} = "{new_req}"{match.group(4)}\n'
    return updated, True


def rewrite_manifest(
    manifest: Path,
    package_versions: dict[str, PackageVersion],
    component_package: PackageVersion | None,
    manifests: list[Path],
) -> tuple[str, bool]:
    original = manifest.read_text(encoding="utf-8")
    lines = original.splitlines(keepends=True)
    changed = False

    changed |= update_package_publish_false(lines, manifest)
    if component_package is not None:
        changed |= update_package_version(lines, component_package)
    changed |= update_workspace_package_version(lines, package_versions, manifest)
    changed |= update_root_patch_crates_io(lines, manifest, package_versions, manifests)

    section: str | None = None
    idx = 0
    while idx < len(lines):
        line = lines[idx]
        section_match = SECTION_RE.match(line)
        if section_match:
            section = section_match.group(1).strip()
            idx += 1
            continue

        if not is_dependency_section(section):
            idx += 1
            continue

        if "{" in line:
            brace_balance = line.count("{") - line.count("}")
            if brace_balance > 0 or re.match(r"^\s*[A-Za-z0-9_.-]+\s*=\s*\{", line):
                end = idx + 1
                while end < len(lines) and brace_balance > 0:
                    brace_balance += lines[end].count("{") - lines[end].count("}")
                    end += 1
                new_block, block_changed = update_inline_dependency_block(
                    lines[idx:end], package_versions
                )
                if block_changed:
                    lines[idx:end] = new_block
                    changed = True
                    idx += len(new_block)
                    continue

        new_line, line_changed = update_string_dependency_line(line, package_versions)
        if line_changed:
            lines[idx] = new_line
            changed = True

        idx += 1

    updated = "".join(lines)
    return updated, changed and updated != original


def collect_manifests() -> list[Path]:
    manifests: list[Path] = []
    for manifest in REPO_ROOT.rglob("Cargo.toml"):
        if is_ignored_manifest(manifest):
            continue
        manifests.append(manifest.resolve())
    return sorted(manifests)


def build_component_manifest_index(
    package_versions: dict[str, PackageVersion],
) -> dict[Path, PackageVersion]:
    return {package.manifest.resolve(): package for package in package_versions.values()}


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Bump publishable internal crates to the minimal version newer than both the "
            "current repository version and the latest non-yanked crates.io version, then "
            "sync internal dependency versions across tracked Cargo.toml files."
        )
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write changes back to Cargo.toml files",
    )
    args = parser.parse_args()

    manifests = collect_manifests()
    package_specs = discover_publishable_package_specs(manifests)
    package_versions = build_package_versions(package_specs)
    if not package_versions:
        print("no publishable internal packages found", file=sys.stderr)
        return 1

    component_index = build_component_manifest_index(package_versions)

    staged_changes: list[Path] = []
    for manifest in manifests:
        component_package = component_index.get(manifest.resolve())
        updated, changed = rewrite_manifest(
            manifest, package_versions, component_package, manifests
        )
        if changed:
            staged_changes.append(manifest)
            if args.apply:
                manifest.write_text(updated, encoding="utf-8")

    print("planned version bumps:")
    for package in sorted(package_versions.values(), key=lambda item: item.name):
        published = package.published or "<unpublished>"
        print(f"  {package.name}: {package.old} -> {package.new} (crates.io: {published})")

    print("\nupdated manifests:")
    for manifest in staged_changes:
        print(f"  {manifest.relative_to(REPO_ROOT)}")

    if not args.apply:
        print("\ndry-run only, rerun with --apply to write changes")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
