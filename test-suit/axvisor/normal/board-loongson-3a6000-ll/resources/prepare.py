#!/usr/bin/env python3
"""Prepare the pinned Linux ELF and an independent ext4 board-test fixture."""

import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import struct
import subprocess
import sys
import uuid


COMMIT = "8cd9520d35a6c38db6567e97dd93b1f11f185dc6"
FIXTURE = b"AXVISOR_LOONGSON_BLOCK_FIXTURE_V1\n"
FIXTURE_NAME = "axvisor-board-fixture.txt"
IMAGE_BYTES = 16 * 1024 * 1024
RESOURCES = Path(__file__).resolve().parent
WORKSPACE = RESOURCES.parents[4]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(argv, *, env=None, capture=False):
    argv = [str(item) for item in argv]
    print("+ " + shlex.join(argv), flush=True)
    result = subprocess.run(argv, env=env, check=True, stdout=subprocess.PIPE if capture else None)
    return result.stdout if capture else None


def source_identity(source):
    def git(*args):
        return run(["git", "-C", source, *args], capture=True).decode().strip()

    require(git("rev-parse", "HEAD") == COMMIT, "only the pinned Linux 7.1 commit is supported")
    require(not git("status", "--porcelain", "--untracked-files=no"), "Linux tracked files are modified")
    # Documentation-only untracked files cannot affect this kernel build.
    untracked = run(
        ["git", "-C", source, "ls-files", "--others", "--exclude-standard", "-z"],
        capture=True,
    ).decode().split("\0")
    require(
        all(not name or name.startswith("Documentation/") for name in untracked),
        "Linux has untracked build inputs outside Documentation/",
    )
    return {"commit": COMMIT, "tree": git("rev-parse", "HEAD^{tree}"), "version": "7.1.0"}


def tool_identities(env):
    names = [
        "git", "tar", "make", "clang-20", "clang++-20", "ld.lld-20", "llvm-ar-20", "llvm-nm-20",
        "llvm-objcopy-20", "llvm-objdump-20", "llvm-readelf-20", "llvm-strip-20",
        "flex", "bison", "bc", "mke2fs", "debugfs",
    ]
    tools = {}
    for name in names:
        path = shutil.which(name, path=env["PATH"])
        require(path, f"required tool not found: {name}")
        # Record binaries as well as version strings; wrappers also invalidate cache.
        result = subprocess.run(
            [path, "-V" if name in ("mke2fs", "debugfs") else "--version"],
            env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        tools[name] = {"sha256": sha256(Path(path)), "version": result.stdout.decode().strip()}
    require("LoongArch" in run(["clang-20", "--print-targets"], env=env, capture=True).decode(),
            "clang-20 does not support LoongArch")
    return tools


def export_source(source, output, env):
    # Kbuild rejects O= when the user's original tree has ignored build files.
    # Export only the verified commit, preserving archive timestamps. Never
    # clean, reconfigure, or create a Git worktree in the user's source.
    exported = output / "linux-source"
    require(not exported.is_symlink(), "Linux source export must not be a symlink")
    if exported.exists():
        shutil.rmtree(exported)
    exported.mkdir()
    print(f"+ git archive {COMMIT} | tar -x -C {exported}", flush=True)
    with subprocess.Popen(["git", "-C", source, "archive", COMMIT], env=env,
                          stdout=subprocess.PIPE) as archive:
        try:
            subprocess.run(["tar", "-x", "-C", exported], env=env, stdin=archive.stdout, check=True)
        finally:
            archive.stdout.close()
        require(archive.wait() == 0, "git archive failed")
    return exported


def configure(source, build, env):
    make = ["make", "-C", source, f"O={build}", "ARCH=loongarch", "LLVM=-20",
            "LLVM_IAS=1", "HOSTCC=clang-20", "HOSTCXX=clang++-20"]
    run([*make, "loongson64_defconfig"], env=env)
    settings = {}
    for line in (RESOURCES / "linux.fragment").read_text().splitlines():
        if line.startswith("CONFIG_"):
            key, value = line.split("=", 1)
            settings[key] = value
        elif re.fullmatch(r"# CONFIG_\w+ is not set", line):
            settings[line.split()[1]] = "n"
    args = [source / "scripts/config", "--file", build / ".config"]
    for key, value in settings.items():
        if value in ("y", "n"):
            args.extend(["--enable" if value == "y" else "--disable", key])
        else:
            require(value == '""', f"unsupported fragment value: {key}={value}")
            args.extend(["--set-str", key, ""])
    run(args, env=env)
    run([*make, "olddefconfig"], env=env)
    config = (build / ".config").read_text()
    effective = dict(line.split("=", 1) for line in config.splitlines()
                     if line.startswith("CONFIG_"))
    for key, value in settings.items():
        require(effective.get(key, "n") == value,
                f"effective configuration did not retain {key}={value}")
    return make


def elf_layout(path):
    with path.open("rb") as stream:
        header = stream.read(64)
        require(header[:7] == b"\x7fELF\x02\x01\x01", "kernel must be ELF64 little-endian")
        fields = struct.unpack("<16sHHIQQQIHHHHHH", header)
        require(fields[1] == 2 and fields[2] == 258, "kernel must be LoongArch ET_EXEC")
        entry, phoff, phsize, phnum = fields[4], fields[5], fields[9], fields[10]
        require(phsize == 56, "unexpected ELF program header size")
        segments = []
        for index in range(phnum):
            stream.seek(phoff + index * phsize)
            kind, flags, offset, va, pa, filesz, memsz, align = struct.unpack("<IIQQQQQQ", stream.read(56))
            if kind != 1:
                continue
            if pa >> 48 == 0x9000:
                pa &= 0x0000FFFFFFFFFFFF
            require(filesz <= memsz and offset + filesz <= path.stat().st_size, "invalid ELF segment")
            end = pa + memsz
            require(any(start <= pa < end <= limit for start, limit in
                        [(0, 0x10000000), (0x80000000, 0xB0000000)]), "kernel segment outside guest RAM")
            # Reserve the FDT-to-kernel gap, boot information, and the entire
            # upper half of low RAM for the separately prepared initramfs.
            for start, limit in [(0x100000, 0x200000), (0x80000000, 0x80100000),
                                 (0x08000000, 0x10000000)]:
                require(end <= start or pa >= limit, "kernel segment overlaps a reserved boot resource")
            require(all(end <= item["physical_start"] or pa >= item["physical_end"]
                        for item in segments), "overlapping kernel segments")
            segments.append({"physical_start": pa, "physical_end": end, "virtual_start": va,
                             "file_size": filesz, "memory_size": memsz, "flags": flags})
        require(segments and any(item["virtual_start"] <= entry <
                item["virtual_start"] + item["memory_size"] and item["flags"] & 1
                for item in segments), "entry is outside executable kernel segments")
    return {"entry": entry, "load_segments": segments}


def prepare_block(output, env, input_hash):
    env = dict(env, MKE2FS_CONFIG=str(RESOURCES / "mke2fs.conf"))
    staging = output / "fixture-root"
    staging.mkdir(exist_ok=True)
    fixture = staging / FIXTURE_NAME
    fixture.write_bytes(FIXTURE)
    fixture.chmod(0o644)
    staging.chmod(0o755)
    epoch = int(env["SOURCE_DATE_EPOCH"])
    os.utime(fixture, (epoch, epoch))
    os.utime(staging, (epoch, epoch))
    image = output / "block.img"
    with image.open("wb") as stream:
        stream.truncate(IMAGE_BYTES)
    fixed_uuid = str(uuid.uuid5(uuid.NAMESPACE_URL, input_hash))
    run(["mke2fs", "-q", "-t", "ext4", "-b", "1024", "-I", "256", "-m", "0",
         "-U", fixed_uuid, "-L", "AXVISOR_FIXTURE",
         "-O", "has_journal,extent,filetype,64bit,flex_bg,sparse_super,large_file,"
               "huge_file,dir_nlink,extra_isize,metadata_csum,dir_index",
         "-E", f"hash_seed={fixed_uuid},lazy_itable_init=0,lazy_journal_init=0,root_owner=0:0",
         "-d", staging, image], env=env)
    # mke2fs -d copies host ownership and ctime; normalize these explicitly.
    for inode in ("/", "/lost+found", "/" + FIXTURE_NAME):
        for field, value in [("uid", "0"), ("gid", "0"), ("atime", f"@{epoch}"),
                             ("ctime", f"@{epoch}"), ("mtime", f"@{epoch}"), ("crtime", f"@{epoch}")]:
            run(["debugfs", "-w", "-R", f"set_inode_field {inode} {field} {value}", image], env=env)
    require(run(["debugfs", "-R", f"cat /{FIXTURE_NAME}", image], env=env, capture=True) == FIXTURE,
            "ext4 fixture content mismatch")
    fixture.unlink()
    staging.rmdir()


def prepare(args, output):
    source_value = os.environ.get("AXVISOR_LINUX_SOURCE")
    require(source_value, "AXVISOR_LINUX_SOURCE must point to the pinned Linux 7.1 source")
    source = Path(source_value).resolve(strict=True)
    require(not output.is_relative_to(source), "output directory must be outside Linux source")
    identity = source_identity(source)
    epoch = os.environ.get("SOURCE_DATE_EPOCH") or run(
        ["git", "-C", source, "show", "-s", "--format=%ct", COMMIT], capture=True
    ).decode().strip()
    require(epoch.isdigit() and 0 <= int(epoch) <= 0x7FFFFFFF, "invalid SOURCE_DATE_EPOCH")
    # Do not let ambient Kbuild flags, local compiler overrides, or mke2fs.conf
    # silently change the recipe. User-specific paths never enter linux.config.
    env = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/"),
           "LANG": "C", "LC_ALL": "C", "TZ": "UTC", "SOURCE_DATE_EPOCH": epoch,
           "KBUILD_BUILD_USER": "axvisor", "KBUILD_BUILD_HOST": "reproducible",
           "KBUILD_BUILD_VERSION": "1",
           "KBUILD_BUILD_TIMESTAMP": datetime.datetime.fromtimestamp(
               int(epoch), datetime.timezone.utc).strftime("%a %b %d %H:%M:%S UTC %Y"),
           "KCONFIG_NOTIMESTAMP": "1",
           "E2FSPROGS_FAKE_TIME": epoch}
    tools = tool_identities(env)
    inputs = {"linux": identity, "tools": tools, "source_date_epoch": int(epoch),
              "source_materialization": "git archive",
              "defconfig": "loongson64_defconfig", "llvm": "-20", "llvm_ias": 1,
              "prepare_sha256": sha256(Path(__file__).resolve()),
              "fragment_sha256": sha256(RESOURCES / "linux.fragment"),
              "mke2fs_config_sha256": sha256(RESOURCES / "mke2fs.conf"),
              "fixture_sha256": hashlib.sha256(FIXTURE).hexdigest(), "block_bytes": IMAGE_BYTES}
    manifest_path = output / "manifest.json"
    previous = None
    if manifest_path.is_file():
        previous = json.loads(manifest_path.read_text())
    if previous and (output / "linux.config").is_file():
        # An unchanged pinned source, recipe, tools, and published config already
        # identify the effective configuration; a hit need not re-export Linux.
        inputs["config_sha256"] = sha256(output / "linux.config")
        input_hash = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
        if (previous.get("input_sha256") == input_hash and all(
            (output / name).is_file() and sha256(output / name) == previous.get("artifacts", {}).get(name)
            for name in ("vmlinux", "linux.config", "block.img")
        )):
            print(f"cache hit: {input_hash}", flush=True)
            return
    print("cache miss: inputs or published artifacts changed", flush=True)
    manifest_path.unlink(missing_ok=True)
    exported = export_source(source, output, env)
    build = output / "linux-build"
    tools_stamp = output / "linux-build-tools.json"
    old_tools = (json.loads(tools_stamp.read_text()) if tools_stamp.is_file()
                 else previous.get("inputs", {}).get("tools") if previous else None)
    # Kbuild tracks command lines, but not replacement tool binaries at the same
    # path. Discard incremental objects when tool identities cannot be reused.
    if build.exists() and old_tools != tools:
        require(not build.is_symlink(), "Linux build directory must not be a symlink")
        shutil.rmtree(build)
    build.mkdir(exist_ok=True)
    tools_stamp.write_text(json.dumps(tools, sort_keys=True) + "\n")
    make = configure(exported, build, env)
    inputs["config_sha256"] = sha256(build / ".config")
    input_hash = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
    print(f"cache miss: {input_hash}; building Linux with -j{args.jobs}", flush=True)
    run([*make, f"-j{args.jobs}", "vmlinux"], env=env)
    require(source_identity(source) == identity, "Linux source changed during build")
    require(sha256(build / ".config") == inputs["config_sha256"], "configuration changed during build")
    layout = elf_layout(build / "vmlinux")
    shutil.copyfile(build / "vmlinux", output / "vmlinux")
    shutil.copyfile(build / ".config", output / "linux.config")
    prepare_block(output, env, input_hash)
    manifest = {"schema": 1, "input_sha256": input_hash, "inputs": inputs, "elf": layout,
                "artifacts": {name: sha256(output / name)
                              for name in ("vmlinux", "linux.config", "block.img")}}
    temporary = output / "manifest.json.tmp"
    temporary.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    temporary.replace(manifest_path)
    print(json.dumps(manifest["artifacts"], indent=2), flush=True)
    print(f"prepared resources: {output}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()
    require(args.jobs > 0, "--jobs must be positive")
    # host_prepare supplies the actual Cargo target ROOT, not target/axbuild.
    target_value = os.environ.get("AXBUILD_TARGET_DIR")
    require(target_value is None or bool(target_value) and Path(target_value).is_absolute(),
            "AXBUILD_TARGET_DIR must be an absolute target root")
    target = Path(target_value if target_value is not None else WORKSPACE / "target").resolve()
    require((WORKSPACE / "Cargo.toml").is_file(), "cannot locate script workspace root")
    output = target / "axbuild/loongson-3a6000"
    output.mkdir(parents=True, exist_ok=True)
    with (output / ".prepare.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        prepare(args, output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Linux resource preparation failed: {error}", file=sys.stderr, flush=True)
        sys.exit(1)
