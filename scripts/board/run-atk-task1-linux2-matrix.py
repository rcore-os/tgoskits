#!/usr/bin/env python3
"""Capture one physical 2-vCPU Linux plus RT-Thread latency window."""

import argparse
import re
import sys
import time
from pathlib import Path

import serial


class Console:
    def __init__(self, path: str, log: Path) -> None:
        self.serial = serial.Serial(path, 1_500_000, timeout=0.1)
        self.log = log.open("wb")
        self.buffer = bytearray()

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def drain(self, seconds: float = 0.2) -> None:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            data = self.serial.read(65536)
            if not data:
                continue
            self.log.write(data)
            self.log.flush()
            self.buffer.extend(data)
            if len(self.buffer) > 2_000_000:
                del self.buffer[:-1_000_000]

    def expect(self, pattern: bytes, timeout: float) -> None:
        expression = re.compile(pattern, re.DOTALL)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain()
            if expression.search(self.buffer):
                return
        raise TimeoutError(f"timeout waiting for {pattern!r}")

    def raw(self, data: bytes) -> None:
        self.serial.write(data)
        self.serial.flush()

    def command(self, text: str, marker: bytes, timeout: float = 30) -> None:
        self.buffer.clear()
        for byte in text.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)
        self.expect(marker, timeout)

    def detach(self) -> None:
        self.buffer.clear()
        self.raw(b"\x18h")
        self.expect(rb"axvisor:(/)?\$", 10)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("log")
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--linux-timeout", type=float, default=90)
    args = parser.parse_args()
    console = Console(args.port, Path(args.log))
    host = rb"axvisor:(/)?\$"
    try:
        console.expect(rb"RT_CPUS total=2", 60)
        console.expect(rb"RT_CYCLICTEST_START", 60)
        console.detach()
        console.command("vm list", host)
        console.command("vmexit stat", host)
        console.command("vm console 2", rb"Attached VM\[2\] console")
        console.buffer.clear()
        console.raw(b"g")
        console.expect(rb"PERIODIC LATENCY START", 10)
        console.expect(rb"PERIODIC LATENCY COMPLETE samples=300", 90)
        console.detach()
        console.command("rt stat", host)
        console.command("vmexit stat", host)
        console.command(
            "vm console 1", rb"RT_CYCLICTEST_COMPLETE", args.linux_timeout
        )
        console.expect(rb"RT_INIT_DONE", 30)
        console.detach()
        console.command("vm list", host)
        console.command("vmexit stat", host)
        console.drain(1)
    except Exception as error:
        console.drain(2)
        print(f"TASK1_LINUX2_MATRIX_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        console.close()
    print("TASK1_LINUX2_MATRIX_COMPLETE")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
