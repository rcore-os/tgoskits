#!/usr/bin/env python3
"""Verify OpenRC persistence and natural QEMU power transitions through xtask."""
import argparse
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import socket
import subprocess
import tempfile
import time
try:
    import tomllib
except ModuleNotFoundError:
    import toml as tomllib

WORKSPACE = Path(__file__).resolve().parents[2]


def connect(path, process, deadline):
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("xtask exited before QEMU exposed its socket")
        connection = socket.socket(socket.AF_UNIX)
        try:
            connection.connect(str(path))
            return connection
        except (FileNotFoundError, ConnectionRefusedError):
            connection.close()
            time.sleep(0.05)
    raise TimeoutError(f"QEMU socket was not ready: {path}")


def write_guest_file(image, directory, guest_path, contents):
    source = directory / "injected-script"
    source.write_text(contents)
    commands = directory / "inject.commands"
    commands.write_text(f'rm {guest_path}\nwrite "{source}" {guest_path}\n'
                        f'set_inode_field {guest_path} mode 0100755\n')
    subprocess.run(["debugfs", "-w", "-f", str(commands), str(image)],
                   check=True, capture_output=True)
    actual = subprocess.run(["debugfs", "-R", f"cat {guest_path}", str(image)],
                            check=True, capture_output=True, text=True)
    if actual.stdout != contents:
        raise RuntimeError(f"failed to inject {guest_path}: {actual.stderr}")


def boot(arch, image, directory, second_boot):
    if second_boot:
        script = "set -eu; rc-service openrc-test-daemon status; test -L /etc/runlevels/default/openrc-test-daemon; test -f /root/openrc-persisted\n"
    else:
        script = (WORKSPACE / "test-suit/starryos/qemu/openrc/sh/openrc-test.sh").read_text()
        script += "touch /root/openrc-persisted\nsync\n"
    script += ('test "$(cat /run/openrc-test-autorun-count)" = autorun\n'
               'test "$(cat /run/openrc-test-visual-count)" = visual\n'
               'echo STARRY_OPENRC_READY\n')
    write_guest_file(image, directory, "/openrc-power-test.sh", script)
    serial_path = directory / "serial.sock"
    qmp_path = directory / "qmp.sock"
    for path in (serial_path, qmp_path):
        path.unlink(missing_ok=True)
    config = tomllib.loads((WORKSPACE / f"os/StarryOS/configs/qemu/qemu-{arch}.toml").read_text())
    args = [arg for arg in config["args"] if arg != "-nographic"]
    args += ["-display", "none", "-monitor", "none", "-no-reboot", "-serial",
             f"unix:{serial_path},server=on,wait=off", "-qmp",
             f"unix:{qmp_path},server=on,wait=off"]
    config_path = directory / "qemu.toml"
    config_path.write_text(f"args = {json.dumps(args)}\nuefi = {str(config.get('uefi', False)).lower()}\n"
                           f"to_bin = {str(config.get('to_bin', False)).lower()}\n"
                           "fail_regex = ['(?i)panic']\ntimeout = 240\n")
    log_path = directory / ("reboot.log" if second_boot else "poweroff.log")
    with log_path.open("wb") as log:
        command = ["cargo", "xtask", "starry", "qemu", "--arch", arch,
                   "--rootfs", str(image), "--rootfs-write-policy", "persist",
                   "--qemu-config", str(config_path)]
        process = subprocess.Popen(command, cwd=WORKSPACE, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 900
            with connect(serial_path, process, deadline) as serial, connect(qmp_path, process, deadline) as qmp:
                serial.setblocking(False)
                qmp.sendall(b'{"execute":"qmp_capabilities"}\r\n')
                selector = selectors.DefaultSelector()
                selector.register(serial, selectors.EVENT_READ, "serial")
                selector.register(qmp, selectors.EVENT_READ, "qmp")
                serial_buffer = b""
                qmp_buffer = b""
                outgoing = b""
                next_send = 0.0
                started = False
                respawn_requested = False
                requested_power = False
                shutdown = False
                stopped = False
                deadline = time.monotonic() + 240
                while time.monotonic() < deadline and not (shutdown and stopped):
                    for key, _ in selector.select(0.005 if outgoing else 0.2):
                        data = key.fileobj.recv(65536)
                        if not data:
                            selector.unregister(key.fileobj)
                            continue
                        if key.data == "qmp":
                            qmp_buffer += data
                            while b"\n" in qmp_buffer:
                                line, qmp_buffer = qmp_buffer.split(b"\n", 1)
                                event = json.loads(line)
                                if requested_power and event.get("event") == "SHUTDOWN":
                                    reason = event.get("data", {}).get("reason")
                                    expected = "guest-reset" if second_boot else "guest-shutdown"
                                    if reason != expected:
                                        raise RuntimeError(f"unexpected QEMU shutdown: {event}")
                                    log.write(b"QMP_POWER_TRANSITION " + line + b"\n")
                                    shutdown = True
                        else:
                            log.write(data)
                            log.flush()
                            serial_buffer += data
                            if b"panic" in serial_buffer.lower() or b"STARRY_OPENRC_FAILED" in serial_buffer:
                                raise RuntimeError("guest reported failure")
                            if b"root@starry:/root # " in serial_buffer and not started and not outgoing:
                                if not respawn_requested:
                                    outgoing = b"exit\n"
                                    respawn_requested = True
                                else:
                                    outgoing = b"sh /openrc-power-test.sh\n"
                                    started = True
                                serial_buffer = b""
                            ready_at = serial_buffer.find(b"\nSTARRY_OPENRC_READY")
                            if (ready_at >= 0 and b"root@starry:/root # " in serial_buffer[ready_at:]
                                    and not requested_power):
                                outgoing = b"reboot\n" if second_boot else b"poweroff\n"
                                requested_power = True
                                serial_buffer = b""
                            if requested_power and b"STARRY_OPENRC_SERVICE_STOPPED" in serial_buffer:
                                stopped = True
                    # Drain output while pacing input: byte-sized UART echoes
                    # can fill socket buffers unless input and output progress together.
                    if outgoing and time.monotonic() >= next_send:
                        try:
                            sent = serial.send(outgoing[:1])
                            outgoing = outgoing[sent:]
                            next_send = time.monotonic() + 0.02
                        except BlockingIOError:
                            pass
                    if not selector.get_map():
                        break
                selector.close()
                if not shutdown or not stopped:
                    raise RuntimeError(f"incomplete shutdown: QMP={shutdown}, service stop={stopped}")
            if process.wait(timeout=30) != 0:
                raise RuntimeError("xtask did not complete successfully after natural QEMU exit")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pass
            # xtask may exit before a blocked QEMU descendant handles SIGTERM.
            # This private session belongs only to this invocation.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
    print(f"PASS {arch} {'reboot and persistence' if second_boot else 'poweroff'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=["x86_64", "aarch64", "riscv64", "loongarch64"])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    subprocess.run(["cargo", "xtask", "starry", "rootfs", "--arch", args.arch], cwd=WORKSPACE, check=True)
    # Respect the same extraction-directory override as axbuild.
    image_config = tomllib.loads((WORKSPACE / "tmp/axbuild/.image.toml").read_text())
    rootfs_dir = Path(os.environ.get("TGOS_IMAGE_EXTRACT_DIR", image_config["extract_dir"]))
    if not rootfs_dir.is_absolute():
        rootfs_dir = WORKSPACE / rootfs_dir
    source = rootfs_dir / f"rootfs-{args.arch}-alpine.img"
    with tempfile.TemporaryDirectory(prefix="openrc-boot-") as temporary:
        directory = Path(temporary)
        image = directory / "rootfs.img"
        subprocess.run(["cp", "--reflink=auto", "--sparse=always", str(source), str(image)], check=True)
        for guest_path, name in (("/usr/bin/starry-run-case-tests", "autorun"),
                                 ("/test_runner.sh", "visual")):
            write_guest_file(image, directory, guest_path,
                             '#!/bin/sh\nset -eu\ntest "$HOME" = /root\n'
                             f'echo {name} >> /run/openrc-test-{name}-count\n')
        try:
            for second_boot in (False, True):
                boot(args.arch, image, directory, second_boot)
                # Read the powered-off disk, not the guest cache: shutdown must
                # persist the service stop hook's final write before power loss.
                # ext4 may have durable committed metadata in its journal;
                # debugfs alone does not replay it before resolving paths.
                replay = subprocess.run(["e2fsck", "-p", "-E", "journal_only", str(image)],
                                        capture_output=True, text=True)
                if replay.returncode not in (0, 1):
                    raise RuntimeError(f"journal replay failed: {replay.stdout} {replay.stderr}")
                stops = subprocess.run(["debugfs", "-R", "cat /root/openrc-stops", str(image)],
                                       check=True, capture_output=True, text=True).stdout
                expected = 4 if second_boot else 3
                if stops.splitlines() != ["stopped"] * expected:
                    raise RuntimeError(f"shutdown writes were not persisted: {stops!r}")
                log_path = directory / ("reboot.log" if second_boot else "poweroff.log")
                with log_path.open("a") as log:
                    log.write(f"HOST_DURABLE_STOP_RECORDS={expected}\n")
        except Exception:
            subprocess.run(["cp", "--reflink=auto", "--sparse=always", str(image),
                            str(args.output / "failed-rootfs.img")], check=True)
            raise
        finally:
            for path in directory.glob("*.log"):
                shutil.copy2(path, args.output / path.name)


if __name__ == "__main__":
    main()
