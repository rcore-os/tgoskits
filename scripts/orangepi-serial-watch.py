#!/usr/bin/env python3
"""Capture serial boot log and look for StarryOS / U-Boot markers."""
from __future__ import annotations

import sys
import time

import serial

PORT = "/dev/ttyUSB0"
BAUD = 1500000
MARKERS = (
    b"STARRY_ORANGEPI_BOOT_OK",
    b"root@starry",
    b"bootm can't read dtb",
    b"Booting StarryOS",
    b"Starting kernel",
    b"SCRIPT FAILED",
    b"orangepi5plus login",
)


def main() -> int:
    ser = serial.Serial(PORT, BAUD, timeout=0.3)
    buf = bytearray()
    deadline = time.time() + 90
    print(f"Reading {PORT} for 90s ...", flush=True)
    while time.time() < deadline:
        chunk = ser.read(8192)
        if chunk:
            buf.extend(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.flush()
            for marker in MARKERS:
                if marker in buf:
                    print(f"\n>>> FOUND: {marker.decode('ascii', 'replace')}", flush=True)
        else:
            time.sleep(0.1)
    ser.close()
    print("\n--- summary ---", flush=True)
    for marker in MARKERS:
        print(f"{marker.decode('ascii','replace')}: {'YES' if marker in buf else 'no'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
