#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Validate a native Zephyr RT-baseline console log and emit JSON evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Sequence


EXPECTED_CONFIG = {
    "schema": "1",
    "os": "zephyr",
    "zephyr_version": "4.3.0",
    "board": "qemu_cortex_a53",
    "cpu_model": "cortex-a53",
    "cpu_count": "1",
    "qemu_icount": "false",
    "period_us": "1000",
    "samples": "10000",
    "warmup": "100",
    "benchmark_priority": "-16",
    "stress_priority": "5",
    "clock_hz": "62500000",
    "ticks_per_sec": "1000",
}
EXPECTED_SOURCE = {
    "version": "v4.3.0",
    "tag_object": "981205b3e7cdf9fdf2e9e71b8b6b64fcc71c12a0",
    "commit": "3568e1b6d5cdd51a6b964a2a1d6d29200fea2056",
}
EXPECTED_METRICS = {
    "periodic_wake_lateness",
    "timer_to_task_dispatch",
}
MEASURED_EXPIRATIONS = int(EXPECTED_CONFIG["samples"])
WARMUP_EXPIRATIONS = int(EXPECTED_CONFIG["warmup"])
MAX_MEASURED_TIMER_MISSES = MEASURED_EXPIRATIONS
MAX_WARMUP_TIMER_MISSES = WARMUP_EXPIRATIONS - 1
STAT_FIELDS = (
    "min_ns",
    "mean_ns",
    "p50_ns",
    "p90_ns",
    "p99_ns",
    "p999_ns",
    "max_ns",
)


class AnalysisError(ValueError):
    """Raised when a console log violates the benchmark result contract."""


def parse_record(line: str, marker: str) -> dict[str, str] | None:
    """Parse one space-separated key/value record with an exact marker."""
    stripped = line.strip()
    prefix = f"{marker} "
    if not stripped.startswith(prefix):
        return None

    fields: dict[str, str] = {}
    for token in stripped.removeprefix(prefix).split():
        key, separator, value = token.partition("=")
        if not separator or not key or not value:
            raise AnalysisError(f"{marker}: malformed field {token!r}")
        if key in fields:
            raise AnalysisError(f"{marker}: duplicate field {key!r}")
        fields[key] = value
    return fields


def select_records(lines: Sequence[str], marker: str) -> list[dict[str, str]]:
    """Return all records for a marker."""
    return [record for line in lines if (record := parse_record(line, marker))]


def require_one(lines: Sequence[str], marker: str) -> dict[str, str]:
    """Return the marker's unique record or reject the log."""
    records = select_records(lines, marker)
    if len(records) != 1:
        raise AnalysisError(f"expected one {marker} record, found {len(records)}")
    return records[0]


def parse_nonnegative(record: dict[str, str], field: str, marker: str) -> int:
    """Parse a required non-negative base-10 field."""
    try:
        value = int(record[field], 10)
    except KeyError as error:
        raise AnalysisError(f"{marker}: missing field {field!r}") from error
    except ValueError as error:
        raise AnalysisError(f"{marker}: {field!r} is not an integer") from error
    if value < 0:
        raise AnalysisError(f"{marker}: {field!r} must be non-negative")
    return value


def parse_integer(record: dict[str, str], field: str, marker: str) -> int:
    """Parse a required signed base-10 field."""
    try:
        return int(record[field], 10)
    except KeyError as error:
        raise AnalysisError(f"{marker}: missing field {field!r}") from error
    except ValueError as error:
        raise AnalysisError(f"{marker}: {field!r} is not an integer") from error


def validate_config(record: dict[str, str], workload: str) -> dict[str, object]:
    """Validate and normalize the fixed benchmark configuration."""
    for field, expected in EXPECTED_CONFIG.items():
        if record.get(field) != expected:
            raise AnalysisError(
                f"RTOS_BASELINE_CONFIG: {field} must be {expected!r}, "
                f"got {record.get(field)!r}"
            )
    if record.get("workload") != workload:
        raise AnalysisError("RTOS_BASELINE_CONFIG: workload does not match the run")
    if parse_nonnegative(record, "clock_hz", "RTOS_BASELINE_CONFIG") == 0:
        raise AnalysisError("RTOS_BASELINE_CONFIG: clock_hz must be positive")
    if parse_nonnegative(record, "ticks_per_sec", "RTOS_BASELINE_CONFIG") != 1000:
        raise AnalysisError("RTOS_BASELINE_CONFIG: ticks_per_sec must be 1000")
    return normalize_record(record)


def validate_workload_ready(record: dict[str, str], workload: str) -> None:
    """Verify that the selected workload started before sampling."""
    if record.get("schema") != "1" or record.get("kind") != workload:
        raise AnalysisError("RTOS_BASELINE_WORKLOAD_READY: wrong schema or workload")
    if record.get("verified") != "true":
        raise AnalysisError("RTOS_BASELINE_WORKLOAD_READY: workload was not verified")
    benchmark_priority = parse_integer(
        record, "benchmark_priority", "RTOS_BASELINE_WORKLOAD_READY"
    )
    stress_priority = parse_integer(
        record, "stress_priority", "RTOS_BASELINE_WORKLOAD_READY"
    )
    blocks = parse_nonnegative(record, "blocks", "RTOS_BASELINE_WORKLOAD_READY")
    if workload == "cpu-stress":
        if record.get("lower_priority") != "true":
            raise AnalysisError("stress thread was not marked lower priority")
        if benchmark_priority >= stress_priority or blocks == 0:
            raise AnalysisError("stress priority or execution proof is invalid")
    elif record.get("lower_priority") != "false" or blocks != 0:
        raise AnalysisError("idle run unexpectedly reports a stress workload")


def validate_results(
    records: Sequence[dict[str, str]], workload: str
) -> dict[str, dict[str, object]]:
    """Validate aggregate statistics for both latency metrics."""
    if len(records) != len(EXPECTED_METRICS):
        raise AnalysisError(
            f"expected {len(EXPECTED_METRICS)} result records, found {len(records)}"
        )
    results: dict[str, dict[str, object]] = {}
    observed_duration_us: int | None = None
    for record in records:
        metric = record.get("metric")
        if metric not in EXPECTED_METRICS or metric in results:
            raise AnalysisError(f"unexpected or duplicate metric {metric!r}")
        if record.get("schema") != "1" or record.get("workload") != workload:
            raise AnalysisError(f"{metric}: wrong schema or workload")
        if record.get("unit") != "ns":
            raise AnalysisError(f"{metric}: unit must be ns")
        if parse_nonnegative(record, "count", metric) != 10000:
            raise AnalysisError(f"{metric}: count must be 10000")

        statistics = [parse_nonnegative(record, field, metric) for field in STAT_FIELDS]
        minimum, mean, p50, p90, p99, p999, maximum = statistics
        if not minimum <= p50 <= p90 <= p99 <= p999 <= maximum:
            raise AnalysisError(f"{metric}: percentiles are not monotonic")
        if not minimum <= mean <= maximum:
            raise AnalysisError(f"{metric}: mean is outside the observed range")
        expected_duration_us = parse_nonnegative(record, "expected_duration_us", metric)
        actual_duration_us = parse_nonnegative(record, "actual_duration_us", metric)
        if expected_duration_us != 10_000_000:
            raise AnalysisError(f"{metric}: expected duration is not ten seconds")
        if actual_duration_us < expected_duration_us:
            raise AnalysisError(f"{metric}: actual duration is shorter than requested")
        if observed_duration_us is None:
            observed_duration_us = actual_duration_us
        elif actual_duration_us != observed_duration_us:
            raise AnalysisError("latency metrics report different run durations")
        results[metric] = normalize_record(record)
    if set(results) != EXPECTED_METRICS:
        raise AnalysisError("result metrics are incomplete")
    return results


def validate_load(record: dict[str, str], workload: str) -> dict[str, object]:
    """Validate runtime-accounted idle and stress execution."""
    if record.get("schema") != "1" or record.get("workload") != workload:
        raise AnalysisError("RTOS_BASELINE_LOAD: wrong schema or workload")
    if record.get("verified") != "true":
        raise AnalysisError("RTOS_BASELINE_LOAD: load verification failed")
    if (
        parse_nonnegative(record, "window_duration_us", "RTOS_BASELINE_LOAD")
        < 10_000_000
    ):
        raise AnalysisError("RTOS_BASELINE_LOAD: load window is too short")

    load_fields = {
        field: parse_nonnegative(record, field, "RTOS_BASELINE_LOAD")
        for field in (
            "cpu_non_idle_permille",
            "cpu_idle_permille",
            "benchmark_permille",
            "stress_permille",
        )
    }
    if any(value > 1000 for value in load_fields.values()):
        raise AnalysisError("RTOS_BASELINE_LOAD: a CPU share exceeds 1000 permille")
    accounted_cpu = (
        load_fields["cpu_non_idle_permille"] + load_fields["cpu_idle_permille"]
    )
    if not 995 <= accounted_cpu <= 1000:
        raise AnalysisError("RTOS_BASELINE_LOAD: CPU accounting is incomplete")
    if load_fields["benchmark_permille"] == 0:
        raise AnalysisError("RTOS_BASELINE_LOAD: benchmark execution was not measured")

    stress_permille = load_fields["stress_permille"]
    stress_blocks = parse_nonnegative(record, "stress_blocks", "RTOS_BASELINE_LOAD")
    stress_rate = parse_nonnegative(
        record, "stress_blocks_per_second", "RTOS_BASELINE_LOAD"
    )
    if workload == "cpu-stress":
        if (
            stress_permille < 900
            or load_fields["cpu_non_idle_permille"] < 900
            or stress_blocks == 0
            or stress_rate == 0
        ):
            raise AnalysisError("RTOS_BASELINE_LOAD: CPU stress was not sustained")
    elif (
        stress_permille != 0
        or stress_blocks != 0
        or stress_rate != 0
        or load_fields["cpu_idle_permille"] < 900
    ):
        raise AnalysisError("RTOS_BASELINE_LOAD: idle workload was not sustained")
    return normalize_record(record)


def validate_complete(record: dict[str, str], workload: str) -> dict[str, object]:
    """Validate and normalize the terminal success record."""
    if (
        record.get("schema") != "1"
        or record.get("workload") != workload
        or record.get("status") != "pass"
    ):
        raise AnalysisError("RTOS_BASELINE_COMPLETE: unsuccessful run")
    timer_misses = parse_nonnegative(
        record, "timer_misses", "RTOS_BASELINE_COMPLETE"
    )
    warmup_timer_misses = parse_nonnegative(
        record, "warmup_timer_misses", "RTOS_BASELINE_COMPLETE"
    )
    # One wake may represent the first warm-up expiration while every measured
    # expiration is already pending, but at least one warm-up expiration must
    # be the represented wake rather than a coalesced miss.
    if (
        timer_misses > MAX_MEASURED_TIMER_MISSES
        or warmup_timer_misses > MAX_WARMUP_TIMER_MISSES
    ):
        raise AnalysisError("RTOS_BASELINE_COMPLETE: invalid timer miss count")
    if parse_nonnegative(record, "early_wakes", "RTOS_BASELINE_COMPLETE") != 0:
        raise AnalysisError("RTOS_BASELINE_COMPLETE: early wake was observed")
    return normalize_record(record)


def normalize_record(record: dict[str, str]) -> dict[str, object]:
    """Convert obvious booleans and integers while preserving labels."""
    normalized: dict[str, object] = {}
    for key, value in record.items():
        if value == "true":
            normalized[key] = True
        elif value == "false":
            normalized[key] = False
        else:
            try:
                normalized[key] = int(value, 10)
            except ValueError:
                normalized[key] = value
    return normalized


def sha256(path: Path) -> str:
    """Return a file's lowercase SHA-256 digest."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def artifact(path: Path, logical_path: str) -> dict[str, object]:
    """Describe a required evidence artifact."""
    if not path.is_file():
        raise AnalysisError(f"missing artifact: {path}")
    return {
        "path": logical_path,
        "bytes": path.stat().st_size,
        "sha256": sha256(path),
    }


def command_output(command: Sequence[str]) -> str:
    """Run a metadata-only command and return its first non-empty line."""
    try:
        result = subprocess.run(
            command,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise AnalysisError(f"metadata command failed: {' '.join(command)}") from error
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if not lines:
        raise AnalysisError(f"metadata command produced no output: {' '.join(command)}")
    return lines[0]


def require_clean_source(zephyr_base: Path) -> None:
    """Reject Zephyr worktree edits that would invalidate the release pin."""
    command = [
        "git",
        "-C",
        str(zephyr_base),
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ]
    try:
        result = subprocess.run(
            command,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise AnalysisError("could not inspect the Zephyr source worktree") from error
    if result.stdout:
        raise AnalysisError("Zephyr source worktree is not clean")


def source_identity(zephyr_base: Path) -> dict[str, str]:
    """Read the pinned identity without rescanning the source worktree."""
    source = {
        "version": command_output(
            ["git", "-C", str(zephyr_base), "describe", "--tags", "--exact-match"]
        ),
        "tag_object": command_output(
            ["git", "-C", str(zephyr_base), "rev-parse", "v4.3.0"]
        ),
        "commit": command_output(
            ["git", "-C", str(zephyr_base), "rev-parse", "v4.3.0^{}"]
        ),
    }
    if source != EXPECTED_SOURCE:
        raise AnalysisError(
            f"Zephyr source does not match the pinned v4.3.0 release: {source!r}"
        )
    return source


def source_metadata(zephyr_base: Path) -> dict[str, str]:
    """Capture the exact identity and a clean-worktree attestation."""
    require_clean_source(zephyr_base)
    autocrlf = subprocess.run(
        ["git", "-C", str(zephyr_base), "config", "--get", "core.autocrlf"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    ).stdout.strip()
    return {
        **source_identity(zephyr_base),
        "worktree": "clean",
        "core_autocrlf": autocrlf or "unset",
    }


def git_index_path(zephyr_base: Path) -> Path:
    """Resolve the index used by the Zephyr checkout."""
    index = Path(
        command_output(
            ["git", "-C", str(zephyr_base), "rev-parse", "--git-path", "index"]
        )
    )
    return index if index.is_absolute() else zephyr_base / index


def load_source_provenance(
    provenance_path: Path, zephyr_base: Path, raw_log: Path
) -> tuple[dict[str, str], dict[str, object]]:
    """Validate one post-capture source attestation without rescanning NTFS."""
    try:
        provenance = json.loads(provenance_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError, UnicodeError) as error:
        raise AnalysisError("could not read source provenance") from error
    if not isinstance(provenance, dict):
        raise AnalysisError("source provenance is not a JSON object")
    if provenance.get("schema_version") != 1:
        raise AnalysisError("source provenance has the wrong schema")
    if Path(provenance.get("zephyr_base", "")).resolve() != zephyr_base.resolve():
        raise AnalysisError("source provenance belongs to another Zephyr checkout")
    source = provenance.get("source")
    if not isinstance(source, dict) or source.get("worktree") != "clean":
        raise AnalysisError("source provenance does not attest a clean worktree")
    if {field: source.get(field) for field in EXPECTED_SOURCE} != EXPECTED_SOURCE:
        raise AnalysisError("source provenance contains the wrong Zephyr release")
    if source_identity(zephyr_base) != EXPECTED_SOURCE:
        raise AnalysisError("Zephyr identity changed after provenance capture")

    index_path = git_index_path(zephyr_base)
    recorded_index = provenance.get("git_index")
    expected_index = artifact(index_path, ".git/index")
    if recorded_index != expected_index:
        raise AnalysisError("Zephyr index metadata does not match the named file")
    if provenance_path.stat().st_mtime < raw_log.stat().st_mtime:
        raise AnalysisError("source provenance predates the measured run")
    normalized_source = {str(key): str(value) for key, value in source.items()}
    return normalized_source, artifact(provenance_path, provenance_path.name)


def host_cpu_name() -> str:
    """Return the host CPU label used for the QEMU run when available."""
    try:
        cpuinfo = Path("/proc/cpuinfo").read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return platform.machine()
    for preferred_field in ("model name", "hardware", "processor"):
        for line in cpuinfo.splitlines():
            field, separator, value = line.partition(":")
            if separator and field.strip().lower() == preferred_field:
                return value.strip()
    return platform.machine()


def tool_metadata() -> dict[str, str]:
    """Record the QEMU and cross-compiler implementations used by the run."""
    qemu = shutil.which("qemu-system-aarch64")
    compiler_prefix = os.environ.get("CROSS_COMPILE", "aarch64-linux-gnu-")
    compiler = shutil.which(f"{compiler_prefix}gcc") or f"{compiler_prefix}gcc"
    if qemu is None:
        raise AnalysisError("qemu-system-aarch64 is not available")
    return {
        "qemu": command_output([qemu, "--version"]),
        "compiler": command_output([compiler, "--version"]),
        "python": platform.python_version(),
        "host_kernel": platform.release(),
        "host_cpu": host_cpu_name(),
    }


def analyze(
    raw_log: Path,
    build_log: Path,
    build_dir: Path,
    zephyr_base: Path,
    source_provenance: Path,
    workload: str,
) -> dict[str, object]:
    """Validate one complete run and assemble self-contained evidence."""
    raw_text = raw_log.read_text(encoding="utf-8", errors="replace")
    lines = raw_text.splitlines()
    if select_records(lines, "RTOS_BASELINE_FATAL"):
        raise AnalysisError("console contains RTOS_BASELINE_FATAL")

    config = validate_config(require_one(lines, "RTOS_BASELINE_CONFIG"), workload)
    validate_workload_ready(
        require_one(lines, "RTOS_BASELINE_WORKLOAD_READY"), workload
    )
    results = validate_results(select_records(lines, "RTOS_BASELINE_RESULT"), workload)
    load = validate_load(require_one(lines, "RTOS_BASELINE_LOAD"), workload)
    completion = validate_complete(
        require_one(lines, "RTOS_BASELINE_COMPLETE"), workload
    )
    source, provenance_artifact = load_source_provenance(
        source_provenance, zephyr_base, raw_log
    )

    return {
        "schema_version": 1,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "scope": "native Zephyr on QEMU; no AxVisor or guest virtualization",
        "workload": workload,
        "configuration": config,
        "metrics": results,
        "load": load,
        "completion": completion,
        "source": source,
        "tools": tool_metadata(),
        "artifacts": {
            "raw_console": artifact(raw_log, raw_log.name),
            "build_log": artifact(build_log, build_log.name),
            "dot_config": artifact(
                build_dir / "zephyr" / ".config", "build/zephyr/.config"
            ),
            "elf": artifact(
                build_dir / "zephyr" / "zephyr.elf", "build/zephyr/zephyr.elf"
            ),
            "binary": artifact(
                build_dir / "zephyr" / "zephyr.bin", "build/zephyr/zephyr.bin"
            ),
            "source_provenance": provenance_artifact,
        },
        "method": {
            "period": "1 ms absolute Zephyr k_timer period",
            "clock": "k_cycle_get_64 backed by the AArch64 architected timer",
            "wake_lateness": "task observation minus absolute timer deadline",
            "dispatch_latency": (
                "task observation minus timer expiry callback timestamp"
            ),
            "percentile": "nearest-rank over 10,000 samples",
            "timer_misses": (
                "coalesced measured deadlines; warm-up coalescing is "
                "reported separately"
            ),
            "measurement_logging": (
                "no console output occurs while samples are collected"
            ),
        },
    }


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("raw_log", type=Path)
    parser.add_argument("--build-log", required=True, type=Path)
    parser.add_argument("--build-dir", required=True, type=Path)
    parser.add_argument("--zephyr-base", required=True, type=Path)
    parser.add_argument("--source-provenance", required=True, type=Path)
    parser.add_argument("--workload", required=True, choices=("idle", "cpu-stress"))
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Command-line entry point."""
    args = parse_args(sys.argv[1:] if argv is None else argv)
    temporary_output = args.output.with_name(f".{args.output.name}.{os.getpid()}.tmp")
    try:
        if args.output.exists():
            raise AnalysisError(f"refusing to overwrite evidence: {args.output}")
        result = analyze(
            args.raw_log,
            args.build_log,
            args.build_dir,
            args.zephyr_base,
            args.source_provenance,
            args.workload,
        )
        rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
        temporary_output.write_text(rendered, encoding="utf-8")
        os.link(temporary_output, args.output)
    except (AnalysisError, OSError, UnicodeError) as error:
        print(f"analysis failed: {error}", file=sys.stderr)
        return 2
    finally:
        temporary_output.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
