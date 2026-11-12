#!/usr/bin/env python3
"""Compile distributable axbuild artifacts and a public-API consumer outside TGOSKits.

Workspace dependencies are staged as normalized .crate files, including versions
not yet released publicly. Registry sources use Cargo's checksum-protected directory
source; no workspace path patches are used. Packaging is preparation, never the
success criterion: both the extracted library and its consumer must actually compile.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


def run(command, *, cwd, env=None):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), cwd=cwd, env=env, check=True)


def path_dependencies(metadata, name):
    packages = {p["name"]: p for p in metadata["packages"]}
    by_path = {Path(p["manifest_path"]).parent.resolve(): p for p in packages.values()}
    selected = {}

    def visit(package):
        if package["name"] in selected:
            return
        selected[package["name"]] = package
        for dependency in package["dependencies"]:
            if dependency.get("path") and dependency.get("kind") != "dev":
                visit(by_path[Path(dependency["path"]).resolve()])

    visit(packages[name])
    return selected


def unpack(archive, destination):
    with tarfile.open(archive) as source:
        source.extractall(destination, filter="data")


def install_package(archive, sources):
    # Cargo verifies both this archive identity and every extracted file.
    with tarfile.open(archive) as source:
        name = source.getmembers()[0].name.split("/", 1)[0]
    destination = sources / name
    if destination.exists():
        shutil.rmtree(destination)
    unpack(archive, sources)
    checksums = {
        str(path.relative_to(destination)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in destination.rglob("*")
        if path.is_file() and path.name != ".cargo-checksum.json"
    }
    (destination / ".cargo-checksum.json").write_text(json.dumps({
        "files": checksums, "package": hashlib.sha256(archive.read_bytes()).hexdigest(),
    }))



def check_source_boundaries(root):
    """Reject literal private-source inclusions; compilation remains authoritative."""
    owner = (root / "scripts/axbuild").resolve()
    paths = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard"],
                                    cwd=root, text=True).splitlines()
    attributes = re.compile(r'^\s*#\[path\s*=\s*"([^"]+)"\s*\]', re.MULTILINE)
    macros = re.compile(r'^\s*include(?:_bytes|_str)?!\(\s*"([^"]+)"', re.MULTILINE)
    for name in paths:
        source = root / name
        if source.suffix != ".rs" or not source.is_file() or source.resolve().is_relative_to(owner):
            continue
        text = source.read_text()
        for pattern in (attributes, macros):
            for match in pattern.finditer(text):
                target = (source.parent / match.group(1)).resolve()
                if target.is_relative_to(owner):
                    raise RuntimeError(f"{name}: includes private axbuild source {match.group(1)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--registry-sources", type=Path,
                        help="Reuse a checksum-protected directory source instead of cargo vendor")
    parser.add_argument("--since", help="Skip only when the dependency/source boundary is unaffected")
    args = parser.parse_args()
    root = args.workspace.resolve()
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"),
        "--no-deps", "--format-version", "1", "--locked",
    ], cwd=root, text=True))
    packages = path_dependencies(metadata, "axbuild")
    check_source_boundaries(root)
    if args.since:
        comparison = subprocess.run([
            "git", "diff", "--name-only", f"{args.since}...HEAD",
        ], cwd=root, text=True, capture_output=True, check=False)
        # A shallow or rewritten history must broaden validation, never skip it.
        changed = comparison.stdout.splitlines() if comparison.returncode == 0 else ["Cargo.toml"]
        if comparison.returncode:
            print("Comparison base unavailable; running the full independent build", flush=True)
        changed += subprocess.check_output(["git", "diff", "--name-only", "HEAD"], cwd=root, text=True).splitlines()
        changed += subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard"], cwd=root, text=True).splitlines()
        boundaries = [Path(p["manifest_path"]).parent.relative_to(root) for p in packages.values()]
        boundaries += [Path("xtask"), Path("test-suit/arceos"), Path("scripts/test"),
                       Path(".github/ci"), Path("os/axvisor/xtask"), Path("os/StarryOS/starryos/xtask")]
        manifests = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "os/axvisor/Cargo.toml",
                     "os/StarryOS/starryos/Cargo.toml"}
        if not any(name in manifests or any(Path(name).is_relative_to(p) for p in boundaries)
                   for name in changed):
            print("AXBUILD_INDEPENDENCE_NOT_APPLICABLE: no boundary inputs changed")
            return
    output = (args.output or Path(metadata["target_directory"]) / "axbuild-independence").resolve()
    output.mkdir(parents=True, exist_ok=True)
    sources = output / "sources"
    with tempfile.TemporaryDirectory(prefix="axbuild-package-") as directory:
        command = ["cargo", "package", "--manifest-path", root / "Cargo.toml", "--locked",
                   "--no-verify", "--allow-dirty", "--target-dir", output / "packaging"]
        for name in sorted(packages):
            command += ["-p", name]
        run(command, cwd=directory)
        if args.registry_sources:
            shutil.copytree(args.registry_sources.resolve(), sources, dirs_exist_ok=True)
        else:
            run(["cargo", "vendor", "--locked", "--versioned-dirs", "--manifest-path",
                 root / "Cargo.toml", sources], cwd=directory)
    archives = output / "packaging/package"
    for name, package in packages.items():
        install_package(archives / f"{name}-{package['version']}.crate", sources)
    with tempfile.TemporaryDirectory(prefix="axbuild-isolated-") as directory:
        unpacked = Path(directory) / "unpacked"
        unpacked.mkdir(exist_ok=True)
        archive = archives / f"axbuild-{packages['axbuild']['version']}.crate"
        library = unpacked / f"axbuild-{packages['axbuild']['version']}"
        if library.exists():
            shutil.rmtree(library)
        unpack(archive, unpacked)
        home = output / "cargo-home"
        home.mkdir(exist_ok=True)
        (home / "config.toml").write_text(
            '[source.crates-io]\nreplace-with="sealed"\n[source.sealed]\n'
            f'directory={json.dumps(str(sources))}\n'
        )
        toolchain = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
        env = os.environ.copy()
        env.update(CARGO_HOME=str(home), CARGO_TARGET_DIR=str(output / "target"), RUSTUP_TOOLCHAIN=toolchain)
        consumer = Path(directory) / "consumer"
        consumer.mkdir(exist_ok=True)
        (consumer / "src").mkdir(exist_ok=True)
        (consumer / "Cargo.toml").write_text(
            '[package]\nname="axbuild-consumer"\nversion="0.0.0"\nedition="2024"\n'
            f'[dependencies]\naxbuild={{path={json.dumps(str(library))}}}\n'
        )
        # A caller's explicit source policy wins over unrelated ancestor settings.
        (consumer / ".cargo").mkdir(exist_ok=True)
        shutil.copyfile(home / "config.toml", consumer / ".cargo/config.toml")
        (consumer / "src/main.rs").write_text(
            'fn main() {\n'
            '    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).unwrap();\n'
            '    let _request = axbuild::run_from(["consumer", "--help"]);\n'
            '    let _arceos = axbuild::arceos::ArceOS::new().expect("external ArceOS context");\n'
            '    let _axvisor = axbuild::axvisor::Axvisor::new().expect("external Axvisor context");\n'
            '    let _starry = axbuild::starry::Starry::new().expect("external Starry context");\n'
            '    let _axloader = axbuild::axloader::Axloader::new().expect("external Axloader context");\n'
            '}\n'
        )
        with tempfile.TemporaryDirectory(prefix="axbuild-compile-") as directory:
            run(["cargo", "check", "--manifest-path", library / "Cargo.toml", "--locked", "--offline"],
                cwd=directory, env=env)
            run(["cargo", "run", "--manifest-path", consumer / "Cargo.toml", "--offline"],
                cwd=directory, env=env)
        (output / "result.json").write_text(json.dumps({
            "library": str(library), "consumer": str(consumer), "toolchain": toolchain,
            "artifact_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "staged_workspace_dependencies": sorted(packages),
            "source_boundary": "temporary external Cargo project; normalized crate artifacts only; no workspace source patches",
            "temporary_workspace": True,
            "checks": {"extracted_library": "pass", "public_api_consumer": "pass", "consumer_context_initialization": "pass"},
            "public_registry_publication": "not performed; staging does not establish public availability",
        }, indent=2) + "\n")
        print("AXBUILD_INDEPENDENCE_PASS: extracted library and public-API consumer compiled and initialized")


if __name__ == "__main__":
    main()
