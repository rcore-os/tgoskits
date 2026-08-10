#!/usr/bin/env python3
"""Boot StarryOS from U-Boot opi# via serial."""
from __future__ import annotations

import sys
import time

import serial

PORT = "/dev/ttyUSB0"
BAUD = 1500000
MARKERS = (
    b"STARRY_ORANGEPI_BOOT_OK",
    b"root@starry",
    b"Booting StarryOS",
    b"bootm can't read dtb",
    b"SCRIPT FAILED",
    b"Starting kernel",
)


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

    def cmd(line: str) -> None:
        ser.write(line.encode("ascii") + b"\r\n")
        ser.flush()
        time.sleep(0.3)
        read(8)

    print("Ensuring U-Boot prompt ...", flush=True)
    ser.write(b"\x03")
    read(1)
    cmd("echo starry-manual-boot")
    cmd("setenv fit_addr_r 0x5480000")
    cmd("load mmc 1:1 ${fit_addr_r} image.fit")
    cmd("iminfo ${fit_addr_r}")
    cmd("bootm ${fit_addr_r}#config-ostool")

    print("\nWaiting for StarryOS (120s) ...", flush=True)
    deadline = time.time() + 120
    while time.time() < deadline:
        read(2)
        for marker in MARKERS:
            if marker in buf:
                print(f"\n>>> {marker.decode('ascii', 'replace')}", flush=True)
                if marker in (b"STARRY_ORANGEPI_BOOT_OK", b"root@starry"):
                    ser.close()
                    return 0
                if marker in (b"bootm can't read dtb", b"SCRIPT FAILED"):
                    ser.close()
                    return 1

    ser.close()
    print("\nTimeout waiting for StarryOS.", flush=True)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
