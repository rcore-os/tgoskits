#!/usr/bin/env python3
"""Boot Orange Pi Linux: Image + uInitrd, let booti find embedded DTB."""
from __future__ import annotations

import sys
import time

import serial

PORT = "/dev/ttyUSB0"
BAUD = 1500000
LINUX_MARKERS = (b"orangepi5plus login:", b"orangepi@orangepi5plus")


def main() -> int:
    ser = serial.Serial(PORT, BAUD, timeout=0.3)
    buf = bytearray()

    def read(seconds: float) -> None:
        deadline = time.time() + seconds
        while time.time() < deadline:
            chunk = ser.read(8192)
            if chunk:
                buf.extend(chunk)
                sys.stdout.buffer.write(chunk)
                sys.stdout.flush()

    def cmd(line: str, wait: float = 12.0) -> None:
        print(f"CMD: {line}", flush=True)
        ser.write(line.encode("ascii") + b"\r\n")
        ser.flush()
        read(wait)

    print("Waiting for opi# (up to 5 min; board may run PXE first) ...", flush=True)
    deadline = time.time() + 300
    while time.time() < deadline:
        ser.write(b"\x03")
        ser.flush()
        read(0.5)
        if b"opi#" in buf:
            break
    else:
        print("No opi# — power-cycle the board and retry.", file=sys.stderr)
        return 1

    cmd("load mmc 1:1 0x400000 Image", 20)
    cmd("load mmc 1:1 0x0a200000 uInitrd", 20)
    cmd("booti 0x400000 0x0a200000", 25)

    deadline = time.time() + 120
    while time.time() < deadline:
        read(2)
        if any(m in buf for m in LINUX_MARKERS):
            print("\nLinux OK.", flush=True)
            ser.close()
            return 0
        if b"Starting kernel" in buf:
            read(90)
            if any(m in buf for m in LINUX_MARKERS):
                print("\nLinux OK.", flush=True)
                ser.close()
                return 0

    ser.close()
    print("\nLinux boot failed.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
