#!/usr/bin/env python3
"""Drive one physical-board Task 1 YOLO/periodic arm over the sole UART."""

from __future__ import annotations

import argparse
import hashlib
import math
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import serial


GUEST_PROMPT = rb"root@starry:[^\r\n]*#"
HOST_PROMPT = rb"axvisor:/\$"
INFERENCE_COMPLETE = rb"TASK3_INFER [^\r\n]*model=yolo11n\.ncnn"
PERIODIC_DUMP_COMMAND = b"d"
POST_SAMPLING_INFERENCE_TIMEOUT_SECONDS = 30
# RT-Thread formats and writes every retained row after sampling. Budget for a
# deliberately conservative rate below the ~118 rows/s observed on RK3588 so
# serial export cannot consume the sampling timeout.
MIN_SERIAL_EXPORT_ROWS_PER_SECOND = 50


@dataclass(frozen=True)
class RunConfig:
    log_path: Path
    metadata_path: Path
    port: str
    baud: int
    scheduler: str
    runtime_seconds: int
    expected_inferences: int
    expected_samples: int
    period_ms: int
    completion_grace_seconds: int
    artifacts: tuple[Path, ...]
    periodic_guest: str = "rtthread"

    @property
    def model_mode(self) -> str:
        return "model-loop" if self.runtime_seconds > 0 else "model-only"

    @property
    def periodic_timeout_seconds(self) -> int:
        nominal_seconds = self.expected_samples * self.period_ms / 1000
        minimum_seconds = max(nominal_seconds, self.runtime_seconds)
        sampling_budget = math.ceil(minimum_seconds * 3)
        export_budget = math.ceil(
            self.expected_samples / MIN_SERIAL_EXPORT_ROWS_PER_SECOND
        )
        return max(
            90,
            sampling_budget + export_budget + self.completion_grace_seconds,
        )


class Console:
    def __init__(self, port: str, baud: int, log_path: Path) -> None:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.serial = serial.Serial(port, baudrate=baud, timeout=0.1)
        self.log = log_path.open("wb")
        self.buffer = bytearray()

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def note(self, message: str) -> None:
        line = f"\nTASK1_RUNNER {message}\n".encode()
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
            # Keep bulk CSV off stdout. A long unattended PTY can stop accepting
            # output, which would block this sole UART reader and invalidate the
            # physical-board run. Stage updates still reach stdout via note().
            self.buffer.extend(chunk)
            if len(self.buffer) > 4_000_000:
                del self.buffer[:-2_000_000]

    def expect(self, expression: bytes, timeout: float) -> None:
        pattern = re.compile(expression, re.DOTALL)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.2)
            if pattern.search(self.buffer):
                return
        raise TimeoutError(f"timeout waiting for {expression!r}")

    def expect_count(self, expression: bytes, count: int, timeout: float) -> None:
        pattern = re.compile(expression)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.2)
            if len(pattern.findall(self.buffer)) >= count:
                return
        observed = len(pattern.findall(self.buffer))
        raise TimeoutError(
            f"timeout waiting for {count} matches of {expression!r}; observed {observed}"
        )

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


def main() -> int:
    config = parse_arguments()
    write_metadata(config, "started")
    console = Console(config.port, config.baud, config.log_path)
    started = time.monotonic()
    try:
        console.note(
            f"scheduler={config.scheduler} model_mode={config.model_mode} "
            f"runtime_seconds={config.runtime_seconds} "
            f"expected_inferences={config.expected_inferences} "
            f"expected_samples={config.expected_samples} period_ms={config.period_ms}"
        )
        prepare_starry_guest(console)
        start_model_workload(console, config.model_mode)
        start_periodic_probe(console)

        measurement_started = time.monotonic()
        console.expect(
            rb"PERIODIC LATENCY SAMPLING COMPLETE samples="
            + str(config.expected_samples).encode()
            + rb"\b",
            config.periodic_timeout_seconds,
        )
        measurement_seconds = time.monotonic() - measurement_started
        console.note(f"periodic_elapsed_seconds={measurement_seconds:.3f}")
        if measurement_seconds < config.runtime_seconds:
            raise RuntimeError(
                "periodic measurement completed before the requested runtime: "
                f"{measurement_seconds:.3f}s < {config.runtime_seconds}s"
            )

        collect_host_diagnostics(console)
        collect_model_results(console, config)
        collect_periodic_results(console, config)
        console.drain(1)
    except Exception as error:
        console.note(f"status=failed error={error!r}")
        console.drain(2)
        write_metadata(config, "failed", error=str(error))
        print(f"\nTASK1_ARM_ERROR: {error}", file=sys.stderr)
        return 1
    finally:
        console.close()

    elapsed_seconds = time.monotonic() - started
    write_metadata(config, "complete", elapsed_seconds=elapsed_seconds)
    print("\nTASK1_ARM_COMPLETE")
    return 0


def prepare_starry_guest(console: Console) -> None:
    console.raw(b"\r")
    console.expect(GUEST_PROMPT, 60)
    console.command(
        "wc -c /proc/initrd; sha256sum /proc/initrd; "
        "mkdir -p /tmp/t1; cd /tmp/t1; "
        "gzip -dc /proc/initrd | cpio -id; "
        "mount --bind /tmp/t1/usr/share /usr/share; "
        "ip addr add 10.0.42.15/24 dev eth0 2>/dev/null || true; "
        "echo TASK1_SETUP_DONE",
        GUEST_PROMPT,
        90,
    )


def start_model_workload(console: Console, model_mode: str) -> None:
    console.clear_match_window()
    console.line(f"/tmp/t1/bin/task2-net {model_mode}")
    console.expect(rb"TASK3_MODEL_READY model=yolo11n\.ncnn runtime=ncnn", 30)
    console.expect(rb"TASK3_INFER_STARTED", 30)


def start_periodic_probe(console: Console) -> None:
    console.detach()
    console.command("vm list", HOST_PROMPT)
    console.command("vm console 2", rb"Attached VM\[2\] console", 30)
    console.clear_match_window()
    console.raw(b"g")
    console.expect(rb"PERIODIC LATENCY START", 10)


def collect_host_diagnostics(console: Console) -> None:
    console.detach()
    console.command("rt stat", HOST_PROMPT, 30)
    console.command("vmexit stat", HOST_PROMPT, 30)
    console.command("vm list", HOST_PROMPT, 30)


def collect_model_results(console: Console, config: RunConfig) -> None:
    console.command("vm console 1", rb"Attached VM\[1\] console", 30)
    console.expect_count(INFERENCE_COMPLETE, config.expected_inferences, 180)
    if config.model_mode == "model-loop":
        # The RT guest is blocked at the sampling/export barrier. Require one
        # fresh completion now so the board run proves AI progress without the
        # high-priority CSV exporter competing for the shared pCPU.
        console.clear_match_window()
        console.expect(
            INFERENCE_COMPLETE, POST_SAMPLING_INFERENCE_TIMEOUT_SECONDS
        )
        console.clear_match_window()
        console.raw(b"\x03")
        console.expect(GUEST_PROMPT, 30)
    else:
        console.expect(GUEST_PROMPT, 30)
    console.detach()


def collect_periodic_results(console: Console, config: RunConfig) -> None:
    console.command("vm console 2", rb"Attached VM\[2\] console", 30)
    console.clear_match_window()
    console.raw(PERIODIC_DUMP_COMMAND)
    console.expect(
        rb"PERIODIC LATENCY COMPLETE samples="
        + str(config.expected_samples).encode()
        + rb"\b",
        config.periodic_timeout_seconds,
    )


def parse_arguments() -> RunConfig:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--port", default="/dev/ttyACM0")
    parser.add_argument("--baud", type=int, default=1_500_000)
    parser.add_argument("--scheduler", choices=("rr", "fp-rr"), default="rr")
    parser.add_argument("--runtime-seconds", type=int, default=0)
    parser.add_argument("--expected-inferences", type=int)
    parser.add_argument("--expected-samples", type=int, default=300)
    parser.add_argument("--period-ms", type=int, default=10)
    parser.add_argument(
        "--periodic-guest",
        choices=("rtthread", "zephyr"),
        default="rtthread",
    )
    parser.add_argument("--completion-grace-seconds", type=int, default=180)
    parser.add_argument("--artifact", action="append", default=[], type=Path)
    parser.add_argument("--metadata", type=Path)
    args = parser.parse_args()

    expected_inferences = args.expected_inferences
    if expected_inferences is None:
        expected_inferences = 50 if args.runtime_seconds > 0 else 1
    positive_values = {
        "baud": args.baud,
        "expected_inferences": expected_inferences,
        "expected_samples": args.expected_samples,
        "period_ms": args.period_ms,
        "completion_grace_seconds": args.completion_grace_seconds,
    }
    invalid = [name for name, value in positive_values.items() if value <= 0]
    if args.runtime_seconds < 0:
        invalid.append("runtime_seconds")
    if invalid:
        parser.error(f"values must be positive (runtime may be zero): {', '.join(invalid)}")

    missing_artifacts = [path for path in args.artifact if not path.is_file()]
    if missing_artifacts:
        parser.error(f"artifact does not exist: {missing_artifacts[0]}")
    metadata_path = args.metadata or args.log.with_name(f"{args.log.name}.metadata.txt")
    return RunConfig(
        log_path=args.log,
        metadata_path=metadata_path,
        port=args.port,
        baud=args.baud,
        scheduler=args.scheduler,
        runtime_seconds=args.runtime_seconds,
        expected_inferences=expected_inferences,
        expected_samples=args.expected_samples,
        period_ms=args.period_ms,
        completion_grace_seconds=args.completion_grace_seconds,
        artifacts=tuple(args.artifact),
        periodic_guest=args.periodic_guest,
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
        f"scheduler={config.scheduler}",
        f"model_mode={config.model_mode}",
        f"runtime_seconds={config.runtime_seconds}",
        f"expected_inferences={config.expected_inferences}",
        f"expected_samples={config.expected_samples}",
        f"period_ms={config.period_ms}",
        f"periodic_guest={config.periodic_guest}",
        f"periodic_timeout_seconds={config.periodic_timeout_seconds}",
    ]
    if elapsed_seconds is not None:
        lines.append(f"elapsed_seconds={elapsed_seconds:.3f}")
    if error is not None:
        lines.append(f"error={error}")
    for artifact in config.artifacts:
        lines.append(
            f"artifact_sha256={sha256(artifact)}  {artifact.resolve()} size={artifact.stat().st_size}"
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
