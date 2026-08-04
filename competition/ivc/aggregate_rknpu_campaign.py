#!/usr/bin/env python3
"""Validate and aggregate repeated StarryOS RK3588 NPU board runs."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import statistics
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path
from typing import Sequence


MANIFEST_FILES = (
    "console.log",
    "console.log.gz",
    "metadata.json",
    "summary.json",
    "raw.csv",
    "raw.csv.gz",
    "rknn.csv",
    "rknn.csv.gz",
    "stage.log",
)
CONTROLLER_METRICS = (
    "full_loop_p50_us",
    "full_loop_p95_us",
    "full_loop_p99_us",
    "full_loop_max_us",
    "pre_send_p50_us",
    "pre_send_p95_us",
    "pre_send_p99_us",
    "pre_send_max_us",
    "transport_p50_us",
    "transport_p95_us",
    "transport_p99_us",
    "transport_max_us",
    "throughput_msg_s",
)
RKNN_METRICS = (
    "device_p50_us",
    "device_p99_us",
    "device_max_us",
    "wall_p50_ns",
    "wall_p99_ns",
    "wall_max_ns",
    "initialization_us",
)
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
MANIFEST_PATTERN = re.compile(r"([0-9a-f]{64})  ([^/\\]+)")
DEFAULT_MODEL_SHA256 = (
    "2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8"
)


class AggregationError(ValueError):
    """Raised when a run set cannot form an authoritative campaign."""


def sha256_file(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise AggregationError(f"cannot read {path}: {error}") from error


def load_object(path: Path, label: str) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AggregationError(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise AggregationError(f"{label} must be a JSON object")
    return value


def require_object(
    parent: dict[str, object], key: str, label: str
) -> dict[str, object]:
    value = parent.get(key)
    if not isinstance(value, dict):
        raise AggregationError(f"{label} {key} must be an object")
    return value


def require_string(parent: dict[str, object], key: str, label: str) -> str:
    value = parent.get(key)
    if not isinstance(value, str) or not value:
        raise AggregationError(f"{label} {key} must be a nonempty string")
    return value


def require_integer(parent: dict[str, object], key: str, label: str) -> int:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, int):
        raise AggregationError(f"{label} {key} must be an integer")
    return value


def require_number(parent: dict[str, object], key: str, label: str) -> int | float:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise AggregationError(f"{label} {key} must be a number")
    return value


def require_equal(
    parent: dict[str, object], key: str, expected: object, label: str
) -> None:
    if parent.get(key) != expected:
        raise AggregationError(
            f"{label} {key} must be {expected!r}, got {parent.get(key)!r}"
        )


def require_sha256(parent: dict[str, object], key: str, label: str) -> str:
    value = require_string(parent, key, label)
    if SHA256_PATTERN.fullmatch(value) is None:
        raise AggregationError(f"{label} {key} is not a SHA-256 digest")
    return value


def parse_timestamp(value: str, label: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise AggregationError(f"{label} is not an ISO-8601 timestamp") from error
    if parsed.tzinfo is None:
        raise AggregationError(f"{label} must include a timezone")
    return parsed


def validate_manifest(run_dir: Path) -> dict[str, str]:
    manifest_path = run_dir / "checksums.sha256"
    try:
        lines = manifest_path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise AggregationError(f"cannot read {manifest_path}: {error}") from error
    records: dict[str, str] = {}
    for line in lines:
        match = MANIFEST_PATTERN.fullmatch(line)
        if match is None:
            raise AggregationError(f"malformed checksum line in {manifest_path}")
        digest, name = match.groups()
        if name in records:
            raise AggregationError(f"duplicate checksum for {name} in {manifest_path}")
        records[name] = digest
    if set(records) != set(MANIFEST_FILES):
        raise AggregationError(
            f"{run_dir.name} checksum manifest does not contain the exact run artifacts"
        )
    for name, expected_digest in records.items():
        if sha256_file(run_dir / name) != expected_digest:
            raise AggregationError(f"{run_dir.name} checksum mismatch for {name}")
    return records


def validate_input_record(record: dict[str, object], label: str) -> None:
    require_string(record, "path", label)
    require_sha256(record, "sha256", label)
    if require_integer(record, "size_bytes", label) <= 0:
        raise AggregationError(f"{label} size_bytes must be positive")


def validate_output_record(
    record: dict[str, object], artifact: Path, label: str
) -> None:
    stored_path = Path(require_string(record, "path", label))
    if stored_path.name != artifact.name:
        raise AggregationError(f"{label} path does not name {artifact.name}")
    require_equal(record, "sha256", sha256_file(artifact), label)
    require_equal(record, "size_bytes", artifact.stat().st_size, label)


def validate_compact_hash_markers(
    console_path: Path, prefix: str, expected_digest: str, label: str
) -> int:
    try:
        console = console_path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        raise AggregationError(f"cannot read {console_path}: {error}") from error
    matches = re.findall(re.escape(prefix) + r"([0-9a-f]{64})", console)
    if len(matches) < 2:
        raise AggregationError(f"{label} lacks a two-record UART quorum")
    counts = Counter(matches)
    if set(counts) != {expected_digest}:
        raise AggregationError(f"{label} contains a conflicting UART digest")
    return counts[expected_digest]


def validate_metadata(
    metadata: dict[str, object],
    run_dir: Path,
    run_number: int,
    expected_runs: int,
    expected_count: int,
    expected_commit: str | None,
    expected_model_sha256: str,
    expected_runtime_api: str,
) -> dict[str, object]:
    label = run_dir.name
    require_equal(metadata, "schema_version", 1, label)
    source = require_object(metadata, "source", label)
    commit = require_string(source, "commit", f"{label} source")
    if COMMIT_PATTERN.fullmatch(commit) is None:
        raise AggregationError(f"{label} source commit is malformed")
    if expected_commit is not None and commit != expected_commit:
        raise AggregationError(f"{label} source commit differs from the requested commit")
    require_equal(source, "dirty", False, f"{label} source")
    require_equal(source, "tracked_change_count", 0, f"{label} source")
    require_equal(source, "untracked_file_count", 0, f"{label} source")

    run = require_object(metadata, "run", label)
    expected_run_id = f"run-{run_number:03d}"
    require_equal(run, "run_id", expected_run_id, f"{label} run")
    require_equal(run, "run_number", run_number, f"{label} run")
    require_equal(run, "execution_order", run_number, f"{label} run")
    require_equal(run, "repeat_count", expected_runs, f"{label} run")
    require_equal(run, "profile", "rknpu-full", f"{label} run")
    require_equal(run, "exit_status", 0, f"{label} run")
    started_at = require_string(run, "started_at", f"{label} run")
    finished_at = require_string(run, "finished_at", f"{label} run")
    started = parse_timestamp(started_at, f"{label} started_at")
    finished = parse_timestamp(finished_at, f"{label} finished_at")
    if finished <= started:
        raise AggregationError(f"{label} finished_at must follow started_at")

    result = require_object(metadata, "result", label)
    require_equal(result, "validated", True, f"{label} result")
    require_equal(result, "successful_marker", True, f"{label} result")
    require_equal(result, "controller_policy", "neural", f"{label} result")
    require_equal(result, "sample_count", expected_count, f"{label} result")
    require_equal(result, "rknn_sample_count", expected_count, f"{label} result")
    require_equal(result, "dropped_samples", 0, f"{label} result")

    model = require_object(metadata, "model", label)
    require_equal(model, "id", "thermal-4x6x1-v1", f"{label} model")
    require_equal(model, "backend", "rknn-npu", f"{label} model")
    require_equal(
        model, "runtime_version", expected_runtime_api, f"{label} model"
    )
    model_artifact = require_object(model, "artifact", f"{label} model")
    validate_input_record(model_artifact, f"{label} model artifact")
    require_equal(
        model_artifact,
        "sha256",
        expected_model_sha256,
        f"{label} model artifact",
    )

    inputs = require_object(metadata, "inputs", label)
    for input_name in (
        "board_config",
        "build_config",
        "rootfs",
        "starry_dtb",
        "starry_kernel",
        "zephyr_guest",
    ):
        validate_input_record(
            require_object(inputs, input_name, f"{label} inputs"),
            f"{label} input {input_name}",
        )

    outputs = require_object(metadata, "outputs", label)
    output_files = {
        "console_log": "console.log.gz",
        "raw_csv": "raw.csv.gz",
        "rknn_csv": "rknn.csv.gz",
        "summary": "summary.json",
    }
    for output_name, filename in output_files.items():
        validate_output_record(
            require_object(outputs, output_name, f"{label} outputs"),
            run_dir / filename,
            f"{label} output {output_name}",
        )

    board = require_object(metadata, "board", label)
    board_id = require_string(board, "id", f"{label} board")
    hostname = require_string(board, "hostname", f"{label} board")
    return {
        "commit": commit,
        "branch": require_string(source, "branch", f"{label} source"),
        "started_at": started_at,
        "finished_at": finished_at,
        "started": started,
        "finished": finished,
        "board_id": board_id,
        "hostname": hostname,
        "cpu_temp_milli_c": require_integer(
            board, "cpu_temp_milli_c", f"{label} board"
        ),
        "input_identity": json.dumps(
            {"inputs": inputs, "model": model}, sort_keys=True
        ),
    }


def validate_latency_order(
    metrics: dict[str, object], prefix: str, label: str
) -> None:
    values = [
        require_number(metrics, f"{prefix}_{suffix}", label)
        for suffix in ("p50_us", "p95_us", "p99_us", "max_us")
    ]
    if values != sorted(values):
        raise AggregationError(f"{label} {prefix} latency percentiles are unordered")


def validate_summary(
    summary: dict[str, object],
    run_dir: Path,
    expected_count: int,
    expected_model_sha256: str,
    expected_runtime_api: str,
) -> dict[str, object]:
    label = run_dir.name
    require_equal(summary, "schema_version", 2, label)
    require_equal(summary, "platform", "orangepi-5-plus", label)
    require_equal(summary, "guest", "starryos", label)
    require_equal(summary, "profile", "normal", label)

    starry = require_object(summary, "starry", label)
    require_equal(starry, "mode", "neural", f"{label} starry")
    require_equal(starry, "backend", "rknn-npu", f"{label} starry")
    require_equal(starry, "fault_profile", "none", f"{label} starry")
    require_equal(starry, "count", expected_count, f"{label} starry")
    require_equal(starry, "period_ms", 100, f"{label} starry")
    require_equal(starry, "vcpus", 2, f"{label} starry")

    controller = require_object(summary, "controller", label)
    require_equal(controller, "policy", "neural", f"{label} controller")
    require_equal(controller, "sent", expected_count, f"{label} controller")
    require_equal(
        controller, "acknowledged", expected_count, f"{label} controller"
    )
    for field in ("errors", "timeouts", "retransmissions", "recoveries"):
        require_equal(controller, field, 0, f"{label} controller")
    require_equal(controller, "success_percent", 100, f"{label} controller")
    deadline_misses = require_integer(
        controller, "deadline_misses", f"{label} controller"
    )
    if deadline_misses < 0:
        raise AggregationError(f"{label} deadline_misses cannot be negative")
    for prefix in ("full_loop", "pre_send", "transport"):
        validate_latency_order(controller, prefix, f"{label} controller")
    for metric in CONTROLLER_METRICS:
        require_number(controller, metric, f"{label} controller")

    rtos = require_object(summary, "rtos", label)
    for field in ("accepted", "applied", "status_sent", "acks_sent"):
        require_equal(rtos, field, expected_count, f"{label} rtos")
    for field in ("duplicates", "acks_dropped", "errors_sent", "protocol_errors"):
        require_equal(rtos, field, 0, f"{label} rtos")

    lifecycle = require_object(summary, "lifecycle", label)
    for field in (
        "board_linux_restored",
        "host_filesystem_synced",
        "rtos_powered_off",
        "starry_done",
        "volatile_block_snapshotted",
    ):
        require_equal(lifecycle, field, True, f"{label} lifecycle")
    snapshot = require_object(lifecycle, "block_snapshot", f"{label} lifecycle")
    require_equal(snapshot, "filesystem_check", "clean", f"{label} snapshot")
    require_sha256(snapshot, "image_sha256", f"{label} snapshot")

    raw = require_object(summary, "raw_samples", label)
    require_equal(raw, "sample_count", expected_count, f"{label} raw")
    require_equal(raw, "dropped_samples", 0, f"{label} raw")
    raw_digest = require_sha256(raw, "sha256", f"{label} raw")
    require_equal(raw, "guest_manifest_sha256", raw_digest, f"{label} raw")
    require_equal(raw, "uart_sha256", raw_digest, f"{label} raw")
    require_equal(raw, "uart_sha256_complete", True, f"{label} raw")
    require_equal(
        raw,
        "artifact_sha256",
        sha256_file(run_dir / "raw.csv.gz"),
        f"{label} raw",
    )
    if sha256_file(run_dir / "raw.csv") != raw_digest:
        raise AggregationError(f"{label} raw CSV content digest differs from summary")

    rknn = require_object(summary, "rknn_samples", label)
    for field in ("sample_count", "positive_device_times", "actuator_matches"):
        require_equal(rknn, field, expected_count, f"{label} rknn")
    require_equal(rknn, "core_mask", 0, f"{label} rknn")
    require_equal(rknn, "runtime_api", expected_runtime_api, f"{label} rknn")
    require_equal(rknn, "model_sha256", expected_model_sha256, f"{label} rknn")
    require_string(rknn, "runtime_driver", f"{label} rknn")
    rknn_digest = require_sha256(rknn, "sha256", f"{label} rknn")
    require_equal(
        rknn, "guest_manifest_sha256", rknn_digest, f"{label} rknn"
    )
    require_equal(
        rknn,
        "artifact_sha256",
        sha256_file(run_dir / "rknn.csv.gz"),
        f"{label} rknn",
    )
    if sha256_file(run_dir / "rknn.csv") != rknn_digest:
        raise AggregationError(f"{label} RKNN CSV content digest differs from summary")
    for metric in RKNN_METRICS:
        require_number(rknn, metric, f"{label} rknn")
    if not (
        require_number(rknn, "device_p50_us", f"{label} rknn")
        <= require_number(rknn, "device_p99_us", f"{label} rknn")
        <= require_number(rknn, "device_max_us", f"{label} rknn")
    ):
        raise AggregationError(f"{label} RKNN device latency percentiles are unordered")
    if not (
        require_number(rknn, "wall_p50_ns", f"{label} rknn")
        <= require_number(rknn, "wall_p99_ns", f"{label} rknn")
        <= require_number(rknn, "wall_max_ns", f"{label} rknn")
    ):
        raise AggregationError(f"{label} RKNN wall latency percentiles are unordered")

    source_log = require_object(summary, "source_log", label)
    require_equal(
        source_log,
        "content_sha256",
        sha256_file(run_dir / "console.log"),
        f"{label} source log",
    )
    require_equal(
        source_log,
        "sha256",
        sha256_file(run_dir / "console.log.gz"),
        f"{label} source log",
    )
    rknn_uart_votes = validate_compact_hash_markers(
        run_dir / "console.log",
        "IVC-STARRY-RKNN-RAW sha256=",
        rknn_digest,
        f"{label} RKNN evidence",
    )
    model_uart_votes = validate_compact_hash_markers(
        run_dir / "console.log",
        "IVC-STARRY-RKNN-MODEL sha256=",
        expected_model_sha256,
        f"{label} model evidence",
    )
    return {
        "controller": controller,
        "rknn": rknn,
        "deadline_misses": deadline_misses,
        "raw_sha256": raw_digest,
        "rknn_sha256": rknn_digest,
        "rknn_uart_votes": rknn_uart_votes,
        "model_uart_votes": model_uart_votes,
    }


def describe(values: list[int | float]) -> dict[str, object]:
    return {
        "values": values,
        "min": min(values),
        "median": statistics.median(values),
        "max": max(values),
        "range": max(values) - min(values),
    }


def aggregate_campaign(
    campaign_root: Path,
    expected_runs: int = 5,
    expected_count: int = 1_800,
    expected_commit: str | None = None,
    expected_model_sha256: str = DEFAULT_MODEL_SHA256,
    expected_runtime_api: str = "2.3.2",
) -> dict[str, object]:
    if expected_runs <= 0 or expected_count <= 0:
        raise AggregationError("expected runs and samples must be positive")
    if (
        expected_commit is not None
        and COMMIT_PATTERN.fullmatch(expected_commit) is None
    ):
        raise AggregationError("expected commit must be a 40-character Git SHA")
    if SHA256_PATTERN.fullmatch(expected_model_sha256) is None:
        raise AggregationError("expected model digest is malformed")
    root = campaign_root.resolve()
    expected_names = [f"run-{number:03d}" for number in range(1, expected_runs + 1)]
    actual_names = sorted(path.name for path in root.glob("run-*") if path.is_dir())
    if actual_names != expected_names:
        raise AggregationError(
            f"campaign runs must be exactly {expected_names}, got {actual_names}"
        )

    run_records: list[dict[str, object]] = []
    commits: set[str] = set()
    branches: set[str] = set()
    board_ids: set[str] = set()
    hostnames: set[str] = set()
    input_identities: set[str] = set()
    runtime_drivers: set[str] = set()
    controller_records: list[dict[str, object]] = []
    rknn_records: list[dict[str, object]] = []
    previous_finished: datetime | None = None
    for run_number, run_name in enumerate(expected_names, start=1):
        run_dir = root / run_name
        manifest = validate_manifest(run_dir)
        summary = load_object(run_dir / "summary.json", f"{run_name} summary")
        metadata = load_object(run_dir / "metadata.json", f"{run_name} metadata")
        metadata_record = validate_metadata(
            metadata,
            run_dir,
            run_number,
            expected_runs,
            expected_count,
            expected_commit,
            expected_model_sha256,
            expected_runtime_api,
        )
        summary_record = validate_summary(
            summary,
            run_dir,
            expected_count,
            expected_model_sha256,
            expected_runtime_api,
        )
        started = metadata_record["started"]
        finished = metadata_record["finished"]
        assert isinstance(started, datetime) and isinstance(finished, datetime)
        if previous_finished is not None and started < previous_finished:
            raise AggregationError(f"{run_name} overlaps the previous physical run")
        previous_finished = finished
        commits.add(str(metadata_record["commit"]))
        branches.add(str(metadata_record["branch"]))
        board_ids.add(str(metadata_record["board_id"]))
        hostnames.add(str(metadata_record["hostname"]))
        input_identities.add(str(metadata_record["input_identity"]))
        rknn = summary_record["rknn"]
        assert isinstance(rknn, dict)
        runtime_drivers.add(str(rknn["runtime_driver"]))
        controller = summary_record["controller"]
        assert isinstance(controller, dict)
        controller_records.append(controller)
        rknn_records.append(rknn)
        run_records.append(
            {
                "run_id": run_name,
                "started_at": metadata_record["started_at"],
                "finished_at": metadata_record["finished_at"],
                "cpu_temp_milli_c": metadata_record["cpu_temp_milli_c"],
                "controller": {
                    field: controller[field]
                    for field in (
                        "sent",
                        "acknowledged",
                        "errors",
                        "timeouts",
                        "retransmissions",
                        "recoveries",
                        "deadline_misses",
                        *CONTROLLER_METRICS,
                    )
                },
                "rknn": {
                    field: rknn[field]
                    for field in (
                        "sample_count",
                        "runtime_api",
                        "runtime_driver",
                        "core_mask",
                        *RKNN_METRICS,
                    )
                },
                "evidence": {
                    "checksums_sha256": sha256_file(run_dir / "checksums.sha256"),
                    "summary_sha256": manifest["summary.json"],
                    "metadata_sha256": manifest["metadata.json"],
                    "raw_sha256": summary_record["raw_sha256"],
                    "rknn_sha256": summary_record["rknn_sha256"],
                    "rknn_uart_votes": summary_record["rknn_uart_votes"],
                    "model_uart_votes": summary_record["model_uart_votes"],
                },
            }
        )

    for values, label in (
        (commits, "commits"),
        (branches, "branches"),
        (board_ids, "board IDs"),
        (hostnames, "hostnames"),
        (input_identities, "input artifact sets"),
        (runtime_drivers, "RKNN driver versions"),
    ):
        if len(values) != 1:
            raise AggregationError(f"campaign mixes multiple {label}")

    total_samples = expected_runs * expected_count
    total_deadline_misses = sum(
        require_integer(record, "deadline_misses", "campaign controller")
        for record in controller_records
    )
    controller_statistics = {
        metric: describe(
            [
                require_number(record, metric, "campaign controller")
                for record in controller_records
            ]
        )
        for metric in CONTROLLER_METRICS
    }
    rknn_statistics = {
        metric: describe(
            [
                require_number(record, metric, "campaign RKNN")
                for record in rknn_records
            ]
        )
        for metric in RKNN_METRICS
    }
    return {
        "schema_version": 1,
        "campaign": {
            "path": str(root),
            "profile": "rknpu-full",
            "run_count": expected_runs,
            "samples_per_run": expected_count,
            "total_samples": total_samples,
        },
        "source": {
            "commit": next(iter(commits)),
            "branch": next(iter(branches)),
            "dirty": False,
        },
        "board": {
            "id": next(iter(board_ids)),
            "hostname": next(iter(hostnames)),
        },
        "model": {
            "id": "thermal-4x6x1-v1",
            "backend": "rknn-npu",
            "sha256": expected_model_sha256,
            "runtime_api": expected_runtime_api,
            "runtime_driver": next(iter(runtime_drivers)),
        },
        "reliability": {
            "sent": total_samples,
            "acknowledged": total_samples,
            "errors": 0,
            "timeouts": 0,
            "retransmissions": 0,
            "recoveries": 0,
            "success_percent": 100.0,
            "deadline_misses": total_deadline_misses,
            "deadline_miss_percent": total_deadline_misses * 100 / total_samples,
            "runs_with_deadline_misses": sum(
                int(
                    require_integer(
                        record, "deadline_misses", "campaign controller"
                    )
                    > 0
                )
                for record in controller_records
            ),
        },
        "statistics": {
            "controller": controller_statistics,
            "rknn": rknn_statistics,
        },
        "runs": run_records,
    }


def write_json(path: Path, value: object) -> None:
    if path.exists():
        raise AggregationError(f"refusing to overwrite existing output {path}")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    except OSError as error:
        raise AggregationError(f"cannot write {path}: {error}") from error


def parse_arguments(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate and aggregate a StarryOS RK3588 NPU board campaign."
    )
    parser.add_argument("campaign_root", type=Path)
    parser.add_argument("--expected-runs", type=int, default=5)
    parser.add_argument("--expected-count", type=int, default=1_800)
    parser.add_argument("--expected-commit")
    parser.add_argument(
        "--expected-model-sha256", default=DEFAULT_MODEL_SHA256
    )
    parser.add_argument("--expected-runtime-api", default="2.3.2")
    parser.add_argument("--output", type=Path)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        result = aggregate_campaign(
            arguments.campaign_root,
            expected_runs=arguments.expected_runs,
            expected_count=arguments.expected_count,
            expected_commit=arguments.expected_commit,
            expected_model_sha256=arguments.expected_model_sha256,
            expected_runtime_api=arguments.expected_runtime_api,
        )
        if arguments.output is None:
            json.dump(result, sys.stdout, indent=2, sort_keys=True, allow_nan=False)
            sys.stdout.write("\n")
        else:
            write_json(arguments.output, result)
            print(f"RKNN campaign aggregation passed: {arguments.output}")
    except AggregationError as error:
        print(f"RKNN campaign aggregation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
