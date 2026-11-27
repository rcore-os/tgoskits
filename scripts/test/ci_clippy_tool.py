#!/usr/bin/env python3
"""Package one revision's host tool and restore it without unsafe tar extraction."""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ARCHIVE = "clippy-tool.tar.xz"
CHECKSUM = "clippy-tool.sha256"


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pack(binary, output):
    output.mkdir(parents=True, exist_ok=True)
    strip = shutil.which("strip") or shutil.which("llvm-strip")
    if not strip:
        raise RuntimeError("strip or llvm-strip is required to package the host tool")
    with tempfile.TemporaryDirectory(prefix="clippy-tool-", dir=output) as directory:
        staged = Path(directory) / "tg-xtask"
        shutil.copyfile(binary, staged)
        subprocess.run([strip, "--strip-debug", str(staged)], check=True)
        archive = Path(directory) / ARCHIVE
        with tarfile.open(archive, "w:xz", preset=1) as tar:
            member = tarfile.TarInfo("tg-xtask")
            member.size = staged.stat().st_size
            member.mode = 0o755
            with staged.open("rb") as stream:
                tar.addfile(member, stream)
        digest = sha256(archive)
        archive.replace(output / ARCHIVE)
        (output / CHECKSUM).write_text(f"{digest}  {ARCHIVE}\n")
    print(f"CLIPPY_TOOL_PACKED: original_bytes={binary.stat().st_size} archive_bytes={(output / ARCHIVE).stat().st_size}")


def unpack(source, destination):
    archive = source / ARCHIVE
    checksum = (source / CHECKSUM).read_text().split()
    if len(checksum) != 2 or checksum[1] != ARCHIVE or sha256(archive) != checksum[0]:
        raise ValueError("Clippy tool archive checksum mismatch")
    restore_archive(archive, "r:xz", "tg-xtask", destination)


def restore_archive(archive, mode, member_name, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, mode) as tar:
        members = tar.getmembers()
        if len(members) != 1 or members[0].name != member_name or not members[0].isfile() or members[0].size == 0:
            raise ValueError(f"tool archive must contain only the regular file {member_name}")
        stream = tar.extractfile(members[0])
        if stream is None:
            raise ValueError("Clippy tool archive has no executable payload")
        with stream, tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
            temporary_path = Path(temporary.name)
            try:
                shutil.copyfileobj(stream, temporary)
                temporary.flush()
                os.chmod(temporary_path, 0o755)
                temporary_path.replace(destination)
            finally:
                temporary_path.unlink(missing_ok=True)
    print(f"CLIPPY_TOOL_RESTORED: bytes={destination.stat().st_size}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    build = sub.add_parser("pack")
    build.add_argument("--binary", type=Path, required=True)
    build.add_argument("--output-dir", type=Path, required=True)
    restore = sub.add_parser("unpack")
    restore.add_argument("--input-dir", type=Path, required=True)
    restore.add_argument("--destination", type=Path, required=True)
    preflight = sub.add_parser("unpack-preflight")
    preflight.add_argument("--archive", type=Path, required=True)
    preflight.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    if args.mode == "pack":
        pack(args.binary, args.output_dir)
    elif args.mode == "unpack":
        unpack(args.input_dir, args.destination)
    else:
        # The workflow downloads only its current run's producer artifact;
        # use the legacy member name without extracting arbitrary tar paths.
        restore_archive(args.archive, "r:", "target/debug/tg-xtask", args.destination)


if __name__ == "__main__":
    main()
