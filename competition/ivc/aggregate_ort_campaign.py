#!/usr/bin/env python3
"""Validate and aggregate repeated StarryOS ONNX Runtime control runs."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import statistics
import sys
from datetime import datetime
from pathlib import Path
from typing import Sequence

try:
    import analyze_board as board_analysis
    import ort_campaign_contract as campaign_contract
except ModuleNotFoundError:
    from competition.ivc import analyze_board as board_analysis
    from competition.ivc import ort_campaign_contract as campaign_contract


MANIFEST_FILES = (
    "console.log",
    "console.log.gz",
    "metadata.json",
    "summary.json",
    "raw.csv",
    "raw.csv.gz",
    "ort.csv",
    "ort.csv.gz",
    "stage.log",
)
MANIFEST_PATTERN = re.compile(r"([0-9a-f]{64})  ([^/\\]+)")


class AggregationError(ValueError):
    """Raised when evidence cannot satisfy the frozen ORT campaign contract."""


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
    if campaign_contract.SHA256_PATTERN.fullmatch(value) is None:
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
        raise AggregationError(f"{run_dir.name} checksum manifest has the wrong file set")
    for name, expected_digest in records.items():
        if sha256_file(run_dir / name) != expected_digest:
            raise AggregationError(f"{run_dir.name} checksum mismatch for {name}")
    return records


def validate_output_record(
    record: dict[str, object], artifact: Path, label: str
) -> None:
    if Path(require_string(record, "path", label)).name != artifact.name:
        raise AggregationError(f"{label} path does not name {artifact.name}")
    require_equal(record, "sha256", sha256_file(artifact), label)
    require_equal(record, "size_bytes", artifact.stat().st_size, label)


def validate_metadata(
    metadata: dict[str, object],
    run_dir: Path,
    run_number: int,
    expected_runs: int,
    expected_count: int,
    expected_commit: str,
    expected_model_sha256: str,
    expected_runtime_version: str,
) -> dict[str, object]:
    label = run_dir.name
    require_equal(metadata, "schema_version", 1, label)
    source = require_object(metadata, "source", label)
    require_equal(source, "commit", expected_commit, f"{label} source")
    require_equal(source, "dirty", False, f"{label} source")
    require_equal(source, "tracked_change_count", 0, f"{label} source")
    require_equal(source, "untracked_file_count", 0, f"{label} source")

    run = require_object(metadata, "run", label)
    require_equal(run, "run_id", f"run-{run_number:03d}", f"{label} run")
    require_equal(run, "run_number", run_number, f"{label} run")
    require_equal(run, "execution_order", run_number, f"{label} run")
    require_equal(run, "repeat_count", expected_runs, f"{label} run")
    require_equal(run, "profile", "ort-full", f"{label} run")
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
    require_equal(result, "ort_sample_count", expected_count, f"{label} result")
    require_equal(result, "dropped_samples", 0, f"{label} result")

    model = require_object(metadata, "model", label)
    require_equal(model, "id", "thermal-4x6x1-v1", f"{label} model")
    require_equal(model, "backend", "onnxruntime", f"{label} model")
    require_equal(
        model, "runtime_version", expected_runtime_version, f"{label} model"
    )
    model_artifact = require_object(model, "artifact", f"{label} model")
    require_equal(
        model_artifact,
        "sha256",
        expected_model_sha256,
        f"{label} model artifact",
    )

    outputs = require_object(metadata, "outputs", label)
    for key, filename in (
        ("console_log", "console.log.gz"),
        ("raw_csv", "raw.csv.gz"),
        ("ort_csv", "ort.csv.gz"),
        ("summary", "summary.json"),
    ):
        validate_output_record(
            require_object(outputs, key, f"{label} outputs"),
            run_dir / filename,
            f"{label} output {key}",
        )
    if outputs.get("rknn_csv") is not None:
        raise AggregationError(f"{label} must not contain RKNN evidence")

    board = require_object(metadata, "board", label)
    inputs = require_object(metadata, "inputs", label)
    return {
        "branch": require_string(source, "branch", f"{label} source"),
        "board_id": require_string(board, "id", f"{label} board"),
        "hostname": require_string(board, "hostname", f"{label} board"),
        "cpu_temp_milli_c": require_integer(
            board, "cpu_temp_milli_c", f"{label} board"
        ),
        "started_at": started_at,
        "finished_at": finished_at,
        "started": started,
        "finished": finished,
        "input_identity": json.dumps(
            {"inputs": inputs, "model": model}, sort_keys=True
        ),
    }


def validate_deadline_contract(rows: list[dict[str, int]]) -> dict[str, object]:
    if len(rows) < 2:
        raise AggregationError("deadline contract requires at least two samples")
    miss_sequences = [
        row["sequence"]
        for row in rows
        if row["full_loop_us"] > campaign_contract.PERIOD_US
    ]
    if len(miss_sequences) > campaign_contract.MAX_DEADLINE_MISSES_PER_RUN:
        raise AggregationError("run exceeds the frozen deadline miss allowance")
    if any(sequence != 1 for sequence in miss_sequences):
        raise AggregationError("only sequence 1 may be a cold-start deadline miss")
    post_first = rows[1:]
    if any(
        row["full_loop_us"] > campaign_contract.PERIOD_US for row in post_first
    ):
        raise AggregationError("post-first control cycles must have zero deadline misses")
    post_latencies = sorted(row["full_loop_us"] for row in post_first)
    return {
        "miss_sequences": miss_sequences,
        "post_first_sample_count": len(post_first),
        "post_first_deadline_misses": 0,
        "post_first_full_loop_p99_us": board_analysis.percentile(
            post_latencies, 99
        ),
        "post_first_full_loop_max_us": post_latencies[-1],
    }


def validate_ort_timing_contract(
    wall_times_ns: list[int], initialization_us: int
) -> dict[str, int]:
    if not wall_times_ns or any(value <= 0 for value in wall_times_ns):
        raise AggregationError("ORT wall timings must be positive")
    sorted_times = sorted(wall_times_ns)
    wall_p99_ns = board_analysis.percentile(sorted_times, 99)
    wall_max_ns = sorted_times[-1]
    if not 0 < initialization_us <= campaign_contract.MAX_INITIALIZATION_US:
        raise AggregationError("ORT initialization exceeds the frozen budget")
    if wall_p99_ns > campaign_contract.MAX_ORT_WALL_P99_NS:
        raise AggregationError("ORT wall p99 exceeds the frozen budget")
    if wall_max_ns > campaign_contract.MAX_ORT_WALL_NS:
        raise AggregationError("ORT wall maximum exceeds the frozen budget")
    return {
        "wall_p50_ns": board_analysis.percentile(sorted_times, 50),
        "wall_p99_ns": wall_p99_ns,
        "wall_max_ns": wall_max_ns,
    }


def validate_uart_quorum(
    console_path: Path, prefix: str, expected_digest: str, label: str
) -> int:
    try:
        console = console_path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        raise AggregationError(f"cannot read {console_path}: {error}") from error
    votes = console.count(prefix + expected_digest)
    if votes < 2:
        raise AggregationError(f"{label} lacks a two-record UART quorum")
    return votes


def validate_summary(
    summary: dict[str, object],
    run_dir: Path,
    expected_count: int,
    expected_model_sha256: str,
    expected_runtime_version: str,
) -> dict[str, object]:
    label = run_dir.name
    require_equal(summary, "schema_version", 2, label)
    require_equal(summary, "platform", "orangepi-5-plus", label)
    require_equal(summary, "guest", "starryos", label)
    require_equal(summary, "profile", "normal", label)
    starry = require_object(summary, "starry", label)
    for key, expected in (
        ("mode", "neural"),
        ("backend", "onnxruntime"),
        ("fault_profile", "none"),
        ("count", expected_count),
        ("period_ms", 100),
        ("vcpus", 2),
    ):
        require_equal(starry, key, expected, f"{label} starry")

    raw_rows = board_analysis.read_raw_rows(run_dir / "raw.csv", expected_count)
    ort_rows = board_analysis.read_ort_rows(run_dir / "ort.csv", expected_count)
    controller = require_object(summary, "controller", label)
    derived = board_analysis.derive_raw_metrics(raw_rows, 100)
    board_analysis.cross_check_raw_metrics(controller, derived)
    require_equal(controller, "policy", "neural", f"{label} controller")
    for key, expected in (
        ("sent", expected_count),
        ("acknowledged", expected_count),
        ("errors", 0),
        ("timeouts", 0),
        ("retransmissions", 0),
        ("recoveries", 0),
        ("success_percent", 100.0),
    ):
        require_equal(controller, key, expected, f"{label} controller")
    if (
        require_integer(controller, "full_loop_p99_us", f"{label} controller")
        > campaign_contract.MAX_FULL_LOOP_P99_US
    ):
        raise AggregationError(f"{label} full-loop p99 exceeds the frozen budget")
    if (
        require_integer(controller, "full_loop_max_us", f"{label} controller")
        > campaign_contract.MAX_FULL_LOOP_US
    ):
        raise AggregationError(f"{label} full-loop maximum exceeds the frozen budget")
    if (
        require_number(controller, "throughput_msg_s", f"{label} controller")
        < campaign_contract.MIN_THROUGHPUT_MSG_S
    ):
        raise AggregationError(f"{label} throughput is below the frozen budget")
    deadline = validate_deadline_contract(raw_rows)
    require_equal(
        controller,
        "deadline_misses",
        len(deadline["miss_sequences"]),
        f"{label} controller",
    )

    for raw_row, ort_row in zip(raw_rows, ort_rows, strict=True):
        if raw_row["command_actuator_permille"] != ort_row["actuator_permille"]:
            raise AggregationError(f"{label} ORT actuator differs from raw evidence")
    ort = require_object(summary, "ort_samples", label)
    initialization_us = require_integer(ort, "initialization_us", f"{label} ORT")
    ort_timing = validate_ort_timing_contract(
        [row["wall_ns"] for row in ort_rows], initialization_us
    )
    for key, expected in (
        ("sample_count", expected_count),
        ("actuator_matches", expected_count),
        ("runtime_version", expected_runtime_version),
        ("provider", campaign_contract.EXPECTED_PROVIDER),
        ("model_sha256", expected_model_sha256),
        *ort_timing.items(),
    ):
        require_equal(ort, key, expected, f"{label} ORT")

    raw_digest = sha256_file(run_dir / "raw.csv")
    ort_digest = sha256_file(run_dir / "ort.csv")
    raw = require_object(summary, "raw_samples", label)
    for key, expected in (
        ("sample_count", expected_count),
        ("dropped_samples", 0),
        ("sha256", raw_digest),
        ("guest_manifest_sha256", raw_digest),
        ("uart_sha256", raw_digest),
        ("uart_sha256_complete", True),
        ("artifact_sha256", sha256_file(run_dir / "raw.csv.gz")),
    ):
        require_equal(raw, key, expected, f"{label} raw")
    for key, expected in (
        ("sha256", ort_digest),
        ("guest_manifest_sha256", ort_digest),
        ("artifact_sha256", sha256_file(run_dir / "ort.csv.gz")),
    ):
        require_equal(ort, key, expected, f"{label} ORT")

    rtos = require_object(summary, "rtos", label)
    for key, expected in (
        ("profile", "normal"),
        ("accepted", expected_count),
        ("applied", expected_count),
        ("status_sent", expected_count),
        ("acks_sent", expected_count),
        ("duplicates", 0),
        ("acks_dropped", 0),
        ("errors_sent", 0),
        ("protocol_errors", 0),
    ):
        require_equal(rtos, key, expected, f"{label} RTOS")
    lifecycle = require_object(summary, "lifecycle", label)
    for key in (
        "board_linux_restored",
        "host_filesystem_synced",
        "rtos_powered_off",
        "starry_done",
        "volatile_block_snapshotted",
    ):
        require_equal(lifecycle, key, True, f"{label} lifecycle")
    snapshot = require_object(lifecycle, "block_snapshot", f"{label} lifecycle")
    require_equal(snapshot, "filesystem_check", "clean", f"{label} snapshot")
    require_sha256(snapshot, "image_sha256", f"{label} snapshot")

    console_path = run_dir / "console.log"
    ort_votes = validate_uart_quorum(
        console_path,
        "IVC-STARRY-ORT-RAW sha256=",
        ort_digest,
        f"{label} ORT evidence",
    )
    model_votes = validate_uart_quorum(
        console_path,
        "IVC-STARRY-ORT-MODEL sha256=",
        expected_model_sha256,
        f"{label} model evidence",
    )
    return {
        "controller": controller,
        "ort": ort,
        "deadline": deadline,
        "raw_sha256": raw_digest,
        "ort_sha256": ort_digest,
        "ort_uart_votes": ort_votes,
        "model_uart_votes": model_votes,
        "snapshot_sha256": snapshot["image_sha256"],
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
    expected_commit: str,
    expected_runs: int = 5,
    expected_count: int = 1_800,
    expected_model_sha256: str = campaign_contract.DEFAULT_MODEL_SHA256,
    expected_runtime_version: str = campaign_contract.DEFAULT_RUNTIME_VERSION,
) -> dict[str, object]:
    if campaign_contract.COMMIT_PATTERN.fullmatch(expected_commit) is None:
        raise AggregationError("expected commit must be a full Git SHA")
    if expected_runs <= 0 or expected_count <= 1:
        raise AggregationError("expected runs and count must be positive")
    if campaign_contract.SHA256_PATTERN.fullmatch(expected_model_sha256) is None:
        raise AggregationError("expected model digest is malformed")
    root = campaign_root.resolve()
    preregistration, preregistration_digest = (
        campaign_contract.load_preregistration_evidence(
            root,
            expected_commit,
            expected_runs,
            expected_count,
            expected_model_sha256,
            expected_runtime_version,
        )
    )
    expected_names = [f"run-{number:03d}" for number in range(1, expected_runs + 1)]
    actual_names = sorted(path.name for path in root.glob("run-*") if path.is_dir())
    if actual_names != expected_names:
        raise AggregationError(
            f"campaign runs must be exactly {expected_names}, got {actual_names}"
        )

    runs: list[dict[str, object]] = []
    branches: set[str] = set()
    board_ids: set[str] = set()
    hostnames: set[str] = set()
    input_identities: set[str] = set()
    first_started: datetime | None = None
    previous_finished: datetime | None = None
    for run_number, run_name in enumerate(expected_names, start=1):
        run_dir = root / run_name
        manifest = validate_manifest(run_dir)
        metadata_record = validate_metadata(
            load_object(run_dir / "metadata.json", f"{run_name} metadata"),
            run_dir,
            run_number,
            expected_runs,
            expected_count,
            expected_commit,
            expected_model_sha256,
            expected_runtime_version,
        )
        summary_record = validate_summary(
            load_object(run_dir / "summary.json", f"{run_name} summary"),
            run_dir,
            expected_count,
            expected_model_sha256,
            expected_runtime_version,
        )
        started = metadata_record["started"]
        finished = metadata_record["finished"]
        if not isinstance(started, datetime) or not isinstance(finished, datetime):
            raise AggregationError(f"{run_name} has invalid parsed timestamps")
        if first_started is None:
            first_started = started
        if previous_finished is not None and started < previous_finished:
            raise AggregationError(f"{run_name} overlaps the previous run")
        previous_finished = finished
        branches.add(str(metadata_record["branch"]))
        board_ids.add(str(metadata_record["board_id"]))
        hostnames.add(str(metadata_record["hostname"]))
        input_identities.add(str(metadata_record["input_identity"]))
        controller = summary_record["controller"]
        ort = summary_record["ort"]
        deadline = summary_record["deadline"]
        if not isinstance(controller, dict):
            raise AggregationError(f"{run_name} controller result must be an object")
        if not isinstance(ort, dict):
            raise AggregationError(f"{run_name} ORT result must be an object")
        if not isinstance(deadline, dict):
            raise AggregationError(f"{run_name} deadline result must be an object")
        runs.append(
            {
                "run_id": run_name,
                "started_at": metadata_record["started_at"],
                "finished_at": metadata_record["finished_at"],
                "cpu_temp_milli_c": metadata_record["cpu_temp_milli_c"],
                "controller": {
                    key: controller[key]
                    for key in (
                        "full_loop_p50_us",
                        "full_loop_p95_us",
                        "full_loop_p99_us",
                        "full_loop_max_us",
                        "throughput_msg_s",
                        "deadline_misses",
                    )
                },
                "ort": {
                    key: ort[key]
                    for key in (
                        "initialization_us",
                        "wall_p50_ns",
                        "wall_p99_ns",
                        "wall_max_ns",
                    )
                },
                "deadline_partition": deadline,
                "evidence": {
                    "checksums_sha256": sha256_file(run_dir / "checksums.sha256"),
                    "summary_sha256": manifest["summary.json"],
                    "metadata_sha256": manifest["metadata.json"],
                    "raw_sha256": summary_record["raw_sha256"],
                    "ort_sha256": summary_record["ort_sha256"],
                    "snapshot_sha256": summary_record["snapshot_sha256"],
                    "ort_uart_votes": summary_record["ort_uart_votes"],
                    "model_uart_votes": summary_record["model_uart_votes"],
                },
            }
        )

    for identities, label in (
        (branches, "branches"),
        (board_ids, "board IDs"),
        (hostnames, "hostnames"),
        (input_identities, "input artifact sets"),
    ):
        if len(identities) != 1:
            raise AggregationError(f"campaign mixes multiple {label}")
    if branches != {preregistration.branch}:
        raise AggregationError("run branch differs from the preregistration")
    if input_identities != {preregistration.input_identity}:
        raise AggregationError("run inputs differ from the preregistered artifacts")
    if first_started is None or preregistration.created_at >= first_started:
        raise AggregationError("preregistration must predate the first run")

    controller_metrics = (
        "full_loop_p50_us",
        "full_loop_p95_us",
        "full_loop_p99_us",
        "full_loop_max_us",
        "throughput_msg_s",
    )
    ort_metrics = (
        "initialization_us",
        "wall_p50_ns",
        "wall_p99_ns",
        "wall_max_ns",
    )
    total_samples = expected_runs * expected_count
    total_deadline_misses = sum(
        require_integer(run["controller"], "deadline_misses", "run controller")
        for run in runs
        if isinstance(run["controller"], dict)
    )
    post_first_samples = sum(
        require_integer(run["deadline_partition"], "post_first_sample_count", "deadline")
        for run in runs
        if isinstance(run["deadline_partition"], dict)
    )
    return {
        "schema_version": 1,
        "campaign": {
            "path": str(root),
            "profile": "ort-full",
            "run_count": expected_runs,
            "samples_per_run": expected_count,
            "total_samples": total_samples,
        },
        "source": {
            "commit": expected_commit,
            "branch": next(iter(branches)),
            "dirty": False,
        },
        "preregistration": {
            "path": str(root / "preregistration.json"),
            "sha256": preregistration_digest,
            "created_at": preregistration.created_at.isoformat(),
            "predates_first_run": True,
        },
        "board": {
            "id": next(iter(board_ids)),
            "hostname": next(iter(hostnames)),
        },
        "model": {
            "id": "thermal-4x6x1-v1",
            "backend": "onnxruntime",
            "sha256": expected_model_sha256,
            "runtime_version": expected_runtime_version,
            "provider": campaign_contract.EXPECTED_PROVIDER,
        },
        "frozen_thresholds": campaign_contract.frozen_thresholds(),
        "reliability": {
            "sent": total_samples,
            "acknowledged": total_samples,
            "errors": 0,
            "timeouts": 0,
            "retransmissions": 0,
            "recoveries": 0,
            "deadline_misses": total_deadline_misses,
            "post_first_sample_count": post_first_samples,
            "post_first_deadline_misses": 0,
        },
        "statistics": {
            "controller": {
                metric: describe(
                    [
                        require_number(run["controller"], metric, "run controller")
                        for run in runs
                        if isinstance(run["controller"], dict)
                    ]
                )
                for metric in controller_metrics
            },
            "ort": {
                metric: describe(
                    [
                        require_number(run["ort"], metric, "run ORT")
                        for run in runs
                        if isinstance(run["ort"], dict)
                    ]
                )
                for metric in ort_metrics
            },
            "post_first_full_loop_p99_us": describe(
                [
                    require_integer(
                        run["deadline_partition"],
                        "post_first_full_loop_p99_us",
                        "deadline",
                    )
                    for run in runs
                    if isinstance(run["deadline_partition"], dict)
                ]
            ),
            "post_first_full_loop_max_us": describe(
                [
                    require_integer(
                        run["deadline_partition"],
                        "post_first_full_loop_max_us",
                        "deadline",
                    )
                    for run in runs
                    if isinstance(run["deadline_partition"], dict)
                ]
            ),
        },
        "formal_gate_passed": True,
        "runs": runs,
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
        description="Validate and aggregate a StarryOS ONNX Runtime campaign."
    )
    parser.add_argument("campaign_root", type=Path)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument("--expected-runs", type=int, default=5)
    parser.add_argument("--expected-count", type=int, default=1_800)
    parser.add_argument(
        "--expected-model-sha256",
        default=campaign_contract.DEFAULT_MODEL_SHA256,
    )
    parser.add_argument(
        "--expected-runtime-version",
        default=campaign_contract.DEFAULT_RUNTIME_VERSION,
    )
    parser.add_argument("--output", type=Path)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        result = aggregate_campaign(
            arguments.campaign_root,
            expected_commit=arguments.expected_commit,
            expected_runs=arguments.expected_runs,
            expected_count=arguments.expected_count,
            expected_model_sha256=arguments.expected_model_sha256,
            expected_runtime_version=arguments.expected_runtime_version,
        )
        if arguments.output is None:
            json.dump(result, sys.stdout, indent=2, sort_keys=True, allow_nan=False)
            sys.stdout.write("\n")
        else:
            write_json(arguments.output, result)
            print(f"ORT campaign aggregation passed: {arguments.output}")
    except (
        AggregationError,
        campaign_contract.ContractError,
        board_analysis.AnalysisError,
        OSError,
    ) as error:
        print(f"ORT campaign aggregation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
