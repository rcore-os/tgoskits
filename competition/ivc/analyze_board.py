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
from pathlib import Path


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
GUEST_CONSOLE_PREFIX = re.compile(r"\[guest-console:[^]]+\]\s*")
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
ACK_LOSS_INJECT_PREFIX = "IVC-RTOS-INJECT "
DUPLICATE_PREFIX = "IVC-RTOS-DUPLICATE "
STARRY_BOOT_PREFIX = "IVC-STARRY-BOOT "
STARRY_NETWORK_PREFIX = "IVC-STARRY-NET "
STARRY_RAW_PREFIX = "IVC-STARRY-RAW "
HARVEST_RAW_PREFIX = "BOARD_RAW_RESULT_HARVESTED "
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
RUN_PROFILES = {"normal", "ack-loss"}


class AnalysisError(ValueError):
    """The physical-board console log violates the acceptance contract."""


def analyze(
    log_path: Path,
    expected_count: int,
    raw_path: Path | None = None,
    *,
    profile: str = "normal",
    drop_ack_every: int = 0,
) -> dict[str, object]:
    if expected_count <= 0:
        raise AnalysisError("expected count must be positive")
    validate_profile(profile, expected_count, drop_ack_every)

    with open_evidence_text(log_path, errors="replace") as source:
        text = ANSI_ESCAPE.sub("", source.read())
    lines = GUEST_CONSOLE_PREFIX.sub("\n", text).splitlines()
    controller = parse_controller(
        lines,
        expected_count,
        profile,
        drop_ack_every,
        console_metrics_required=raw_path is None,
    )
    rtos = parse_rtos(lines, expected_count, profile, drop_ack_every)
    starry = parse_starry_boot(lines, expected_count, str(controller["policy"]))
    raw_samples = None
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
    if board is not None:
        result["board"] = board
    return result


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
    record_reader = find_record if metrics_required else find_optional_record
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
        fields = record_reader(lines, prefix, required_fields)
        if fields is None:
            continue
        result.update(parse_latency_fields(fields, family))
        if family == "transport":
            result["throughput_msg_s"] = floating(
                fields, "throughput_msg_s", TRANSPORT_PREFIX
            )

    control = record_reader(
        lines,
        CONTROL_PREFIX,
        ("rmse_milli_c", "iae_milli_c_s", "max_overshoot_milli_c"),
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
    else:
        validate_ack_loss_rtos(lines, result, expected_count, drop_ack_every)

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
    lines: list[str], expected_count: int, controller_policy: str
) -> dict[str, object]:
    fields = find_record(
        lines, STARRY_BOOT_PREFIX, ("mode", "count", "period_ms", "vcpus")
    )
    mode = required(fields, "mode", STARRY_BOOT_PREFIX)
    expected_mode = "manual" if controller_policy == "manual-fixed" else controller_policy
    if mode != expected_mode:
        raise AnalysisError("StarryOS boot mode does not match controller policy")
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
    guest_record = find_record(
        lines, STARRY_RAW_PREFIX, ("path", "samples", "sha256")
    )
    harvest_record = find_record(
        lines, HARVEST_RAW_PREFIX, ("path", "samples", "sha256")
    )
    if required(guest_record, "path", STARRY_RAW_PREFIX) != "/var/lib/ivc/raw.csv":
        raise AnalysisError("unexpected StarryOS raw CSV path")
    for record, prefix in (
        (guest_record, STARRY_RAW_PREFIX),
        (harvest_record, HARVEST_RAW_PREFIX),
    ):
        if integer(record, "samples", prefix) != expected_count:
            raise AnalysisError(f"{prefix.strip()} sample count does not match expected count")
        digest = required(record, "sha256", prefix)
        if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise AnalysisError(f"invalid SHA-256 in {prefix.strip()} record")

    actual_sha256 = sha256_evidence_content(raw_path)
    guest_sha256 = required(guest_record, "sha256", STARRY_RAW_PREFIX)
    harvest_sha256 = required(harvest_record, "sha256", HARVEST_RAW_PREFIX)
    if actual_sha256 != guest_sha256 or actual_sha256 != harvest_sha256:
        raise AnalysisError("raw CSV SHA-256 does not match guest and harvest records")

    rows = read_raw_rows(raw_path, expected_count)
    derived = derive_raw_metrics(rows, period_ms)
    cross_check_raw_metrics(controller, derived)
    controller.update(derived)
    return {
        "path": str(raw_path),
        "sha256": actual_sha256,
        "artifact_sha256": sha256_file(raw_path),
        "sample_count": len(rows),
        "dropped_samples": expected_count - len(rows),
        "deadline_misses": derived["deadline_misses"],
    }


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
    compact_names = ("a",) if profile == "ack-loss" else ("n", "ns", "m", "ms")
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
        raise AnalysisError(f"conflicting complete {prefix.strip()} records")
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


def validate_profile(profile: str, expected_count: int, drop_ack_every: int) -> None:
    if profile not in RUN_PROFILES:
        raise AnalysisError(f"unsupported run profile: {profile}")
    if profile == "normal":
        if drop_ack_every != 0:
            raise AnalysisError("normal profile requires drop-ack-every=0")
        return
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
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    try:
        result = analyze(
            arguments.log,
            arguments.expected_count,
            arguments.raw_csv,
            profile=arguments.profile,
            drop_ack_every=arguments.drop_ack_every,
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
