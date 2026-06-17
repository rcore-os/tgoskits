#!/usr/bin/env python3
"""Materialize visual-test rootfs_extras from Alpine package manifests."""

from __future__ import annotations

import argparse
import io
import os
import re
import shutil
import tarfile
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path


ALPINE_BRANCH = os.environ.get("VISUAL_ALPINE_BRANCH", "edge")
ALPINE_MIRROR = os.environ.get("VISUAL_ALPINE_MIRROR", "https://dl-cdn.alpinelinux.org/alpine")
REPOS = ("main", "community")


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    repo: str
    depends: tuple[str, ...]
    provides: tuple[str, ...]

    @property
    def filename(self) -> str:
        return f"{self.name}-{self.version}.apk"


def fetch_bytes(url: str) -> bytes:
    with urllib.request.urlopen(url) as response:
        return response.read()


def parse_manifest(path: Path) -> list[str]:
    packages: list[str] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if line:
            packages.append(line)
    if not packages:
        raise SystemExit(f"{path} does not list any packages")
    return packages


def normalize_dependency(dep: str) -> str:
    dep = dep.strip()
    if dep.startswith("!"):
        return ""
    return re.split(r"[<>=~]", dep, maxsplit=1)[0]


def parse_apkindex(data: bytes, repo: str) -> list[Package]:
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        index_member = archive.extractfile("APKINDEX")
        if index_member is None:
            raise SystemExit(f"APKINDEX missing from {repo}")
        text = index_member.read().decode("utf-8")

    packages: list[Package] = []
    for record in text.strip().split("\n\n"):
        fields: dict[str, list[str]] = {}
        for line in record.splitlines():
            if len(line) >= 2 and line[1] == ":":
                fields.setdefault(line[0], []).append(line[2:])
        names = fields.get("P", [])
        versions = fields.get("V", [])
        if not names or not versions:
            continue
        depends = tuple(
            dep for value in fields.get("D", []) for dep in value.split() if normalize_dependency(dep)
        )
        provides = tuple(
            normalize_dependency(provide)
            for value in fields.get("p", [])
            for provide in value.split()
            if normalize_dependency(provide)
        )
        packages.append(Package(names[0], versions[0], repo, depends, provides))
    return packages


def load_indexes(arch: str) -> tuple[dict[str, Package], dict[str, Package]]:
    by_name: dict[str, Package] = {}
    by_provide: dict[str, Package] = {}
    for repo in REPOS:
        url = f"{ALPINE_MIRROR}/{ALPINE_BRANCH}/{repo}/{arch}/APKINDEX.tar.gz"
        print(f"[visual] fetching {url}")
        for package in parse_apkindex(fetch_bytes(url), repo):
            by_name.setdefault(package.name, package)
            for provide in package.provides:
                by_provide.setdefault(provide, package)
    return by_name, by_provide


def resolve_packages(roots: list[str], by_name: dict[str, Package], by_provide: dict[str, Package]) -> list[Package]:
    resolved: dict[str, Package] = {}
    visiting = list(roots)
    while visiting:
        dep = normalize_dependency(visiting.pop())
        if not dep:
            continue
        package = by_name.get(dep) or by_provide.get(dep)
        if package is None:
            raise SystemExit(f"could not resolve Alpine dependency {dep!r}")
        if package.name in resolved:
            continue
        resolved[package.name] = package
        visiting.extend(package.depends)
    return sorted(resolved.values(), key=lambda package: package.name)


def safe_extract_apk(apk_path: Path, dest: Path) -> None:
    with tarfile.open(apk_path, mode="r:gz") as archive:
        for member in archive.getmembers():
            name = member.name
            if name.startswith(".") or name in {"dev", "proc", "sys"}:
                continue
            target = (dest / name).resolve()
            if not str(target).startswith(str(dest.resolve()) + os.sep):
                raise SystemExit(f"refusing unsafe apk member path {name!r}")
            archive.extract(member, dest)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", required=True, help="Alpine architecture, for example riscv64")
    parser.add_argument("--scenario", required=True, help="visual scenario name")
    parser.add_argument("--repo-root", type=Path, default=None)
    args = parser.parse_args()

    repo_root = args.repo_root or Path(__file__).resolve().parents[2]
    scenario_dir = repo_root / "apps" / "starry" / "visual" / "scenarios" / args.scenario
    manifest = scenario_dir / "rootfs_extras.packages"
    output = scenario_dir / "rootfs_extras"

    roots = parse_manifest(manifest)
    by_name, by_provide = load_indexes(args.arch)
    packages = resolve_packages(roots, by_name, by_provide)

    with tempfile.TemporaryDirectory(prefix="visual-rootfs-extras-") as temp_name:
        temp = Path(temp_name)
        staging = temp / "rootfs_extras"
        staging.mkdir()
        for package in packages:
            url = f"{ALPINE_MIRROR}/{ALPINE_BRANCH}/{package.repo}/{args.arch}/{package.filename}"
            apk_path = temp / package.filename
            print(f"[visual] fetching {package.name} ({package.repo})")
            apk_path.write_bytes(fetch_bytes(url))
            safe_extract_apk(apk_path, staging)

        if output.exists():
            shutil.rmtree(output)
        shutil.copytree(staging, output, symlinks=True)

    print(f"[visual] wrote {output} with {len(packages)} packages")


if __name__ == "__main__":
    main()
