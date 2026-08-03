#!/usr/bin/env python3
"""Validate and aggregate a formal physical-board IVC campaign."""

from __future__ import annotations

import argparse
import csv
import gzip
import hashlib
import json
import re
import statistics
import sys
from pathlib import Path
from typing import Sequence


MANIFEST_FILES = (
    "console.log",
    "console.log.gz",
    "metadata.json",
    "summary.json",
    "raw.csv",
    "raw.csv.gz",
)
RESTART_MANIFEST_FILES = MANIFEST_FILES + (
    "raw-before-reset.csv",
    "raw-before-reset.csv.gz",
)
LATENCY_METRICS = (
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
)
DESCRIPTIVE_METRICS = (
    "throughput_msg_s",
    "rmse_milli_c",
    "iae_milli_c_s",
    "max_overshoot_milli_c",
    "deadline_misses",
)
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
MANIFEST_PATTERN = re.compile(r"([0-9a-f]{64})  ([^/\\]+)")
SUPPORTED_PROFILE_PAIRS = {
    ("fault-ack-loss", "ack-loss"),
    ("fault-error", "error"),
    ("fault-restart", "restart"),
}


class AggregationError(ValueError):
    """Raised when evidence cannot form an authoritative campaign."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    try:
        return sha256_bytes(path.read_bytes())
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


def require_list(parent: dict[str, object], key: str, label: str) -> list[object]:
    value = parent.get(key)
    if not isinstance(value, list):
        raise AggregationError(f"{label} {key} must be an array")
    return value


def require_string(parent: dict[str, object], key: str, label: str) -> str:
    value = parent.get(key)
    if not isinstance(value, str) or not value:
        raise AggregationError(f"{label} {key} must be a nonempty string")
    return value


def require_number(parent: dict[str, object], key: str, label: str) -> int | float:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise AggregationError(f"{label} {key} must be a number")
    return value


def require_integer(parent: dict[str, object], key: str, label: str) -> int:
    value = parent.get(key)
    if isinstance(value, bool) or not isinstance(value, int):
        raise AggregationError(f"{label} {key} must be an integer")
    return value


def require_equal(
    parent: dict[str, object], key: str, expected: object, label: str
) -> None:
    if parent.get(key) != expected:
        raise AggregationError(
            f"{label} {key} must be {expected!r}, got {parent.get(key)!r}"
        )


def resolve_inside(root: Path, path: Path, label: str) -> Path:
    root = root.resolve()
    resolved = path.resolve() if path.is_absolute() else (root / path).resolve()
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise AggregationError(f"{label} must stay inside campaign root") from error
    return resolved


def file_identity(root: Path, path: Path) -> dict[str, object]:
    try:
        relative_path = path.resolve().relative_to(root.resolve()).as_posix()
        size_bytes = path.stat().st_size
    except (OSError, ValueError) as error:
        raise AggregationError(
            f"cannot identify campaign file {path}: {error}"
        ) from error
    return {
        "path": relative_path,
        "sha256": sha256_file(path),
        "size_bytes": size_bytes,
    }


def validate_preregistration(
    campaign_root: Path,
) -> tuple[Path, dict[str, object], dict[str, object]]:
    path = campaign_root / "campaign-preregistration.json"
    preregistration = load_object(path, "preregistration")
    require_equal(preregistration, "schema_version", 1, "preregistration")
    require_equal(
        preregistration,
        "status",
        "frozen-before-first-board-capture",
        "preregistration",
    )
    require_string(preregistration, "campaign_id", "preregistration")
    platform = require_object(preregistration, "platform", "preregistration")
    require_string(platform, "board_type", "preregistration platform")
    capture = require_object(preregistration, "capture_contract", "preregistration")
    runner_profile = require_string(capture, "runner_profile", "capture contract")
    analyzer_profile = require_string(
        capture, "analyzer_profile", "capture contract"
    )
    if (runner_profile, analyzer_profile) not in SUPPORTED_PROFILE_PAIRS:
        raise AggregationError(
            "capture contract uses an unsupported runner/analyzer profile pair"
        )
    require_equal(capture, "repeat_count", 3, "capture contract")
    order = require_list(capture, "execution_order", "capture contract")
    if order != ["run-001", "run-002", "run-003"]:
        raise AggregationError(
            "capture contract execution_order is not the formal order"
        )
    statistics_policy = require_object(
        preregistration, "statistics_policy", "preregistration"
    )
    required_statistics = {
        "median",
        "IQR",
        "single-run maximum",
        "worst-of-runs",
    }
    reported_statistics = set(
        require_list(statistics_policy, "latency_summary", "statistics policy")
    )
    if reported_statistics != required_statistics:
        raise AggregationError("statistics policy differs from the formal contract")
    return path, preregistration, capture


def validate_amendment_chain(
    campaign_root: Path,
    latest_path: Path,
    preregistration_path: Path,
) -> tuple[list[dict[str, object]], dict[str, object]]:
    expected_preregistration_hash = sha256_file(preregistration_path)
    preregistration = load_object(preregistration_path, "preregistration")
    expected_campaign_id = require_string(
        preregistration, "campaign_id", "preregistration"
    )
    current_path = resolve_inside(campaign_root, latest_path, "latest amendment")
    seen: set[Path] = set()
    reversed_chain: list[dict[str, object]] = []
    latest: dict[str, object] | None = None
    expected_number: int | None = None

    while True:
        if current_path in seen:
            raise AggregationError("amendment chain contains a cycle")
        seen.add(current_path)
        amendment = load_object(current_path, "amendment")
        if latest is None:
            latest = amendment
        require_equal(amendment, "schema_version", 1, "amendment")
        numbered = "amendment" in amendment
        amendment_number: int | None = None
        if numbered:
            amendment_number = require_integer(amendment, "amendment", "amendment")
            if amendment_number < 1:
                raise AggregationError("amendment number must be positive")
            if expected_number is not None and amendment_number != expected_number:
                raise AggregationError("numbered amendment chain is not contiguous")
            amendment_id = f"campaign-amendment-{amendment_number:03d}"
            require_equal(amendment, "campaign_id", expected_campaign_id, amendment_id)
            status = require_string(amendment, "status", amendment_id)
            if status not in {
                "frozen-before-amendment-first-board-capture",
                "frozen-before-post-capture-aggregation",
            }:
                raise AggregationError(f"{amendment_id} has an invalid frozen status")
            require_equal(
                amendment,
                "preregistration_sha256",
                expected_preregistration_hash,
                amendment_id,
            )
        else:
            amendment_id = require_string(amendment, "amendment_id", "amendment")
            amendment_preregistration = require_object(
                amendment, "preregistration", amendment_id
            )
            require_equal(
                amendment_preregistration,
                "sha256",
                expected_preregistration_hash,
                f"{amendment_id} preregistration",
            )
            require_equal(
                amendment_preregistration,
                "modified",
                False,
                f"{amendment_id} preregistration",
            )
        reversed_chain.append(
            {
                "amendment_id": amendment_id,
                **file_identity(campaign_root, current_path),
            }
        )

        if numbered:
            declared_hash_value = amendment.get("previous_amendment_sha256")
            if declared_hash_value is None:
                if amendment_number != 1:
                    raise AggregationError(
                        f"{amendment_id} is missing its previous amendment hash"
                    )
                break
            if not isinstance(declared_hash_value, str):
                raise AggregationError(
                    f"{amendment_id} previous amendment hash must be a string"
                )
            declared_hash = declared_hash_value
            if SHA256_PATTERN.fullmatch(declared_hash) is None:
                raise AggregationError(
                    f"{amendment_id} previous amendment hash is invalid"
                )
            candidates = [
                candidate.resolve()
                for candidate in campaign_root.glob("campaign-amendment-*.json")
                if candidate.resolve() != current_path
                and sha256_file(candidate) == declared_hash
            ]
            if len(candidates) != 1:
                raise AggregationError(
                    f"{amendment_id} previous amendment hash must identify "
                    "exactly one campaign file"
                )
            previous_path = candidates[0]
            if amendment_number is None:
                raise AggregationError("numbered amendment has no number")
            expected_number = amendment_number - 1
        else:
            previous = amendment.get("previous_amendment")
            if previous is None:
                break
            if not isinstance(previous, dict):
                raise AggregationError(
                    f"{amendment_id} previous_amendment must be an object"
                )
            previous_path = resolve_inside(
                campaign_root,
                Path(
                    require_string(
                        previous, "path", f"{amendment_id} previous amendment"
                    )
                ),
                "previous amendment",
            )
            declared_hash = require_string(
                previous, "sha256", f"{amendment_id} previous amendment"
            )
        if SHA256_PATTERN.fullmatch(declared_hash) is None:
            raise AggregationError(f"{amendment_id} previous amendment hash is invalid")
        if sha256_file(previous_path) != declared_hash:
            raise AggregationError(
                f"{amendment_id} previous amendment checksum mismatch"
            )
        current_path = previous_path

    if latest is None:
        raise AggregationError("latest amendment is missing")
    return list(reversed(reversed_chain)), latest


def expected_source_and_rootfs(latest: dict[str, object]) -> tuple[str, str]:
    correction = latest.get("correction")
    if isinstance(correction, dict) and "source_commit" in correction:
        source_commit = require_string(
            correction, "source_commit", "latest correction"
        )
        require_equal(correction, "worktree_clean", True, "latest correction")
    else:
        source = require_object(latest, "source", "latest amendment")
        source_commit = require_string(source, "commit", "latest source")
        require_equal(source, "worktree_clean", True, "latest source")
    if COMMIT_PATTERN.fullmatch(source_commit) is None:
        raise AggregationError("latest source commit is invalid")
    artifacts = require_object(latest, "artifacts", "latest amendment")
    if "starry_rootfs_sha256" in artifacts:
        rootfs_sha256 = require_string(
            artifacts, "starry_rootfs_sha256", "latest amendment artifacts"
        )
    else:
        rootfs = require_object(artifacts, "starry_rootfs", "latest artifacts")
        rootfs_sha256 = require_string(rootfs, "sha256", "latest rootfs artifact")
    if SHA256_PATTERN.fullmatch(rootfs_sha256) is None:
        raise AggregationError("latest rootfs SHA-256 is invalid")
    return source_commit, rootfs_sha256


def validate_result_root(
    campaign_root: Path,
    result_root: Path,
    latest: dict[str, object],
    execution_order: list[object],
) -> Path:
    result_root = resolve_inside(campaign_root, result_root, "result root")
    capture_key = (
        "resumed_capture"
        if isinstance(latest.get("resumed_capture"), dict)
        else "continued_capture_contract"
    )
    resumed_capture = require_object(latest, capture_key, "latest amendment")
    amendment_result_root = require_string(
        resumed_capture, "result_root", "latest resumed capture"
    )
    relative_parts = result_root.relative_to(campaign_root.resolve()).parts
    if not relative_parts or relative_parts[0] != amendment_result_root:
        raise AggregationError("result root differs from the latest amendment")
    amended_order = resumed_capture.get("execution_order")
    if amended_order is not None and amended_order != execution_order:
        raise AggregationError(
            "latest amendment execution order differs from preregistration"
        )
    expected_entries = {str(run_id) for run_id in execution_order}
    try:
        actual_entries = {path.name for path in result_root.iterdir() if path.is_dir()}
    except OSError as error:
        raise AggregationError(f"cannot inspect result root: {error}") from error
    if actual_entries != expected_entries:
        expected = sorted(expected_entries)
        raise AggregationError(
            f"result root must contain exactly the registered runs {expected}"
        )
    return result_root


def parse_manifest(
    run_dir: Path, run_id: str, capture: dict[str, object]
) -> dict[str, str]:
    path = run_dir / "checksums.sha256"
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise AggregationError(
            f"{run_id} cannot read checksum manifest: {error}"
        ) from error
    manifest: dict[str, str] = {}
    for line in lines:
        match = MANIFEST_PATTERN.fullmatch(line)
        if match is None:
            raise AggregationError(f"{run_id} checksum manifest has an invalid line")
        digest, name = match.groups()
        if name in manifest:
            raise AggregationError(f"{run_id} checksum manifest repeats {name}")
        manifest[name] = digest
    expected_files = (
        RESTART_MANIFEST_FILES
        if capture["analyzer_profile"] == "restart"
        else MANIFEST_FILES
    )
    if set(manifest) != set(expected_files):
        raise AggregationError(f"{run_id} checksum manifest has the wrong file set")
    for name, expected in manifest.items():
        observed = sha256_file(run_dir / name)
        if observed != expected:
            raise AggregationError(f"{run_id} checksum mismatch for {name}")
    return manifest


def validate_compressed_twins(
    run_dir: Path, run_id: str, capture: dict[str, object]
) -> None:
    twins = [
        ("console.log", "console.log.gz"),
        ("raw.csv", "raw.csv.gz"),
    ]
    if capture["analyzer_profile"] == "restart":
        twins.append(("raw-before-reset.csv", "raw-before-reset.csv.gz"))
    for plain_name, gzip_name in twins:
        try:
            decompressed = gzip.decompress((run_dir / gzip_name).read_bytes())
            plain = (run_dir / plain_name).read_bytes()
        except (OSError, gzip.BadGzipFile) as error:
            raise AggregationError(
                f"{run_id} cannot validate {gzip_name}: {error}"
            ) from error
        if decompressed != plain:
            raise AggregationError(
                f"{run_id} {gzip_name} does not reproduce {plain_name}"
            )


def validate_raw_csv(
    run_dir: Path,
    run_id: str,
    expected_count: int,
    filename: str = "raw.csv",
) -> None:
    try:
        with (run_dir / filename).open(newline="", encoding="utf-8") as stream:
            rows = list(csv.DictReader(stream))
    except (OSError, csv.Error) as error:
        raise AggregationError(f"{run_id} cannot parse {filename}: {error}") from error
    if len(rows) != expected_count:
        raise AggregationError(
            f"{run_id} {filename} sample count must be {expected_count}, "
            f"got {len(rows)}"
        )
    try:
        sequences = [int(row["sequence"]) for row in rows]
    except (KeyError, TypeError, ValueError) as error:
        raise AggregationError(
            f"{run_id} {filename} sequence column is invalid"
        ) from error
    if sequences != list(range(1, expected_count + 1)):
        raise AggregationError(f"{run_id} {filename} sequences are not contiguous")


def validate_output_identity(
    output: dict[str, object],
    label: str,
    run_dir: Path,
    filename: str,
) -> None:
    path = require_string(output, "path", label)
    if Path(path).name != filename:
        raise AggregationError(f"{label} path must name {filename}")
    require_equal(output, "sha256", sha256_file(run_dir / filename), label)
    require_equal(output, "size_bytes", (run_dir / filename).stat().st_size, label)


def expected_error_faults(capture: dict[str, object]) -> list[dict[str, object]]:
    faults = require_list(capture, "faults", "capture contract")
    expected: list[dict[str, object]] = []
    for index, value in enumerate(faults, start=1):
        if not isinstance(value, dict):
            raise AggregationError(f"capture fault {index} must be an object")
        expected.append(
            {
                "kind": require_string(value, "kind", f"capture fault {index}"),
                "sequence": require_integer(
                    value, "sequence", f"capture fault {index}"
                ),
                "error_code": require_integer(
                    value, "expected_error_code", f"capture fault {index}"
                ),
                "reason": require_string(
                    value, "expected_rtos_reason", f"capture fault {index}"
                ),
            }
        )
    expected_error_frames = require_integer(
        capture, "expected_error_frames", "capture contract"
    )
    expected_protocol_errors = require_integer(
        capture, "expected_protocol_errors", "capture contract"
    )
    if len(expected) != expected_error_frames:
        raise AggregationError("capture fault count differs from expected_error_frames")
    if len(expected) != expected_protocol_errors:
        raise AggregationError(
            "capture fault count differs from expected_protocol_errors"
        )
    return expected


def validate_error_summary(
    summary: dict[str, object], capture: dict[str, object], label: str
) -> None:
    expected_faults = expected_error_faults(capture)
    observed_faults = require_list(summary, "error_evidence", label)
    expected_evidence = [
        {
            "controller_observed": True,
            **fault,
            "rtos_observed": True,
        }
        for fault in expected_faults
    ]
    if observed_faults != expected_evidence:
        raise AggregationError(f"{label} error_evidence differs from the contract")
    require_equal(
        capture,
        "normal_control_must_continue_after_faults",
        True,
        "capture contract",
    )
    recovery = require_object(summary, "error_recovery", label)
    recovery_contract = {
        "continued": True,
        "errors_received": capture["expected_error_frames"],
        "injected": len(expected_faults),
        "normal_acknowledged": capture["command_count"],
    }
    if recovery != recovery_contract:
        raise AggregationError(f"{label} error_recovery differs from the contract")


def validate_restart_summary(
    summary: dict[str, object],
    run_dir: Path,
    capture: dict[str, object],
    label: str,
) -> None:
    require_equal(
        capture,
        "actual_vm_reset_required",
        True,
        "capture contract",
    )
    pre_reset_count = require_integer(
        capture, "pre_reset_command_count", "capture contract"
    )
    expected_duplicate_sequences = require_list(
        capture, "expected_duplicate_sequences", "capture contract"
    )
    if expected_duplicate_sequences != [1]:
        raise AggregationError(
            "restart capture contract must preregister duplicate sequence 1"
        )
    expected_duplicate_receives = require_integer(
        capture, "expected_duplicate_receives", "capture contract"
    )
    if expected_duplicate_receives != len(expected_duplicate_sequences):
        raise AggregationError(
            "restart duplicate count differs from its preregistered sequence set"
        )
    expected_fresh_applications = require_integer(
        capture, "expected_fresh_applications", "capture contract"
    )
    expected_status_frames = require_integer(
        capture, "expected_status_frames", "capture contract"
    )
    expected_ack_frames = require_integer(
        capture, "expected_ack_frames", "capture contract"
    )
    expected_stale_status_frames = require_integer(
        capture, "expected_stale_status_frames", "capture contract"
    )
    expected_stale_ack_frames = require_integer(
        capture, "expected_stale_ack_frames", "capture contract"
    )
    if expected_status_frames != (
        expected_fresh_applications
        + expected_duplicate_receives
        + expected_stale_status_frames
    ):
        raise AggregationError(
            "restart STATUS count differs from fresh, duplicate, and stale responses"
        )
    if expected_ack_frames != (
        expected_fresh_applications
        + expected_duplicate_receives
        + expected_stale_ack_frames
    ):
        raise AggregationError(
            "restart ACK count differs from fresh, duplicate, and stale responses"
        )
    pre_reset = require_object(summary, "pre_reset_raw_samples", label)
    pre_reset_hash = sha256_file(run_dir / "raw-before-reset.csv")
    require_equal(pre_reset, "sha256", pre_reset_hash, f"{label} pre-reset raw")
    require_equal(
        pre_reset,
        "guest_manifest_sha256",
        pre_reset_hash,
        f"{label} pre-reset raw",
    )
    require_equal(
        pre_reset,
        "artifact_sha256",
        sha256_file(run_dir / "raw-before-reset.csv.gz"),
        f"{label} pre-reset raw",
    )
    require_equal(
        pre_reset, "uart_sha256", pre_reset_hash, f"{label} pre-reset raw"
    )
    require_equal(
        pre_reset, "uart_sha256_complete", True, f"{label} pre-reset raw"
    )
    require_equal(
        pre_reset, "sample_count", pre_reset_count, f"{label} pre-reset raw"
    )
    require_equal(pre_reset, "dropped_samples", 0, f"{label} pre-reset raw")

    recovery = require_object(summary, "restart_recovery", label)
    ready_wait_ms = require_integer(recovery, "ready_wait_ms", f"{label} restart")
    observed_delay_ms = require_integer(
        recovery, "observed_delay_ms", f"{label} restart"
    )
    ready_timeout_ms = require_integer(
        capture, "restart_ready_timeout_ms", "capture contract"
    )
    requested_delay_ms = require_integer(
        capture, "restart_delay_ms", "capture contract"
    )
    if not 0 <= ready_wait_ms <= ready_timeout_ms:
        raise AggregationError(f"{label} restart ready wait exceeds its bound")
    if observed_delay_ms < requested_delay_ms:
        raise AggregationError(f"{label} restart occurred before its requested delay")
    recovery_contract = {
        "actual_vm_reset": True,
        "vm_id": capture["restart_vm_id"],
        "host_cpu": capture["restart_host_cpu"],
        "reset_count": 1,
        "ready_wait_ms": ready_wait_ms,
        "requested_delay_ms": requested_delay_ms,
        "observed_delay_ms": observed_delay_ms,
        "old_session": capture["previous_session"],
        "new_session": capture["current_session"],
        "pre_reset_samples": pre_reset_count,
        "post_reset_samples": capture["command_count"],
        "safe_fallback_observed": True,
        "recovered": True,
        "stale_ack_ignored": capture["expected_stale_ack_frames"],
        "stale_status_ignored": capture["expected_stale_status_frames"],
        "retired_control_rejected": capture[
            "expected_retired_control_rejections"
        ],
    }
    if recovery != recovery_contract:
        raise AggregationError(f"{label} restart_recovery differs from the contract")


def validate_summary(
    summary: dict[str, object],
    run_dir: Path,
    run_id: str,
    board_id: str,
    capture: dict[str, object],
) -> None:
    label = f"{run_id} summary"
    require_equal(summary, "schema_version", 2, label)
    require_equal(summary, "platform", "orangepi-5-plus", label)
    require_equal(summary, "guest", "starryos", label)
    require_equal(summary, "profile", capture["analyzer_profile"], label)

    board = require_object(summary, "board", label)
    require_equal(board, "board_id", board_id, f"{label} board")
    require_equal(board, "hostname", "orangepi5plus", f"{label} board")
    require_number(board, "cpu_temp_milli_c", f"{label} board")

    controller = require_object(summary, "controller", label)
    controller_contract = {
        "policy": capture["controller_policy"],
        "sent": capture["command_count"],
        "acknowledged": capture["command_count"],
        "errors": 0,
        "timeouts": 0,
        "retransmissions": capture["expected_retransmissions"],
        "recoveries": capture["expected_recoveries"],
        "success_percent": 100.0,
    }
    for key, expected in controller_contract.items():
        require_equal(controller, key, expected, f"{label} controller")
    for metric in (*LATENCY_METRICS, *DESCRIPTIVE_METRICS):
        require_number(controller, metric, f"{label} controller")

    rtos = require_object(summary, "rtos", label)
    rtos_contract: dict[str, object] = {
        "profile": capture["analyzer_profile"],
        "accepted": capture["expected_fresh_applications"],
        "applied": capture["expected_fresh_applications"],
        "duplicates": capture["expected_duplicate_receives"],
        "status_sent": capture["expected_status_frames"],
        "acks_sent": capture["expected_ack_frames"],
        "errors_sent": capture["expected_error_frames"],
        "protocol_errors": capture["expected_protocol_errors"],
    }
    if capture["analyzer_profile"] == "ack-loss":
        rtos_contract.update(
            {
                "drop_ack_every": capture["drop_ack_every"],
                "expected_recoveries": capture["expected_recoveries"],
                "acks_dropped": capture["configured_ack_losses"],
                "injected_sequences": capture["injected_sequences"],
                "duplicate_sequences": capture["injected_sequences"],
            }
        )
    elif capture["analyzer_profile"] == "restart":
        rtos_contract.update(
            {
                "acks_dropped": 0,
                "duplicate_sequences": capture["expected_duplicate_sequences"],
                "session_resets": capture["expected_session_resets"],
                "session_rejections": capture[
                    "expected_session_rejections"
                ],
                "safe_fallbacks": capture["expected_safe_fallbacks"],
                "recoveries": capture["expected_endpoint_recoveries"],
                "stale_status_sent": capture["expected_stale_status_frames"],
                "stale_acks_sent": capture["expected_stale_ack_frames"],
            }
        )
    else:
        rtos_contract["acks_dropped"] = 0
    for key, expected in rtos_contract.items():
        require_equal(rtos, key, expected, f"{label} RTOS")

    starry = require_object(summary, "starry", label)
    require_equal(starry, "mode", capture["controller_policy"], f"{label} StarryOS")
    require_equal(starry, "count", capture["command_count"], f"{label} StarryOS")
    require_equal(starry, "period_ms", capture["period_ms"], f"{label} StarryOS")
    if "controller_fault_profile" in capture:
        require_equal(
            starry,
            "fault_profile",
            capture["controller_fault_profile"],
            f"{label} StarryOS",
        )

    if capture["analyzer_profile"] == "error":
        validate_error_summary(summary, capture, label)
    elif capture["analyzer_profile"] == "restart":
        validate_restart_summary(summary, run_dir, capture, label)

    lifecycle = require_object(summary, "lifecycle", label)
    for key in (
        "starry_done",
        "rtos_powered_off",
        "host_filesystem_synced",
        "volatile_block_snapshotted",
        "board_linux_restored",
    ):
        require_equal(lifecycle, key, True, f"{label} lifecycle")
    snapshot = require_object(lifecycle, "block_snapshot", f"{label} lifecycle")
    require_equal(snapshot, "filesystem_check", "clean", f"{label} snapshot")

    raw = require_object(summary, "raw_samples", label)
    raw_hash = sha256_file(run_dir / "raw.csv")
    require_equal(raw, "sha256", raw_hash, f"{label} raw samples")
    require_equal(raw, "guest_manifest_sha256", raw_hash, f"{label} raw samples")
    require_equal(
        raw,
        "artifact_sha256",
        sha256_file(run_dir / "raw.csv.gz"),
        f"{label} raw samples",
    )
    require_equal(raw, "sample_count", capture["command_count"], f"{label} raw samples")
    require_equal(raw, "dropped_samples", 0, f"{label} raw samples")

    source_log = require_object(summary, "source_log", label)
    require_equal(
        source_log,
        "sha256",
        sha256_file(run_dir / "console.log.gz"),
        f"{label} source log",
    )
    require_equal(
        source_log,
        "content_sha256",
        sha256_file(run_dir / "console.log"),
        f"{label} source log",
    )


def validate_run(
    run_dir: Path,
    run_id: str,
    run_number: int,
    capture: dict[str, object],
    board_type: str,
    source_commit: str,
    rootfs_sha256: str,
    expected_board_id: str | None,
) -> tuple[dict[str, object], str]:
    parse_manifest(run_dir, run_id, capture)
    validate_compressed_twins(run_dir, run_id, capture)
    command_count = capture.get("command_count")
    if isinstance(command_count, bool) or not isinstance(command_count, int):
        raise AggregationError("capture contract command_count must be an integer")
    validate_raw_csv(run_dir, run_id, command_count)
    if capture["analyzer_profile"] == "restart":
        pre_reset_count = require_integer(
            capture, "pre_reset_command_count", "capture contract"
        )
        validate_raw_csv(
            run_dir,
            run_id,
            pre_reset_count,
            filename="raw-before-reset.csv",
        )

    summary = load_object(run_dir / "summary.json", f"{run_id} summary")
    metadata = load_object(run_dir / "metadata.json", f"{run_id} metadata")
    require_equal(metadata, "schema_version", 1, f"{run_id} metadata")
    source = require_object(metadata, "source", f"{run_id} metadata")
    require_equal(source, "commit", source_commit, f"{run_id} source")
    require_equal(source, "dirty", False, f"{run_id} source")
    require_equal(source, "tracked_change_count", 0, f"{run_id} source")
    require_equal(source, "untracked_file_count", 0, f"{run_id} source")

    run = require_object(metadata, "run", f"{run_id} metadata")
    run_contract = {
        "board_type": board_type,
        "execution_order": run_number,
        "exit_status": 0,
        "profile": capture["runner_profile"],
        "repeat_count": capture["repeat_count"],
        "run_id": run_id,
        "run_number": run_number,
    }
    for key, expected in run_contract.items():
        require_equal(run, key, expected, f"{run_id} run")

    board = require_object(metadata, "board", f"{run_id} metadata")
    require_equal(board, "type", board_type, f"{run_id} board")
    board_id = require_string(board, "id", f"{run_id} board")
    if expected_board_id is not None and board_id != expected_board_id:
        raise AggregationError(f"{run_id} board id differs from earlier runs")

    inputs = require_object(metadata, "inputs", f"{run_id} metadata")
    rootfs = require_object(inputs, "rootfs", f"{run_id} inputs")
    require_equal(rootfs, "sha256", rootfs_sha256, f"{run_id} rootfs")
    model = require_object(metadata, "model", f"{run_id} metadata")
    require_equal(model, "backend", capture["inference_backend"], f"{run_id} model")

    outputs = require_object(metadata, "outputs", f"{run_id} metadata")
    validate_output_identity(
        require_object(outputs, "console_log", f"{run_id} outputs"),
        f"{run_id} console output",
        run_dir,
        "console.log.gz",
    )
    validate_output_identity(
        require_object(outputs, "raw_csv", f"{run_id} outputs"),
        f"{run_id} raw output",
        run_dir,
        "raw.csv.gz",
    )
    validate_output_identity(
        require_object(outputs, "summary", f"{run_id} outputs"),
        f"{run_id} summary output",
        run_dir,
        "summary.json",
    )

    result = require_object(metadata, "result", f"{run_id} metadata")
    require_equal(
        result,
        "controller_policy",
        capture["controller_policy"],
        f"{run_id} result",
    )
    require_equal(result, "sample_count", command_count, f"{run_id} result")
    require_equal(result, "dropped_samples", 0, f"{run_id} result")
    require_equal(result, "successful_marker", True, f"{run_id} result")
    require_equal(result, "validated", True, f"{run_id} result")

    validate_summary(summary, run_dir, run_id, board_id, capture)
    controller = require_object(summary, "controller", f"{run_id} summary")
    summary_board = require_object(summary, "board", f"{run_id} summary")
    raw = require_object(summary, "raw_samples", f"{run_id} summary")
    source_log = require_object(summary, "source_log", f"{run_id} summary")
    summary_rtos = require_object(summary, "rtos", f"{run_id} summary")
    run_evidence: dict[str, object] = {
        "run_id": run_id,
        "execution_order": run_number,
        "metadata": file_identity(run_dir, run_dir / "metadata.json"),
        "summary": file_identity(run_dir, run_dir / "summary.json"),
        "manifest": file_identity(run_dir, run_dir / "checksums.sha256"),
        "raw": {
            "sample_count": command_count,
            "content_sha256": raw["sha256"],
            "gzip_sha256": raw["artifact_sha256"],
        },
        "console": {
            "content_sha256": source_log["content_sha256"],
            "gzip_sha256": source_log["sha256"],
        },
        "fault_counts": {
            "retransmissions": controller["retransmissions"],
            "recoveries": controller["recoveries"],
            "duplicate_receives": summary_rtos["duplicates"],
            "error_frames": summary_rtos["errors_sent"],
            "protocol_errors": summary_rtos["protocol_errors"],
        },
        "lifecycle_gate_met": True,
        "validated": True,
        "controller": {
            metric: controller[metric]
            for metric in (*LATENCY_METRICS, *DESCRIPTIVE_METRICS)
        },
        "cpu_temp_milli_c": summary_board["cpu_temp_milli_c"],
    }
    if capture["analyzer_profile"] == "error":
        run_evidence["error_evidence"] = summary["error_evidence"]
        run_evidence["error_recovery"] = summary["error_recovery"]
    elif capture["analyzer_profile"] == "restart":
        pre_reset = require_object(
            summary, "pre_reset_raw_samples", f"{run_id} summary"
        )
        run_evidence["pre_reset_raw"] = {
            "sample_count": pre_reset["sample_count"],
            "content_sha256": pre_reset["sha256"],
            "gzip_sha256": pre_reset["artifact_sha256"],
        }
        run_evidence["restart_recovery"] = summary["restart_recovery"]
    return run_evidence, board_id


def validate_final_board_check(
    campaign_root: Path,
    path: Path,
    campaign_id: str,
    board_type: str,
) -> dict[str, object]:
    path = resolve_inside(campaign_root, path, "final board check")
    check = load_object(path, "final board check")
    require_equal(check, "schema_version", 1, "final board check")
    require_equal(check, "campaign_id", campaign_id, "final board check")
    board = require_object(check, "board", "final board check")
    require_equal(board, "type", board_type, "final board check board")
    require_equal(board, "hostname", "orangepi5plus", "final board check board")
    lease = require_object(check, "lease", "final board check")
    require_equal(lease, "allocated_before_ssh", True, "final board check lease")
    require_equal(lease, "released_after_ssh", True, "final board check lease")
    if require_number(lease, "available_after_release", "final board check lease") < 1:
        raise AggregationError("final board check did not release an available board")
    linux_root = require_object(check, "linux_root", "final board check")
    require_equal(linux_root, "filesystem", "ext4", "final board Linux root")
    if linux_root.get("read_write") is not True:
        raise AggregationError("final board Linux root is not read-write")
    options = require_string(linux_root, "options", "final board Linux root")
    if "rw" not in options.split(","):
        raise AggregationError("final board Linux root is not read-write")
    probe = require_object(check, "probe", "final board check")
    require_equal(probe, "exit_status", 0, "final board probe")
    require_equal(
        probe,
        "success_marker",
        "BOARD_FINAL_LINUX_RW_VERIFIED",
        "final board probe",
    )
    require_equal(check, "result", "PASS", "final board check")
    return file_identity(campaign_root, path)


def rounded(value: int | float) -> int | float:
    if isinstance(value, int):
        return value
    result = round(value, 6)
    return int(result) if result.is_integer() else result


def summarize_values(
    values: list[int | float], worst: str = "maximum"
) -> dict[str, object]:
    if len(values) < 2:
        raise AggregationError("at least two values are required for quartiles")
    quartiles = statistics.quantiles(values, n=4, method="inclusive")
    worst_value = max(values) if worst == "maximum" else min(values)
    return {
        "values_by_run": values,
        "median": rounded(statistics.median(values)),
        "q1": rounded(quartiles[0]),
        "q3": rounded(quartiles[2]),
        "iqr": rounded(quartiles[2] - quartiles[0]),
        "single_run_minimum": min(values),
        "single_run_maximum": max(values),
        "worst_of_runs": worst_value,
        "worst_direction": worst,
    }


def aggregate_campaign(
    campaign_root: Path,
    result_root: Path,
    latest_amendment: Path,
    final_board_check: Path,
) -> dict[str, object]:
    """Validate all frozen evidence and return a descriptive campaign summary."""
    campaign_root = campaign_root.resolve()
    preregistration_path, preregistration, capture = validate_preregistration(
        campaign_root
    )
    amendment_chain, latest = validate_amendment_chain(
        campaign_root, latest_amendment, preregistration_path
    )
    source_commit, rootfs_sha256 = expected_source_and_rootfs(latest)
    execution_order = require_list(capture, "execution_order", "capture contract")
    result_root = validate_result_root(
        campaign_root, result_root, latest, execution_order
    )
    platform = require_object(preregistration, "platform", "preregistration")
    board_type = require_string(platform, "board_type", "preregistration platform")
    campaign_id = require_string(preregistration, "campaign_id", "preregistration")

    runs: list[dict[str, object]] = []
    board_id: str | None = None
    for run_number, run_id_value in enumerate(execution_order, start=1):
        if not isinstance(run_id_value, str):
            raise AggregationError("capture execution order entries must be strings")
        run, board_id = validate_run(
            result_root / run_id_value,
            run_id_value,
            run_number,
            capture,
            board_type,
            source_commit,
            rootfs_sha256,
            board_id,
        )
        runs.append(run)

    final_check_identity = validate_final_board_check(
        campaign_root, final_board_check, campaign_id, board_type
    )
    latency_metrics = {
        metric: summarize_values(
            [require_object(run, "controller", "validated run")[metric] for run in runs]
        )
        for metric in LATENCY_METRICS
    }
    descriptive_metrics = {
        metric: summarize_values(
            [
                require_object(run, "controller", "validated run")[metric]
                for run in runs
            ],
            worst="minimum" if metric == "throughput_msg_s" else "maximum",
        )
        for metric in DESCRIPTIVE_METRICS
    }
    temperatures = summarize_values(
        [require_number(run, "cpu_temp_milli_c", "validated run") for run in runs]
    )
    statistics_policy = require_object(
        preregistration, "statistics_policy", "preregistration"
    )
    fault_contract: dict[str, object] = {
        "analyzer_profile": capture["analyzer_profile"],
        "expected_retransmissions_per_run": capture["expected_retransmissions"],
        "expected_recoveries_per_run": capture["expected_recoveries"],
        "expected_duplicate_receives_per_run": capture[
            "expected_duplicate_receives"
        ],
        "exact_equality_gate_met": True,
    }
    if capture["analyzer_profile"] == "ack-loss":
        fault_contract["configured_ack_losses_per_run"] = capture[
            "configured_ack_losses"
        ]
        assessment_reason = (
            "all three preregistered physical ACK-loss runs and the final "
            "Linux restoration gate passed"
        )
    elif capture["analyzer_profile"] == "error":
        fault_contract.update(
            {
                "expected_error_frames_per_run": capture["expected_error_frames"],
                "expected_protocol_errors_per_run": capture[
                    "expected_protocol_errors"
                ],
                "normal_control_must_continue_after_faults": capture[
                    "normal_control_must_continue_after_faults"
                ],
                "faults": expected_error_faults(capture),
            }
        )
        assessment_reason = (
            "all three preregistered physical ERROR runs and the final "
            "Linux restoration gate passed"
        )
    else:
        fault_contract.update(
            {
                "actual_vm_reset_required": capture[
                    "actual_vm_reset_required"
                ],
                "pre_reset_commands_per_run": capture[
                    "pre_reset_command_count"
                ],
                "post_reset_commands_per_run": capture["command_count"],
                "expected_session_resets_per_run": capture[
                    "expected_session_resets"
                ],
                "expected_session_rejections_per_run": capture[
                    "expected_session_rejections"
                ],
                "expected_safe_fallbacks_per_run": capture[
                    "expected_safe_fallbacks"
                ],
                "expected_endpoint_recoveries_per_run": capture[
                    "expected_endpoint_recoveries"
                ],
                "expected_stale_status_frames_per_run": capture[
                    "expected_stale_status_frames"
                ],
                "expected_stale_ack_frames_per_run": capture[
                    "expected_stale_ack_frames"
                ],
                "expected_retired_control_rejections_per_run": capture[
                    "expected_retired_control_rejections"
                ],
                "restart_vm_id": capture["restart_vm_id"],
                "restart_host_cpu": capture["restart_host_cpu"],
                "restart_delay_ms": capture["restart_delay_ms"],
                "restart_ready_timeout_ms": capture[
                    "restart_ready_timeout_ms"
                ],
                "previous_session": capture["previous_session"],
                "current_session": capture["current_session"],
            }
        )
        assessment_reason = (
            "all three preregistered physical guest-restart runs and the final "
            "Linux restoration gate passed"
        )

    return {
        "schema_version": 1,
        "campaign": {
            "campaign_id": campaign_id,
            "board_type": board_type,
            "board_id": board_id,
            "profile": capture["runner_profile"],
            "source_commit": source_commit,
            "rootfs_sha256": rootfs_sha256,
            "repeat_count": capture["repeat_count"],
            "run_order": execution_order,
            "result_root": result_root.relative_to(campaign_root).as_posix(),
        },
        "evidence": {
            "preregistration": file_identity(campaign_root, preregistration_path),
            "amendment_chain": amendment_chain,
            "final_board_linux_root_check": final_check_identity,
            "runs": runs,
        },
        "fault_contract": fault_contract,
        "latency": {
            "unit": "microseconds",
            "quartile_method": "inclusive quartiles",
            "claim_scope": require_string(
                statistics_policy, "latency_claim", "statistics policy"
            ),
            "metrics": latency_metrics,
        },
        "descriptive_metrics": descriptive_metrics,
        "board_temperature_milli_c": temperatures,
        "assessment": {
            "registered_run_count_met": len(runs) == capture["repeat_count"],
            "all_runs_validated": all(run["validated"] is True for run in runs),
            "all_manifests_verified": True,
            "all_raw_and_gzip_twins_verified": True,
            "exact_fault_contract_met": True,
            "all_lifecycle_gates_met": True,
            "final_board_linux_root_rw": True,
            "rt_isolation_claim": False,
            "campaign_gate_met": True,
            "reason": assessment_reason,
        },
    }


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign_root", type=Path)
    parser.add_argument("--result-root", required=True, type=Path)
    parser.add_argument("--latest-amendment", required=True, type=Path)
    parser.add_argument("--final-board-check", required=True, type=Path)
    parser.add_argument("--output", type=Path, help="summary JSON; defaults to stdout")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        result = aggregate_campaign(
            args.campaign_root,
            args.result_root,
            args.latest_amendment,
            args.final_board_check,
        )
    except AggregationError as error:
        print(f"IVC board campaign aggregation failed: {error}", file=sys.stderr)
        return 2

    rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        try:
            args.output.write_bytes(rendered.encode("utf-8"))
        except OSError as error:
            print(f"cannot write campaign summary: {error}", file=sys.stderr)
            return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
