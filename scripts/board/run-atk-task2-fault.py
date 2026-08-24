#!/usr/bin/env python3
"""Verify one T2N1 ERROR notification and recovery on the physical RK3588 board."""

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
FATAL_PATTERNS = (rb"ESR_EL2=", rb"Unhandled acknowledged host IRQ", rb"(?i)\bpanic(?:ked)?\b")
FAULT_EVIDENCE = {
    "invalid-parameter": (
        rb"STARRY_T2N1_REMOTE_ERROR code=InvalidParameter sequence=1",
        rb"TASK2_PROTOCOL_ERROR invalid_parameter seq=1",
    ),
    "out-of-order": (
        rb"STARRY_T2N1_REMOTE_ERROR code=OutOfOrder sequence=2",
        rb"TASK2_PROTOCOL_ERROR out_of_order=2 expected=[0-9]+",
    ),
}


def main() -> int:
    args = parse_arguments()
    console: Console | None = None
    try:
        console = Console(args.port, args.baud, args.log)
        console.reach_host_prompt()
        console.attach(1)
        prepare_starry_guest(console)
        run_fault_scenario(console, args.mode)
        collect_zephyr_and_host_evidence(console, args.mode)
        console.drain(1)
        validate_evidence(args.log.read_bytes(), args.mode)
    except Exception as error:
        print(f"TASK2_FAULT_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        if console is not None:
            console.close()
    print(f"TASK2_FAULT_COMPLETE mode={args.mode} log={args.log}")
    return 0


class Console:
    def __init__(self, port: str, baud: int, log_path: Path) -> None:
        refuse_shared_console(port)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.serial = serial.Serial(port, baudrate=baud, timeout=0.1, exclusive=True)
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

    def raw(self, payload: bytes) -> None:
        self.serial.write(payload)
        self.serial.flush()

    def line(self, command: str) -> None:
        for byte in command.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)

    def command(self, command: str, prompt: bytes, timeout: float = 30) -> None:
        self.clear()
        self.line(command)
        self.expect(prompt, timeout)

    def reach_host_prompt(self) -> None:
        self.clear()
        self.raw(b"\r")
        try:
            self.expect(HOST_PROMPT, 3)
            return
        except TimeoutError:
            self.clear()
        self.raw(b"\x18h")
        self.expect(HOST_PROMPT, 10)

    def attach(self, vm_id: int) -> None:
        self.command(
            f"vm console {vm_id}",
            rb"Attached VM\[[12]\] console|root@starry|TASK2_",
            10,
        )

    def detach(self) -> None:
        self.clear()
        self.raw(b"\x18h")
        self.expect(HOST_PROMPT, 10)


def prepare_starry_guest(console: Console) -> None:
    console.clear()
    console.raw(b"\x03\r")
    console.expect(GUEST_PROMPT, 30)
    console.command(
        "mkdir -p /tmp/t123; cd /tmp/t123; "
        "gzip -dc /proc/initrd | cpio -id; "
        "mount --bind /tmp/t123/usr/share /usr/share 2>/dev/null || true; "
        "ip addr add 10.0.42.15/24 dev eth0 2>/dev/null || true; "
        "echo TASK2_FAULT_SETUP_DONE",
        GUEST_PROMPT,
        120,
    )


def run_fault_scenario(console: Console, mode: str) -> None:
    remote_error, _ = FAULT_EVIDENCE[mode]
    console.clear()
    console.line(f"/tmp/t123/bin/task2-net {mode}")
    console.expect(rb"STARRY_T2N1_FAULT_SENT mode=" + mode.encode(), 30)
    console.expect(remote_error, 30)
    console.expect(
        rb"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode="
        + mode.encode()
        + rb" [^\r\n]*safe_observed=true recovered=true",
        60,
    )
    console.clear()
    console.raw(b"\x03")
    console.expect(GUEST_PROMPT, 30)


def collect_zephyr_and_host_evidence(console: Console, mode: str) -> None:
    _, zephyr_error = FAULT_EVIDENCE[mode]
    console.detach()
    console.attach(2)
    console.expect(zephyr_error, 30)
    console.detach()
    console.command("vm list", HOST_PROMPT)


def validate_evidence(log: bytes, mode: str) -> None:
    for pattern in FATAL_PATTERNS:
        if re.search(pattern, log):
            raise RuntimeError(f"fatal marker found: {pattern!r}")
    remote_error, zephyr_error = FAULT_EVIDENCE[mode]
    required = (
        rb"STARRY_T2N1_FAULT_SENT mode=" + mode.encode(),
        remote_error,
        rb"STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode="
        + mode.encode()
        + rb" [^\r\n]*safe_observed=true recovered=true",
        zephyr_error,
        rb"atk-task123-starry running",
        rb"atk-task123-zephyr running",
    )
    missing = [pattern for pattern in required if re.search(pattern, log) is None]
    if missing:
        raise RuntimeError(f"missing evidence marker: {missing[0]!r}")
    fault_position = re.search(required[0], log).start()  # type: ignore[union-attr]
    error_position = re.search(remote_error, log).start()  # type: ignore[union-attr]
    recovery_position = re.search(required[2], log).start()  # type: ignore[union-attr]
    if not fault_position < error_position < recovery_position:
        raise RuntimeError("StarryOS fault, ERROR, and recovery markers are out of order")


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
    parser.add_argument("mode", choices=tuple(FAULT_EVIDENCE))
    parser.add_argument("log", type=Path)
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--baud", type=int, default=1_500_000)
    args = parser.parse_args()
    if args.baud <= 0:
        parser.error("baud must be positive")
    return args


if __name__ == "__main__":
    raise SystemExit(main())
