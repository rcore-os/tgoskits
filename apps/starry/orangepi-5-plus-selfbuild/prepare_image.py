#!/usr/bin/env python3
"""Prepare the complete build filesystem on the host, without mounting it."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def prepare_source(repo, archive):
    # A commit identifies exactly the files delivered to the guest. Untracked
    # host files are never included; tracked edits must first be committed.
    run("git", "-C", str(repo), "diff", "--exit-code", "HEAD", "--")
    commit = subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    run("git", "-C", str(repo), "archive", "--format=tar",
        "--output=" + str(archive), commit)
    return commit


def main():
    app = Path(os.environ["STARRY_CASE_DIR"]).resolve()
    repo = app.parents[2]
    work = Path(os.environ["STARRY_CASE_WORK_DIR"]).resolve()
    overlay = Path(os.environ["STARRY_CASE_OVERLAY_DIR"]).resolve() / "usr/share/selfbuild"
    environment = repo / "target/starry-orangepi5plus-selfbuild/environment/rootfs.tar"
    cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
    work.mkdir(parents=True, exist_ok=True)
    overlay.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="image-build-", dir=work) as temporary:
        build = Path(temporary)
        source = build / "source.tar"
        commit = prepare_source(repo, source)
        registry = []
        for kind in ("cache", "index", "src"):
            registry.extend(sorted((cargo_home / "registry" / kind).glob("index.crates.io-*")))
        if not any(path.parent.name == "src" for path in registry):
            raise RuntimeError("cargo fetch did not prepare registry sources")
        identity = hashlib.sha256()
        identity.update(b"selfbuild-ext4-v1\n")
        for value in (digest(environment), digest(source)):
            identity.update(value.encode())
        for directory in registry:
            for path in sorted(directory.rglob("*")):
                if path.is_file():
                    identity.update(str(path.relative_to(cargo_home)).encode() + b"\0")
                    identity.update(digest(path).encode())
        recipe = identity.hexdigest()
        recipe_file = overlay / "recipe-id"
        if recipe_file.is_file() and recipe_file.read_text().strip() == recipe:
            manifest = json.loads((overlay / "manifest.json").read_text())
            if all(digest(overlay / part["name"]) == part["sha256"]
                   for part in manifest["parts"]):
                (overlay / "epoch").write_text(str(int(time.time())) + "\n")
                print("selfbuild_host_image=reused", flush=True)
                return
        root = build / "root"
        root.mkdir()
        run("tar", "-xf", str(environment), "-C", str(root), "--no-same-owner")
        sources = root / "opt/tgoskits"
        sources.mkdir(parents=True)
        run("tar", "-xf", str(source), "-C", str(sources), "--no-same-owner")
        (sources / ".tgoskits-source-meta").write_text(
            "commit=" + commit + "\nsnapshot_sha256=" + digest(source) + "\n")
        for directory in registry:
            target = root / "root/.cargo" / directory.relative_to(cargo_home)
            shutil.copytree(directory, target, symlinks=True)
        for directory in ("proc", "dev", "sys", "work/targets", "output/runs", "opt/tgoskits/tmp"):
            (root / directory).mkdir(parents=True, exist_ok=True)
        if (sources / "target").exists():
            raise RuntimeError("source snapshot contains build products")
        size = int(subprocess.check_output(["du", "-sb", str(root)], text=True).split()[0])
        alignment = 64 * 1024 * 1024
        image_size = ((size * 112 // 100 + 128 * 1024 * 1024 + alignment - 1)
                      // alignment * alignment)
        image = build / "image.ext4"
        with image.open("wb") as output:
            output.truncate(image_size)
        print("selfbuild_host_image=populating bytes=" + str(image_size), flush=True)
        run("mkfs.ext4", "-q", "-F", "-b", "4096", "-m", "0", "-E",
            "lazy_itable_init=0,lazy_journal_init=0,root_owner=0:0", "-d", str(root), str(image))
        shutil.rmtree(root)
        assets = build / "assets"
        assets.mkdir()
        parts = []
        whole = hashlib.sha256()
        with image.open("rb") as stream:
            while True:
                data = stream.read(60 * 1024 * 1024)
                if not data:
                    break
                name = "image.part-{:03d}".format(len(parts))
                (assets / name).write_bytes(data)
                parts.append(dict(name=name, size=len(data), sha256=hashlib.sha256(data).hexdigest()))
                whole.update(data)
        manifest = dict(format=1, size=image_size, sha256=whole.hexdigest(), parts=parts)
        (assets / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        (assets / "recipe-id").write_text(recipe + "\n")
        (assets / "epoch").write_text(str(int(time.time())) + "\n")
        shutil.copyfile(app / "session-selfbuild.sh", assets / "run.sh")
        shutil.copyfile(app / "receive_image.py", assets / "receive_image.py")
        if overlay.exists():
            shutil.rmtree(overlay)
        assets.rename(overlay)
        print("selfbuild_host_image=ready sha256=" + manifest["sha256"], flush=True)


if __name__ == "__main__":
    main()
