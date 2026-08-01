#!/usr/bin/env python3
"""Validate one Linux/Zephyr AxVisor IVC console log and emit JSON evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
from pathlib import Path
from typing import Sequence


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
CONTROLLER_PREFIX = "IVC-CONTROLLER-RESULT "
RTOS_PREFIX = "IVC-RTOS-PROGRESS "
RTOS_READY_PREFIX = "IVC-RTOS-READY "
RTOS_RESULT_PREFIX = "IVC-RTOS-RESULT "
ACK_LOSS_INJECT_PREFIX = "IVC-RTOS-INJECT "
DUPLICATE_PREFIX = "IVC-RTOS-DUPLICATE "
LINUX_DONE = "IVC-LINUX-DONE exit=0"
POLICIES = {"neural", "manual-fixed"}
RUN_PROFILES = {"normal", "ack-loss"}
INTEGER_FIELDS = (
    "sent",
    "acknowledged",
    "errors",
    "timeouts",
    "retransmissions",
    "recoveries",
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
    "max_overshoot_milli_c",
)
FLOAT_FIELDS = (
    "success_percent",
    "throughput_msg_s",
    "rmse_milli_c",
    "iae_milli_c_s",
)


class AnalysisError(ValueError):
    """The raw console log violates the IVC acceptance contract."""


def analyze(
    log_path: Path,
    expected_count: int,
    *,
    profile: str = "normal",
    drop_ack_every: int = 0,
) -> dict[str, object]:
    if expected_count <= 0:
        raise AnalysisError("expected count must be positive")
    validate_profile(profile, expected_count, drop_ack_every)
    text = ANSI_ESCAPE.sub("", log_path.read_text(encoding="utf-8", errors="replace"))
    lines = text.splitlines()
    controller = find_single_record(lines, CONTROLLER_PREFIX)
    progress_index, progress = find_final_progress(lines, expected_count)
    if LINUX_DONE not in lines:
        raise AnalysisError(f"missing terminal marker: {LINUX_DONE}")

    result = parse_controller(controller, expected_count)
    if profile == "normal":
        validate_normal_controller(result)
        reject_fault_markers(lines)
        rtos = parse_rtos_progress(progress, expected_count, expected_duplicates=0)
    else:
        expected_recoveries = expected_count // drop_ack_every
        validate_ack_loss_controller(result, expected_recoveries)
        rtos = parse_ack_loss_rtos(
            lines,
            progress_index,
            progress,
            expected_count,
            drop_ack_every,
            expected_recoveries,
        )
    return {
        "schema_version": 1,
        "profile": profile,
        "source_log": {
            "path": str(log_path),
            "sha256": sha256_file(log_path),
        },
        "controller": result,
        "rtos": rtos,
    }


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


def find_single_record(lines: list[str], prefix: str) -> dict[str, str]:
    records = [parse_fields(line, prefix) for line in lines if line.startswith(prefix)]
    if len(records) != 1:
        raise AnalysisError(f"expected exactly one {prefix.strip()} record, found {len(records)}")
    return records[0]


def find_final_progress(
    lines: list[str], expected_count: int
) -> tuple[int, dict[str, str]]:
    records = [
        (index, parse_fields(line, RTOS_PREFIX))
        for index, line in enumerate(lines)
        if line.startswith(RTOS_PREFIX)
    ]
    matching = [
        (index, record)
        for index, record in records
        if record.get("accepted") == str(expected_count)
    ]
    if not matching:
        raise AnalysisError(f"missing RTOS progress record for accepted={expected_count}")
    return matching[-1]


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


def parse_controller(fields: dict[str, str], expected_count: int) -> dict[str, object]:
    policy = required(fields, "policy")
    if policy not in POLICIES:
        raise AnalysisError(f"unsupported controller policy: {policy}")
    integers = {field: integer(fields, field) for field in INTEGER_FIELDS}
    floats = {field: floating(fields, field) for field in FLOAT_FIELDS}
    if integers["sent"] != expected_count or integers["acknowledged"] != expected_count:
        raise AnalysisError("controller sent/acknowledged count does not match expected count")
    if integers["errors"] != 0 or integers["timeouts"] != 0:
        raise AnalysisError("controller reported application errors or timeouts")
    if floats["success_percent"] != 100.0:
        raise AnalysisError("controller success percentage is not 100")
    for family in ("full_loop", "pre_send", "transport"):
        values = [integers[f"{family}_{rank}_us"] for rank in ("p50", "p95", "p99", "max")]
        if values != sorted(values):
            raise AnalysisError(f"{family} latency percentiles are not monotonic")
    return {"policy": policy, **integers, **floats}


def validate_normal_controller(controller: dict[str, object]) -> None:
    if controller["retransmissions"] != 0 or controller["recoveries"] != 0:
        raise AnalysisError("normal run reported retransmissions or recoveries")


def reject_fault_markers(lines: list[str]) -> None:
    prefixes = (ACK_LOSS_INJECT_PREFIX, DUPLICATE_PREFIX, RTOS_RESULT_PREFIX)
    if any(line.startswith(prefixes) for line in lines):
        raise AnalysisError("normal run contains ACK-loss evidence markers")


def validate_ack_loss_controller(
    controller: dict[str, object], expected_recoveries: int
) -> None:
    if controller["retransmissions"] != expected_recoveries:
        raise AnalysisError(
            "controller retransmissions do not match deterministic ACK-loss count"
        )
    if controller["recoveries"] != expected_recoveries:
        raise AnalysisError(
            "controller recoveries do not match deterministic ACK-loss count"
        )


def parse_rtos_progress(
    fields: dict[str, str], expected_count: int, *, expected_duplicates: int
) -> dict[str, object]:
    accepted = integer(fields, "accepted")
    sequence = integer(fields, "seq")
    duplicates = integer(fields, "duplicates")
    protocol_errors = integer(fields, "protocol_errors")
    if accepted != expected_count or sequence != expected_count:
        raise AnalysisError("RTOS accepted/sequence count does not match expected count")
    if duplicates != expected_duplicates:
        raise AnalysisError("RTOS progress duplicate count does not match the run profile")
    if protocol_errors != 0:
        raise AnalysisError("RTOS reported protocol errors")
    return {
        "accepted": accepted,
        "sequence": sequence,
        "mode": required(fields, "mode"),
        "actuator_permille": integer(fields, "actuator_permille"),
        "measured_milli_c": integer(fields, "measured_milli_c"),
        "duplicates": duplicates,
        "protocol_errors": protocol_errors,
    }


def parse_ack_loss_rtos(
    lines: list[str],
    progress_index: int,
    progress: dict[str, str],
    expected_count: int,
    drop_ack_every: int,
    expected_recoveries: int,
) -> dict[str, object]:
    ready_index, ready = find_single_indexed_record(lines, RTOS_READY_PREFIX)
    if integer(ready, "ack_loss_drop_every") != drop_ack_every:
        raise AnalysisError("RTOS READY ACK-loss interval does not match the analyzer input")
    if integer(ready, "expected_commands") != expected_count:
        raise AnalysisError("RTOS READY command count does not match the analyzer input")

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
    duplicate_sequences = parse_duplicate_records(lines)
    if duplicate_sequences != expected_sequences:
        raise AnalysisError(
            "duplicate sequence set does not match the deterministic ACK-loss profile"
        )

    progress_duplicates = expected_recoveries
    if expected_count % drop_ack_every == 0:
        # The final fresh-command progress record precedes its retransmitted duplicate.
        progress_duplicates -= 1
    progress_result = parse_rtos_progress(
        progress,
        expected_count,
        expected_duplicates=progress_duplicates,
    )

    terminal_index, terminal = find_single_indexed_record(lines, RTOS_RESULT_PREFIX)
    validate_ack_loss_marker_order(
        lines,
        expected_sequences,
        ready_index,
        progress_index,
        terminal_index,
    )
    if required(terminal, "profile") != "ack-loss":
        raise AnalysisError("RTOS terminal result has the wrong profile")
    expected_terminal = {
        "accepted": expected_count,
        "applied": expected_count,
        "duplicates": expected_recoveries,
        "acks_dropped": expected_recoveries,
        "status_sent": expected_count + expected_recoveries,
        "acks_sent": expected_count,
        "errors_sent": 0,
        "protocol_errors": 0,
    }
    terminal_values = {
        field: integer(terminal, field) for field in expected_terminal
    }
    for field, expected in expected_terminal.items():
        if terminal_values[field] != expected:
            raise AnalysisError(
                f"RTOS terminal {field}={terminal_values[field]} does not match {expected}"
            )

    return {
        "accepted": terminal_values["accepted"],
        "applied": terminal_values["applied"],
        "sequence": progress_result["sequence"],
        "mode": progress_result["mode"],
        "actuator_permille": progress_result["actuator_permille"],
        "measured_milli_c": progress_result["measured_milli_c"],
        "duplicates": terminal_values["duplicates"],
        "progress_duplicates": progress_result["duplicates"],
        "acks_dropped": terminal_values["acks_dropped"],
        "status_sent": terminal_values["status_sent"],
        "acks_sent": terminal_values["acks_sent"],
        "errors_sent": terminal_values["errors_sent"],
        "protocol_errors": terminal_values["protocol_errors"],
        "injected_sequences": injected_sequences,
        "duplicate_sequences": duplicate_sequences,
    }


def parse_sequence_records(
    lines: list[str], prefix: str, sequence_field: str, description: str
) -> list[int]:
    records = [parse_fields(line, prefix) for line in lines if line.startswith(prefix)]
    sequences = [integer(record, sequence_field) for record in records]
    if len(sequences) != len(set(sequences)):
        raise AnalysisError(f"{description} records contain duplicate sequence markers")
    return sequences


def validate_ack_loss_marker_order(
    lines: list[str],
    expected_sequences: list[int],
    ready_index: int,
    progress_index: int,
    terminal_index: int,
) -> None:
    expected_events = [
        event
        for sequence in expected_sequences
        for event in (("inject", sequence), ("duplicate", sequence))
    ]
    observed_events: list[tuple[str, int]] = []
    event_indices: list[int] = []
    for index, line in enumerate(lines):
        if line.startswith(ACK_LOSS_INJECT_PREFIX):
            record = parse_fields(line, ACK_LOSS_INJECT_PREFIX)
            observed_events.append(("inject", integer(record, "drop_ack_seq")))
            event_indices.append(index)
        elif line.startswith(DUPLICATE_PREFIX):
            record = parse_fields(line, DUPLICATE_PREFIX)
            observed_events.append(("duplicate", integer(record, "seq")))
            event_indices.append(index)
    if observed_events != expected_events:
        raise AnalysisError("ACK-loss injection and recovery markers are not ordered")
    if not ready_index < progress_index < terminal_index:
        raise AnalysisError("RTOS READY, final progress, and terminal result are not ordered")
    if any(index <= ready_index or index >= terminal_index for index in event_indices):
        raise AnalysisError("ACK-loss marker appears outside the RTOS evidence interval")


def parse_duplicate_records(lines: list[str]) -> list[int]:
    records = [
        parse_fields(line, DUPLICATE_PREFIX)
        for line in lines
        if line.startswith(DUPLICATE_PREFIX)
    ]
    sequences: list[int] = []
    for index, record in enumerate(records, start=1):
        if integer(record, "duplicates") != index:
            raise AnalysisError("duplicate marker counter is not consecutive")
        sequences.append(integer(record, "seq"))
    if len(sequences) != len(set(sequences)):
        raise AnalysisError("duplicate records contain repeated sequence markers")
    return sequences


def find_single_indexed_record(
    lines: list[str], prefix: str
) -> tuple[int, dict[str, str]]:
    records = [
        (index, parse_fields(line, prefix))
        for index, line in enumerate(lines)
        if line.startswith(prefix)
    ]
    if len(records) != 1:
        raise AnalysisError(f"expected exactly one {prefix.strip()} record, found {len(records)}")
    return records[0]


def required(fields: dict[str, str], field: str) -> str:
    try:
        return fields[field]
    except KeyError as error:
        raise AnalysisError(f"missing required field: {field}") from error


def integer(fields: dict[str, str], field: str) -> int:
    value = required(fields, field)
    try:
        result = int(value)
    except ValueError as error:
        raise AnalysisError(f"field {field} is not an integer: {value}") from error
    if result < 0:
        raise AnalysisError(f"field {field} must be nonnegative: {value}")
    return result


def floating(fields: dict[str, str], field: str) -> float:
    value = required(fields, field)
    try:
        result = float(value)
    except ValueError as error:
        raise AnalysisError(f"field {field} is not numeric: {value}") from error
    if not math.isfinite(result) or result < 0.0:
        raise AnalysisError(f"field {field} must be finite and nonnegative: {value}")
    return result


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-count", type=int, default=1800)
    parser.add_argument("--profile", choices=sorted(RUN_PROFILES), default="normal")
    parser.add_argument("--drop-ack-every", type=int, default=0)
    return parser.parse_args(argv)


def main() -> int:
    args = parse_args()
    try:
        result = analyze(
            args.log,
            args.expected_count,
            profile=args.profile,
            drop_ack_every=args.drop_ack_every,
        )
    except (AnalysisError, OSError) as error:
        raise SystemExit(f"IVC analysis failed: {error}") from error
    args.output.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
