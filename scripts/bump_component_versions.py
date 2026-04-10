#!/usr/bin/env python3
"""Bump internal package versions and sync internal dependency versions."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent

PACKAGE_LINE_RE = re.compile(r'^(\s*version\s*=\s*")([^"]+)(".*)$')
INLINE_VERSION_RE = re.compile(r'(\bversion\s*=\s*")([^"]+)(")')
INLINE_PACKAGE_RE = re.compile(r'\bpackage\s*=\s*"([^"]+)"')
STRING_DEP_RE = re.compile(r'^(\s*)([A-Za-z0-9_.-]+)\s*=\s*"([^"]+)"(\s*(?:#.*)?)$')
SECTION_RE = re.compile(r"^\s*\[(.+)]\s*$")
SEMVER_CORE_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)")


@dataclass(frozen=True)
class PackageVersion:
    name: str
    old: str
    new: str
    manifest: Path
    version_source: Path


@dataclass(frozen=True)
class MetadataPackage:
    manifest: Path
    version: str
    workspace_root: Path


def bump_version(version: str) -> str:
    match = SEMVER_CORE_RE.match(version)
    if match is None:
        raise ValueError(f"unsupported semver version: {version}")
    major, minor, patch = (int(part) for part in match.groups())
    return f"{major}.{minor + 1}.{patch}"


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


def resolve_workspace_package_version(manifest: Path) -> tuple[Path, str] | None:
    current = manifest.parent
    repo_root = REPO_ROOT.resolve()

    while True:
        workspace_manifest = current / "Cargo.toml"
        if workspace_manifest.exists():
            data = tomllib.loads(workspace_manifest.read_text(encoding="utf-8"))
            workspace_package = data.get("workspace", {}).get("package")
            version = None
            if isinstance(workspace_package, dict):
                version = workspace_package.get("version")
            if isinstance(version, str):
                return workspace_manifest.resolve(), version

        if current.resolve() == repo_root or current.parent == current:
            return None
        current = current.parent


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


def discover_internal_package_versions(manifests: list[Path]) -> dict[str, PackageVersion]:
    versions: dict[str, PackageVersion] = {}
    metadata_packages = load_workspace_metadata()

    for manifest in manifests:
        manifest = manifest.resolve()
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        package = data.get("package")
        if not isinstance(package, dict):
            continue

        name = package.get("name")
        version = package.get("version")
        version_source = manifest
        if not isinstance(name, str):
            continue
        if not isinstance(version, str):
            raw_text = manifest.read_text(encoding="utf-8")
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

        versions[name] = PackageVersion(
            name=name,
            old=version,
            new=bump_version(version),
            manifest=manifest,
            version_source=version_source.resolve(),
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


def update_workspace_package_version(lines: list[str], package_versions: dict[str, PackageVersion], manifest: Path) -> bool:
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
    if manifest.resolve() != (REPO_ROOT / "Cargo.toml").resolve():
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

    if patch_end > patch_start + 1 and lines[patch_end - 1].strip():
        insertion = ["\n"]
    else:
        insertion = []
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
    new_req = package.new
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
    new_req = package.new
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
    return sorted(REPO_ROOT.rglob("Cargo.toml"))


def build_component_manifest_index(
    package_versions: dict[str, PackageVersion],
) -> dict[Path, PackageVersion]:
    return {package.manifest.resolve(): package for package in package_versions.values()}


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Bump versions for internal workspace/path crates by increasing the minor "
            "version and removing prerelease suffixes, then sync internal dependency "
            "versions across all tracked Cargo.toml files."
        )
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write changes back to Cargo.toml files",
    )
    args = parser.parse_args()

    manifests = collect_manifests()
    package_versions = discover_internal_package_versions(manifests)
    if not package_versions:
        print("no internal packages found", file=sys.stderr)
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
        print(f"  {package.name}: {package.old} -> {package.new}")

    print("\nupdated manifests:")
    for manifest in staged_changes:
        print(f"  {manifest.relative_to(REPO_ROOT)}")

    if not args.apply:
        print("\ndry-run only, rerun with --apply to write changes")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
