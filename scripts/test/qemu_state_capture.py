#!/usr/bin/env python3
"""Keep QMP state for a CI emulator without owning its lifetime or console."""

from collections import deque
import ctypes
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sys
import tempfile
import time


class Monitor:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(2)
        try:
            self.socket.connect(str(path))
            self.stream = self.socket.makefile("rwb", buffering=0)
            self.stream.readline()
            self.sequence = 0
            self.call("qmp_capabilities")
        except BaseException:
            self.socket.close()
            raise

    def call(self, operation, arguments=None):
        self.sequence += 1
        request = {"execute": operation, "id": self.sequence}
        if arguments is not None:
            request["arguments"] = arguments
        self.stream.write(json.dumps(request).encode() + b"\n")
        while True:
            raw = self.stream.readline()
            if not raw:
                raise EOFError("QMP closed")
            result = json.loads(raw)
            if result.get("id") == self.sequence:
                return result

    def close(self):
        self.stream.close()
        self.socket.close()


def collect(socket_path, destination, parent):
    def stop(_signal, _frame):
        raise SystemExit(0)

    signal.signal(signal.SIGTERM, stop)
    # The observer never keeps a terminal pipe open after QEMU exits.
    null = os.open(os.devnull, os.O_RDWR)
    for descriptor in (0, 1, 2):
        os.dup2(null, descriptor)
    if null > 2:
        os.close(null)
    monitor = None
    try:
        # This Linux-only CI observer belongs to the exact exec'd QEMU PID.
        # Arm before checking PPID to cover death between fork and prctl.
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_PDEATHSIG failed")
        if os.getppid() != parent:
            return
        while os.getppid() == parent:
            try:
                if monitor is None:
                    monitor = Monitor(socket_path)
                cpus = monitor.call("query-cpus-fast")
                sample = {
                    "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "status": monitor.call("query-status"),
                    "cpus": cpus,
                    "registers": [],
                }
                for cpu in cpus.get("return", []):
                    sample["registers"].append(monitor.call("human-monitor-command", {
                        "command-line": "info registers", "cpu-index": cpu["cpu-index"],
                    }))
                for name in ("irq", "pic"):
                    sample[name] = monitor.call("human-monitor-command", {
                        "command-line": "info " + name,
                    })
                with (destination / "state.jsonl").open("a") as output:
                    output.write(json.dumps(sample) + "\n")
                time.sleep(10)
            except (ConnectionError, FileNotFoundError, EOFError, TimeoutError):
                if monitor is not None:
                    monitor.close()
                    monitor = None
                time.sleep(0.2)
    except SystemExit:
        pass
    except Exception as error:
        (destination / "collector-error.txt").write_text(repr(error) + "\n")
    finally:
        if monitor is not None:
            monitor.close()
        shutil.rmtree(socket_path.parent)


def launch(arguments):
    executable = os.environ["TGOS_QEMU_CAPTURE_REAL"]
    # axbuild may query the executable before launching a machine.
    if "--version" in arguments or "-version" in arguments:
        os.execv(executable, [executable, *arguments])
    if sys.platform != "linux":
        raise RuntimeError("QEMU CI state capture requires Linux")
    root = Path(os.environ["TGOS_QEMU_CAPTURE_DIR"])
    root.mkdir(parents=True, exist_ok=True)
    destination = Path(tempfile.mkdtemp(prefix="qemu-", dir=root))
    socket_path = Path(tempfile.mkdtemp(prefix="tgos-qmp-")) / "monitor.sock"
    command = [executable, *arguments, "-qmp", "unix:" + str(socket_path) + ",server=on,wait=off"]
    (destination / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    parent = os.getpid()
    (destination / "qemu.pid").write_text(str(parent) + "\n")
    observer = os.fork()
    if observer == 0:
        collect(socket_path, destination, parent)
        os._exit(0)
    (destination / "observer.pid").write_text(str(observer) + "\n")
    try:
        # Keep the PID, stdin/stdout/stderr, exit status and kill target that
        # ostool owns. No proxy process can leave an orphaned emulator behind.
        os.execv(executable, command)
    except BaseException:
        os.kill(observer, signal.SIGTERM)
        os.waitpid(observer, 0)
        raise


def report(root):
    for destination in sorted(Path(root).glob("qemu-*")):
        print("QEMU_STATE_CAPTURE:", destination.name)
        for name in ("command.json", "collector-error.txt"):
            path = destination / name
            if path.exists():
                print(path.read_text().rstrip())
        path = destination / "state.jsonl"
        if not path.exists():
            print("No QMP sample was captured.")
            continue
        with path.open() as source:
            samples = deque(source, maxlen=3)
        for raw in samples:
            sample = json.loads(raw)
            print("QEMU_STATE_SAMPLE:", sample["utc"], sample["status"])
            print("CPUS:", sample["cpus"])
            for registers in sample["registers"]:
                print(registers.get("return", registers))
            for name in ("irq", "pic"):
                print(name.upper() + ":", sample[name].get("return", sample[name]))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--report":
        report(sys.argv[2])
    else:
        launch(sys.argv[1:])
