#!/usr/bin/env python3
"""Capture one AxVisor hybrid-topology Zephyr latency run over UART."""

import argparse
import re
import sys
import time
from pathlib import Path

import serial


HOST_PROMPT = rb"axvisor:(/)?\$"


class Console:
    def __init__(self, port: str, log: Path) -> None:
        log.parent.mkdir(parents=True, exist_ok=True)
        self.serial = serial.Serial(port, 1_500_000, timeout=0.1, exclusive=True)
        self.log = log.open("xb")
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
            if len(self.buffer) > 4_000_000:
                del self.buffer[:-2_000_000]

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

    def command(self, command: str, marker: bytes, timeout: float = 30) -> None:
        self.buffer.clear()
        for byte in command.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)
        self.expect(marker, timeout)

    def detach(self) -> None:
        self.buffer.clear()
        self.raw(b"\x18h")
        self.expect(HOST_PROMPT, 10)

    def ensure_host(self) -> None:
        self.buffer.clear()
        self.raw(b"\r")
        self.drain(1)
        if re.search(HOST_PROMPT, self.buffer, re.DOTALL):
            return
        self.detach()


def main() -> int:
    args = parse_arguments()
    validate_arguments(args)
    validate_causal_evidence(args.causal_evidence)

    console = Console(args.port, args.log)
    started = time.monotonic()
    try:
        sampling_seconds = capture_sampling_window(console, args)
        print(f"HYBRID_LATENCY_SAMPLING_COMPLETE wall_seconds={sampling_seconds:.3f}")
        collect_host_diagnostics(console)
        export_started = time.monotonic()
        export_samples(console, args.samples)
        print(
            "HYBRID_LATENCY_EXPORT_COMPLETE "
            f"wall_seconds={time.monotonic() - export_started:.3f}"
        )
    except Exception as error:
        console.drain(2)
        print(f"HYBRID_LATENCY_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        console.close()

    elapsed = time.monotonic() - started
    print(f"HYBRID_LATENCY_COMPLETE wall_seconds={elapsed:.3f}")
    return 0


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--samples", type=int, default=300)
    parser.add_argument("--causal-evidence", type=Path)
    parser.add_argument("--idle", action="store_true")
    parser.add_argument("--resume-sampling", action="store_true")
    return parser.parse_args()


def validate_arguments(args: argparse.Namespace) -> None:
    if args.samples <= 0:
        raise ValueError("--samples must be positive")
    if args.idle and args.causal_evidence is not None:
        raise ValueError("--idle and --causal-evidence are mutually exclusive")


def validate_causal_evidence(evidence_path: Path | None) -> None:
    if evidence_path is None:
        return
    evidence = evidence_path.read_text(encoding="utf-8", errors="replace")
    for marker in ("RKNN_CONTROL_EVENT", "TASK3_RKNN_INPUT", "TASK3_STATUS_RECEIVED"):
        if evidence.count(marker) < 3:
            raise ValueError(f"causal evidence has fewer than three {marker} records")


def capture_sampling_window(console: Console, args: argparse.Namespace) -> float:
    sample_marker = (
        rb"PERIODIC LATENCY SAMPLING COMPLETE samples=" + str(args.samples).encode()
    )
    timeout = args.samples * 0.03 + 60
    if args.resume_sampling:
        started = time.monotonic()
        console.expect(sample_marker, timeout)
        console.drain(1)
        return time.monotonic() - started

    if args.causal_evidence is None and not args.idle:
        console.expect(rb"RKNN_CONTROL_EVENT .*generation=\d+\b", 60)
        console.expect(rb"TASK3_RKNN_INPUT .*generation=\d+\b", 20)
        console.expect(rb"TASK3_STATUS_RECEIVED .*request=\d+\b", 20)

    console.ensure_host()
    console.command("vm list", HOST_PROMPT)
    console.command("vm console 2", rb"Attached VM\[2\] console")
    console.buffer.clear()
    console.raw(b"g")
    console.expect(rb"PERIODIC LATENCY START", 10)
    started = time.monotonic()
    console.expect(sample_marker, timeout)
    console.drain(1)
    return time.monotonic() - started


def collect_host_diagnostics(console: Console) -> None:
    console.detach()
    console.command("rt stat", HOST_PROMPT)
    console.command("vmexit stat", HOST_PROMPT)
    console.command("vm list", HOST_PROMPT)


def export_samples(console: Console, samples: int) -> None:
    console.command("vm console 2", rb"Attached VM\[2\] console")
    console.buffer.clear()
    console.raw(b"d")
    marker = rb"PERIODIC LATENCY COMPLETE samples=" + str(samples).encode()
    console.expect(marker, max(60, samples * 0.03 + 60))
    console.drain(1)
    console.detach()
    console.drain(1)


if __name__ == "__main__":
    raise SystemExit(main())
