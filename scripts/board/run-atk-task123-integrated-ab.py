#!/usr/bin/env python3
"""Run one integrated Task 1/2/3 arm on the physical RK3588 board."""

from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import serial


GUEST_PROMPT = rb"root@starry:[^\r\n]*#"
HOST_PROMPT = rb"axvisor:/\$"
EXPECTED_SAMPLE_IDS = (
    b"vase-left",
    b"vase-center",
    b"vase-right",
    b"no-target",
    b"small-target",
)
EXPECTED_SAMPLE_SHA256 = (
    b"570d32eef9a9f5fd7101c5058b625ef62469e5ba778a77c38da675bea4752cf9",
    b"608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f",
    b"f9d70fd3e0f85185ce8e45b2d714d8b4083833da73cdaeac5d981abc890ef698",
    b"a50f32c63aff52e8abab6c82bbd8e202512ab23ff12e213d6833aa719af56bc9",
    b"a6a77909aed2ed499df3ccd54d5db3384a2de0cfcaa799b15e1c896c7a058370",
)
FATAL_PATTERNS = (
    rb"STARRY_T2N1_FAIL",
    rb"ESR_EL2=",
    rb"Unhandled acknowledged host IRQ 26",
    rb"(?i)\bpanic(?:ked)?\b",
)
ANSI_ESCAPE = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")


@dataclass(frozen=True)
class RunConfig:
    mode: str
    rtos_name: str
    log_path: Path
    metadata_path: Path
    port: str
    baud: int
    artifacts: tuple[Path, ...]

    @property
    def expected_controls(self) -> int:
        return 5 if self.mode == "manual" else 3

    @property
    def expected_rejections(self) -> int:
        return 0 if self.mode == "manual" else 2


class Console:
    def __init__(self, port: str, baud: int, log_path: Path) -> None:
        refuse_shared_console(port)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.serial = serial.Serial(
            port, baudrate=baud, timeout=0.1, exclusive=True
        )
        self.log = log_path.open("wb")
        self.buffer = bytearray()

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def note(self, message: str) -> None:
        line = f"\nTASK123_RUNNER {message}\n".encode()
        self.log.write(line)
        self.log.flush()
        sys.stdout.buffer.write(line)
        sys.stdout.buffer.flush()

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
            self.drain(0.2)
            if pattern.search(self.buffer):
                return
        raise TimeoutError(f"timeout waiting for {expression!r}")

    def clear_match_window(self) -> None:
        self.buffer.clear()

    def raw(self, data: bytes) -> None:
        self.serial.write(data)
        self.serial.flush()

    def line(self, command: str) -> None:
        for byte in command.encode() + b"\r":
            self.raw(bytes((byte,)))
            time.sleep(0.002)

    def command(self, command: str, prompt: bytes, timeout: float = 30) -> None:
        self.clear_match_window()
        self.line(command)
        self.expect(prompt, timeout)

    def detach(self) -> None:
        self.clear_match_window()
        self.raw(b"\x18h")
        self.expect(HOST_PROMPT, 10)

    def snapshot_log(self) -> bytes:
        self.log.flush()
        return self.log_path.read_bytes()

    @property
    def log_path(self) -> Path:
        return Path(self.log.name)


def main() -> int:
    config = parse_arguments()
    write_metadata(config, "started")
    console: Console | None = None
    started = time.monotonic()
    try:
        console = Console(config.port, config.baud, config.log_path)
        console.note(
            f"mode={config.mode} scheduler=fp-rr shared_pcpu=0x100 "
            f"rtos={config.rtos_name} rtos_priority=90 "
            "starry_priority=89 ram_only=true"
        )
        prepare_starry_guest(console)
        run_experiment(console, config.mode)
        collect_host_diagnostics(console)
        console.drain(1)
        validate_log(console.snapshot_log(), config)
    except Exception as error:
        if console is not None:
            console.note(f"status=failed error={error!r}")
            console.drain(1)
        write_metadata(config, "failed", error=str(error))
        print(f"TASK123_RUNNER_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        if console is not None:
            console.close()

    elapsed_seconds = time.monotonic() - started
    write_metadata(config, "complete", elapsed_seconds=elapsed_seconds)
    print(f"TASK123_RUNNER_COMPLETE mode={config.mode}")
    return 0


def refuse_shared_console(port: str) -> None:
    result = subprocess.run(
        ["sudo", "-n", "fuser", port],
        check=False,
        capture_output=True,
        text=True,
    )
    holders = f"{result.stdout} {result.stderr}".strip()
    if result.returncode == 0 and holders:
        raise RuntimeError(f"serial port {port} is already open by {holders}")
    if result.returncode not in {0, 1}:
        raise RuntimeError(f"could not verify exclusive ownership of {port}: {holders}")


def prepare_starry_guest(console: Console) -> None:
    console.raw(b"\r")
    console.expect(GUEST_PROMPT, 120)
    console.command(
        "wc -c /proc/initrd; sha256sum /proc/initrd; "
        "mkdir -p /tmp/t123; cd /tmp/t123; "
        "gzip -dc /proc/initrd | cpio -id; "
        "mount --bind /tmp/t123/usr/share /usr/share; "
        "ip addr add 10.0.42.15/24 dev eth0 2>/dev/null || true; "
        "echo TASK123_SETUP_DONE",
        GUEST_PROMPT,
        120,
    )


def run_experiment(console: Console, mode: str) -> None:
    console.clear_match_window()
    console.line(f"/tmp/t123/bin/task2-net {mode}")
    ready = (
        rb"TASK3_EXPERIMENT_READY run_mode=manual"
        if mode == "manual"
        else rb"TASK3_MODEL_READY [^\r\n]*run_mode=yolo"
    )
    console.expect(ready, 60)
    console.expect(
        rb"TASK3_EXPERIMENT_COMPLETE run_mode="
        + mode.encode()
        + rb" samples=5\b",
        360,
    )
    console.clear_match_window()
    console.raw(b"\x03")
    console.expect(GUEST_PROMPT, 30)


def collect_host_diagnostics(console: Console) -> None:
    console.detach()
    console.command("rt stat", HOST_PROMPT, 30)
    console.command("vm list", HOST_PROMPT, 30)


def validate_log(log: bytes, config: RunConfig) -> None:
    log = ANSI_ESCAPE.sub(b"", log).replace(b"\r", b"")
    for pattern in FATAL_PATTERNS:
        if re.search(pattern, log):
            raise RuntimeError(f"fatal marker found: {pattern!r}")
    samples = re.findall(rb"(?m)^TASK3_SAMPLE [^\r\n]*", log)
    controls = re.findall(rb"(?m)^TASK3_CONTROL_SENT [^\r\n]*", log)
    statuses = re.findall(rb"(?m)^TASK3_STATUS_RECEIVED [^\r\n]*", log)
    rejections = re.findall(rb"(?m)^TASK3_MODEL_REJECTED [^\r\n]*", log)
    if len(samples) != 5:
        raise RuntimeError(f"expected 5 samples, observed {len(samples)}")
    if len(controls) != config.expected_controls or len(statuses) != config.expected_controls:
        raise RuntimeError(
            "CONTROL/STATUS count mismatch: "
            f"expected {config.expected_controls}, got {len(controls)}/{len(statuses)}"
        )
    if len(rejections) != config.expected_rejections:
        raise RuntimeError(
            f"expected {config.expected_rejections} model rejections, got {len(rejections)}"
        )
    observed_ids = tuple(
        re.search(rb"\bimage_id=([^ ]+)", sample).group(1) for sample in samples
    )
    if observed_ids != EXPECTED_SAMPLE_IDS:
        raise RuntimeError(f"unexpected sample order: {observed_ids!r}")
    observed_hashes = tuple(
        re.search(rb"\bimage_sha256=([0-9a-f]{64})", sample).group(1)
        for sample in samples
    )
    if observed_hashes != EXPECTED_SAMPLE_SHA256:
        raise RuntimeError(f"unexpected sample hashes: {observed_hashes!r}")
    request_ids = tuple(
        int(re.search(rb"\brequest=([0-9]+)", sample).group(1)) for sample in samples
    )
    if len(set(request_ids)) != 5:
        raise RuntimeError(f"sample request IDs are not unique: {request_ids!r}")
    sample_sources = tuple(
        re.search(rb"\bsource=([^ ]+)", sample).group(1) for sample in samples
    )
    if sample_sources != (config.mode.encode(),) * len(samples):
        raise RuntimeError(f"unexpected sample sources: {sample_sources!r}")
    outcomes = tuple(
        re.search(rb"\boutcome=([^ ]+)", sample).group(1) for sample in samples
    )
    expected = tuple(
        re.search(rb"\bexpected=([^ ]+)", sample).group(1) for sample in samples
    )
    if config.mode == "yolo":
        expected_outcomes = tuple(
            b"accepted" if behavior == b"accept" else b"rejected"
            for behavior in expected
        )
        if outcomes != expected_outcomes:
            raise RuntimeError(
                f"YOLO expected/outcome mismatch: {expected!r} / {outcomes!r}"
            )
    accepted_requests = tuple(
        request
        for request, outcome in zip(request_ids, outcomes, strict=True)
        if outcome == b"accepted"
    )
    control_requests = tuple(
        int(re.search(rb"\brequest=([0-9]+)", line).group(1)) for line in controls
    )
    status_requests = tuple(
        int(re.search(rb"\brequest=([0-9]+)", line).group(1)) for line in statuses
    )
    if control_requests != accepted_requests or status_requests != accepted_requests:
        raise RuntimeError(
            "sample/CONTROL/STATUS request mismatch: "
            f"{accepted_requests!r} / {control_requests!r} / {status_requests!r}"
        )
    if config.mode == "yolo":
        rejected_requests = tuple(
            request
            for request, outcome in zip(request_ids, outcomes, strict=True)
            if outcome == b"rejected"
        )
        rejection_requests = tuple(
            int(re.search(rb"\brequest=([0-9]+)", line).group(1))
            for line in rejections
        )
        if rejection_requests != rejected_requests:
            raise RuntimeError(
                "sample/rejection request mismatch: "
                f"{rejected_requests!r} / {rejection_requests!r}"
            )
    required = (
        b"STARRY_T2N1_PASS",
        b"FP-RR scheduler counters:",
        b"TASK3_EXPERIMENT_COMPLETE run_mode=" + config.mode.encode(),
    )
    missing = [marker for marker in required if marker not in log]
    if missing:
        raise RuntimeError(f"missing completion evidence: {missing!r}")


def parse_arguments() -> RunConfig:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("manual", "yolo"))
    parser.add_argument("log", type=Path)
    parser.add_argument("--rtos", choices=("rtthread", "zephyr"), default="rtthread")
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--baud", type=int, default=1_500_000)
    parser.add_argument("--artifact", action="append", default=[], type=Path)
    parser.add_argument("--metadata", type=Path)
    args = parser.parse_args()
    if args.baud <= 0:
        parser.error("baud must be positive")
    missing = [path for path in args.artifact if not path.is_file()]
    if missing:
        parser.error(f"artifact does not exist: {missing[0]}")
    return RunConfig(
        mode=args.mode,
        rtos_name=args.rtos,
        log_path=args.log,
        metadata_path=args.metadata
        or args.log.with_name(f"{args.log.name}.metadata.txt"),
        port=args.port,
        baud=args.baud,
        artifacts=tuple(args.artifact),
    )


def write_metadata(
    config: RunConfig,
    status: str,
    *,
    error: str | None = None,
    elapsed_seconds: float | None = None,
) -> None:
    config.metadata_path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        f"status={status}",
        f"mode={config.mode}",
        f"rtos={config.rtos_name}",
        "scheduler=fp-rr",
        "shared_pcpu=0x100",
        "rtos_priority=90",
        "starry_priority=89",
        "ram_only=true",
    ]
    if elapsed_seconds is not None:
        lines.append(f"elapsed_seconds={elapsed_seconds:.3f}")
    if error is not None:
        lines.append(f"error={error}")
    for artifact in config.artifacts:
        lines.append(
            f"artifact_sha256={sha256(artifact)}  {artifact.resolve()} "
            f"size={artifact.stat().st_size}"
        )
    config.metadata_path.write_text("\n".join(lines) + "\n")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


if __name__ == "__main__":
    raise SystemExit(main())
