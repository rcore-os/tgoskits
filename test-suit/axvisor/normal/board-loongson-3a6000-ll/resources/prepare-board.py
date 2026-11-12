#!/usr/bin/env python3
"""Prepare board-only host initramfs with the manifest-bound block fixture."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    with path.open("rb") as stream:
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    resources = Path(__file__).resolve().parent
    workspace = resources.parents[4]
    target = os.environ.get("AXBUILD_TARGET_DIR", "")
    require(target and Path(target).is_absolute(),
            "board host_prepare requires AXBUILD_TARGET_DIR as an absolute target root")
    output = Path(target).resolve() / "axbuild/loongson-3a6000"
    # Keep the Linux recipe and cache identity independent of board packaging.
    subprocess.run([sys.executable, "-B", resources / "prepare.py", *sys.argv[1:]],
                   cwd=workspace, check=True)
    with (output / ".prepare.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        manifest = json.loads((output / "manifest.json").read_text())
        expected = manifest["artifacts"]["block.img"]
        require(sha256(output / "block.img") == expected, "block.img does not match manifest")
        root = output / "host-root"
        guest = root / "guest"
        image = guest / "loongson-block.img"
        # This case owns only these entries. Refuse unknown contents rather than
        # deleting them or accidentally adding them to the base archive.
        for directory in (root, guest):
            require(not directory.is_symlink(), f"refusing directory symlink: {directory}")
            directory.mkdir(exist_ok=True)
            directory.chmod(0o755)
        for entry in root.rglob("*"):
            require(entry in (guest, image) and not entry.is_symlink(),
                    f"unexpected host-root entry: {entry}")
        require(not image.exists() or image.is_file(), f"not a regular file: {image}")
        shutil.copyfile(output / "block.img", image)
        image.chmod(0o644)
        require(sha256(image) == expected, "copied block fixture does not match manifest")
        archive = output / "block-host.cpio.gz"
        require(not archive.is_symlink(), f"refusing archive symlink: {archive}")
        subprocess.run(["cargo", "xtask", "image", "pack-initramfs",
                        root, archive, "--gzip"], cwd=workspace, check=True)
        print(f"board block SHA256: {expected}", flush=True)
        print(f"board archive: {archive}", flush=True)
        print(f"board archive SHA256: {sha256(archive)}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Board resource preparation failed: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
