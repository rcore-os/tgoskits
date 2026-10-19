#!/usr/bin/env python3
"""Capture an unchanged vCPU scenario on a CI runner, including stalled CPUs."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import threading
import time


def copy_output(process, log):
    with log.open("wb") as output:
        for chunk in iter(lambda: process.stdout.read(1024), b""):
            output.write(chunk)
            output.flush()
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()


def execute(command, log):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    copier = threading.Thread(target=copy_output, args=(process, log), daemon=True)
    copier.start()
    status = process.wait()
    copier.join()
    return status


def qmp_snapshot(socket_path, destination):
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(5)
    connection.connect(str(socket_path))
    stream = connection.makefile("rwb", buffering=0)
    replies = []

    def command(name, arguments=None):
        request = {"execute": name}
        if arguments is not None:
            request["arguments"] = arguments
        stream.write(json.dumps(request).encode() + b"\n")
        while True:
            line = stream.readline()
            if not line:
                raise RuntimeError("QMP disconnected")
            response = json.loads(line)
            replies.append(response)
            if "return" in response:
                return response["return"]
            if "error" in response:
                raise RuntimeError(response["error"])

    try:
        replies.append(json.loads(stream.readline()))
        command("qmp_capabilities")
        command("stop")
        cpus = command("query-cpus-fast")
        command("human-monitor-command", {"command-line": "info registers -a"})
        for cpu in cpus:
            command("human-monitor-command", {"command-line": f"cpu {cpu['cpu-index']}"})
            command("human-monitor-command", {"command-line": "x /12i $pc"})
            command("human-monitor-command", {"command-line": "x /64gx $sp"})
    finally:
        try:
            command("cont")
        except Exception as error:
            replies.append({"resume_error": str(error)})
        destination.write_text(json.dumps(replies, indent=2))
        stream.close()
        connection.close()


def preserve_images(output):
    root = Path("target/aarch64-unknown-linux-musl/release")
    identities = {}
    for path in [root / "axvisor", *root.glob("arceos-virtio-net-peer-vm*.*")]:
        if not path.is_file():
            continue
        identities[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        shutil.copy2(path, output / path.name)
    (output / "image-identities.json").write_text(json.dumps(identities, indent=2))


def run(output, prefix):
    output.mkdir(parents=True, exist_ok=True)
    facts = {
        "source_sha": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "uname": subprocess.check_output(["uname", "-srmo"], text=True).strip(),
        "qemu": subprocess.check_output(["qemu-system-aarch64", "--version"], text=True),
        "cpu_count": os.cpu_count(),
        "affinity": sorted(os.sched_getaffinity(0)),
        "load": os.getloadavg(),
        "cpu_quota": Path("/sys/fs/cgroup/cpu.max").read_text() if Path("/sys/fs/cgroup/cpu.max").exists() else None,
    }
    print(json.dumps(facts, indent=2), flush=True)
    (output / "facts.json").write_text(json.dumps(facts, indent=2))
    if prefix:
        status = execute(["cargo", "xtask", "axvisor", "test", "qemu", "--arch", "aarch64", "--test-case", "smoke,qemu-host-initramfs"], output / "smoke.log")
        if status:
            return status
        status = execute(["timeout", "240s", "apps/arceos/virtio-blk-test/run.sh"], output / "virtio-blk.log")
        if status:
            return status

    qmp = output / "qmp.sock"
    config = output / "qemu.toml"
    original = Path("os/axvisor/configs/qemu/qemu-aarch64-vcpus.toml").read_text()
    config.write_text(original.replace('  "-nographic",', '  "-nographic",\n  "-qmp", "unix:' + str(qmp) + ',server=on,wait=off",'))
    script = Path("apps/arceos/virtio-net-peer/run.sh").read_text()
    script = script.replace('workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)', "workspace=" + str(Path.cwd()))
    script = script.replace("qemu_config=os/axvisor/configs/qemu/qemu-aarch64-vcpus.toml", "qemu_config=" + str(config))
    script = script.replace('        "$output"\n}', '        "$output"\n    cp target/aarch64-unknown-linux-musl/release/arceos-virtio-net-peer "${output%.bin}.elf"\n}')
    runner = output / "run.sh"
    runner.write_text(script)
    log = output / "vcpus.log"
    process = subprocess.Popen(["bash", str(runner), "--vcpus"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    copier = threading.Thread(target=copy_output, args=(process, log), daemon=True)
    copier.start()
    ready = None
    last_snapshot = -1000
    index = 0
    while process.poll() is None:
        now = time.monotonic()
        if qmp.exists():
            if ready is None:
                ready = now
                preserve_images(output)
            silent = time.time() - log.stat().st_mtime if log.exists() else 0
            elapsed = now - ready
            if now - last_snapshot >= 45 and (silent >= 20 or (index == 0 and elapsed >= 45)):
                print(f"DIAGNOSTIC_SNAPSHOT {index} elapsed={elapsed:.3f} silent={silent:.3f}", flush=True)
                try:
                    qmp_snapshot(qmp, output / f"snapshot-{index}.json")
                except Exception as error:
                    (output / f"snapshot-{index}-error.txt").write_text(str(error))
                index += 1
                last_snapshot = time.monotonic()
        time.sleep(1)
    copier.join()
    (output / "result.json").write_text(json.dumps({"status": process.returncode, "snapshots": index}))
    return process.returncode


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--prefix", action="store_true")
    args = parser.parse_args()
    raise SystemExit(run(args.output.resolve(), args.prefix))
