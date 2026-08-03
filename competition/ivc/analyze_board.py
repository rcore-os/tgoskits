#!/usr/bin/env python3
"""Validate one StarryOS/Zephyr Orange Pi IVC console capture."""

from __future__ import annotations

import argparse
import csv
import gzip
import hashlib
import json
import math
import re
import sys
import zlib
from pathlib import Path


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
GUEST_CONSOLE_PREFIX = re.compile(r"\[guest-console:[^\]\r\n]+\][ \t]*")
OUTCOME_PREFIX = "IVC-CONTROLLER-OUTCOME "
RELIABILITY_PREFIX = "IVC-CONTROLLER-RELIABILITY "
FULL_LOOP_PREFIX = "IVC-CONTROLLER-FULL-LOOP "
PRE_SEND_PREFIX = "IVC-CONTROLLER-PRE-SEND "
TRANSPORT_PREFIX = "IVC-CONTROLLER-TRANSPORT "
CONTROL_PREFIX = "IVC-CONTROLLER-CONTROL "
LEGACY_RESULT_PREFIX = "IVC-CONTROLLER-RESULT "
RTOS_RESULT_PREFIX = "IVC-RTOS-RESULT "
RTOS_OUTCOME_PREFIX = "IVC-RTOS-OUTCOME "
RTOS_MESSAGES_PREFIX = "IVC-RTOS-MESSAGES "
RTOS_POWEROFF_PREFIX = "IVC-RTOS-POWEROFF "
RTOS_READY_PREFIX = "IVC-RTOS-READY "
RTOS_RESTART_PREFIX = "IVC-RTOS-RESTART "
RTOS_RESTART_READY_PREFIX = "IVC-RTOS-RESTART-READY "
RTOS_SAFE_FALLBACK_PREFIX = "IVC-RTOS-SAFE-FALLBACK "
RTOS_RECOVERY_PREFIX = "IVC-RTOS-RECOVERY "
RTOS_STALE_REPLAY_PREFIX = "IVC-RTOS-STALE-REPLAY "
ACK_LOSS_INJECT_PREFIX = "IVC-RTOS-INJECT "
DUPLICATE_PREFIX = "IVC-RTOS-DUPLICATE "
CONTROLLER_ERROR_PREFIX = "IVC-ERROR-C "
RTOS_ERROR_PREFIX = "IVC-ERROR-Z "
CONTROLLER_ERROR_RESULT_PREFIX = "IVC-ERROR-RESULT "
CONTROLLER_RESTART_DUPLICATE_PREFIX = "IVC-RESTART-D "
CONTROLLER_RESTART_PREFIX = "IVC-RESTART-C "
CONTROLLER_RESTART_RESULT_PREFIX = "IVC-RESTART-RESULT "
ERROR_EVIDENCE_PREFIXES = (
    CONTROLLER_ERROR_PREFIX,
    RTOS_ERROR_PREFIX,
    CONTROLLER_ERROR_RESULT_PREFIX,
)
STARRY_BOOT_PREFIX = "IVC-STARRY-BOOT "
STARRY_NETWORK_PREFIX = "IVC-STARRY-NET "
STARRY_RAW_PREFIX = "IVC-STARRY-RAW "
STARRY_RESTART_ARMED_PREFIX = "IVC-STARRY-RESTART-ARMED "
STARRY_RESTART_RAW_PREFIX = "IVC-STARRY-RESTART-RAW "
STARRY_RESTART_RESUME_PREFIX = "IVC-STARRY-RESTART-RESUME "
GUEST_RAW_MANIFEST_PREFIX = "BOARD_GUEST_RAW_MANIFEST "
HARVEST_RAW_PREFIX = "BOARD_RAW_RESULT_HARVESTED "
GUEST_PRE_RESET_RAW_MANIFEST_PREFIX = "BOARD_GUEST_PRE_RESET_RAW_MANIFEST "
HARVEST_PRE_RESET_RAW_PREFIX = "BOARD_PRE_RESET_RAW_RESULT_HARVESTED "
BOARD_IDENTITY_PREFIX = "BOARD_IDENTITY "
BLOCK_SNAPSHOT_PREFIX = "BOARD_RESULT_IMAGE_VALIDATED "
STARRY_DONE = "IVC-STARRY-DONE exit=0"
SNAPSHOT_SYNCED = "AXVISOR_SNAPSHOT_SYNC_OK"
HOST_FILESYSTEM_SYNCED = "AXVISOR_HOST_FILESYSTEM_SYNCED"
HOST_FILESYSTEM_SYNC_CONFIRMED = (
    "Axvisor filesystem sync marker was confirmed by the board test"
)
BOARD_LINUX_RESTORED = "BOARD_LINUX_RESTORED"
RAW_COLUMNS = (
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
AXVISOR_RESTART_ARMED_PREFIX = "AXVISOR_GUEST_RESTART_ARMED "
AXVISOR_RESTART_PLACED_PREFIX = "AXVISOR_GUEST_RESTART_PLACED "
AXVISOR_RESTART_RUNNING_PREFIX = "AXVISOR_GUEST_RESTART_RUNNING "
AXVISOR_RESTART_TRIGGER_PREFIX = "AXVISOR_GUEST_RESTART_TRIGGER "
AXVISOR_RESTART_COMPLETE_PREFIX = "AXVISOR_GUEST_RESTART_COMPLETE "
AXVISOR_RESTART_TIMING_PREFIX = "AXVISOR_GUEST_RESTART_TIMING "
AXVISOR_RESTART_RECORD_PREFIXES = (
    AXVISOR_RESTART_ARMED_PREFIX,
    AXVISOR_RESTART_PLACED_PREFIX,
    AXVISOR_RESTART_RUNNING_PREFIX,
    AXVISOR_RESTART_TRIGGER_PREFIX,
    AXVISOR_RESTART_COMPLETE_PREFIX,
    AXVISOR_RESTART_TIMING_PREFIX,
)
RUN_PROFILES = {"normal", "ack-loss", "error", "restart"}
RESTART_PREVIOUS_SESSION = 0x1111_1111
RESTART_CURRENT_SESSION = 0x2222_2222
ERROR_FAULT_CONTRACT = (
    ("unsupported-version", 1001, 2, "unsupported-version"),
    ("length-mismatch", 1002, 1, "length-mismatch"),
    ("checksum-mismatch", 1003, 3, "checksum-mismatch"),
    ("unexpected-message-type", 1004, 5, "unexpected-message-type"),
    ("invalid-session-transition", 1005, 4, "zero-session-or-sequence"),
)


class AnalysisError(ValueError):
    """The physical-board console log violates the acceptance contract."""


class ConflictingRecordsError(AnalysisError):
    """Multiple complete UART records disagree."""


def analyze(
    log_path: Path,
    expected_count: int,
    raw_path: Path | None = None,
    *,
    profile: str = "normal",
    drop_ack_every: int = 0,
    pre_reset_raw_path: Path | None = None,
    expected_pre_reset_count: int = 0,
) -> dict[str, object]:
    if expected_count <= 0:
        raise AnalysisError("expected count must be positive")
    validate_profile(
        profile,
        expected_count,
        drop_ack_every,
        pre_reset_raw_path,
        expected_pre_reset_count,
    )

    with open_evidence_text(log_path, errors="replace") as source:
        text = ANSI_ESCAPE.sub("", source.read())
    text = split_axvisor_restart_records(text)
    lines = GUEST_CONSOLE_PREFIX.sub("\n", text).splitlines()
    controller = parse_controller(
        lines,
        expected_count,
        profile,
        drop_ack_every,
        console_metrics_required=raw_path is None,
    )
    rtos_expected_count = expected_count + expected_pre_reset_count
    rtos = parse_rtos(lines, rtos_expected_count, profile, drop_ack_every)
    error_evidence = None
    error_recovery = None
    if profile == "error":
        error_evidence, error_recovery = parse_error_evidence(lines, expected_count)
    starry = parse_starry_boot(
        lines, expected_count, str(controller["policy"]), profile
    )
    raw_samples = None
    pre_reset_raw_samples = None
    board = None
    if raw_path is not None:
        raw_samples = parse_raw_samples(
            lines,
            raw_path,
            expected_count,
            int(starry["period_ms"]),
            controller,
        )
        board = parse_board_identity(lines)
    if pre_reset_raw_path is not None:
        pre_reset_raw_samples = parse_pre_reset_raw_samples(
            lines,
            pre_reset_raw_path,
            expected_pre_reset_count,
            int(starry["period_ms"]),
        )
    restart_recovery = None
    if profile == "restart":
        restart_recovery = parse_restart_recovery(
            lines,
            expected_count,
            expected_pre_reset_count,
        )
    network = parse_starry_network(lines)
    block_snapshot = parse_block_snapshot(lines, profile)
    require_marker(lines, STARRY_DONE)
    require_marker(lines, SNAPSHOT_SYNCED)
    require_any_marker(
        lines,
        (SNAPSHOT_SYNCED, HOST_FILESYSTEM_SYNCED, HOST_FILESYSTEM_SYNC_CONFIRMED),
    )
    require_marker(lines, BOARD_LINUX_RESTORED)

    result = {
        "schema_version": 2,
        "platform": "orangepi-5-plus",
        "guest": "starryos",
        "profile": profile,
        "source_log": {
            "path": str(log_path),
            "sha256": sha256_file(log_path),
            "content_sha256": sha256_evidence_content(log_path),
        },
        "controller": controller,
        "rtos": rtos,
        "starry": starry,
        "network": network,
        "lifecycle": {
            "starry_done": True,
            "rtos_powered_off": True,
            "volatile_block_snapshotted": True,
            "block_snapshot": block_snapshot,
            "host_filesystem_synced": True,
            "board_linux_restored": True,
        },
    }
    if raw_samples is not None:
        result["raw_samples"] = raw_samples
    if pre_reset_raw_samples is not None:
        result["pre_reset_raw_samples"] = pre_reset_raw_samples
    if board is not None:
        result["board"] = board
    if error_evidence is not None and error_recovery is not None:
        result["error_evidence"] = error_evidence
        result["error_recovery"] = error_recovery
    if restart_recovery is not None:
        result["restart_recovery"] = restart_recovery
    return result


def split_axvisor_restart_records(text: str) -> str:
    """Expose intact restart records even when adjacent UART text is damaged."""
    for prefix in AXVISOR_RESTART_RECORD_PREFIXES:
        text = text.replace(prefix, f"\n{prefix}")
    return text


def parse_controller(
    lines: list[str],
    expected_count: int,
    profile: str,
    drop_ack_every: int,
    *,
    console_metrics_required: bool = True,
) -> dict[str, object]:
    outcome_fields = ("policy", "sent", "acknowledged", "errors", "timeouts")
    reliability_fields = ("retransmissions", "recoveries", "success_percent")
    legacy_result = find_optional_record(
        lines,
        LEGACY_RESULT_PREFIX,
        (*outcome_fields, *reliability_fields),
    )
    outcome = select_controller_record(
        find_optional_record(lines, OUTCOME_PREFIX, outcome_fields),
        legacy_result,
        outcome_fields,
        OUTCOME_PREFIX,
    )
    reliability = select_controller_record(
        find_optional_record(lines, RELIABILITY_PREFIX, reliability_fields),
        legacy_result,
        reliability_fields,
        RELIABILITY_PREFIX,
    )
    policy = required(outcome, "policy", OUTCOME_PREFIX)
    if policy not in {"neural", "manual-fixed"}:
        raise AnalysisError(f"unsupported controller policy: {policy}")

    result: dict[str, object] = {
        "policy": policy,
        "sent": integer(outcome, "sent", OUTCOME_PREFIX),
        "acknowledged": integer(outcome, "acknowledged", OUTCOME_PREFIX),
        "errors": integer(outcome, "errors", OUTCOME_PREFIX),
        "timeouts": integer(outcome, "timeouts", OUTCOME_PREFIX),
        "retransmissions": integer(
            reliability, "retransmissions", RELIABILITY_PREFIX
        ),
        "recoveries": integer(reliability, "recoveries", RELIABILITY_PREFIX),
        "success_percent": floating(
            reliability, "success_percent", RELIABILITY_PREFIX
        ),
    }
    result.update(parse_console_metrics(lines, console_metrics_required))

    if result["sent"] != expected_count or result["acknowledged"] != expected_count:
        raise AnalysisError("controller sent/acknowledged count does not match expected count")
    for field in ("errors", "timeouts"):
        if result[field] != 0:
            raise AnalysisError(f"controller {field} must be zero")
    expected_recoveries = (
        expected_count // drop_ack_every if profile == "ack-loss" else 0
    )
    for field in ("retransmissions", "recoveries"):
        if result[field] != expected_recoveries:
            raise AnalysisError(
                f"controller {field} does not match the deterministic ACK-loss count"
            )
    if result["success_percent"] != 100.0:
        raise AnalysisError("controller success percentage is not 100")
    if "throughput_msg_s" in result and result["throughput_msg_s"] <= 0:
        raise AnalysisError("controller throughput must be positive")
    return result


def select_controller_record(
    compact_record: dict[str, str] | None,
    legacy_result: dict[str, str] | None,
    required_fields: tuple[str, ...],
    compact_prefix: str,
) -> dict[str, str]:
    if compact_record is None:
        if legacy_result is None:
            raise AnalysisError(f"missing complete {compact_prefix.strip()} record")
        return legacy_result
    if legacy_result is not None and any(
        compact_record[field] != legacy_result[field] for field in required_fields
    ):
        raise AnalysisError(
            f"conflicting complete {compact_prefix.strip()} and "
            f"{LEGACY_RESULT_PREFIX.strip()} records"
        )
    return compact_record


def parse_console_metrics(
    lines: list[str], metrics_required: bool
) -> dict[str, object]:
    latency_fields = ("p50_us", "p95_us", "p99_us", "max_us")
    latency_records = (
        (FULL_LOOP_PREFIX, "full_loop", latency_fields),
        (PRE_SEND_PREFIX, "pre_send", latency_fields),
        (
            TRANSPORT_PREFIX,
            "transport",
            (*latency_fields, "throughput_msg_s"),
        ),
    )
    result: dict[str, object] = {}
    for prefix, family, required_fields in latency_records:
        fields = read_console_metric_record(
            lines,
            prefix,
            required_fields,
            metrics_required=metrics_required,
        )
        if fields is None:
            continue
        result.update(parse_latency_fields(fields, family))
        if family == "transport":
            result["throughput_msg_s"] = floating(
                fields, "throughput_msg_s", TRANSPORT_PREFIX
            )

    control = read_console_metric_record(
        lines,
        CONTROL_PREFIX,
        ("rmse_milli_c", "iae_milli_c_s", "max_overshoot_milli_c"),
        metrics_required=metrics_required,
    )
    if control is not None:
        result.update(
            {
                "rmse_milli_c": floating(
                    control, "rmse_milli_c", CONTROL_PREFIX
                ),
                "iae_milli_c_s": floating(
                    control, "iae_milli_c_s", CONTROL_PREFIX
                ),
                "max_overshoot_milli_c": integer(
                    control, "max_overshoot_milli_c", CONTROL_PREFIX
                ),
            }
        )
    return result


def read_console_metric_record(
    lines: list[str],
    prefix: str,
    required_fields: tuple[str, ...],
    *,
    metrics_required: bool,
) -> dict[str, str] | None:
    if metrics_required:
        return find_record(lines, prefix, required_fields)
    try:
        return find_optional_record(lines, prefix, required_fields)
    except ConflictingRecordsError:
        # Hash-verified raw samples replace UART metric copies damaged in transit.
        return None


def parse_latency_fields(fields: dict[str, str], family: str) -> dict[str, int]:
    prefix_by_family = {
        "full_loop": FULL_LOOP_PREFIX,
        "pre_send": PRE_SEND_PREFIX,
        "transport": TRANSPORT_PREFIX,
    }
    prefix = prefix_by_family[family]
    values = [integer(fields, rank, prefix) for rank in ("p50_us", "p95_us", "p99_us", "max_us")]
    if values != sorted(values):
        raise AnalysisError(f"{family} latency percentiles are not monotonic")
    return {
        f"{family}_p50_us": values[0],
        f"{family}_p95_us": values[1],
        f"{family}_p99_us": values[2],
        f"{family}_max_us": values[3],
    }


def parse_rtos(
    lines: list[str],
    expected_count: int,
    profile: str,
    drop_ack_every: int,
) -> dict[str, object]:
    outcome = find_record(
        lines,
        RTOS_OUTCOME_PREFIX,
        (
            "profile",
            "accepted",
            "applied",
            "duplicates",
            "acks_dropped",
        ),
    )
    messages = find_record(
        lines,
        RTOS_MESSAGES_PREFIX,
        (
            "status_sent",
            "acks_sent",
            "errors_sent",
            "protocol_errors",
        ),
    )
    result: dict[str, object] = {
        "profile": required(outcome, "profile", RTOS_OUTCOME_PREFIX),
        "accepted": integer(outcome, "accepted", RTOS_OUTCOME_PREFIX),
        "applied": integer(outcome, "applied", RTOS_OUTCOME_PREFIX),
        "duplicates": integer(outcome, "duplicates", RTOS_OUTCOME_PREFIX),
        "acks_dropped": integer(outcome, "acks_dropped", RTOS_OUTCOME_PREFIX),
        "status_sent": integer(messages, "status_sent", RTOS_MESSAGES_PREFIX),
        "acks_sent": integer(messages, "acks_sent", RTOS_MESSAGES_PREFIX),
        "errors_sent": integer(messages, "errors_sent", RTOS_MESSAGES_PREFIX),
        "protocol_errors": integer(
            messages, "protocol_errors", RTOS_MESSAGES_PREFIX
        ),
    }
    if result["profile"] != profile:
        raise AnalysisError("RTOS profile does not match the selected run profile")
    if profile == "normal":
        validate_normal_rtos(lines, result, expected_count)
    elif profile == "ack-loss":
        validate_ack_loss_rtos(lines, result, expected_count, drop_ack_every)
    elif profile == "error":
        validate_error_rtos(lines, result, expected_count)
    else:
        validate_restart_rtos(lines, result, expected_count)

    poweroff = find_record(lines, RTOS_POWEROFF_PREFIX, ("accepted",))
    if integer(poweroff, "accepted", RTOS_POWEROFF_PREFIX) != expected_count:
        raise AnalysisError("RTOS poweroff count does not match expected count")
    return result


def validate_normal_rtos(
    lines: list[str], result: dict[str, object], expected_count: int
) -> None:
    for field in ("accepted", "applied", "status_sent", "acks_sent"):
        if result[field] != expected_count:
            raise AnalysisError(f"RTOS {field} does not match expected count")
    for field in ("duplicates", "acks_dropped", "errors_sent", "protocol_errors"):
        if result[field] != 0:
            raise AnalysisError(f"RTOS {field} must be zero")
    if any(
        line.startswith((ACK_LOSS_INJECT_PREFIX, DUPLICATE_PREFIX)) for line in lines
    ):
        raise AnalysisError("normal run contains ACK-loss evidence markers")
    if any(
        line.startswith(ERROR_EVIDENCE_PREFIXES)
        for line in lines
    ):
        raise AnalysisError("normal run contains error-profile evidence markers")


def validate_ack_loss_rtos(
    lines: list[str],
    result: dict[str, object],
    expected_count: int,
    drop_ack_every: int,
) -> None:
    ready = find_record(
        lines,
        RTOS_READY_PREFIX,
        ("ack_loss_drop_every", "expected_commands", "exit_after_expected"),
    )
    if integer(ready, "ack_loss_drop_every", RTOS_READY_PREFIX) != drop_ack_every:
        raise AnalysisError("RTOS READY ACK-loss interval does not match the run profile")
    if integer(ready, "expected_commands", RTOS_READY_PREFIX) != expected_count:
        raise AnalysisError("RTOS READY command count does not match the run profile")
    if integer(ready, "exit_after_expected", RTOS_READY_PREFIX) != 1:
        raise AnalysisError("physical ACK-loss endpoint must power off after completion")

    expected_sequences = list(range(drop_ack_every, expected_count + 1, drop_ack_every))
    injected_sequences = parse_sequence_records(
        lines,
        ACK_LOSS_INJECT_PREFIX,
        "drop_ack_seq",
        "injected ACK-loss",
    )
    if injected_sequences != expected_sequences:
        raise AnalysisError(
            "injected ACK-loss sequence set does not match the deterministic profile"
        )
    duplicate_sequences = parse_sequence_records(
        lines,
        DUPLICATE_PREFIX,
        "seq",
        "duplicate command",
    )
    if duplicate_sequences != expected_sequences:
        raise AnalysisError(
            "duplicate sequence set does not match the deterministic ACK-loss profile"
        )
    validate_ack_loss_marker_order(lines, expected_sequences)

    expected_recoveries = len(expected_sequences)
    expected_fields = {
        "accepted": expected_count,
        "applied": expected_count,
        "duplicates": expected_recoveries,
        "acks_dropped": expected_recoveries,
        "status_sent": expected_count + expected_recoveries,
        "acks_sent": expected_count,
        "errors_sent": 0,
        "protocol_errors": 0,
    }
    for field, expected in expected_fields.items():
        if result[field] != expected:
            raise AnalysisError(
                f"RTOS {field}={result[field]} does not match deterministic "
                f"ACK-loss value {expected}"
            )
    result.update(
        {
            "drop_ack_every": drop_ack_every,
            "expected_recoveries": expected_recoveries,
            "injected_sequences": injected_sequences,
            "duplicate_sequences": duplicate_sequences,
        }
    )
    if any(
        line.startswith(ERROR_EVIDENCE_PREFIXES)
        for line in lines
    ):
        raise AnalysisError("ACK-loss run contains error-profile evidence markers")


def validate_error_rtos(
    lines: list[str], result: dict[str, object], expected_count: int
) -> None:
    expected_errors = len(ERROR_FAULT_CONTRACT)
    ready = find_record(
        lines,
        RTOS_READY_PREFIX,
        (
            "ack_loss_drop_every",
            "expected_commands",
            "expected_protocol_errors",
            "exit_after_expected",
        ),
    )
    ready_contract = {
        "ack_loss_drop_every": 0,
        "expected_commands": expected_count,
        "expected_protocol_errors": expected_errors,
        "exit_after_expected": 1,
    }
    for field, expected in ready_contract.items():
        if integer(ready, field, RTOS_READY_PREFIX) != expected:
            raise AnalysisError(
                f"RTOS READY {field} does not match the error profile"
            )
    expected_fields = {
        "accepted": expected_count,
        "applied": expected_count,
        "duplicates": 0,
        "acks_dropped": 0,
        "status_sent": expected_count,
        "acks_sent": expected_count,
        "errors_sent": expected_errors,
        "protocol_errors": expected_errors,
    }
    for field, expected in expected_fields.items():
        if result[field] != expected:
            raise AnalysisError(
                f"RTOS {field}={result[field]} does not match deterministic "
                f"error-profile value {expected}"
            )
    if any(
        line.startswith((ACK_LOSS_INJECT_PREFIX, DUPLICATE_PREFIX)) for line in lines
    ):
        raise AnalysisError("error profile contains ACK-loss evidence markers")


def validate_restart_rtos(
    lines: list[str], result: dict[str, object], expected_count: int
) -> None:
    ready = find_record(
        lines,
        RTOS_RESTART_READY_PREFIX,
        (
            "commands",
            "errors",
            "resets",
            "rejections",
            "safe",
            "drop",
            "exit",
        ),
    )
    ready_contract = {
        "commands": expected_count,
        "errors": 1,
        "resets": 1,
        "rejections": 1,
        "safe": 1,
        "drop": 0,
        "exit": 1,
    }
    for field, expected in ready_contract.items():
        if integer(ready, field, RTOS_RESTART_READY_PREFIX) != expected:
            raise AnalysisError(
                f"RTOS restart READY {field} does not match the profile"
            )

    duplicate_probe = find_integrity_record(
        lines,
        CONTROLLER_RESTART_DUPLICATE_PREFIX,
        ("seq", "status", "ack"),
    )
    duplicate_probe_contract = {"seq": 1, "status": 1, "ack": 1}
    for field, expected in duplicate_probe_contract.items():
        if integer(duplicate_probe, field, CONTROLLER_RESTART_DUPLICATE_PREFIX) != (
            expected
        ):
            raise AnalysisError(f"controller restart duplicate {field} is invalid")

    # The post-reset controller explicitly resends sequence one. The
    # checksummed controller record identifies the probe even when its
    # best-effort Zephyr event line is damaged on the shared UART. The RTOS
    # terminal counters below still prove that exactly one duplicate occurred.
    duplicate_sequences = [duplicate_probe_contract["seq"]]
    expected_response_frames = expected_count + len(duplicate_sequences) + 1
    expected_fields = {
        "accepted": expected_count,
        "applied": expected_count,
        "duplicates": len(duplicate_sequences),
        "acks_dropped": 0,
        "status_sent": expected_response_frames,
        "acks_sent": expected_response_frames,
        "errors_sent": 1,
        "protocol_errors": 1,
    }
    for field, expected in expected_fields.items():
        if result[field] != expected:
            raise AnalysisError(
                f"RTOS {field}={result[field]} does not match deterministic "
                f"restart-profile value {expected}"
            )
    result["duplicate_sequences"] = duplicate_sequences
    result["duplicate_probe"] = {
        "sequence": duplicate_probe_contract["seq"],
        "status_received": True,
        "ack_received": True,
    }

    restart = find_record(
        lines,
        RTOS_RESTART_PREFIX,
        (
            "session_resets",
            "session_rejections",
            "safe_fallbacks",
            "recoveries",
            "stale_status_sent",
            "stale_acks_sent",
        ),
    )
    for field in (
        "session_resets",
        "session_rejections",
        "safe_fallbacks",
        "recoveries",
        "stale_status_sent",
        "stale_acks_sent",
    ):
        value = integer(restart, field, RTOS_RESTART_PREFIX)
        if value != 1:
            raise AnalysisError(f"RTOS restart {field} must equal one")
        result[field] = value
    if any(line.startswith(ACK_LOSS_INJECT_PREFIX) for line in lines):
        raise AnalysisError("restart profile contains ACK-loss injection markers")


def parse_integrity_fields(
    line: str, prefix: str, field_order: tuple[str, ...]
) -> dict[str, str] | None:
    try:
        fields = parse_fields(line, prefix)
    except AnalysisError:
        return None
    if set(fields) != set(field_order) | {"crc"}:
        return None
    checksum = fields["crc"]
    if re.fullmatch(r"[0-9a-f]{8}", checksum) is None:
        return None
    body = " ".join(f"{field}={fields[field]}" for field in field_order)
    try:
        encoded_body = body.encode("ascii")
    except UnicodeEncodeError:
        return None
    expected = zlib.crc32(encoded_body) & 0xFFFF_FFFF
    if checksum != f"{expected:08x}":
        return None
    return {field: fields[field] for field in field_order}


def parse_distinct_integrity_records(
    lines: list[str],
    prefix: str,
    field_order: tuple[str, ...],
    identity_field: str,
) -> dict[str, dict[str, str]]:
    records: dict[str, dict[str, str]] = {}
    for line in lines:
        if not line.startswith(prefix):
            continue
        fields = parse_integrity_fields(line, prefix, field_order)
        if fields is None:
            continue
        identity = fields[identity_field]
        previous = records.get(identity)
        if previous is not None and previous != fields:
            raise AnalysisError(
                f"conflicting error evidence records for {identity_field}={identity}"
            )
        records[identity] = fields
    return records


def find_integrity_record(
    lines: list[str], prefix: str, field_order: tuple[str, ...]
) -> dict[str, str]:
    records = [
        fields
        for line in lines
        if line.startswith(prefix)
        and (fields := parse_integrity_fields(line, prefix, field_order)) is not None
    ]
    if not records:
        raise AnalysisError(f"missing complete checksummed {prefix.strip()} record")
    reference = records[0]
    if any(record != reference for record in records[1:]):
        raise ConflictingRecordsError(
            f"conflicting complete checksummed {prefix.strip()} records"
        )
    return reference


def parse_error_evidence(
    lines: list[str], expected_count: int
) -> tuple[list[dict[str, object]], dict[str, object]]:
    controller_records = parse_distinct_integrity_records(
        lines,
        CONTROLLER_ERROR_PREFIX,
        ("kind", "seq", "expected", "observed"),
        "kind",
    )
    rtos_records = parse_distinct_integrity_records(
        lines,
        RTOS_ERROR_PREFIX,
        ("seq", "code", "reason"),
        "seq",
    )
    if len(controller_records) != len(ERROR_FAULT_CONTRACT) or len(
        rtos_records
    ) != len(ERROR_FAULT_CONTRACT):
        raise AnalysisError("error evidence does not contain all five fault records")

    evidence: list[dict[str, object]] = []
    for kind, sequence, error_code, reason in ERROR_FAULT_CONTRACT:
        controller = controller_records.get(kind)
        rtos = rtos_records.get(str(sequence))
        if controller is None or rtos is None:
            raise AnalysisError(f"error evidence is missing {kind}")
        controller_contract = {
            "seq": sequence,
            "expected": error_code,
            "observed": error_code,
        }
        for field, expected in controller_contract.items():
            if integer(controller, field, CONTROLLER_ERROR_PREFIX) != expected:
                raise AnalysisError(
                    f"controller error evidence for {kind} has invalid {field}"
                )
        if integer(rtos, "code", RTOS_ERROR_PREFIX) != error_code:
            raise AnalysisError(f"RTOS error evidence for {kind} has the wrong code")
        if required(rtos, "reason", RTOS_ERROR_PREFIX) != reason:
            raise AnalysisError(f"RTOS error evidence for {kind} has the wrong reason")
        evidence.append(
            {
                "kind": kind,
                "sequence": sequence,
                "error_code": error_code,
                "reason": reason,
                "controller_observed": True,
                "rtos_observed": True,
            }
        )

    terminal = find_integrity_record(
        lines,
        CONTROLLER_ERROR_RESULT_PREFIX,
        (
            "profile",
            "injected",
            "received",
            "acknowledged",
            "continued",
        ),
    )
    require_equal_fields = {
        "profile": "error",
        "injected": str(len(ERROR_FAULT_CONTRACT)),
        "received": str(len(ERROR_FAULT_CONTRACT)),
        "acknowledged": str(expected_count),
        "continued": "1",
    }
    for field, expected in require_equal_fields.items():
        if required(terminal, field, CONTROLLER_ERROR_RESULT_PREFIX) != expected:
            raise AnalysisError(
                f"controller error recovery {field} does not match the profile"
            )
    return evidence, {
        "injected": len(ERROR_FAULT_CONTRACT),
        "errors_received": len(ERROR_FAULT_CONTRACT),
        "normal_acknowledged": expected_count,
        "continued": True,
    }


def parse_sequence_records(
    lines: list[str], prefix: str, sequence_field: str, description: str
) -> list[int]:
    sequences = [
        integer(parse_fields(line, prefix), sequence_field, prefix)
        for line in lines
        if line.startswith(prefix)
    ]
    if len(sequences) != len(set(sequences)):
        raise AnalysisError(f"{description} records contain duplicate sequence markers")
    return sequences


def validate_ack_loss_marker_order(
    lines: list[str], expected_sequences: list[int]
) -> None:
    ready_indexes = [
        index for index, line in enumerate(lines) if line.startswith(RTOS_READY_PREFIX)
    ]
    outcome_indexes = [
        index for index, line in enumerate(lines) if line.startswith(RTOS_OUTCOME_PREFIX)
    ]
    if len(ready_indexes) != 1:
        raise AnalysisError("ACK-loss run must contain exactly one RTOS READY record")
    if not outcome_indexes:
        raise AnalysisError("ACK-loss run is missing the RTOS outcome record")

    events: list[tuple[str, int, int]] = []
    for index, line in enumerate(lines):
        if line.startswith(ACK_LOSS_INJECT_PREFIX):
            fields = parse_fields(line, ACK_LOSS_INJECT_PREFIX)
            events.append(
                (
                    "inject",
                    integer(fields, "drop_ack_seq", ACK_LOSS_INJECT_PREFIX),
                    index,
                )
            )
        elif line.startswith(DUPLICATE_PREFIX):
            fields = parse_fields(line, DUPLICATE_PREFIX)
            events.append(
                ("duplicate", integer(fields, "seq", DUPLICATE_PREFIX), index)
            )
    expected_events = [
        event
        for sequence in expected_sequences
        for event in (("inject", sequence), ("duplicate", sequence))
    ]
    if [(kind, sequence) for kind, sequence, _ in events] != expected_events:
        raise AnalysisError("ACK-loss injection and duplicate markers are out of order")
    if not all(
        ready_indexes[0] < index < outcome_indexes[0] for _, _, index in events
    ):
        raise AnalysisError("ACK-loss markers are outside the READY/result interval")


def parse_starry_boot(
    lines: list[str],
    expected_count: int,
    controller_policy: str,
    run_profile: str,
) -> dict[str, object]:
    fields = find_record(
        lines, STARRY_BOOT_PREFIX, ("mode", "count", "period_ms", "vcpus")
    )
    mode = required(fields, "mode", STARRY_BOOT_PREFIX)
    expected_mode = "manual" if controller_policy == "manual-fixed" else controller_policy
    if mode != expected_mode:
        raise AnalysisError("StarryOS boot mode does not match controller policy")
    fault_profile = fields.get("fault_profile", "none")
    expected_fault_profile = (
        run_profile if run_profile in {"error", "restart"} else "none"
    )
    if fault_profile != expected_fault_profile:
        raise AnalysisError("StarryOS boot fault profile does not match the run profile")
    count = integer(fields, "count", STARRY_BOOT_PREFIX)
    if count != expected_count:
        raise AnalysisError("StarryOS boot count does not match expected count")
    period_ms = integer(fields, "period_ms", STARRY_BOOT_PREFIX)
    if period_ms <= 0:
        raise AnalysisError("StarryOS control period must be positive")
    vcpus = integer(fields, "vcpus", STARRY_BOOT_PREFIX)
    if vcpus < 2:
        raise AnalysisError("StarryOS must boot at least two vCPUs")
    return {
        "mode": mode,
        "fault_profile": fault_profile,
        "count": count,
        "period_ms": period_ms,
        "vcpus": vcpus,
    }


def parse_starry_network(lines: list[str]) -> dict[str, object]:
    fields = find_record(
        lines,
        STARRY_NETWORK_PREFIX,
        ("iface", "mac", "ip", "peer", "udp_port", "segment"),
    )
    expected = {
        "iface": "eth0",
        "ip": "10.0.0.1/24",
        "peer": "10.0.0.2",
        "udp_port": "5500",
        "segment": "1",
    }
    for field, value in expected.items():
        if required(fields, field, STARRY_NETWORK_PREFIX) != value:
            raise AnalysisError(f"unexpected StarryOS network {field}")
    return {
        "iface": fields["iface"],
        "mac": required(fields, "mac", STARRY_NETWORK_PREFIX),
        "ip": fields["ip"],
        "peer": fields["peer"],
        "udp_port": integer(fields, "udp_port", STARRY_NETWORK_PREFIX),
        "segment": integer(fields, "segment", STARRY_NETWORK_PREFIX),
    }


def parse_raw_samples(
    lines: list[str],
    raw_path: Path,
    expected_count: int,
    period_ms: int,
    controller: dict[str, object],
) -> dict[str, object]:
    uart_record = find_record(
        lines, STARRY_RAW_PREFIX, ("path", "samples", "sha256")
    )
    guest_manifest_record = find_record(
        lines, GUEST_RAW_MANIFEST_PREFIX, ("path", "samples", "sha256")
    )
    harvest_record = find_record(
        lines, HARVEST_RAW_PREFIX, ("path", "samples", "sha256")
    )
    guest_raw_path = "/var/lib/ivc/raw.csv"
    if required(uart_record, "path", STARRY_RAW_PREFIX) != guest_raw_path:
        raise AnalysisError("unexpected StarryOS raw CSV path")
    if (
        required(guest_manifest_record, "path", GUEST_RAW_MANIFEST_PREFIX)
        != guest_raw_path
    ):
        raise AnalysisError("unexpected snapshot guest raw CSV path")

    for record, prefix in (
        (uart_record, STARRY_RAW_PREFIX),
        (guest_manifest_record, GUEST_RAW_MANIFEST_PREFIX),
        (harvest_record, HARVEST_RAW_PREFIX),
    ):
        if integer(record, "samples", prefix) != expected_count:
            raise AnalysisError(
                f"{prefix.strip()} sample count does not match expected count"
            )

    uart_sha256 = required(uart_record, "sha256", STARRY_RAW_PREFIX)
    if re.fullmatch(r"[0-9a-f]{1,64}", uart_sha256) is None:
        raise AnalysisError("invalid SHA-256 fragment in IVC-STARRY-RAW record")
    uart_sha256_complete = len(uart_sha256) == 64
    guest_manifest_sha256 = complete_sha256(
        guest_manifest_record, GUEST_RAW_MANIFEST_PREFIX
    )
    harvest_sha256 = complete_sha256(harvest_record, HARVEST_RAW_PREFIX)
    actual_sha256 = sha256_evidence_content(raw_path)
    if actual_sha256 != guest_manifest_sha256 or actual_sha256 != harvest_sha256:
        raise AnalysisError(
            "raw CSV SHA-256 does not match snapshot guest manifest and harvest records"
        )
    if uart_sha256_complete and actual_sha256 != uart_sha256:
        raise AnalysisError("complete UART SHA-256 conflicts with harvested raw CSV")

    rows = read_raw_rows(raw_path, expected_count)
    derived = derive_raw_metrics(rows, period_ms)
    cross_check_raw_metrics(controller, derived)
    controller.update(derived)
    return {
        "path": str(raw_path),
        "sha256": actual_sha256,
        "artifact_sha256": sha256_file(raw_path),
        "guest_manifest_sha256": guest_manifest_sha256,
        "uart_sha256": uart_sha256,
        "uart_sha256_complete": uart_sha256_complete,
        "sample_count": len(rows),
        "dropped_samples": expected_count - len(rows),
        "deadline_misses": derived["deadline_misses"],
    }


def parse_pre_reset_raw_samples(
    lines: list[str],
    raw_path: Path,
    expected_count: int,
    period_ms: int,
) -> dict[str, object]:
    uart_record = find_record(
        lines, STARRY_RESTART_RAW_PREFIX, ("path", "samples", "sha256")
    )
    guest_manifest_record = find_record(
        lines,
        GUEST_PRE_RESET_RAW_MANIFEST_PREFIX,
        ("path", "samples", "sha256"),
    )
    harvest_record = find_record(
        lines,
        HARVEST_PRE_RESET_RAW_PREFIX,
        ("path", "samples", "sha256"),
    )
    guest_raw_path = "/var/lib/ivc/raw-before-reset.csv"
    if required(uart_record, "path", STARRY_RESTART_RAW_PREFIX) != guest_raw_path:
        raise AnalysisError("unexpected StarryOS pre-reset raw CSV path")
    if (
        required(
            guest_manifest_record, "path", GUEST_PRE_RESET_RAW_MANIFEST_PREFIX
        )
        != guest_raw_path
    ):
        raise AnalysisError("unexpected snapshot guest pre-reset raw CSV path")
    for record, prefix in (
        (uart_record, STARRY_RESTART_RAW_PREFIX),
        (guest_manifest_record, GUEST_PRE_RESET_RAW_MANIFEST_PREFIX),
        (harvest_record, HARVEST_PRE_RESET_RAW_PREFIX),
    ):
        if integer(record, "samples", prefix) != expected_count:
            raise AnalysisError(
                f"{prefix.strip()} sample count does not match expected count"
            )

    uart_sha256 = required(uart_record, "sha256", STARRY_RESTART_RAW_PREFIX)
    if re.fullmatch(r"[0-9a-f]{1,64}", uart_sha256) is None:
        raise AnalysisError("invalid SHA-256 fragment in pre-reset raw record")
    guest_manifest_sha256 = complete_sha256(
        guest_manifest_record, GUEST_PRE_RESET_RAW_MANIFEST_PREFIX
    )
    harvest_sha256 = complete_sha256(harvest_record, HARVEST_PRE_RESET_RAW_PREFIX)
    actual_sha256 = sha256_evidence_content(raw_path)
    if actual_sha256 not in {guest_manifest_sha256, harvest_sha256} or (
        guest_manifest_sha256 != harvest_sha256
    ):
        raise AnalysisError(
            "pre-reset raw CSV SHA-256 does not match snapshot and harvest records"
        )
    if len(uart_sha256) == 64 and actual_sha256 != uart_sha256:
        raise AnalysisError("complete pre-reset UART SHA-256 conflicts with raw CSV")

    rows = read_raw_rows(raw_path, expected_count)
    derived = derive_raw_metrics(rows, period_ms)
    return {
        "path": str(raw_path),
        "sha256": actual_sha256,
        "artifact_sha256": sha256_file(raw_path),
        "guest_manifest_sha256": guest_manifest_sha256,
        "uart_sha256": uart_sha256,
        "uart_sha256_complete": len(uart_sha256) == 64,
        "sample_count": len(rows),
        "dropped_samples": expected_count - len(rows),
        "deadline_misses": derived["deadline_misses"],
        "full_loop_p99_us": derived["full_loop_p99_us"],
        "full_loop_max_us": derived["full_loop_max_us"],
    }


def parse_restart_recovery(
    lines: list[str], expected_count: int, expected_pre_reset_count: int
) -> dict[str, object]:
    armed = find_record(
        lines,
        AXVISOR_RESTART_ARMED_PREFIX,
        ("schema", "vm_id", "host_cpu", "delay_ms", "ready_timeout_ms"),
    )
    placed = find_record(
        lines,
        AXVISOR_RESTART_PLACED_PREFIX,
        ("schema", "vm_id", "requested_pcpu", "actual_pcpu", "affinity_mask"),
    )
    running = find_record(
        lines,
        AXVISOR_RESTART_RUNNING_PREFIX,
        ("schema", "vm_id", "host_cpu", "ready_wait_ms", "status"),
    )
    trigger = find_record(
        lines,
        AXVISOR_RESTART_TRIGGER_PREFIX,
        (
            "schema",
            "vm_id",
            "host_cpu",
            "requested_delay_ms",
            "observed_delay_ms",
            "before_status",
            "reset_count",
        ),
    )
    complete = find_record(
        lines,
        AXVISOR_RESTART_COMPLETE_PREFIX,
        (
            "schema",
            "vm_id",
            "host_cpu",
            "before_status",
            "after_status",
            "reset_count",
        ),
    )
    timing = find_record(
        lines,
        AXVISOR_RESTART_TIMING_PREFIX,
        (
            "schema",
            "vm_id",
            "host_cpu",
            "ready_wait_ms",
            "requested_delay_ms",
            "observed_delay_ms",
        ),
    )
    for record, prefix in (
        (armed, AXVISOR_RESTART_ARMED_PREFIX),
        (placed, AXVISOR_RESTART_PLACED_PREFIX),
        (running, AXVISOR_RESTART_RUNNING_PREFIX),
        (trigger, AXVISOR_RESTART_TRIGGER_PREFIX),
        (complete, AXVISOR_RESTART_COMPLETE_PREFIX),
        (timing, AXVISOR_RESTART_TIMING_PREFIX),
    ):
        if integer(record, "schema", prefix) != 1:
            raise AnalysisError(f"{prefix.strip()} schema must equal one")
        if integer(record, "vm_id", prefix) != 1:
            raise AnalysisError(f"{prefix.strip()} must target StarryOS VM 1")
    host_cpu = integer(armed, "host_cpu", AXVISOR_RESTART_ARMED_PREFIX)
    if host_cpu != 3:
        raise AnalysisError("Axvisor restart worker must use reserved host CPU 3")
    placement_contract = {
        "requested_pcpu": host_cpu,
        "actual_pcpu": host_cpu,
        "affinity_mask": 1 << host_cpu,
    }
    for field, expected in placement_contract.items():
        if integer(placed, field, AXVISOR_RESTART_PLACED_PREFIX) != expected:
            raise AnalysisError(f"Axvisor restart placement {field} is invalid")
    for record, prefix in (
        (running, AXVISOR_RESTART_RUNNING_PREFIX),
        (trigger, AXVISOR_RESTART_TRIGGER_PREFIX),
        (complete, AXVISOR_RESTART_COMPLETE_PREFIX),
        (timing, AXVISOR_RESTART_TIMING_PREFIX),
    ):
        if integer(record, "host_cpu", prefix) != host_cpu:
            raise AnalysisError(f"{prefix.strip()} host CPU conflicts with placement")
    if required(running, "status", AXVISOR_RESTART_RUNNING_PREFIX) != "running":
        raise AnalysisError("Axvisor restart target was not observed running")
    for record, prefix in (
        (trigger, AXVISOR_RESTART_TRIGGER_PREFIX),
        (complete, AXVISOR_RESTART_COMPLETE_PREFIX),
    ):
        if required(record, "before_status", prefix) != "running":
            raise AnalysisError("Axvisor reset did not start from a running VM")
        if integer(record, "reset_count", prefix) != 1:
            raise AnalysisError("Axvisor reset count must equal one")
    if required(complete, "after_status", AXVISOR_RESTART_COMPLETE_PREFIX) != "running":
        raise AnalysisError("Axvisor reset did not produce a running replacement VM")

    requested_delay_ms = integer(armed, "delay_ms", AXVISOR_RESTART_ARMED_PREFIX)
    ready_timeout_ms = integer(
        armed, "ready_timeout_ms", AXVISOR_RESTART_ARMED_PREFIX
    )
    ready_wait_ms = integer(running, "ready_wait_ms", AXVISOR_RESTART_RUNNING_PREFIX)
    observed_delay_ms = integer(
        trigger, "observed_delay_ms", AXVISOR_RESTART_TRIGGER_PREFIX
    )
    if requested_delay_ms <= 0 or ready_timeout_ms <= 0:
        raise AnalysisError("Axvisor restart bounds must be positive")
    if ready_wait_ms > ready_timeout_ms:
        raise AnalysisError("Axvisor restart target exceeded its ready timeout")
    if observed_delay_ms < requested_delay_ms:
        raise AnalysisError("Axvisor guest reset fired before its configured delay")
    timing_contract = {
        "ready_wait_ms": ready_wait_ms,
        "requested_delay_ms": requested_delay_ms,
        "observed_delay_ms": observed_delay_ms,
    }
    if integer(trigger, "requested_delay_ms", AXVISOR_RESTART_TRIGGER_PREFIX) != (
        requested_delay_ms
    ):
        raise AnalysisError("Axvisor restart trigger delay conflicts with armed config")
    for field, expected in timing_contract.items():
        if integer(timing, field, AXVISOR_RESTART_TIMING_PREFIX) != expected:
            raise AnalysisError(f"Axvisor restart timing {field} conflicts")

    starry_armed = find_record(
        lines,
        STARRY_RESTART_ARMED_PREFIX,
        ("phase", "session_id", "samples"),
    )
    starry_resume = find_record(
        lines,
        STARRY_RESTART_RESUME_PREFIX,
        ("phase", "old_session", "new_session", "first_samples"),
    )
    if required(starry_armed, "phase", STARRY_RESTART_ARMED_PREFIX) != "before-reset":
        raise AnalysisError("StarryOS restart arm phase is invalid")
    if required(starry_resume, "phase", STARRY_RESTART_RESUME_PREFIX) != "after-reset":
        raise AnalysisError("StarryOS restart resume phase is invalid")
    starry_contract = (
        (starry_armed, STARRY_RESTART_ARMED_PREFIX, "session_id", RESTART_PREVIOUS_SESSION),
        (starry_armed, STARRY_RESTART_ARMED_PREFIX, "samples", expected_pre_reset_count),
        (starry_resume, STARRY_RESTART_RESUME_PREFIX, "old_session", RESTART_PREVIOUS_SESSION),
        (starry_resume, STARRY_RESTART_RESUME_PREFIX, "new_session", RESTART_CURRENT_SESSION),
        (starry_resume, STARRY_RESTART_RESUME_PREFIX, "first_samples", expected_pre_reset_count),
    )
    for record, prefix, field, expected in starry_contract:
        if integer(record, field, prefix) != expected:
            raise AnalysisError(f"StarryOS restart {field} does not match the contract")

    safe_fallback = find_record(
        lines,
        RTOS_SAFE_FALLBACK_PREFIX,
        (
            "reason",
            "actuator_permille",
            "last_sequence",
            "session",
            "safe_fallbacks",
        ),
    )
    safe_contract = {
        "actuator_permille": 0,
        "last_sequence": expected_pre_reset_count,
        "session": RESTART_PREVIOUS_SESSION,
        "safe_fallbacks": 1,
    }
    if required(safe_fallback, "reason", RTOS_SAFE_FALLBACK_PREFIX) != (
        "controller-timeout"
    ):
        raise AnalysisError("RTOS SAFE-FALLBACK reason is not controller timeout")
    for field, expected in safe_contract.items():
        if integer(safe_fallback, field, RTOS_SAFE_FALLBACK_PREFIX) != expected:
            raise AnalysisError(f"RTOS SAFE-FALLBACK {field} is invalid")

    stale_replay = find_record(
        lines,
        RTOS_STALE_REPLAY_PREFIX,
        (
            "old_session",
            "old_sequence",
            "new_session",
            "stale_status_sent",
            "stale_acks_sent",
        ),
    )
    stale_contract = {
        "old_session": RESTART_PREVIOUS_SESSION,
        "old_sequence": expected_pre_reset_count,
        "new_session": RESTART_CURRENT_SESSION,
        "stale_status_sent": 1,
        "stale_acks_sent": 1,
    }
    for field, expected in stale_contract.items():
        if integer(stale_replay, field, RTOS_STALE_REPLAY_PREFIX) != expected:
            raise AnalysisError(f"RTOS stale replay {field} is invalid")

    recovery = find_record(
        lines,
        RTOS_RECOVERY_PREFIX,
        ("session", "seq", "from", "mode", "actuator_permille", "recoveries"),
    )
    if integer(recovery, "session", RTOS_RECOVERY_PREFIX) != RESTART_CURRENT_SESSION:
        raise AnalysisError("RTOS recovery used the wrong session")
    if integer(recovery, "seq", RTOS_RECOVERY_PREFIX) != 1:
        raise AnalysisError("RTOS recovery did not begin at sequence one")
    if required(recovery, "from", RTOS_RECOVERY_PREFIX) != "controller-timeout":
        raise AnalysisError("RTOS recovery did not follow controller timeout")
    if required(recovery, "mode", RTOS_RECOVERY_PREFIX) != "Neural":
        raise AnalysisError("RTOS recovery did not restore neural control")
    actuator = integer(recovery, "actuator_permille", RTOS_RECOVERY_PREFIX)
    if not 0 <= actuator <= 1000:
        raise AnalysisError("RTOS recovery actuator is outside its valid range")
    if integer(recovery, "recoveries", RTOS_RECOVERY_PREFIX) != 1:
        raise AnalysisError("RTOS recovery count must equal one")

    transport = find_integrity_record(
        lines,
        CONTROLLER_RESTART_PREFIX,
        ("old", "new", "ack_ignored", "status_ignored", "control_rejected"),
    )
    transport_contract = {
        "old": RESTART_PREVIOUS_SESSION,
        "new": RESTART_CURRENT_SESSION,
        "ack_ignored": 1,
        "status_ignored": 1,
        "control_rejected": 1,
    }
    for field, expected in transport_contract.items():
        if integer(transport, field, CONTROLLER_RESTART_PREFIX) != expected:
            raise AnalysisError(f"controller restart {field} is invalid")
    controller_result = find_integrity_record(
        lines,
        CONTROLLER_RESTART_RESULT_PREFIX,
        ("profile", "sent", "acknowledged", "continued"),
    )
    expected_controller_result = {
        "profile": "restart",
        "sent": str(expected_count),
        "acknowledged": str(expected_count),
        "continued": "1",
    }
    for field, expected in expected_controller_result.items():
        if required(controller_result, field, CONTROLLER_RESTART_RESULT_PREFIX) != expected:
            raise AnalysisError(f"controller restart result {field} is invalid")
    retired_error = find_integrity_record(
        lines, RTOS_ERROR_PREFIX, ("seq", "code", "reason")
    )
    retired_contract = {
        "seq": expected_pre_reset_count + 1,
        "code": 4,
    }
    for field, expected in retired_contract.items():
        if integer(retired_error, field, RTOS_ERROR_PREFIX) != expected:
            raise AnalysisError(f"retired-session ERROR {field} is invalid")
    if required(retired_error, "reason", RTOS_ERROR_PREFIX) != (
        "retired-or-invalid-session"
    ):
        raise AnalysisError("retired-session ERROR reason is invalid")

    ordered_events = (
        (
            AXVISOR_RESTART_ARMED_PREFIX,
            ("schema", "vm_id", "host_cpu", "delay_ms", "ready_timeout_ms"),
        ),
        (
            AXVISOR_RESTART_PLACED_PREFIX,
            ("schema", "vm_id", "requested_pcpu", "actual_pcpu", "affinity_mask"),
        ),
        (
            AXVISOR_RESTART_RUNNING_PREFIX,
            ("schema", "vm_id", "host_cpu", "ready_wait_ms", "status"),
        ),
        (STARRY_RESTART_ARMED_PREFIX, ("phase", "session_id", "samples")),
        (
            RTOS_SAFE_FALLBACK_PREFIX,
            ("reason", "actuator_permille", "last_sequence", "session", "safe_fallbacks"),
        ),
        (
            AXVISOR_RESTART_TRIGGER_PREFIX,
            (
                "schema",
                "vm_id",
                "host_cpu",
                "requested_delay_ms",
                "observed_delay_ms",
                "before_status",
                "reset_count",
            ),
        ),
        (
            STARRY_RESTART_RESUME_PREFIX,
            ("phase", "old_session", "new_session", "first_samples"),
        ),
        (
            RTOS_STALE_REPLAY_PREFIX,
            (
                "old_session",
                "old_sequence",
                "new_session",
                "stale_status_sent",
                "stale_acks_sent",
            ),
        ),
        (
            RTOS_RECOVERY_PREFIX,
            ("session", "seq", "from", "mode", "actuator_permille", "recoveries"),
        ),
        (
            AXVISOR_RESTART_COMPLETE_PREFIX,
            (
                "schema",
                "vm_id",
                "host_cpu",
                "before_status",
                "after_status",
                "reset_count",
            ),
        ),
    )
    event_indexes = [
        first_complete_record_index(lines, prefix, fields)
        for prefix, fields in ordered_events
    ]
    if event_indexes != sorted(event_indexes) or len(set(event_indexes)) != len(
        event_indexes
    ):
        raise AnalysisError("restart evidence markers are not in causal order")

    return {
        "actual_vm_reset": True,
        "vm_id": 1,
        "host_cpu": host_cpu,
        "reset_count": 1,
        "ready_wait_ms": ready_wait_ms,
        "requested_delay_ms": requested_delay_ms,
        "observed_delay_ms": observed_delay_ms,
        "old_session": RESTART_PREVIOUS_SESSION,
        "new_session": RESTART_CURRENT_SESSION,
        "pre_reset_samples": expected_pre_reset_count,
        "post_reset_samples": expected_count,
        "safe_fallback_observed": True,
        "recovered": True,
        "stale_ack_ignored": 1,
        "stale_status_ignored": 1,
        "retired_control_rejected": 1,
    }


def complete_sha256(record: dict[str, str], prefix: str) -> str:
    digest = required(record, "sha256", prefix)
    if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise AnalysisError(f"invalid SHA-256 in {prefix.strip()} record")
    return digest


def parse_board_identity(lines: list[str]) -> dict[str, object]:
    fields = find_record(
        lines,
        BOARD_IDENTITY_PREFIX,
        ("board_id", "hostname", "cpu_temp_milli_c"),
    )
    board_id = required(fields, "board_id", BOARD_IDENTITY_PREFIX)
    hostname = required(fields, "hostname", BOARD_IDENTITY_PREFIX)
    if board_id in {"unknown", "unavailable"}:
        raise AnalysisError("physical board ID is unavailable")
    if hostname in {"unknown", "unavailable"}:
        raise AnalysisError("physical board hostname is unavailable")
    cpu_temp_milli_c = integer(fields, "cpu_temp_milli_c", BOARD_IDENTITY_PREFIX)
    if not -40_000 <= cpu_temp_milli_c <= 150_000:
        raise AnalysisError("physical board CPU temperature is outside the valid range")
    return {
        "board_id": board_id,
        "hostname": hostname,
        "cpu_temp_milli_c": cpu_temp_milli_c,
    }


def parse_block_snapshot(lines: list[str], profile: str) -> dict[str, object]:
    fields = find_record(
        lines,
        BLOCK_SNAPSHOT_PREFIX,
        ("vm", "index", "path", "bytes", "sha256", "fsck"),
    )
    vm_id = integer(fields, "vm", BLOCK_SNAPSHOT_PREFIX)
    backing_index = integer(fields, "index", BLOCK_SNAPSHOT_PREFIX)
    image_bytes = integer(fields, "bytes", BLOCK_SNAPSHOT_PREFIX)
    image_path = required(fields, "path", BLOCK_SNAPSHOT_PREFIX)
    image_sha256 = required(fields, "sha256", BLOCK_SNAPSHOT_PREFIX)
    filesystem_check = required(fields, "fsck", BLOCK_SNAPSHOT_PREFIX)
    if vm_id != 1 or backing_index != 0:
        raise AnalysisError("BOARD_RESULT_IMAGE_VALIDATED selected an unexpected VM backing")
    if image_bytes <= 0:
        raise AnalysisError("BOARD_RESULT_IMAGE_VALIDATED image size must be positive")
    legacy_result_path = image_path.startswith(
        "/home/orangepi/"
    ) and image_path.endswith(".result.img")
    if profile == "ack-loss":
        compact_names = ("a",)
    elif profile == "error":
        compact_names = ("e",)
    elif profile == "restart":
        compact_names = ("r",)
    else:
        compact_names = ("n", "ns", "m", "ms")
    compact_result_path = image_path in {
        f"/home/orangepi/ivc-{name}" for name in compact_names
    }
    if not legacy_result_path and not compact_result_path:
        raise AnalysisError("BOARD_RESULT_IMAGE_VALIDATED path is not a result image")
    if re.fullmatch(r"[0-9a-f]{64}", image_sha256) is None:
        raise AnalysisError("BOARD_RESULT_IMAGE_VALIDATED SHA-256 is invalid")
    if filesystem_check != "clean":
        raise AnalysisError("BOARD_RESULT_IMAGE_VALIDATED filesystem check is not clean")
    return {
        "vm_id": vm_id,
        "backing_index": backing_index,
        "image_path": image_path,
        "image_bytes": image_bytes,
        "image_sha256": image_sha256,
        "filesystem_check": filesystem_check,
    }


def read_raw_rows(raw_path: Path, expected_count: int) -> list[dict[str, int]]:
    with open_evidence_text(raw_path, newline="") as source:
        reader = csv.DictReader(source)
        if tuple(reader.fieldnames or ()) != RAW_COLUMNS:
            raise AnalysisError("raw CSV columns do not match the evidence schema")
        encoded_rows = list(reader)
    if len(encoded_rows) != expected_count:
        raise AnalysisError("raw CSV sample count does not match expected count")

    rows: list[dict[str, int]] = []
    previous_response_us = -1
    previous_measured_milli_c: int | None = None
    for expected_sequence, encoded in enumerate(encoded_rows, start=1):
        if None in encoded or any(encoded[column] is None for column in RAW_COLUMNS):
            raise AnalysisError(f"raw CSV row {expected_sequence} has extra or missing fields")
        try:
            row = {column: int(encoded[column]) for column in RAW_COLUMNS}
        except ValueError as error:
            raise AnalysisError(
                f"raw CSV row {expected_sequence} contains a non-integer field"
            ) from error
        validate_raw_row(
            row,
            expected_sequence,
            previous_response_us,
            previous_measured_milli_c,
        )
        rows.append(row)
        previous_response_us = row["response_completed_us"]
        previous_measured_milli_c = row["measured_milli_c"]
    return rows


def validate_raw_row(
    row: dict[str, int],
    expected_sequence: int,
    previous_response_us: int,
    previous_measured_milli_c: int | None,
) -> None:
    if row["sequence"] != expected_sequence:
        raise AnalysisError(f"raw CSV sequence {row['sequence']} is not contiguous")
    cycle_started_us = row["cycle_started_us"]
    command_sent_us = row["command_sent_us"]
    response_completed_us = row["response_completed_us"]
    if not 0 <= cycle_started_us <= command_sent_us <= response_completed_us:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} has invalid timestamps")
    if cycle_started_us < previous_response_us:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} overlaps its predecessor")

    expected_pre_send = command_sent_us - cycle_started_us
    expected_transport = response_completed_us - command_sent_us
    expected_full_loop = response_completed_us - cycle_started_us
    if row["pre_send_us"] != expected_pre_send:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} has invalid pre-send latency")
    if row["transport_us"] != expected_transport:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} has invalid transport latency")
    if row["full_loop_us"] != expected_full_loop:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} has invalid full-loop latency")
    if (
        previous_measured_milli_c is not None
        and row["observed_milli_c"] != previous_measured_milli_c
    ):
        raise AnalysisError(f"raw CSV sequence {expected_sequence} breaks observation continuity")
    if row["command_actuator_permille"] != row["status_actuator_permille"]:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} actuator was not applied")
    expected_error = row["setpoint_milli_c"] - row["measured_milli_c"]
    if row["error_milli_c"] != expected_error:
        raise AnalysisError(f"raw CSV sequence {expected_sequence} has invalid control error")


def derive_raw_metrics(rows: list[dict[str, int]], period_ms: int) -> dict[str, object]:
    result: dict[str, object] = {}
    for family in ("full_loop", "pre_send", "transport"):
        values = sorted(row[f"{family}_us"] for row in rows)
        result.update(
            {
                f"{family}_p50_us": percentile(values, 50),
                f"{family}_p95_us": percentile(values, 95),
                f"{family}_p99_us": percentile(values, 99),
                f"{family}_max_us": values[-1],
            }
        )
    errors = [row["error_milli_c"] for row in rows]
    if len(rows) == 1:
        throughput_msg_s = 1000.0 / period_ms
    else:
        scheduled_span_us = rows[-1]["cycle_started_us"] - rows[0]["cycle_started_us"]
        if scheduled_span_us <= 0:
            raise AnalysisError("raw CSV scheduling span must be positive")
        throughput_msg_s = (len(rows) - 1) * 1_000_000 / scheduled_span_us
    result.update(
        {
            "throughput_msg_s": throughput_msg_s,
            "rmse_milli_c": math.sqrt(
                sum(error * error for error in errors) / len(errors)
            ),
            "iae_milli_c_s": sum(abs(error) for error in errors)
            * period_ms
            / 1000.0,
            "max_overshoot_milli_c": max(
                0,
                *(row["measured_milli_c"] - row["setpoint_milli_c"] for row in rows),
            ),
            "deadline_misses": sum(
                row["full_loop_us"] > period_ms * 1000 for row in rows
            ),
        }
    )
    return result


def percentile(sorted_values: list[int], percentage: int) -> int:
    return sorted_values[((len(sorted_values) - 1) * percentage) // 100]


def cross_check_raw_metrics(
    controller: dict[str, object], derived: dict[str, object]
) -> None:
    for key, derived_value in derived.items():
        if key in {"deadline_misses", "throughput_msg_s"} or key not in controller:
            continue
        console_value = controller.get(key)
        if isinstance(derived_value, float):
            if not isinstance(console_value, (int, float)) or not math.isclose(
                float(console_value), derived_value, rel_tol=0.0, abs_tol=0.000_501
            ):
                raise AnalysisError(f"console {key} does not match raw CSV")
        elif console_value != derived_value:
            raise AnalysisError(f"console {key} does not match raw CSV")


def find_record(
    lines: list[str], prefix: str, required_fields: tuple[str, ...]
) -> dict[str, str]:
    record = find_optional_record(lines, prefix, required_fields)
    if record is None:
        raise AnalysisError(f"missing complete {prefix.strip()} record")
    return record


def first_complete_record_index(
    lines: list[str], prefix: str, required_fields: tuple[str, ...]
) -> int:
    for index, line in enumerate(lines):
        if not line.startswith(prefix):
            continue
        try:
            fields = parse_fields(line, prefix)
        except AnalysisError:
            continue
        if all(field in fields for field in required_fields):
            return index
    raise AnalysisError(f"missing complete {prefix.strip()} record")


def find_optional_record(
    lines: list[str], prefix: str, required_fields: tuple[str, ...]
) -> dict[str, str] | None:
    complete_records: list[dict[str, str]] = []
    for line in lines:
        if not line.startswith(prefix):
            continue
        try:
            fields = parse_fields(line, prefix)
        except AnalysisError:
            continue
        if all(field in fields for field in required_fields):
            complete_records.append(fields)

    if not complete_records:
        return None
    reference = complete_records[0]
    if any(record != reference for record in complete_records[1:]):
        raise ConflictingRecordsError(
            f"conflicting complete {prefix.strip()} records"
        )
    return reference


def parse_fields(line: str, prefix: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for token in line.removeprefix(prefix).split():
        if "=" not in token:
            raise AnalysisError(f"malformed token in {prefix.strip()} record: {token}")
        key, value = token.split("=", 1)
        if not key or not value or key in fields:
            raise AnalysisError(f"invalid or duplicate field in {prefix.strip()} record: {token}")
        fields[key] = value
    return fields


def required(fields: dict[str, str], name: str, prefix: str) -> str:
    try:
        return fields[name]
    except KeyError as error:
        raise AnalysisError(f"missing {name} in {prefix.strip()} record") from error


def integer(fields: dict[str, str], name: str, prefix: str) -> int:
    value = required(fields, name, prefix)
    try:
        return int(value)
    except ValueError as error:
        raise AnalysisError(f"invalid integer {name}={value} in {prefix.strip()} record") from error


def floating(fields: dict[str, str], name: str, prefix: str) -> float:
    value = required(fields, name, prefix)
    try:
        return float(value)
    except ValueError as error:
        raise AnalysisError(f"invalid float {name}={value} in {prefix.strip()} record") from error


def require_marker(lines: list[str], marker: str) -> None:
    if marker not in lines:
        raise AnalysisError(f"missing terminal marker: {marker}")


def require_any_marker(lines: list[str], markers: tuple[str, ...]) -> None:
    if not any(marker in lines for marker in markers):
        raise AnalysisError(f"missing terminal marker: {' or '.join(markers)}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_evidence_content(path: Path) -> str:
    digest = hashlib.sha256()
    if path.suffix == ".gz":
        source_context = gzip.open(path, "rb")
    else:
        source_context = path.open("rb")
    with source_context as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def open_evidence_text(
    path: Path, *, errors: str | None = None, newline: str | None = None
):
    if path.suffix == ".gz":
        return gzip.open(
            path,
            "rt",
            encoding="utf-8",
            errors=errors,
            newline=newline,
        )
    return path.open(
        "r",
        encoding="utf-8",
        errors=errors,
        newline=newline,
    )


def validate_profile(
    profile: str,
    expected_count: int,
    drop_ack_every: int,
    pre_reset_raw_path: Path | None,
    expected_pre_reset_count: int,
) -> None:
    if profile not in RUN_PROFILES:
        raise AnalysisError(f"unsupported run profile: {profile}")
    if profile in {"normal", "error"}:
        if drop_ack_every != 0:
            raise AnalysisError(f"{profile} profile requires drop-ack-every=0")
        if pre_reset_raw_path is not None or expected_pre_reset_count != 0:
            raise AnalysisError(f"{profile} profile cannot use pre-reset raw samples")
        return
    if profile == "restart":
        if drop_ack_every != 0:
            raise AnalysisError("restart profile requires drop-ack-every=0")
        if pre_reset_raw_path is None or expected_pre_reset_count <= 0:
            raise AnalysisError(
                "restart profile requires positive pre-reset raw samples"
            )
        return
    if pre_reset_raw_path is not None or expected_pre_reset_count != 0:
        raise AnalysisError("ack-loss profile cannot use pre-reset raw samples")
    if drop_ack_every <= 0:
        raise AnalysisError("ack-loss profile requires a positive drop-ack-every value")
    if drop_ack_every > expected_count:
        raise AnalysisError("ack-loss profile must inject at least one dropped ACK")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--raw-csv", type=Path, required=True)
    parser.add_argument("--expected-count", type=int, required=True)
    parser.add_argument("--profile", choices=sorted(RUN_PROFILES), default="normal")
    parser.add_argument("--drop-ack-every", type=int, default=0)
    parser.add_argument("--pre-reset-raw-csv", type=Path)
    parser.add_argument("--expected-pre-reset-count", type=int, default=0)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    try:
        result = analyze(
            arguments.log,
            arguments.expected_count,
            arguments.raw_csv,
            profile=arguments.profile,
            drop_ack_every=arguments.drop_ack_every,
            pre_reset_raw_path=arguments.pre_reset_raw_csv,
            expected_pre_reset_count=arguments.expected_pre_reset_count,
        )
        arguments.output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    except (AnalysisError, OSError) as error:
        print(f"IVC board analysis failed: {error}", file=sys.stderr)
        return 1
    print(f"IVC board analysis passed: {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
