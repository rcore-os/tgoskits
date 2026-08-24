#!/usr/bin/env python3
"""Capture one short dual-Guest Task 2/3 trace from the physical RK3588 board.

The board must already be running the unified AxVisor image. The runner refuses
to append to an existing hypervisor capture so evidence from separate boots
cannot be mixed. It captures a bounded live window, suspends both producers,
streams the buffer over UART, writes one classic pcap per Guest, and resumes the
Guests before returning.
"""

from __future__ import annotations

import argparse
import re
import struct
import subprocess
import sys
import time
from pathlib import Path

import serial


HOST_PROMPT = rb"axvisor:/\$"
DUMP_BEGIN = b"CAPDUMP_BEGIN"
DUMP_END = b"CAPDUMP_END"
PCAP_GLOBAL_HEADER = bytes.fromhex(
    "d4c3b2a1"  # magic, little-endian
    "02000400"  # version 2.4
    "00000000"  # thiszone
    "00000000"  # sigfigs
    "ffff0000"  # snaplen 65535
    "01000000"  # linktype Ethernet
)


def main() -> int:
    args = parse_arguments()
    console: Console | None = None
    suspended: list[int] = []
    capture_enabled = False
    try:
        console = Console(args.port, args.baud, args.log)
        console.reach_host_prompt()
        require_fresh_capture(console)
        console.command("virtnet capture on")
        capture_enabled = True
        console.drain(args.capture_seconds)
        console.command("virtnet capture off")
        capture_enabled = False
        for vm_id in (1, 2):
            console.command(f"vm suspend {vm_id}")
            suspended.append(vm_id)
        dump = console.capture_dump()
        records = parse_capture_dump(dump)
        paths = write_pcaps(args.prefix, records)
        for path in paths:
            print(f"pcap: wrote {len(records[int(path.stem[-1])])} frames to {path}")
    except Exception as error:
        print(f"TASK23_PCAP_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        if console is not None:
            if capture_enabled:
                console.best_effort_command("virtnet capture off")
            for vm_id in reversed(suspended):
                console.best_effort_command(f"vm resume {vm_id}")
            console.close()
    print(f"TASK23_PCAP_COMPLETE prefix={args.prefix}")
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

    def drain(self, seconds: float) -> None:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            chunk = self.serial.read(65536)
            if not chunk:
                continue
            self.log.write(chunk)
            self.log.flush()
            self.buffer.extend(chunk)

    def wait_for(self, expression: bytes, timeout: float) -> bool:
        pattern = re.compile(expression, re.DOTALL)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.2)
            if pattern.search(self.buffer):
                return True
        return False

    def clear(self) -> None:
        self.buffer.clear()

    def raw(self, payload: bytes) -> None:
        self.serial.write(payload)
        self.serial.flush()

    def line(self, command: str) -> None:
        for byte in command.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)

    def reach_host_prompt(self) -> None:
        self.clear()
        self.raw(b"\r")
        if self.wait_for(HOST_PROMPT, 3):
            return
        self.clear()
        self.raw(b"\x18h")
        if not self.wait_for(HOST_PROMPT, 10):
            raise TimeoutError("could not reach the AxVisor host prompt")

    def command(self, command: str, timeout: float = 30) -> bytes:
        self.clear()
        self.line(command)
        if not self.wait_for(HOST_PROMPT, timeout):
            raise TimeoutError(f"timeout waiting for host reply to {command!r}")
        return bytes(self.buffer)

    def best_effort_command(self, command: str) -> None:
        try:
            self.command(command, 10)
        except Exception as error:
            print(f"warning: cleanup command {command!r} failed: {error}", file=sys.stderr)

    def capture_dump(self) -> bytes:
        self.clear()
        self.line("virtnet capture dump")
        if not self.wait_for(DUMP_END, 180):
            raise TimeoutError("capture dump did not reach CAPDUMP_END")
        if not self.wait_for(HOST_PROMPT, 10):
            raise TimeoutError("host prompt did not return after capture dump")
        return bytes(self.buffer)


def require_fresh_capture(console: Console) -> None:
    output = console.command("virtnet show")
    match = re.search(rb"capture=(ON|off) frames=([0-9]+)", output)
    if match is None:
        raise RuntimeError("virtnet show did not report capture state")
    if match.group(1) != b"off" or int(match.group(2)) != 0:
        raise RuntimeError(
            "capture buffer is not fresh; RAM-only reboot the frozen FIT before collecting evidence"
        )
    for vm_id in (1, 2):
        if not re.search(rb"port vm" + str(vm_id).encode() + rb" .* active", output):
            raise RuntimeError(f"virtio-net port for VM {vm_id} is not active")


def parse_capture_dump(dump: bytes) -> dict[int, list[tuple[int, bytes]]]:
    begin = dump.find(DUMP_BEGIN)
    end = dump.find(DUMP_END, begin + len(DUMP_BEGIN))
    if begin < 0 or end < 0:
        raise ValueError("capture dump markers are missing or incomplete")
    records: dict[int, list[tuple[int, bytes]]] = {1: [], 2: []}
    for line in dump[begin + len(DUMP_BEGIN) : end].splitlines():
        match = re.search(rb"CAPTURE ([0-9]+) ([0-9]+) ([0-9a-f]+)$", line.strip())
        if match is None:
            continue
        vm_id = int(match.group(1))
        if vm_id not in records:
            raise ValueError(f"capture dump contains unexpected VM {vm_id}")
        try:
            frame = bytes.fromhex(match.group(3).decode("ascii"))
        except ValueError as error:
            raise ValueError(f"capture dump contains malformed hex for VM {vm_id}") from error
        records[vm_id].append((int(match.group(2)), frame))
    for vm_id, vm_records in records.items():
        if not vm_records:
            raise ValueError(f"capture dump has no records for VM {vm_id}")
    return records


def write_pcaps(
    prefix: Path, records: dict[int, list[tuple[int, bytes]]]
) -> list[Path]:
    prefix.parent.mkdir(parents=True, exist_ok=True)
    paths: list[Path] = []
    for vm_id in (1, 2):
        path = prefix.with_name(f"{prefix.name}.vm{vm_id}.pcap")
        with path.open("wb") as stream:
            stream.write(PCAP_GLOBAL_HEADER)
            for nanos, frame in records[vm_id]:
                seconds = nanos // 1_000_000_000
                micros = (nanos // 1_000) % 1_000_000
                length = len(frame)
                stream.write(struct.pack("<IIII", seconds, micros, length, length))
                stream.write(frame)
        paths.append(path)
    return paths


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
    parser.add_argument("prefix", type=Path)
    parser.add_argument("log", type=Path)
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--baud", type=int, default=1_500_000)
    parser.add_argument("--capture-seconds", type=float, default=10.0)
    args = parser.parse_args()
    if args.baud <= 0:
        parser.error("baud must be positive")
    if args.capture_seconds <= 0 or args.capture_seconds > 60:
        parser.error("capture-seconds must be in (0, 60]")
    return args


if __name__ == "__main__":
    raise SystemExit(main())
