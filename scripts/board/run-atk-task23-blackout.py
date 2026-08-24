#!/usr/bin/env python3
"""Verify Task-2 blackout, Safe entry, and recovery on the physical board."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import time
from pathlib import Path

import serial


GUEST_PROMPT = rb"root@starry:[^\r\n]*#"
HOST_PROMPT = rb"axvisor:/\$"
FATAL_PATTERNS = (rb"ESR_EL2=", rb"(?i)\bpanic(?:ked)?\b")


class Console:
    def __init__(self, port: str, baud: int, log_path: Path) -> None:
        refuse_shared_console(port)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.serial = serial.Serial(port, baudrate=baud, timeout=0.1, exclusive=True)
        self.log_path = log_path
        self.log = log_path.open("wb")
        self.buffer = bytearray()

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def drain(self, seconds: float = 0.2) -> None:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            chunk = self.serial.read(65536)
            if not chunk:
                continue
            self.log.write(chunk)
            self.log.flush()
            self.buffer.extend(chunk)
            if len(self.buffer) > 2_000_000:
                del self.buffer[:-1_000_000]

    def expect(self, expression: bytes, timeout: float) -> None:
        pattern = re.compile(expression, re.DOTALL)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain()
            if pattern.search(self.buffer):
                return
        raise TimeoutError(f"timeout waiting for {expression!r}")

    def clear(self) -> None:
        self.buffer.clear()

    def raw(self, data: bytes) -> None:
        self.serial.write(data)
        self.serial.flush()

    def line(self, command: str) -> None:
        for byte in command.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)

    def command(self, command: str, prompt: bytes, timeout: float = 30) -> None:
        self.clear()
        self.line(command)
        self.expect(prompt, timeout)

    def detach(self) -> None:
        self.clear()
        self.raw(b"\x18h")
        self.expect(HOST_PROMPT, 10)

    def attach(self, vm_id: int) -> None:
        self.command(
            f"vm console {vm_id}",
            rb"Attached VM\[[12]\] console|root@starry|TASK2_",
            10,
        )


def main() -> int:
    args = parse_arguments()
    console: Console | None = None
    try:
        console = Console(args.port, args.baud, args.log)
        prepare_normal_loop(console)
        enter_blackout(console)
        leave_blackout_and_verify_recovery(console)
        collect_final_state(console)
        console.drain(1)
        validate_evidence(args.log.read_bytes())
    except Exception as error:
        print(f"TASK23_BLACKOUT_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        if console is not None:
            console.close()
    print(f"TASK23_BLACKOUT_COMPLETE log={args.log}")
    return 0


def prepare_normal_loop(console: Console) -> None:
    console.raw(b"\r")
    console.expect(GUEST_PROMPT, 120)
    console.command(
        "mkdir -p /tmp/t123; cd /tmp/t123; "
        "gzip -dc /proc/initrd | cpio -id; "
        "mount --bind /tmp/t123/usr/share /usr/share; "
        "ip addr add 10.0.42.15/24 dev eth0 2>/dev/null || true; "
        "echo TASK23_BLACKOUT_SETUP_DONE",
        GUEST_PROMPT,
        120,
    )
    console.clear()
    console.line("/tmp/t123/bin/task2-net normal")
    console.expect(rb"STARRY_T2N1_STATUS_DELIVERED", 180)


def enter_blackout(console: Console) -> None:
    console.detach()
    console.command("virtnet drop on", HOST_PROMPT)
    console.attach(1)
    console.expect(rb"STARRY_T2N1_SAFE source=protocol", 30)
    console.detach()
    console.attach(2)
    console.expect(rb"TASK2_SAFE state=Safe event=(HeartbeatTimeout|RetryExhausted)", 30)


def leave_blackout_and_verify_recovery(console: Console) -> None:
    console.detach()
    console.command("virtnet drop off", HOST_PROMPT)
    console.attach(1)
    console.expect(rb"STARRY_T2N1_RECOVERED state=Active", 30)
    console.expect(rb"STARRY_T2N1_STATUS_DELIVERED", 180)
    console.detach()
    console.attach(2)
    console.expect(rb"TASK2_CONTROL_RECEIVED", 30)
    console.expect(rb"TASK2_STATUS_SENT", 30)


def collect_final_state(console: Console) -> None:
    console.detach()
    console.command("virtnet show", HOST_PROMPT)
    console.command("vm list", HOST_PROMPT)


def validate_evidence(log: bytes) -> None:
    for pattern in FATAL_PATTERNS:
        if re.search(pattern, log):
            raise RuntimeError(f"fatal marker found: {pattern!r}")
    markers = (
        b"STARRY_T2N1_STATUS_DELIVERED",
        b"virtnet: blackout ON",
        b"STARRY_T2N1_SAFE source=protocol",
        b"TASK2_SAFE state=Safe event=",
        b"virtnet: blackout OFF",
        b"STARRY_T2N1_RECOVERED state=Active",
        b"TASK2_CONTROL_RECEIVED",
        b"TASK2_STATUS_SENT",
        b"virtnet switch: blackout=off",
        b"atk-task123-starry running",
        b"atk-task123-zephyr running",
    )
    positions = []
    cursor = 0
    for marker in markers:
        position = log.find(marker, cursor)
        if position < 0:
            raise RuntimeError(f"missing evidence marker: {marker!r}")
        positions.append(position)
        cursor = position + len(marker)
    if positions != sorted(positions):
        raise RuntimeError("blackout evidence markers are out of order")


def refuse_shared_console(port: str) -> None:
    result = subprocess.run(
        ["sudo", "-n", "fuser", port], check=False, capture_output=True, text=True
    )
    holders = f"{result.stdout} {result.stderr}".strip()
    if result.returncode == 0 and holders:
        raise RuntimeError(f"serial port {port} is already open by {holders}")
    if result.returncode not in {0, 1}:
        raise RuntimeError(f"could not verify exclusive ownership of {port}: {holders}")


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--baud", type=int, default=1_500_000)
    args = parser.parse_args()
    if args.baud <= 0:
        parser.error("baud must be positive")
    return args


if __name__ == "__main__":
    raise SystemExit(main())
