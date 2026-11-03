#!/usr/bin/env python3
"""Prepare disposable QEMU disks from an existing AArch64 BusyBox rootfs.

This manual system regression supplements the allocation-credit unit test with
the real page cache, journal, loop device, fsync and remount paths. It needs
e2fsprogs and aarch64-linux-gnu-gcc; it never mounts a disk on the host.

From the repository root, prepare once with:
    python3 fs/rsext4/tests/qemu/prepare.py /absolute/path/to/rootfs-aarch64-busybox.img
Then run through the project entry point:
    cargo xtask starry test qemu --build-config fs/rsext4/tests/qemu/build-aarch64-unknown-none-softfloat.toml --qemu-config fs/rsext4/tests/qemu/qemu-aarch64.toml --rootfs tmp/ext4-fragmented-writeback/rootfs.img

QEMU discards all writes, so the same prepared disks can be used for A/B runs.
"""

from pathlib import Path
import shutil
import struct
import subprocess
import sys


CASE = Path(__file__).resolve().parent
REPO = CASE.parents[3]
OUTPUT = REPO / "tmp/ext4-fragmented-writeback"


def run(*args):
    subprocess.run(args, check=True)


def check_fragmentation(image):
    fragmented = 0
    contiguous = 0
    with image.open("rb") as stream:
        stream.seek(1024)
        superblock = stream.read(1024)
        blocks = struct.unpack_from("<I", superblock, 4)[0]
        per_group = struct.unpack_from("<I", superblock, 32)[0]
        desc_size = struct.unpack_from("<H", superblock, 254)[0]
        stream.seek(4096)
        descriptors = stream.read((blocks // per_group) * desc_size)
        for offset in range(0, len(descriptors), desc_size):
            bitmap_block = struct.unpack_from("<I", descriptors, offset)[0]
            free = struct.unpack_from("<H", descriptors, offset + 12)[0]
            stream.seek(bitmap_block * 4096)
            bitmap = stream.read(4096)
            longest = current = 0
            for bit in range(per_group):
                current = 0 if bitmap[bit // 8] & (1 << (bit % 8)) else current + 1
                longest = max(longest, current)
            fragmented += free >= 256 and longest < 256
            contiguous += longest >= 256
    if fragmented <= 256 or not contiguous:
        raise RuntimeError("fixture does not force a fragmented search beyond the credit budget")
    print("fragmented_groups={} contiguous_groups={}".format(fragmented, contiguous), flush=True)


def prepare(rootfs):
    OUTPUT.mkdir(parents=True, exist_ok=True)
    root = OUTPUT / "rootfs.img"
    if root.exists() or (OUTPUT / "fragmented.img").exists():
        raise RuntimeError("use a fresh output directory; do not overwrite an active QEMU disk")
    shutil.copyfile(rootfs, root)
    program = OUTPUT / "fragmented-writeback"
    run("aarch64-linux-gnu-gcc", "-static", "-O2", "-Wall", "-Wextra", "-Werror",
        str(CASE / "main.c"), "-o", str(program))
    run("debugfs", "-w", "-R", "write {} /fragmented-writeback".format(program), str(root))
    image = OUTPUT / "fragmented.img"
    with image.open("wb") as stream:
        stream.truncate(1280 * 1024 * 1024)
    run("mkfs.ext4", "-F", "-q", "-b", "4096", "-g", "1024", "-N", "4096",
        "-O", "^metadata_csum,^orphan_file,^flex_bg,^resize_inode",
        "-E", "lazy_itable_init=0,lazy_journal_init=0", str(image))
    empty = OUTPUT / "empty"
    empty.touch()
    # Reserve most data blocks, then free alternating logical blocks. The
    # remaining tail can satisfy a 256-block write; the preceding groups have
    # enough free blocks in total but no contiguous run of that length. More
    # than 128 searched groups force bitmap eviction, and 305 exceed the
    # preallocation handle's bounded credit budget in the faulty implementation.
    allocated = 305 * 1024
    commands = OUTPUT / "fragment.debugfs"
    with commands.open("w") as stream:
        stream.write("write {} /fragment\n".format(empty))
        stream.write("fallocate /fragment 0 {}\n".format(allocated - 1))
        for block in range(1, allocated, 2):
            stream.write("punch /fragment {} {}\n".format(block, block))
    log_path = OUTPUT / "prepare.log"
    with log_path.open("w") as log:
        result = subprocess.run(["debugfs", "-w", "-f", str(commands), str(image)],
                                stdout=log, stderr=log)
    if result.returncode:
        print(log_path.read_text(), end="", flush=True)
        result.check_returncode()
    run("e2fsck", "-fn", str(image))
    check_fragmentation(image)
    with root.open("r+b") as stream:
        stream.truncate(1200 * 1024 * 1024)
    run("resize2fs", str(root))
    run("debugfs", "-w", "-R", "write {} /fixture.img".format(image), str(root))
    run("e2fsck", "-fn", str(root))
    print("ext4_fragmented_fixture=ready output=" + str(OUTPUT), flush=True)


if __name__ == "__main__":
    prepare(Path(sys.argv[1]).resolve(strict=True))
