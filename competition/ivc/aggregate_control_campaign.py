#!/usr/bin/env python3
"""Validate and aggregate a formal five-pair StarryOS control campaign."""

from __future__ import annotations

import argparse
import csv
import json
import math
import sys
from datetime import datetime
from pathlib import Path

try:
    import aggregate_board_campaign as common
except ModuleNotFoundError:
    from competition.ivc import aggregate_board_campaign as common


PAIR_SCHEDULE = [
    {"pair_id": "pair-001", "order": ["manual", "neural"]},
    {"pair_id": "pair-002", "order": ["neural", "manual"]},
    {"pair_id": "pair-003", "order": ["manual", "neural"]},
    {"pair_id": "pair-004", "order": ["neural", "manual"]},
    {"pair_id": "pair-005", "order": ["manual", "neural"]},
]
LOWER_IS_BETTER_METRICS = (
    "rmse_milli_c",
    "iae_milli_c_s",
    "max_overshoot_milli_c",
    "full_loop_p99_us",
    "full_loop_max_us",
    "deadline_misses",
)
HIGHER_IS_BETTER_METRICS = ("throughput_msg_s",)
CONTROLLER_METRICS = (
    *common.LATENCY_METRICS,
    *common.DESCRIPTIVE_METRICS,
)


AggregationError = common.AggregationError


def require_sha256(parent: dict[str, object], key: str, label: str) -> str:
    digest = common.require_string(parent, key, label)
    if common.SHA256_PATTERN.fullmatch(digest) is None:
        raise AggregationError(f"{label} {key} is not a complete SHA-256")
    return digest


def require_timestamp(parent: dict[str, object], key: str, label: str) -> datetime:
    value = common.require_string(parent, key, label)
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise AggregationError(f"{label} {key} is not an ISO timestamp") from error
    if parsed.tzinfo is None:
        raise AggregationError(f"{label} {key} has no timezone")
    return parsed


def validate_file_record(
    observed: dict[str, object], expected: dict[str, object], label: str
) -> None:
    common.require_equal(
        observed,
        "path",
        common.require_string(expected, "path", f"{label} contract"),
        label,
    )
    common.require_equal(
        observed,
        "sha256",
        require_sha256(expected, "sha256", f"{label} contract"),
        label,
    )
    if "size_bytes" in expected:
        common.require_equal(
            observed,
            "size_bytes",
            common.require_integer(expected, "size_bytes", f"{label} contract"),
            label,
        )


def validate_preregistration(
    campaign_root: Path,
) -> tuple[Path, dict[str, object], dict[str, object]]:
    path = campaign_root / "campaign-preregistration.json"
    preregistration = common.load_object(path, "control preregistration")
    common.require_equal(preregistration, "schema_version", 1, "preregistration")
    common.require_equal(
        preregistration,
        "status",
        "frozen-before-first-board-capture",
        "preregistration",
    )
    common.require_string(preregistration, "campaign_id", "preregistration")
    platform = common.require_object(preregistration, "platform", "preregistration")
    common.require_string(platform, "board_type", "preregistration platform")

    source = common.require_object(preregistration, "source", "preregistration")
    commit = common.require_string(source, "commit", "preregistration source")
    if common.COMMIT_PATTERN.fullmatch(commit) is None:
        raise AggregationError("preregistration source commit is invalid")
    common.require_equal(source, "worktree_clean", True, "preregistration source")

    capture = common.require_object(
        preregistration, "capture_contract", "preregistration"
    )
    common.require_equal(capture, "pair_count", 5, "capture contract")
    common.require_equal(capture, "analyzer_profile", "normal", "capture contract")
    common.require_equal(capture, "repeat_count_per_half", 1, "capture contract")
    common.require_equal(capture, "command_count", 1800, "capture contract")
    common.require_equal(capture, "period_ms", 100, "capture contract")
    common.require_equal(capture, "expected_retransmissions", 0, "capture contract")
    common.require_equal(capture, "expected_recoveries", 0, "capture contract")
    common.require_equal(capture, "expected_duplicates", 0, "capture contract")
    common.require_equal(capture, "expected_errors", 0, "capture contract")
    common.require_equal(capture, "expected_protocol_errors", 0, "capture contract")
    common.require_equal(capture, "settling_band_milli_c", 1000, "capture contract")
    common.require_equal(capture, "settling_minimum_samples", 20, "capture contract")
    if common.require_list(capture, "execution_order", "capture contract") != PAIR_SCHEDULE:
        raise AggregationError("capture contract does not use the frozen AB/BA order")

    expected_setpoints = [
        {"start_sequence": 1, "end_sequence": 600, "setpoint_milli_c": 45000},
        {"start_sequence": 601, "end_sequence": 1200, "setpoint_milli_c": 65000},
        {"start_sequence": 1201, "end_sequence": 1800, "setpoint_milli_c": 50000},
    ]
    if common.require_list(capture, "setpoint_schedule", "capture contract") != expected_setpoints:
        raise AggregationError("capture contract setpoint schedule is not frozen")

    profiles = common.require_object(capture, "profiles", "capture contract")
    if set(profiles) != {"manual", "neural"}:
        raise AggregationError("capture contract must define manual and neural profiles")
    expected_profiles = {
        "manual": ("manual-full", "manual-fixed", "manual", "manual-fixed-500"),
        "neural": ("full", "neural", "neural", "thermal-4x6x1-v1"),
    }
    for name, expected in expected_profiles.items():
        profile = common.require_object(profiles, name, "capture profiles")
        for key, value in zip(
            ("runner_profile", "controller_policy", "starry_mode", "model_id"),
            expected,
            strict=True,
        ):
            common.require_equal(profile, key, value, f"{name} profile")
        common.require_equal(profile, "inference_backend", "native", f"{name} profile")
        inputs = common.require_object(profile, "inputs", f"{name} profile")
        if set(inputs) != {"build_config", "board_config", "rootfs"}:
            raise AggregationError(f"{name} profile input set is incomplete")
        for input_name in inputs:
            record = common.require_object(inputs, input_name, f"{name} inputs")
            common.require_string(record, "path", f"{name} {input_name}")
            require_sha256(record, "sha256", f"{name} {input_name}")

    artifacts = common.require_object(preregistration, "artifacts", "preregistration")
    if set(artifacts) != {"starry_kernel", "starry_dtb", "zephyr_guest", "model_source"}:
        raise AggregationError("preregistration shared artifact set is incomplete")
    for name in artifacts:
        record = common.require_object(artifacts, name, "preregistration artifacts")
        common.require_string(record, "path", f"{name} artifact")
        require_sha256(record, "sha256", f"{name} artifact")

    statistics = common.require_object(
        preregistration, "statistics_policy", "preregistration"
    )
    if set(common.require_list(statistics, "summary", "statistics policy")) != {
        "paired values",
        "median",
        "IQR",
        "single-run maximum",
        "worst-of-runs",
    }:
        raise AggregationError("control statistics policy differs from the frozen set")
    return path, preregistration, capture


def validate_result_root(campaign_root: Path, result_root: Path) -> Path:
    result_root = common.resolve_inside(campaign_root, result_root, "result root")
    expected_pairs = {entry["pair_id"] for entry in PAIR_SCHEDULE}
    try:
        actual_pairs = {path.name for path in result_root.iterdir() if path.is_dir()}
    except OSError as error:
        raise AggregationError(f"cannot inspect result root: {error}") from error
    if actual_pairs != expected_pairs:
        raise AggregationError("result root does not contain exactly five frozen pairs")
    return result_root


def read_control_raw(
    path: Path, capture: dict[str, object], label: str
) -> dict[str, object]:
    try:
        with path.open(newline="", encoding="utf-8") as stream:
            rows = list(csv.DictReader(stream))
    except (OSError, csv.Error) as error:
        raise AggregationError(f"cannot read {label}: {error}") from error
    expected_count = common.require_integer(capture, "command_count", "capture contract")
    if len(rows) != expected_count:
        raise AggregationError(f"{label} must contain {expected_count} samples")

    parsed: list[dict[str, int]] = []
    numeric_fields = (
        "sequence",
        "cycle_started_us",
        "command_sent_us",
        "response_completed_us",
        "full_loop_us",
        "pre_send_us",
        "transport_us",
        "setpoint_milli_c",
        "observed_milli_c",
        "measured_milli_c",
        "command_actuator_permille",
        "status_actuator_permille",
        "error_milli_c",
    )
    try:
        for row in rows:
            parsed.append({field: int(row[field]) for field in numeric_fields})
    except (KeyError, TypeError, ValueError) as error:
        raise AggregationError(f"{label} has an invalid numeric field") from error
    if [row["sequence"] for row in parsed] != list(range(1, expected_count + 1)):
        raise AggregationError(f"{label} sequence is not contiguous")
    previous_response_us = 0
    previous_measured_milli_c: int | None = None
    for row in parsed:
        cycle_started_us = row["cycle_started_us"]
        command_sent_us = row["command_sent_us"]
        response_completed_us = row["response_completed_us"]
        if not 0 <= cycle_started_us <= command_sent_us <= response_completed_us:
            raise AggregationError(f"{label} has invalid event timestamps")
        if cycle_started_us < previous_response_us:
            raise AggregationError(f"{label} samples overlap")
        for field, expected in (
            ("pre_send_us", command_sent_us - cycle_started_us),
            ("transport_us", response_completed_us - command_sent_us),
            ("full_loop_us", response_completed_us - cycle_started_us),
        ):
            if row[field] != expected:
                raise AggregationError(f"{label} {field} is inconsistent")
        if (
            previous_measured_milli_c is not None
            and row["observed_milli_c"] != previous_measured_milli_c
        ):
            raise AggregationError(f"{label} observation continuity is broken")
        if row["command_actuator_permille"] != row["status_actuator_permille"]:
            raise AggregationError(f"{label} actuator application is inconsistent")
        if row["error_milli_c"] != (
            row["setpoint_milli_c"] - row["measured_milli_c"]
        ):
            raise AggregationError(f"{label} error column is inconsistent")
        previous_response_us = response_completed_us
        previous_measured_milli_c = row["measured_milli_c"]

    schedule = common.require_list(capture, "setpoint_schedule", "capture contract")
    band = common.require_integer(capture, "settling_band_milli_c", "capture contract")
    minimum = common.require_integer(
        capture, "settling_minimum_samples", "capture contract"
    )
    settling: list[dict[str, object]] = []
    for segment_value in schedule:
        if not isinstance(segment_value, dict):
            raise AggregationError("setpoint schedule entry must be an object")
        start = common.require_integer(segment_value, "start_sequence", "setpoint segment")
        end = common.require_integer(segment_value, "end_sequence", "setpoint segment")
        setpoint = common.require_integer(segment_value, "setpoint_milli_c", "setpoint segment")
        segment = parsed[start - 1 : end]
        if len(segment) != end - start + 1 or any(
            row["setpoint_milli_c"] != setpoint for row in segment
        ):
            raise AggregationError(f"{label} setpoint schedule differs from the contract")
        settled_index: int | None = None
        latest_candidate = len(segment) - minimum
        for index in range(max(0, latest_candidate + 1)):
            if all(abs(row["error_milli_c"]) <= band for row in segment[index:]):
                settled_index = index
                break
        settling.append(
            {
                "start_sequence": start,
                "end_sequence": end,
                "setpoint_milli_c": setpoint,
                "settled": settled_index is not None,
                "settling_time_ms": (
                    None
                    if settled_index is None
                    else round(
                        (
                            segment[settled_index]["cycle_started_us"]
                            - segment[0]["cycle_started_us"]
                        )
                        / 1000,
                        6,
                    )
                ),
            }
        )

    errors = [row["error_milli_c"] for row in parsed]
    period_ms = common.require_integer(capture, "period_ms", "capture contract")
    deadline_us = period_ms * 1000
    metrics: dict[str, object] = {}
    for family in ("full_loop", "pre_send", "transport"):
        values = sorted(row[f"{family}_us"] for row in parsed)
        metrics.update(
            {
                f"{family}_p50_us": values[((len(values) - 1) * 50) // 100],
                f"{family}_p95_us": values[((len(values) - 1) * 95) // 100],
                f"{family}_p99_us": values[((len(values) - 1) * 99) // 100],
                f"{family}_max_us": values[-1],
            }
        )
    scheduled_span_us = (
        parsed[-1]["cycle_started_us"] - parsed[0]["cycle_started_us"]
    )
    if scheduled_span_us <= 0:
        raise AggregationError(f"{label} scheduling span is not positive")
    metrics.update(
        {
            "throughput_msg_s": (len(parsed) - 1) * 1_000_000 / scheduled_span_us,
            "rmse_milli_c": math.sqrt(
                sum(error * error for error in errors) / len(errors)
            ),
            "iae_milli_c_s": sum(abs(error) for error in errors) * period_ms / 1000,
            "max_overshoot_milli_c": max(0, max(-error for error in errors)),
            "deadline_misses": sum(
                row["full_loop_us"] > deadline_us for row in parsed
            ),
        }
    )
    metrics.update(
        {
            "settling": settling,
            "settled_step_count": sum(segment["settled"] for segment in settling),
        }
    )
    return metrics


def require_close(observed: object, expected: float, label: str) -> None:
    if isinstance(observed, bool) or not isinstance(observed, (int, float)):
        raise AggregationError(f"{label} must be numeric")
    if not math.isclose(float(observed), expected, rel_tol=1e-12, abs_tol=1e-6):
        raise AggregationError(f"{label} differs from raw-derived evidence")


def validate_run(
    run_dir: Path,
    pair_id: str,
    profile_name: str,
    preregistration: dict[str, object],
    capture: dict[str, object],
    expected_board_id: str | None,
) -> tuple[dict[str, object], str, datetime, datetime]:
    label = f"{pair_id} {profile_name}"
    common.parse_manifest(run_dir, label, {"analyzer_profile": "normal"})
    common.validate_compressed_twins(
        run_dir, label, {"analyzer_profile": "normal"}
    )
    expected_count = common.require_integer(capture, "command_count", "capture contract")
    common.validate_raw_csv(run_dir, label, expected_count)

    profiles = common.require_object(capture, "profiles", "capture contract")
    contract = common.require_object(profiles, profile_name, "capture profiles")
    summary = common.load_object(run_dir / "summary.json", f"{label} summary")
    metadata = common.load_object(run_dir / "metadata.json", f"{label} metadata")

    common.require_equal(metadata, "schema_version", 1, f"{label} metadata")
    source = common.require_object(metadata, "source", f"{label} metadata")
    prereg_source = common.require_object(
        preregistration, "source", "preregistration"
    )
    common.require_equal(
        source,
        "commit",
        common.require_string(prereg_source, "commit", "preregistration source"),
        f"{label} source",
    )
    for key, expected in (
        ("dirty", False),
        ("tracked_change_count", 0),
        ("untracked_file_count", 0),
    ):
        common.require_equal(source, key, expected, f"{label} source")

    run = common.require_object(metadata, "run", f"{label} metadata")
    common.require_equal(
        run,
        "profile",
        common.require_string(contract, "runner_profile", f"{profile_name} profile"),
        f"{label} run",
    )
    for key, expected in (
        ("run_id", "run-001"),
        ("run_number", 1),
        ("execution_order", 1),
        ("repeat_count", 1),
        ("exit_status", 0),
    ):
        common.require_equal(run, key, expected, f"{label} run")
    board_type = common.require_string(
        common.require_object(preregistration, "platform", "preregistration"),
        "board_type",
        "preregistration platform",
    )
    common.require_equal(run, "board_type", board_type, f"{label} run")
    started_at = require_timestamp(run, "started_at", f"{label} run")
    finished_at = require_timestamp(run, "finished_at", f"{label} run")
    if finished_at <= started_at:
        raise AggregationError(f"{label} run has a non-positive duration")

    board = common.require_object(metadata, "board", f"{label} metadata")
    common.require_equal(board, "type", board_type, f"{label} board")
    board_id = common.require_string(board, "id", f"{label} board")
    if expected_board_id is not None and board_id != expected_board_id:
        raise AggregationError(f"{label} board ID differs from the campaign board")

    inputs = common.require_object(metadata, "inputs", f"{label} metadata")
    expected_inputs = common.require_object(contract, "inputs", f"{profile_name} profile")
    for name in ("build_config", "board_config", "rootfs"):
        validate_file_record(
            common.require_object(inputs, name, f"{label} inputs"),
            common.require_object(expected_inputs, name, f"{profile_name} inputs"),
            f"{label} {name}",
        )
    artifacts = common.require_object(preregistration, "artifacts", "preregistration")
    for input_name, artifact_name in (
        ("starry_kernel", "starry_kernel"),
        ("starry_dtb", "starry_dtb"),
        ("zephyr_guest", "zephyr_guest"),
    ):
        validate_file_record(
            common.require_object(inputs, input_name, f"{label} inputs"),
            common.require_object(artifacts, artifact_name, "preregistration artifacts"),
            f"{label} {input_name}",
        )

    model = common.require_object(metadata, "model", f"{label} metadata")
    common.require_equal(
        model,
        "id",
        common.require_string(contract, "model_id", f"{profile_name} profile"),
        f"{label} model",
    )
    common.require_equal(
        model,
        "backend",
        common.require_string(
            contract, "inference_backend", f"{profile_name} profile"
        ),
        f"{label} model",
    )
    validate_file_record(
        common.require_object(model, "artifact", f"{label} model"),
        common.require_object(artifacts, "model_source", "preregistration artifacts"),
        f"{label} model artifact",
    )

    outputs = common.require_object(metadata, "outputs", f"{label} metadata")
    for key, filename in (
        ("console_log", "console.log.gz"),
        ("raw_csv", "raw.csv.gz"),
        ("summary", "summary.json"),
    ):
        common.validate_output_identity(
            common.require_object(outputs, key, f"{label} outputs"),
            f"{label} {key}",
            run_dir,
            filename,
        )
    result = common.require_object(metadata, "result", f"{label} metadata")
    for key, expected in (
        (
            "controller_policy",
            common.require_string(contract, "controller_policy", f"{profile_name} profile"),
        ),
        ("sample_count", expected_count),
        ("dropped_samples", 0),
        ("successful_marker", True),
        ("validated", True),
    ):
        common.require_equal(result, key, expected, f"{label} result")

    common.require_equal(summary, "schema_version", 2, f"{label} summary")
    common.require_equal(summary, "platform", "orangepi-5-plus", f"{label} summary")
    common.require_equal(summary, "guest", "starryos", f"{label} summary")
    common.require_equal(summary, "profile", "normal", f"{label} summary")
    summary_board = common.require_object(summary, "board", f"{label} summary")
    common.require_equal(summary_board, "board_id", board_id, f"{label} summary board")
    common.require_equal(
        summary_board, "hostname", "orangepi5plus", f"{label} summary board"
    )
    cpu_temp = common.require_number(
        summary_board, "cpu_temp_milli_c", f"{label} summary board"
    )

    controller = common.require_object(summary, "controller", f"{label} summary")
    for key, expected in (
        (
            "policy",
            common.require_string(contract, "controller_policy", f"{profile_name} profile"),
        ),
        ("sent", expected_count),
        ("acknowledged", expected_count),
        ("errors", 0),
        ("timeouts", 0),
        ("retransmissions", 0),
        ("recoveries", 0),
        ("success_percent", 100.0),
    ):
        common.require_equal(controller, key, expected, f"{label} controller")
    for metric in CONTROLLER_METRICS:
        common.require_number(controller, metric, f"{label} controller")

    rtos = common.require_object(summary, "rtos", f"{label} summary")
    for key, expected in (
        ("profile", "normal"),
        ("accepted", expected_count),
        ("applied", expected_count),
        ("duplicates", 0),
        ("acks_dropped", 0),
        ("status_sent", expected_count),
        ("acks_sent", expected_count),
        ("errors_sent", 0),
        ("protocol_errors", 0),
    ):
        common.require_equal(rtos, key, expected, f"{label} RTOS")

    starry = common.require_object(summary, "starry", f"{label} summary")
    for key, expected in (
        (
            "mode",
            common.require_string(contract, "starry_mode", f"{profile_name} profile"),
        ),
        ("backend", "native"),
        ("fault_profile", "none"),
        ("count", expected_count),
        ("period_ms", common.require_integer(capture, "period_ms", "capture contract")),
        ("vcpus", 2),
    ):
        common.require_equal(starry, key, expected, f"{label} StarryOS")

    network = common.require_object(summary, "network", f"{label} summary")
    expected_network = common.require_object(capture, "network", "capture contract")
    for key in ("iface", "ip", "peer", "udp_port", "segment"):
        common.require_equal(network, key, expected_network[key], f"{label} network")

    lifecycle = common.require_object(summary, "lifecycle", f"{label} summary")
    for key in (
        "starry_done",
        "rtos_powered_off",
        "host_filesystem_synced",
        "volatile_block_snapshotted",
        "board_linux_restored",
    ):
        common.require_equal(lifecycle, key, True, f"{label} lifecycle")
    snapshot = common.require_object(lifecycle, "block_snapshot", f"{label} lifecycle")
    common.require_equal(snapshot, "filesystem_check", "clean", f"{label} snapshot")
    common.require_equal(
        snapshot,
        "image_path",
        common.require_string(contract, "result_snapshot_path", f"{profile_name} profile"),
        f"{label} snapshot",
    )
    common.require_equal(snapshot, "vm_id", 1, f"{label} snapshot")

    raw = common.require_object(summary, "raw_samples", f"{label} summary")
    raw_sha256 = common.sha256_file(run_dir / "raw.csv")
    for key, expected in (
        ("sha256", raw_sha256),
        ("guest_manifest_sha256", raw_sha256),
        ("artifact_sha256", common.sha256_file(run_dir / "raw.csv.gz")),
        ("sample_count", expected_count),
        ("dropped_samples", 0),
    ):
        common.require_equal(raw, key, expected, f"{label} raw")
    common.validate_sha256_fragment(raw, raw_sha256, f"{label} raw")

    raw_metrics = read_control_raw(run_dir / "raw.csv", capture, f"{label} raw")
    for metric in CONTROLLER_METRICS:
        require_close(controller.get(metric), float(raw_metrics[metric]), f"{label} {metric}")
    common.require_equal(
        raw,
        "deadline_misses",
        raw_metrics["deadline_misses"],
        f"{label} raw",
    )
    common.require_equal(
        controller,
        "deadline_misses",
        raw_metrics["deadline_misses"],
        f"{label} controller",
    )

    source_log = common.require_object(summary, "source_log", f"{label} summary")
    common.require_equal(
        source_log,
        "sha256",
        common.sha256_file(run_dir / "console.log.gz"),
        f"{label} source log",
    )
    common.require_equal(
        source_log,
        "content_sha256",
        common.sha256_file(run_dir / "console.log"),
        f"{label} source log",
    )

    evidence = {
        "profile": profile_name,
        "runner_profile": contract["runner_profile"],
        "started_at": run["started_at"],
        "finished_at": run["finished_at"],
        "cpu_temp_milli_c": cpu_temp,
        "metadata": common.file_identity(run_dir, run_dir / "metadata.json"),
        "summary": common.file_identity(run_dir, run_dir / "summary.json"),
        "manifest": common.file_identity(run_dir, run_dir / "checksums.sha256"),
        "console": {
            "content_sha256": common.sha256_file(run_dir / "console.log"),
            "gzip_sha256": common.sha256_file(run_dir / "console.log.gz"),
        },
        "raw": {
            "content_sha256": raw_sha256,
            "gzip_sha256": common.sha256_file(run_dir / "raw.csv.gz"),
            "sample_count": expected_count,
        },
        "controller": {metric: controller[metric] for metric in CONTROLLER_METRICS},
        "settling": raw_metrics["settling"],
        "settled_step_count": raw_metrics["settled_step_count"],
        "lifecycle_gate_met": True,
        "validated": True,
    }
    return evidence, board_id, started_at, finished_at


def paired_metric(
    pairs: list[dict[str, object]], metric: str, lower_is_better: bool
) -> dict[str, object]:
    values: list[int | float] = []
    by_pair: list[dict[str, object]] = []
    for pair in pairs:
        profiles = common.require_object(pair, "profiles", "validated pair")
        manual = common.require_object(profiles, "manual", "validated pair")
        neural = common.require_object(profiles, "neural", "validated pair")
        manual_controller = common.require_object(manual, "controller", "manual run")
        neural_controller = common.require_object(neural, "controller", "neural run")
        manual_value = common.require_number(manual_controller, metric, "manual run")
        neural_value = common.require_number(neural_controller, metric, "neural run")
        delta = (
            manual_value - neural_value
            if lower_is_better
            else neural_value - manual_value
        )
        values.append(delta)
        by_pair.append(
            {
                "pair_id": pair["pair_id"],
                "manual": manual_value,
                "neural": neural_value,
                "favorable_delta": delta,
                "favors_neural": delta > 0,
            }
        )
    return {
        "direction": "lower-is-better" if lower_is_better else "higher-is-better",
        "favorable_delta_definition": (
            "manual - neural" if lower_is_better else "neural - manual"
        ),
        "pairs": by_pair,
        "statistics": common.summarize_values(values, worst="minimum"),
        "all_pairs_favor_neural": all(value > 0 for value in values),
    }


def profile_metric(
    pairs: list[dict[str, object]], profile_name: str, metric: str
) -> dict[str, object]:
    values = [
        common.require_number(
            common.require_object(
                common.require_object(pair, "profiles", "validated pair"),
                profile_name,
                "validated pair",
            )["controller"],
            metric,
            f"{profile_name} controller",
        )
        for pair in pairs
    ]
    worst = "minimum" if metric == "throughput_msg_s" else "maximum"
    return common.summarize_values(values, worst=worst)


def aggregate_campaign(
    campaign_root: Path, result_root: Path, final_board_check: Path
) -> dict[str, object]:
    campaign_root = campaign_root.resolve()
    prereg_path, preregistration, capture = validate_preregistration(campaign_root)
    result_root = validate_result_root(campaign_root, result_root)
    board_id: str | None = None
    previous_finish: datetime | None = None
    pairs: list[dict[str, object]] = []
    profiles = common.require_object(capture, "profiles", "capture contract")

    for pair_contract in PAIR_SCHEDULE:
        pair_id = str(pair_contract["pair_id"])
        pair_dir = result_root / pair_id
        expected_profile_dirs = {
            common.require_string(
                common.require_object(profiles, name, "capture profiles"),
                "runner_profile",
                f"{name} profile",
            )
            for name in ("manual", "neural")
        }
        actual_profile_dirs = {path.name for path in pair_dir.iterdir() if path.is_dir()}
        if actual_profile_dirs != expected_profile_dirs:
            raise AggregationError(f"{pair_id} does not contain both profile runs")

        pair_evidence: dict[str, object] = {}
        times: dict[str, tuple[datetime, datetime]] = {}
        for profile_name in ("manual", "neural"):
            contract = common.require_object(profiles, profile_name, "capture profiles")
            runner_profile = common.require_string(
                contract, "runner_profile", f"{profile_name} profile"
            )
            evidence, board_id, started, finished = validate_run(
                pair_dir / runner_profile / "run-001",
                pair_id,
                profile_name,
                preregistration,
                capture,
                board_id,
            )
            pair_evidence[profile_name] = evidence
            times[profile_name] = (started, finished)

        first, second = pair_contract["order"]
        first_start, first_finish = times[str(first)]
        second_start, second_finish = times[str(second)]
        if first_finish > second_start:
            raise AggregationError(f"{pair_id} timestamps violate the frozen order")
        if previous_finish is not None and first_start < previous_finish:
            raise AggregationError(f"{pair_id} overlaps the previous pair")
        previous_finish = second_finish
        pairs.append(
            {
                "pair_id": pair_id,
                "order": pair_contract["order"],
                "profiles": pair_evidence,
            }
        )

    platform = common.require_object(preregistration, "platform", "preregistration")
    campaign_id = common.require_string(
        preregistration, "campaign_id", "preregistration"
    )
    board_type = common.require_string(platform, "board_type", "platform")
    final_check = common.validate_final_board_check(
        campaign_root, final_board_check, campaign_id, board_type
    )

    paired_effects = {
        metric: paired_metric(pairs, metric, lower_is_better=True)
        for metric in LOWER_IS_BETTER_METRICS
    }
    paired_effects.update(
        {
            metric: paired_metric(pairs, metric, lower_is_better=False)
            for metric in HIGHER_IS_BETTER_METRICS
        }
    )
    profile_statistics = {
        profile_name: {
            metric: profile_metric(pairs, profile_name, metric)
            for metric in (*LOWER_IS_BETTER_METRICS, *HIGHER_IS_BETTER_METRICS)
        }
        for profile_name in ("manual", "neural")
    }
    settling_pairs = [
        {
            "pair_id": pair["pair_id"],
            "manual": common.require_object(
                common.require_object(pair, "profiles", "validated pair"),
                "manual",
                "validated pair",
            )["settling"],
            "neural": common.require_object(
                common.require_object(pair, "profiles", "validated pair"),
                "neural",
                "validated pair",
            )["settling"],
        }
        for pair in pairs
    ]
    source = common.require_object(preregistration, "source", "preregistration")
    return {
        "schema_version": 1,
        "campaign": {
            "campaign_id": campaign_id,
            "profile": "starry-control-paired",
            "board_type": board_type,
            "board_id": board_id,
            "source_commit": source["commit"],
            "pair_count": len(pairs),
            "result_root": result_root.relative_to(campaign_root).as_posix(),
            "execution_order": PAIR_SCHEDULE,
        },
        "assessment": {
            "campaign_gate_met": True,
            "all_ten_runs_validated": True,
            "all_manifests_verified": True,
            "all_raw_and_gzip_twins_verified": True,
            "same_physical_board": True,
            "frozen_ab_ba_order_met": True,
            "paired_configuration_gate_met": True,
            "final_board_linux_root_rw": True,
            "performance_superiority_required_for_validity": False,
            "reason": "all five preregistered StarryOS manual/neural pairs and lifecycle gates passed",
        },
        "paired_effects": paired_effects,
        "profile_statistics": profile_statistics,
        "settling": {
            "band_milli_c": capture["settling_band_milli_c"],
            "minimum_samples": capture["settling_minimum_samples"],
            "rule": "first sample with at least the minimum samples remaining for which every remaining sample in the current setpoint segment stays inside the absolute error band",
            "pairs": settling_pairs,
        },
        "evidence": {
            "preregistration": common.file_identity(campaign_root, prereg_path),
            "final_board_linux_root_check": final_check,
            "pairs": pairs,
        },
    }


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign_root", type=Path)
    parser.add_argument("--result-root", required=True, type=Path)
    parser.add_argument("--final-board-check", required=True, type=Path)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    try:
        result = aggregate_campaign(
            arguments.campaign_root,
            arguments.result_root,
            arguments.final_board_check,
        )
        rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
        if arguments.output is None:
            print(rendered, end="")
        else:
            arguments.output.parent.mkdir(parents=True, exist_ok=True)
            arguments.output.write_text(rendered, encoding="utf-8", newline="\n")
    except (AggregationError, OSError) as error:
        print(f"control campaign aggregation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
