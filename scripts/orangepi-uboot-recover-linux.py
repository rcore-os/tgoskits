#!/usr/bin/env python3
"""Recover Orange Pi Linux by sourcing boot.scr.linux.bak from U-Boot over serial."""
from __future__ import annotations

import sys
import time

import serial

PORT = "/dev/ttyUSB0"
BAUD = 1500000
TIMEOUT = 0.2
PROMPT = b"opi#"
LINUX_MARKERS = (b"orangepi5plus login:", b"orangepi@orangepi5plus")


def read_available(ser: serial.Serial, buf: bytearray, seconds: float) -> None:
    deadline = time.time() + seconds
    while time.time() < deadline:
        chunk = ser.read(4096)
        if chunk:
            buf.extend(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.flush()
        else:
            time.sleep(0.05)


def send_line(ser: serial.Serial, line: str) -> None:
    ser.write(line.encode("ascii") + b"\r\n")
    ser.flush()


def main() -> int:
    print(f"Opening {PORT} @ {BAUD} ...", flush=True)
    try:
        ser = serial.Serial(PORT, BAUD, timeout=TIMEOUT)
    except serial.SerialException as exc:
        print(f"Cannot open serial: {exc}", file=sys.stderr)
        print("Close picocom/other programs using /dev/ttyUSB0 first.", file=sys.stderr)
        return 1

    buf = bytearray()
    print(">>> Please POWER-CYCLE the board now (off 5s, then on). <<<", flush=True)
    print("Sending Ctrl+C to catch U-Boot autoboot ...", flush=True)

    deadline = time.time() + 120
    while time.time() < deadline:
        ser.write(b"\x03")
        ser.flush()
        read_available(ser, buf, 0.4)
        if PROMPT in buf or b"=> " in buf:
            break
        if any(marker in buf for marker in LINUX_MARKERS):
            print("\nAlready in Linux — no U-Boot recovery needed.", flush=True)
            ser.close()
            return 0
    else:
        print("\nTimed out waiting for U-Boot prompt.", file=sys.stderr)
        ser.close()
        return 1

    print("\nU-Boot prompt detected. Restoring Linux boot script ...", flush=True)
    for cmd in (
        "setenv loadaddr 0x5480000",
        "load mmc 1:1 ${loadaddr} boot.scr.linux.bak",
        "source ${loadaddr}",
    ):
        send_line(ser, cmd)
        buf.clear()
        read_available(ser, buf, 8)

    print("\nWaiting for Linux login banner (up to 120s) ...", flush=True)
    buf.clear()
    deadline = time.time() + 120
    while time.time() < deadline:
        read_available(ser, buf, 2)
        if any(marker in buf for marker in LINUX_MARKERS):
            print("\nLinux boot OK.", flush=True)
            ser.close()
            return 0

    print("\nLinux banner not seen yet — check serial output manually.", file=sys.stderr)
    ser.close()
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
