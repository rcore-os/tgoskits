#!/usr/bin/env python3
"""Prepare the independent ext4 fixture and its host initramfs."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid


FIXTURE = b"AXVISOR_LOONGSON_BLOCK_FIXTURE_V1\n"
FIXTURE_NAME = "axvisor-board-fixture.txt"
IMAGE_BYTES = 16 * 1024 * 1024
ARTIFACTS = ("block.img", "block-host.cpio.gz")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    with path.open("rb") as stream:
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(argv, *, env, capture=False):
    result = subprocess.run([str(arg) for arg in argv], env=env, check=True,
                            stdout=subprocess.PIPE if capture else None)
    return result.stdout if capture else None


def prepare_block(output, env, input_hash):
    with tempfile.TemporaryDirectory(prefix=".block-", dir=output) as temporary:
        staging = Path(temporary) / "root"
        staging.mkdir()
        fixture = staging / FIXTURE_NAME
        fixture.write_bytes(FIXTURE)
        fixture.chmod(0o644)
        staging.chmod(0o755)
        epoch = int(env["SOURCE_DATE_EPOCH"])
        os.utime(fixture, (epoch, epoch))
        os.utime(staging, (epoch, epoch))
        image = Path(temporary) / "block.img"
        with image.open("wb") as stream:
            stream.truncate(IMAGE_BYTES)
        fixed_uuid = str(uuid.uuid5(uuid.NAMESPACE_URL, input_hash))
        run(["mke2fs", "-q", "-t", "ext4", "-b", "1024", "-I", "256", "-m", "0",
             "-U", fixed_uuid, "-L", "AXVISOR_FIXTURE",
             "-O", "has_journal,extent,filetype,64bit,flex_bg,sparse_super,large_file,"
                   "huge_file,dir_nlink,extra_isize,metadata_csum,dir_index",
             "-E", f"hash_seed={fixed_uuid},lazy_itable_init=0,lazy_journal_init=0,root_owner=0:0",
             "-d", staging, image], env=env)
        # Normalize host ownership and ctime copied by mke2fs -d.
        for inode in ("/", "/lost+found", "/" + FIXTURE_NAME):
            for field, value in [("uid", "0"), ("gid", "0"), ("atime", f"@{epoch}"),
                                 ("ctime", f"@{epoch}"), ("mtime", f"@{epoch}"),
                                 ("crtime", f"@{epoch}")]:
                run(["debugfs", "-w", "-R", f"set_inode_field {inode} {field} {value}", image],
                    env=env)
        require(run(["debugfs", "-R", f"cat /{FIXTURE_NAME}", image], env=env, capture=True)
                == FIXTURE, "ext4 fixture content mismatch")
        image.chmod(0o644)
        image.replace(output / "block.img")


def prepare(resources, workspace, output):
    # A positive epoch also fixes e2fsprogs' superblock creation timestamps.
    epoch = os.environ.get("SOURCE_DATE_EPOCH", "1")
    require(epoch.isdigit() and 0 < int(epoch) <= 0x7FFFFFFF, "invalid SOURCE_DATE_EPOCH")
    env = {"PATH": os.environ["PATH"], "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
           "SOURCE_DATE_EPOCH": epoch, "E2FSPROGS_FAKE_TIME": epoch,
           "MKE2FS_CONFIG": str(resources / "mke2fs.conf")}
    tools = {}
    for name in ("mke2fs", "debugfs"):
        path = shutil.which(name, path=env["PATH"])
        require(path, f"required tool not found: {name}")
        tools[name] = sha256(Path(path))
    inputs = {"schema": 3, "source_date_epoch": int(epoch), "tools": tools,
              "prepare_sha256": sha256(Path(__file__).resolve()),
              "mke2fs_config_sha256": sha256(resources / "mke2fs.conf"),
              "fixture_sha256": hashlib.sha256(FIXTURE).hexdigest(), "block_bytes": IMAGE_BYTES}
    input_hash = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
    manifest_path = output / "board-manifest.json"
    for path in [manifest_path, *(output / name for name in ARTIFACTS)]:
        require(not path.is_symlink(), f"refusing output symlink: {path}")
    try:
        previous = json.loads(manifest_path.read_text()) if manifest_path.is_file() else {}
    except (ValueError, UnicodeError):
        previous = {}
    if not isinstance(previous, dict) or not isinstance(previous.get("artifacts"), dict):
        previous = {}
    if previous.get("input_sha256") == input_hash and all(
        (output / name).is_file()
        and sha256(output / name) == previous.get("artifacts", {}).get(name)
        for name in ARTIFACTS
    ):
        print(f"board resources cache hit: {input_hash}", flush=True)
        artifacts = previous["artifacts"]
    else:
        manifest_path.unlink(missing_ok=True)
        prepare_block(output, env, input_hash)
        root = output / "host-root"
        guest = root / "guest"
        image = guest / "loongson-block.img"
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
        require(sha256(image) == sha256(output / "block.img"), "copied block fixture mismatch")
        subprocess.run(["cargo", "xtask", "image", "pack-initramfs", root,
                        output / "block-host.cpio.gz", "--gzip"], cwd=workspace, check=True)
        artifacts = {name: sha256(output / name) for name in ARTIFACTS}
        manifest = {"inputs": inputs, "input_sha256": input_hash, "artifacts": artifacts}
        with tempfile.NamedTemporaryFile(mode="w", dir=output, delete=False) as temporary:
            json.dump(manifest, temporary, indent=2, sort_keys=True)
            temporary.write("\n")
        Path(temporary.name).replace(manifest_path)
        print(f"board resources prepared: {input_hash}", flush=True)
    print(json.dumps(artifacts, indent=2), flush=True)


def main():
    resources = Path(__file__).resolve().parent
    workspace = resources.parents[4]
    target = os.environ.get("AXBUILD_TARGET_DIR", "")
    require(target and Path(target).is_absolute(),
            "board host_prepare requires AXBUILD_TARGET_DIR as an absolute target root")
    output = Path(target).resolve() / "axbuild/loongson-3a6000"
    output.mkdir(parents=True, exist_ok=True)
    with (output / ".prepare.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        prepare(resources, workspace, output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Board resource preparation failed: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
