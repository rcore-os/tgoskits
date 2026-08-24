"""Immutable slot receipts for a physical-board StarryOS RT campaign."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Mapping

import formal_campaign_contract as contract


Slot = contract.Slot
EVIDENCE_NAMES = (
    "stage_log",
    "console_log",
    "harvest_log",
    "summary",
    "raw",
    "guest_irq",
    "host_trace",
)


def campaign_slots() -> tuple[Slot, ...]:
    pairs = tuple(
        Slot("pair", pair, profile)
        for pair, order in enumerate(contract.PAIR_ORDER, start=1)
        for profile in order
    )
    soaks = tuple(Slot("soak", None, profile) for profile in contract.SOAK_ORDER)
    return pairs + soaks


def slot_directory(result_root: Path, slot: Slot) -> Path:
    if slot.phase == "pair" and slot.pair is not None:
        return result_root / f"pair-{slot.pair}" / slot.profile
    if slot.phase == "soak" and slot.pair is None:
        return result_root / "soak" / slot.profile
    raise contract.ContractError(f"invalid formal campaign slot: {slot}")


def receipt_path(result_root: Path, slot: Slot) -> Path:
    return slot_directory(result_root, slot) / "receipt.json"


def next_slot(
    preregistration: Mapping[str, object], result_root: Path
) -> Slot | None:
    """Return the next slot after revalidating every completed receipt."""

    if preregistration.get("pair_order") != contract.pair_order_document():
        raise contract.ContractError("pair order differs from the frozen AB/BA contract")
    first_missing: Slot | None = None
    for slot in campaign_slots():
        path = receipt_path(result_root, slot)
        if not path.is_file():
            if first_missing is None:
                first_missing = slot
            continue
        if first_missing is not None:
            raise contract.ContractError(
                f"out-of-order receipt exists for {slot.phase} {slot.profile}"
            )
        validate_receipt(preregistration, result_root, slot, path)
    return first_missing


def build_receipt(
    preregistration: Mapping[str, object],
    result_root: Path,
    slot: Slot,
    stage_log: Path,
    console_log: Path,
    harvest_log: Path,
    summary_path: Path,
    raw_path: Path,
    guest_irq_path: Path,
    host_trace_path: Path,
    started_at: str,
    finished_at: str,
) -> dict[str, object]:
    """Build one receipt only for the next fully validated campaign slot."""

    expected = next_slot(preregistration, result_root)
    if expected != slot:
        raise contract.ContractError(
            f"requested slot {slot} is not the next frozen slot {expected}"
        )
    started = parse_timestamp(started_at, "started_at")
    finished = parse_timestamp(finished_at, "finished_at")
    if finished < started:
        raise contract.ContractError("finished_at precedes started_at")
    evidence_paths = {
        "stage_log": stage_log,
        "console_log": console_log,
        "harvest_log": harvest_log,
        "summary": summary_path,
        "raw": raw_path,
        "guest_irq": guest_irq_path,
        "host_trace": host_trace_path,
    }
    validate_attempt_paths(result_root, slot, evidence_paths)
    stage_identity, harvest_identity = validate_runtime_evidence(
        preregistration, slot, evidence_paths
    )
    source = contract.require_object(preregistration, "source", "preregistration")
    receipt = {
        "schema_version": 1,
        "slot": {
            "phase": slot.phase,
            "pair": slot.pair,
            "profile": slot.profile,
        },
        "source": {
            "commit": source["commit"],
            "tree": source["tree"],
        },
        "board": {
            "type": stage_identity["type"],
            "service_id": stage_identity["service_id"],
            "board_id": stage_identity["board_id"],
            "hostname": stage_identity["hostname"],
            "stage_cpu_temp_milli_c": stage_identity["cpu_temp_milli_c"],
            "harvest_cpu_temp_milli_c": harvest_identity["cpu_temp_milli_c"],
        },
        "started_at": started.isoformat().replace("+00:00", "Z"),
        "finished_at": finished.isoformat().replace("+00:00", "Z"),
        "evidence": {
            name: file_evidence(path, result_root)
            for name, path in evidence_paths.items()
        },
    }
    validate_receipt_document(preregistration, result_root, slot, receipt)
    return receipt


def validate_receipt(
    preregistration: Mapping[str, object],
    result_root: Path,
    slot: Slot,
    path: Path,
) -> None:
    document = contract.read_json(path, "formal receipt")
    validate_receipt_document(preregistration, result_root, slot, document)


def validate_receipt_document(
    preregistration: Mapping[str, object],
    result_root: Path,
    slot: Slot,
    document: Mapping[str, object],
) -> None:
    expected_fields = {
        "schema_version",
        "slot",
        "source",
        "board",
        "started_at",
        "finished_at",
        "evidence",
    }
    if set(document) != expected_fields or document.get("schema_version") != 1:
        raise contract.ContractError("formal receipt schema is invalid")
    receipt_slot = contract.require_object(document, "slot", "receipt")
    if receipt_slot != {
        "phase": slot.phase,
        "pair": slot.pair,
        "profile": slot.profile,
    }:
        raise contract.ContractError("formal receipt slot differs from its path")
    source = contract.require_object(document, "source", "receipt")
    frozen_source = contract.require_object(
        preregistration, "source", "preregistration"
    )
    if source != {
        "commit": frozen_source.get("commit"),
        "tree": frozen_source.get("tree"),
    }:
        raise contract.ContractError("formal receipt source differs from preregistration")
    started = parse_timestamp(
        contract.require_string(document, "started_at", "receipt"), "started_at"
    )
    finished = parse_timestamp(
        contract.require_string(document, "finished_at", "receipt"), "finished_at"
    )
    if finished < started:
        raise contract.ContractError("formal receipt finished_at precedes started_at")

    evidence = contract.require_object(document, "evidence", "receipt")
    if set(evidence) != set(EVIDENCE_NAMES):
        raise contract.ContractError("formal receipt evidence set is invalid")
    evidence_paths = {
        name: validate_evidence_record(
            name,
            contract.require_object(evidence, name, "receipt evidence"),
            result_root,
        )
        for name in EVIDENCE_NAMES
    }
    validate_attempt_paths(result_root, slot, evidence_paths)
    stage_identity, harvest_identity = validate_runtime_evidence(
        preregistration, slot, evidence_paths
    )
    board = contract.require_object(document, "board", "receipt")
    expected_board = {
        "type": stage_identity["type"],
        "service_id": stage_identity["service_id"],
        "board_id": stage_identity["board_id"],
        "hostname": stage_identity["hostname"],
        "stage_cpu_temp_milli_c": stage_identity["cpu_temp_milli_c"],
        "harvest_cpu_temp_milli_c": harvest_identity["cpu_temp_milli_c"],
    }
    if board != expected_board:
        raise contract.ContractError("formal receipt board evidence is inconsistent")


def validate_runtime_evidence(
    preregistration: Mapping[str, object],
    slot: Slot,
    evidence_paths: Mapping[str, Path],
) -> tuple[dict[str, object], dict[str, object]]:
    stage_text = evidence_paths["stage_log"].read_text(
        encoding="utf-8", errors="strict"
    )
    # A raw serial capture may contain firmware bytes outside UTF-8 during
    # reset. Marker validation uses decoded text, while the receipt continues
    # to bind the original console bytes by size and SHA-256.
    console_text = evidence_paths["console_log"].read_text(
        encoding="utf-8", errors="replace"
    )
    harvest_text = evidence_paths["harvest_log"].read_text(
        encoding="utf-8", errors="strict"
    )
    stage_identity = contract.validate_stage_log(preregistration, stage_text)
    contract.validate_console_log(
        preregistration,
        console_text,
        slot.profile,
        soak=slot.phase == "soak",
    )
    if "AXVISOR_RT_STARRY_HARVESTED " not in harvest_text:
        raise contract.ContractError("harvest completion marker is missing")
    harvest_identity = contract.validate_harvest_identity(
        preregistration, harvest_text, stage_identity
    )
    summary = contract.read_json(evidence_paths["summary"], "RT summary")
    contract.validate_summary(
        preregistration,
        summary,
        slot.profile,
        soak=slot.phase == "soak",
    )
    validate_summary_file_identity(
        summary,
        evidence_paths["raw"],
        evidence_paths["guest_irq"],
        evidence_paths["host_trace"],
    )
    return stage_identity, harvest_identity


def validate_attempt_paths(
    result_root: Path, slot: Slot, evidence_paths: Mapping[str, Path]
) -> None:
    attempt_root = (slot_directory(result_root, slot) / "attempts").resolve()
    parents = set()
    for name, path in evidence_paths.items():
        resolved = path.resolve()
        if not resolved.is_relative_to(attempt_root):
            raise contract.ContractError(
                f"formal receipt {name} is outside the slot attempts directory"
            )
        parents.add(resolved.parent)
    if len(parents) != 1:
        raise contract.ContractError("formal receipt evidence spans multiple attempts")


def validate_evidence_record(
    name: str, record: Mapping[str, object], result_root: Path
) -> Path:
    if set(record) != {"path", "sha256", "size_bytes"}:
        raise contract.ContractError(f"formal receipt {name} record is invalid")
    display_path = contract.require_string(record, "path", f"receipt {name}")
    candidate = Path(display_path)
    if candidate.is_absolute():
        raise contract.ContractError(f"formal receipt {name} path must be relative")
    path = (result_root.resolve() / candidate).resolve()
    if not path.is_relative_to(result_root.resolve()) or not path.is_file():
        raise contract.ContractError(f"formal receipt {name} file is missing")
    size = record.get("size_bytes")
    if isinstance(size, bool) or not isinstance(size, int) or size <= 0:
        raise contract.ContractError(f"formal receipt {name} size is invalid")
    if path.stat().st_size != size:
        raise contract.ContractError(f"formal receipt {name} byte length differs")
    digest = contract.require_string(record, "sha256", f"receipt {name}")
    if contract.SHA256_PATTERN.fullmatch(digest) is None:
        raise contract.ContractError(f"formal receipt {name} SHA-256 is malformed")
    if contract.sha256_file(path) != digest:
        raise contract.ContractError(f"formal receipt {name} SHA-256 differs")
    return path


def parse_timestamp(value: str, label: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise contract.ContractError(f"{label} is not an ISO-8601 timestamp") from error
    if parsed.tzinfo is None:
        raise contract.ContractError(f"{label} must include a timezone")
    return parsed.astimezone(timezone.utc)


def file_evidence(path: Path, result_root: Path) -> dict[str, object]:
    resolved = path.resolve()
    if not resolved.is_file() or resolved.stat().st_size <= 0:
        raise contract.ContractError(
            f"required formal evidence is missing or empty: {resolved}"
        )
    try:
        display_path = resolved.relative_to(result_root.resolve()).as_posix()
    except ValueError as error:
        raise contract.ContractError(
            f"formal evidence is outside the result root: {resolved}"
        ) from error
    return {
        "path": display_path,
        "sha256": contract.sha256_file(resolved),
        "size_bytes": resolved.stat().st_size,
    }


def validate_summary_file_identity(
    summary: Mapping[str, object],
    raw_path: Path,
    guest_irq_path: Path,
    host_trace_path: Path,
) -> None:
    raw = contract.require_object(summary, "input", "summary")
    if raw.get("sha256") != contract.sha256_file(raw_path):
        raise contract.ContractError(
            "summary raw SHA-256 differs from the archived raw file"
        )
    irq = contract.require_object(summary, "direct_irq_trace", "summary")
    inputs = contract.require_object(irq, "inputs", "direct_irq_trace")
    for name, path in (("guest", guest_irq_path), ("host", host_trace_path)):
        record = contract.require_object(inputs, name, "direct_irq_trace inputs")
        if record.get("sha256") != contract.sha256_file(path):
            raise contract.ContractError(
                f"summary {name} trace SHA-256 differs from the archived file"
            )
