#!/usr/bin/env python3
"""Run isolated LoongArch EFI resource, HTTP, and FAT OTA regressions."""

import argparse
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[3]
AXLOADER = Path("bootloader/axloader")
BUILD = AXLOADER / "build-loongarch64-unknown-none-softfloat.toml"
ARTIFACTS = WORKSPACE / "target/loongarch64-unknown-linux-musl/release"
IMAGE = ARTIFACTS / "axloader.bin"
ELF = ARTIFACTS / "arceos-helloworld"
INITRAMFS = WORKSPACE / "target/axbuild/host-initramfs/loong-host.cpio"


def run(*args, **kwargs):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=WORKSPACE, check=True, **kwargs)


def build():
    # The resource-only application shares the helloworld output path. Run it
    # before building the independent payload uploaded by the HTTP regression.
    run("cargo", "xtask", "arceos", "qemu", "--package", "arceos-helloworld",
        "--config", "apps/arceos/helloworld/build-loongarch64-boot-resources.toml",
        "--qemu-config", "apps/arceos/helloworld/qemu-loongarch64-boot-resources.toml")
    run("cargo", "xtask", "image", "pack-initramfs", "test-suit/host-initramfs", INITRAMFS)
    run("cargo", "xtask", "arceos", "build", "--package", "arceos-helloworld",
        "--config", AXLOADER / "build-loongarch64-helloworld-efi.toml")
    run("cargo", "xtask", "arceos", "build", "--package", "axloader", "--config", BUILD)


def create_fat(path, storage_full):
    with path.open("xb") as image:
        image.truncate(64 * 1024 * 1024)
    run("mkfs.fat", "-F", "32", path, stdout=subprocess.DEVNULL)
    if not storage_full:
        return
    # Seed the verified stable image, then leave space for the two state
    # records but less than one valid inactive-slot image.
    run("mmd", "-i", path, "::/axloader")
    run("mcopy", "-i", path, IMAGE, "::/axloader/A.EFI")
    listing = run("mdir", "-i", path, "::/", capture_output=True, text=True,
                  env={**os.environ, "LC_ALL": "C"}).stdout
    match = re.search(r"([0-9][0-9 ]*) bytes free", listing)
    if match is None:
        raise RuntimeError(f"cannot determine FAT free space: {listing}")
    free = int(match[1].replace(" ", ""))
    leave = max(128 * 1024, IMAGE.stat().st_size // 2)
    if not leave < IMAGE.stat().st_size < free:
        raise RuntimeError("FAT fixture cannot fit its stable image and failure boundary")
    filler = path.with_suffix(".filler")
    with filler.open("xb") as stream:
        stream.truncate(free - leave)
    run("mcopy", "-i", path, filler, "::/FILLER.BIN")
    filler.unlink()


def configuration(directory, fat, port):
    template = (WORKSPACE / AXLOADER / "qemu-loongarch64-http.toml").read_text()
    qmp = directory / "qmp.sock"
    if len(str(qmp).encode()) >= 100:
        raise RuntimeError("use a shorter output directory for the QMP Unix socket")
    replacements = {
        '"id=disk0,if=none,format=raw,file=${workspace}/target/axbuild/rootfs/arceos-loongarch64-fat32.img"':
            json.dumps(f"id=disk0,if=none,format=raw,file={fat}"),
        '"unix:${workspace}/target/axbuild/loong-axloader-qmp.sock,server=on,wait=off"':
            json.dumps(f"unix:{qmp},server=on,wait=off"),
        "hostfwd=tcp::3999-:2999": f"hostfwd=tcp:127.0.0.1:{port}-:2999",
    }
    for old, new in replacements.items():
        if template.count(old) != 1:
            raise RuntimeError(f"QEMU template must contain exactly one {old}")
        template = template.replace(old, new)
    config = directory / "qemu.toml"
    config.write_text(template)
    return config, qmp


def stop(process, group=False):
    if process is None:
        return
    try:
        if group:
            os.killpg(process.pid, signal.SIGTERM)
        elif process.poll() is None:
            process.terminate()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        if group:
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
        process.wait()


def check_log(log, qemu, markers):
    deadline = time.monotonic() + 90
    while True:
        text = log.read_text(errors="replace")
        if re.search(r"\bpanic(?:ked)?\b", text, re.IGNORECASE):
            raise RuntimeError("LoongArch EFI kernel panicked")
        if all(marker in text for marker in markers):
            return
        if qemu.poll() is not None:
            raise RuntimeError(f"QEMU exited before payload execution: {qemu.returncode}")
        if time.monotonic() >= deadline:
            raise RuntimeError(f"missing LoongArch payload markers: {markers}")
        time.sleep(0.25)


def run_case(root, storage_full):
    name = "full-fat" if storage_full else "http-rollback"
    directory = root / name
    directory.mkdir()
    fat = directory / "fat.img"
    create_fat(fat, storage_full)
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    config, qmp = configuration(directory, fat, port)
    log = directory / "qemu.log"
    qemu = None
    client = None
    try:
        with log.open("w") as output:
            qemu = subprocess.Popen(
                ["cargo", "xtask", "arceos", "qemu", "--package", "axloader",
                 "--config", str(BUILD), "--qemu-config", str(config)],
                cwd=WORKSPACE, stdout=output, stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            args = [sys.executable, "-u", str(AXLOADER / "scripts/loong-http-smoke.py"),
                    "--port", str(port), "--image", str(IMAGE), "--elf-image", str(ELF),
                    "--initramfs", str(INITRAMFS), "--qmp-socket", str(qmp)]
            if storage_full:
                args.append("--storage-full")
            client = subprocess.Popen(args, cwd=WORKSPACE)
            while client.poll() is None:
                if qemu.poll() is not None:
                    raise RuntimeError(f"QEMU exited during {name}: {qemu.returncode}")
                time.sleep(0.25)
            if client.returncode:
                raise RuntimeError(f"LoongArch HTTP regression failed: {name}")
            markers = ["network_ready: arch=loongarch64"] if storage_full else [
                "elf_loaded: load=0x200000", "ready_to_handoff",
                "HOST_CMDLINE: axloader-positive=1", "HOST_INITRAMFS_PASSED", "Hello, world!",
            ]
            check_log(log, qemu, markers)
        print(f"LoongArch EFI {name}: passed", flush=True)
    except BaseException:
        if log.exists():
            print(f"==> {log}\n" + "\n".join(log.read_text(errors="replace").splitlines()[-200:]),
                  file=sys.stderr, flush=True)
        raise
    finally:
        stop(client)
        stop(qemu, group=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path,
                        help="retain a fresh run directory here, including FAT images and logs")
    args = parser.parse_args()
    build()
    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="run-", dir=args.output_dir)).resolve()
        print(f"LoongArch EFI evidence: {root}", flush=True)
        run_case(root, storage_full=True)
        run_case(root, storage_full=False)
    else:
        with tempfile.TemporaryDirectory(prefix="loong-efi-") as temporary:
            root = Path(temporary)
            run_case(root, storage_full=True)
            run_case(root, storage_full=False)


if __name__ == "__main__":
    main()
