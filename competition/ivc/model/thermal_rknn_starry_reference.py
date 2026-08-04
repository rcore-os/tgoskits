#!/usr/bin/env python3
"""Validate physical AxVisor/StarryOS RK3588 NPU evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np


MODEL_DIR = Path(__file__).resolve().parent
if str(MODEL_DIR) not in sys.path:
    sys.path.insert(0, str(MODEL_DIR))

import thermal_rknn_linux_reference as reference


EXPECTED_SNAPSHOT_BYTES = 96 * 1024 * 1024
EXPECTED_STARRY_DRIVER_VERSION = "0.9.8"
ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
SOURCE_COMMIT = re.compile(r"[0-9a-f]{40}")
RUN_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
SOURCE_PROVENANCE_VALUES = frozenset(
    {
        "captured-before-run",
        "reconstructed-post-run",
    }
)


@dataclass(frozen=True)
class StarryEvidenceInputs:
    """Physical artifacts and source provenance for one StarryOS run."""

    raw_path: Path
    raw_manifest_path: Path
    console_path: Path
    profile_path: Path
    board_facts_path: Path
    snapshot_path: Path
    embedded_runner_path: Path
    embedded_model_path: Path
    embedded_corpus_path: Path
    embedded_runtime_path: Path
    built_runner_path: Path
    run_id: str
    source_commit: str
    source_branch: str
    source_provenance: str
    source_dirty: bool
    tracked_change_count: int
    untracked_file_count: int
    started_at: str
    finished_at: str
    require_clean_source: bool


def marker_candidates(console: str, marker: str) -> list[dict[str, str]]:
    """Return well-formed key/value copies of a redundant UART marker."""

    candidates: list[dict[str, str]] = []
    for line in console.splitlines():
        line = ANSI_CSI.sub("", line)
        marker_offsets: list[int] = []
        search_offset = 0
        while (marker_offset := line.find(marker, search_offset)) >= 0:
            marker_offsets.append(marker_offset)
            search_offset = marker_offset + len(marker)
        if not marker_offsets:
            continue
        for index, marker_offset in enumerate(marker_offsets):
            marker_end = (
                marker_offsets[index + 1]
                if index + 1 < len(marker_offsets)
                else len(line)
            )
            fields: dict[str, str] = {}
            valid = True
            marker_body = line[marker_offset + len(marker) : marker_end]
            for token in marker_body.strip().split():
                if "=" not in token:
                    valid = False
                    break
                name, value = token.split("=", maxsplit=1)
                if not name or not value or name in fields:
                    valid = False
                    break
                fields[name] = value
            if valid:
                candidates.append(fields)
    return candidates


def matching_marker(
    console: str,
    marker: str,
    expected: dict[str, str],
) -> tuple[dict[str, str], int]:
    candidates = marker_candidates(console, marker)
    matches = [
        fields
        for fields in candidates
        if all(fields.get(name) == value for name, value in expected.items())
    ]
    reference.require(matches, f"no valid {marker} marker satisfies the frozen contract")
    return matches[0], len(matches)


def modal_marker_fields(
    console: str,
    marker: str,
    expected: dict[str, str],
    required_fields: tuple[str, ...],
) -> tuple[dict[str, str], int]:
    """Return the most frequent complete copy of a split UART marker."""

    candidates = [
        fields
        for fields in marker_candidates(console, marker)
        if all(fields.get(name) == value for name, value in expected.items())
        and all(name in fields for name in required_fields)
    ]
    reference.require(
        candidates,
        f"no valid {marker} marker satisfies the frozen contract",
    )
    grouped: dict[tuple[str, ...], tuple[dict[str, str], int]] = {}
    for fields in candidates:
        values = tuple(fields[name] for name in required_fields)
        _, copies = grouped.get(values, (fields, 0))
        grouped[values] = (fields, copies + 1)
    return max(grouped.values(), key=lambda item: item[1])


def modal_text_value(values: list[str]) -> tuple[str, int]:
    """Return the first most-frequent value from a nonempty sequence."""

    grouped: dict[str, int] = {}
    for value in values:
        grouped[value] = grouped.get(value, 0) + 1
    return max(grouped.items(), key=lambda item: item[1])


def parse_raw_manifest(path: Path) -> str:
    text = path.read_text(encoding="utf-8").strip()
    match = re.fullmatch(r"([0-9a-f]{64})  /var/lib/rknn/raw\.csv", text)
    reference.require(match is not None, "raw manifest differs from the guest schema")
    return match.group(1)


def validate_source(evidence: StarryEvidenceInputs) -> dict[str, Any]:
    reference.require(RUN_ID.fullmatch(evidence.run_id) is not None, "invalid run ID")
    reference.require(
        SOURCE_COMMIT.fullmatch(evidence.source_commit) is not None,
        "invalid source commit",
    )
    reference.require(evidence.source_branch != "", "source branch is empty")
    reference.require(
        evidence.source_provenance in SOURCE_PROVENANCE_VALUES,
        "invalid source provenance",
    )
    reference.require(
        evidence.tracked_change_count >= 0 and evidence.untracked_file_count >= 0,
        "source change count is negative",
    )
    reference.require(
        evidence.source_dirty
        == (evidence.tracked_change_count > 0 or evidence.untracked_file_count > 0),
        "source dirty flag conflicts with change counts",
    )
    if evidence.require_clean_source:
        reference.require(
            not evidence.source_dirty,
            "formal StarryOS evidence requires a clean source tree",
        )
        reference.require(
            evidence.source_provenance == "captured-before-run",
            "formal StarryOS evidence requires pre-run source capture",
        )
    started_at = reference.parse_timestamp(evidence.started_at, "run start time")
    finished_at = reference.parse_timestamp(evidence.finished_at, "run finish time")
    reference.require(finished_at >= started_at, "run finish time precedes start time")
    return {
        "branch": evidence.source_branch,
        "clean_source_required": evidence.require_clean_source,
        "commit": evidence.source_commit,
        "dirty": evidence.source_dirty,
        "provenance": evidence.source_provenance,
        "tracked_change_count": evidence.tracked_change_count,
        "untracked_file_count": evidence.untracked_file_count,
    }


def validate_artifacts(
    evidence: StarryEvidenceInputs,
    documents: dict[str, Any],
) -> dict[str, Any]:
    profile = reference.parse_key_value_file(evidence.profile_path)
    expected_profile = {
        "schema": "1",
        "vectors": str(reference.EXPECTED_VECTORS),
        "warmup": "32",
        "core_mask": "0",
    }
    for name, expected in expected_profile.items():
        reference.require(profile.get(name) == expected, f"profile {name} differs")

    expected_corpus = reference.corpus_bytes(documents["vectors"])
    corpus_sha256 = hashlib.sha256(expected_corpus).hexdigest()
    expected_hashes = {
        "runner_sha256": reference.sha256_file(evidence.built_runner_path),
        "model_sha256": reference.sha256_file(reference.RKNN_PATH),
        "corpus_sha256": corpus_sha256,
        "runtime_sha256": reference.sha256_file(reference.RUNTIME_PATH),
    }
    for name, expected in expected_hashes.items():
        reference.require(profile.get(name) == expected, f"profile {name} differs")

    embedded = {
        "runner": reference.file_record(evidence.embedded_runner_path, "snapshot/runner"),
        "model": reference.file_record(evidence.embedded_model_path, "snapshot/model.rknn"),
        "corpus": reference.file_record(evidence.embedded_corpus_path, "snapshot/corpus.csv"),
        "runtime": reference.file_record(
            evidence.embedded_runtime_path,
            "snapshot/lib/librknnrt.so",
        ),
    }
    reference.require(
        evidence.embedded_corpus_path.read_bytes() == expected_corpus,
        "snapshot corpus differs from the frozen source",
    )
    for artifact, profile_name in (
        ("runner", "runner_sha256"),
        ("model", "model_sha256"),
        ("corpus", "corpus_sha256"),
        ("runtime", "runtime_sha256"),
    ):
        reference.require(
            embedded[artifact]["sha256"] == profile[profile_name],
            f"snapshot {artifact} hash differs from the profile",
        )

    raw_sha256 = reference.sha256_file(evidence.raw_path)
    reference.require(
        parse_raw_manifest(evidence.raw_manifest_path) == raw_sha256,
        "raw CSV hash differs from the guest manifest",
    )
    facts = reference.parse_key_value_file(evidence.board_facts_path)
    reference.require(facts.get("hostname") == "orangepi5plus", "unexpected board hostname")
    reference.require(facts.get("machine") == "aarch64", "unexpected board architecture")
    reference.require(facts.get("root_fstype") == "ext4", "restored Linux root is not ext4")
    reference.require(
        facts.get("root_source") == "/dev/mmcblk1p2",
        "restored Linux root device differs",
    )
    snapshot = reference.file_record(evidence.snapshot_path, "starry-rknpu-result.img")
    reference.require(
        snapshot["size_bytes"] == EXPECTED_SNAPSHOT_BYTES,
        "StarryOS result snapshot size differs",
    )
    reference.require(
        facts.get("snapshot_size") == str(EXPECTED_SNAPSHOT_BYTES),
        "board-reported snapshot size differs",
    )
    reference.require(
        facts.get("snapshot_sha256") == snapshot["sha256"],
        "local snapshot hash differs from the board",
    )
    return {
        "board_facts": facts,
        "embedded": embedded,
        "expected_hashes": expected_hashes,
        "profile": profile,
        "raw_sha256": raw_sha256,
        "snapshot": snapshot,
    }


def decode_runtime_version(fields: dict[str, str]) -> tuple[str, str]:
    try:
        api_version = reference.decode_hex_text(fields["api_version_hex"], "API version")
        driver_version = reference.decode_hex_text(
            fields["driver_version_hex"],
            "driver version",
        )
    except KeyError as error:
        raise reference.ReferenceError("runtime marker is incomplete") from error
    reference.require(api_version.startswith("2.3.2 "), "unexpected RKNN Runtime API")
    reference.require(
        driver_version == EXPECTED_STARRY_DRIVER_VERSION,
        "unexpected StarryOS RKNPU driver version",
    )
    return api_version, driver_version


def matching_runtime_marker(console: str) -> tuple[str, str, int, int]:
    """Select complete runtime markers while tolerating damaged UART copies."""

    valid_versions: list[tuple[str, str]] = []
    for fields in marker_candidates(console, "IVC_RKNN_RUNTIME"):
        try:
            valid_versions.append(decode_runtime_version(fields))
        except (reference.ReferenceError, ValueError):
            continue
    if valid_versions:
        api_version, driver_version = valid_versions[0]
        return api_version, driver_version, len(valid_versions), 0

    api_versions: list[str] = []
    for fields in marker_candidates(console, "IVC_RKNN_RUNTIME_API"):
        try:
            api_version = reference.decode_hex_text(
                fields["version_hex"], "API version"
            )
        except (KeyError, reference.ReferenceError):
            continue
        if api_version.startswith("2.3.2 "):
            api_versions.append(api_version)
    reference.require(
        api_versions,
        "no valid runtime API marker satisfies the frozen contract",
    )

    driver_versions: list[str] = []
    for fields in marker_candidates(console, "IVC_RKNN_RUNTIME_DRIVER"):
        try:
            driver_version = reference.decode_hex_text(
                fields["version_hex"], "driver version"
            )
        except (KeyError, reference.ReferenceError):
            continue
        if driver_version == EXPECTED_STARRY_DRIVER_VERSION:
            driver_versions.append(driver_version)
    reference.require(
        driver_versions,
        "no valid runtime driver marker satisfies the frozen contract",
    )

    api_version, api_copies = modal_text_value(api_versions)
    driver_version, driver_copies = modal_text_value(driver_versions)
    compact_sets = min(api_copies, driver_copies)
    return api_version, driver_version, compact_sets, compact_sets


def matching_handoff_markers(console: str) -> tuple[int, int]:
    legacy_handoff = re.compile(
        r"AXVISOR_RK3588_NPU_HANDOFF_READY cores=3 power_domains=3 clocks=8 "
        r"resets=6 scmi_clock_id=6 scmi_rate_hz=200000000 host_submit=false"
    )
    legacy_copies = len(legacy_handoff.findall(console))
    compact_contract = (
        ("AXVISOR_RK3588_NPU_HANDOFF_READY", {}),
        (
            "AXVISOR_RK3588_NPU_RESOURCES",
            {
                "cores": "3",
                "power_domains": "3",
                "clocks": "8",
                "resets": "6",
            },
        ),
        (
            "AXVISOR_RK3588_NPU_SCMI",
            {"clock_id": "6", "rate_hz": "200000000"},
        ),
        (
            "AXVISOR_RK3588_NPU_OWNERSHIP",
            {"host_submit": "false"},
        ),
    )
    compact_copies = [
        sum(
            all(fields.get(name) == value for name, value in expected.items())
            for fields in marker_candidates(console, marker)
        )
        for marker, expected in compact_contract
    ]
    compact_sets = min(compact_copies)
    reference.require(
        legacy_copies > 0 or compact_sets > 0,
        "NPU handoff marker is missing",
    )
    return legacy_copies, compact_sets


def matching_result_marker(console: str) -> tuple[dict[str, str], int, int]:
    legacy_expected = {
        "status": "pass",
        "vectors": str(reference.EXPECTED_VECTORS),
        "warmup": "32",
        "core_mask": "0",
        "perf_query_errors": "0",
        "run_errors": "0",
    }
    legacy_required = (
        "init_us",
        "exact_actuator_matches",
        "maximum_absolute_error",
        "maximum_absolute_actuator_delta",
    )
    legacy_candidates = [
        fields
        for fields in marker_candidates(console, "IVC_RKNN_LINUX_RESULT")
        if all(fields.get(name) == value for name, value in legacy_expected.items())
        and all(name in fields for name in legacy_required)
    ]

    compact_markers = (
        (
            "IVC_RKNN_RESULT_META",
            {
                "status": "pass",
                "vectors": str(reference.EXPECTED_VECTORS),
                "warmup": "32",
                "core_mask": "0",
            },
            ("init_us",),
        ),
        (
            "IVC_RKNN_RESULT_ACCURACY",
            {},
            ("exact_actuator_matches", "maximum_absolute_actuator_delta"),
        ),
        (
            "IVC_RKNN_RESULT_ERROR",
            {},
            ("maximum_absolute_error",),
        ),
        (
            "IVC_RKNN_RESULT_HEALTH",
            {"perf_query_errors": "0", "run_errors": "0"},
            (),
        ),
    )
    compact_fields: dict[str, str] = {}
    compact_copies: list[int] = []
    try:
        for marker, expected, required in compact_markers:
            fields, copies = modal_marker_fields(
                console, marker, expected, required
            )
            compact_fields.update(expected)
            compact_fields.update(
                {name: fields[name] for name in required}
            )
            compact_copies.append(copies)
    except reference.ReferenceError:
        compact_fields.clear()
        compact_copies.clear()

    if compact_fields:
        return compact_fields, len(legacy_candidates), min(compact_copies)
    reference.require(
        legacy_candidates,
        "no valid RKNN result marker satisfies the frozen contract",
    )
    return legacy_candidates[0], len(legacy_candidates), 0


def validate_console(
    console_path: Path,
    artifacts: dict[str, Any],
) -> dict[str, Any]:
    console = console_path.read_bytes().decode(encoding="utf-8", errors="replace")
    utf8_replacement_characters = console.count("\ufffd")
    for forbidden in (
        "THERMAL_RKNN_STARRY_FAIL",
        "IVC_RKNN_LINUX_ERROR",
        "AXVISOR_HOST_FILESYSTEM_SYNC_FAILED",
        "AXVISOR_VM_BLOCK_SNAPSHOT_FAILED",
        "Unhandled synchronous exception from current EL:",
        "panicked at",
    ):
        reference.require(forbidden not in console, f"console contains failure marker: {forbidden}")

    legacy_handoff_copies, compact_handoff_sets = matching_handoff_markers(console)
    reference.require("NPU registered successfully" in console, "guest NPU registration is missing")
    for completed in range(1000, reference.EXPECTED_VECTORS + 1, 1000):
        reference.require(
            f"IVC_RKNN_PROGRESS completed={completed}" in console,
            f"NPU progress marker {completed} is missing",
        )

    begin, _ = matching_marker(
        console,
        "THERMAL_RKNN_STARRY_BEGIN",
        {
            "schema": "1",
            "vectors": str(reference.EXPECTED_VECTORS),
            "warmup": "32",
            "core_mask": "0",
            "backend": "rknn-npu",
        },
    )
    (
        api_version,
        driver_version,
        runtime_marker_copies,
        compact_runtime_marker_sets,
    ) = matching_runtime_marker(console)
    (
        result,
        legacy_result_marker_copies,
        compact_result_marker_sets,
    ) = matching_result_marker(console)
    pass_expected = {
        "schema": "1",
        "vectors": str(reference.EXPECTED_VECTORS),
        "warmup": "32",
        "core_mask": "0",
        "backend": "rknn-npu",
    }
    _, valid_pass_copies = matching_marker(
        console,
        "THERMAL_RKNN_STARRY_PASS",
        pass_expected,
    )
    legacy_pass_expected = {
        **pass_expected,
        "model_sha256": artifacts["profile"]["model_sha256"],
        "corpus_sha256": artifacts["profile"]["corpus_sha256"],
        "runtime_sha256": artifacts["profile"]["runtime_sha256"],
        "raw_sha256": artifacts["raw_sha256"],
    }
    legacy_pass_artifact_copies = sum(
        all(fields.get(name) == value for name, value in legacy_pass_expected.items())
        for fields in marker_candidates(console, "THERMAL_RKNN_STARRY_PASS")
    )
    raw_marker_expected = {
        "schema": "1",
        "vectors": str(reference.EXPECTED_VECTORS),
        "sha256": artifacts["raw_sha256"],
    }
    valid_raw_marker_copies = sum(
        all(fields.get(name) == value for name, value in raw_marker_expected.items())
        for fields in marker_candidates(console, "THERMAL_RKNN_STARRY_RAW")
    )
    reference.require(
        legacy_pass_artifact_copies > 0 or valid_raw_marker_copies > 0,
        "no valid StarryOS UART marker binds completion to the raw evidence",
    )
    snapshot_markers = len(marker_candidates(console, "AXVISOR_SNAPSHOT_SYNC_OK"))
    host_sync_markers = len(
        marker_candidates(console, "AXVISOR_HOST_FILESYSTEM_SYNCED")
    )
    reference.require(snapshot_markers >= 1, "snapshot sync marker is missing")
    reference.require(host_sync_markers >= 1, "host filesystem sync marker is missing")
    reference.require(
        "=== SUCCESS PATTERN MATCHED: (?m)^AXVISOR_HOST_FILESYSTEM_SYNCED\\r?$ ==="
        in console,
        "ostool did not terminate on the bounded final sync marker",
    )
    return {
        "api_version": api_version,
        "begin": begin,
        "compact_handoff_marker_sets": compact_handoff_sets,
        "compact_result_marker_sets": compact_result_marker_sets,
        "compact_runtime_marker_sets": compact_runtime_marker_sets,
        "driver_version": driver_version,
        "host_sync_marker_copies": host_sync_markers,
        "legacy_handoff_marker_copies": legacy_handoff_copies,
        "legacy_pass_artifact_marker_copies": legacy_pass_artifact_copies,
        "legacy_result_marker_copies": legacy_result_marker_copies,
        "result": result,
        "runtime_marker_copies": runtime_marker_copies,
        "snapshot_sync_marker_copies": snapshot_markers,
        "utf8_replacement_characters": utf8_replacement_characters,
        "valid_pass_marker_copies": valid_pass_copies,
        "valid_raw_marker_copies": valid_raw_marker_copies,
    }


def analyze(evidence: StarryEvidenceInputs, output_path: Path) -> dict[str, Any]:
    documents = reference.load_documents()
    artifacts = validate_artifacts(evidence, documents)
    raw = reference.read_raw(evidence.raw_path, documents["vectors"])
    console = validate_console(evidence.console_path, artifacts)
    source = validate_source(evidence)

    native_outputs = np.asarray(
        [vector["output"] for vector in documents["vectors"]],
        dtype=np.float32,
    )
    native_commands = np.asarray(
        [vector["actuator_permille"] for vector in documents["vectors"]],
        dtype=np.int32,
    )
    output_errors = np.abs(raw["outputs"].astype(np.float64) - native_outputs.astype(np.float64))
    maximum_f32_error = float(np.max(output_errors))
    command_deltas = raw["commands"] - native_commands
    maximum_command_delta = int(np.max(np.abs(command_deltas)))
    inputs = reference.decode_inputs(documents["vectors"])
    oracle_outputs = reference.fp16_oracle(documents["weights"], inputs)
    fp16_errors = np.abs(raw["outputs"].astype(np.float64) - oracle_outputs.astype(np.float64))
    maximum_fp16_error = float(np.max(fp16_errors))

    reference.require(
        maximum_f32_error <= reference.F32_MAX_ABSOLUTE_ERROR_GATE,
        "StarryOS NPU f32 error exceeds the frozen gate",
    )
    reference.require(
        maximum_command_delta <= reference.ACTUATOR_MAX_ABSOLUTE_DELTA_GATE,
        "StarryOS NPU command delta exceeds the frozen gate",
    )
    reference.require(
        maximum_fp16_error <= reference.FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
        "StarryOS NPU FP16-oracle error exceeds the frozen gate",
    )
    unique_deltas, counts = np.unique(command_deltas, return_counts=True)
    histogram = {
        str(int(delta)): int(count)
        for delta, count in zip(unique_deltas, counts, strict=True)
    }
    exact_actuator_matches = int(np.count_nonzero(command_deltas == 0))
    reference.require(
        int(console["result"]["exact_actuator_matches"]) == exact_actuator_matches,
        "console exact-actuator count differs from raw evidence",
    )
    reference.require(
        int(console["result"]["maximum_absolute_actuator_delta"])
        == maximum_command_delta,
        "console actuator delta differs from raw evidence",
    )
    reference.require(
        float(console["result"]["maximum_absolute_error"]) == maximum_f32_error,
        "console maximum error differs from raw evidence",
    )

    report = {
        "schema_version": 1,
        "status": "pass",
        "model_id": reference.MODEL_ID,
        "platform": "AxVisor/StarryOS on OrangePi 5 Plus",
        "run": {
            "id": evidence.run_id,
            "started_at": evidence.started_at,
            "finished_at": evidence.finished_at,
        },
        "source": source,
        "board": {
            "hostname": artifacts["board_facts"]["hostname"],
            "kernel_release_after_restore": artifacts["board_facts"]["kernel_release"],
            "linux_restored": True,
            "root_filesystem": {
                "source": artifacts["board_facts"]["root_source"],
                "type": artifacts["board_facts"]["root_fstype"],
            },
        },
        "backend": {
            "kind": "rknn-runtime-npu",
            "physical_compiled_rknn_executed": True,
            "host_submit": False,
            "guest_exclusive_handoff": True,
            "core_mask": "0",
            "api_version": console["api_version"],
            "driver_version": console["driver_version"],
            "positive_device_time_samples": len(raw["device_us"]),
        },
        "artifacts": {
            "board_facts_sha256": reference.sha256_file(evidence.board_facts_path),
            "console_sha256": reference.sha256_file(evidence.console_path),
            "profile_sha256": reference.sha256_file(evidence.profile_path),
            "raw_manifest_sha256": reference.sha256_file(evidence.raw_manifest_path),
            "raw_sha256": artifacts["raw_sha256"],
            "snapshot": artifacts["snapshot"],
            "embedded": artifacts["embedded"],
            "expected_hashes": artifacts["expected_hashes"],
        },
        "vectors": reference.EXPECTED_VECTORS,
        "comparison_to_native_f32": {
            "actuator_command_delta_histogram": histogram,
            "exact_actuator_matches": exact_actuator_matches,
            "maximum_absolute_actuator_command_delta": maximum_command_delta,
            "maximum_absolute_actuator_command_delta_gate": (
                reference.ACTUATOR_MAX_ABSOLUTE_DELTA_GATE
            ),
            "maximum_absolute_error": maximum_f32_error,
            "maximum_absolute_error_gate": reference.F32_MAX_ABSOLUTE_ERROR_GATE,
        },
        "comparison_to_fp16_oracle": {
            "exact_output_matches": int(
                np.count_nonzero(raw["outputs"] == oracle_outputs.astype(np.float32))
            ),
            "maximum_absolute_error": maximum_fp16_error,
            "maximum_absolute_error_gate": reference.FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
        },
        "latency": {
            "device": reference.latency_summary(raw["device_us"], "us"),
            "wall": reference.latency_summary(raw["wall_ns"], "ns"),
            "initialization_us": int(console["result"]["init_us"]),
            "warmup_vectors": int(console["result"]["warmup"]),
        },
        "console_evidence": {
            "compact_handoff_marker_sets": console[
                "compact_handoff_marker_sets"
            ],
            "compact_result_marker_sets": console[
                "compact_result_marker_sets"
            ],
            "compact_runtime_marker_sets": console[
                "compact_runtime_marker_sets"
            ],
            "host_sync_marker_copies": console["host_sync_marker_copies"],
            "legacy_pass_artifact_marker_copies": console[
                "legacy_pass_artifact_marker_copies"
            ],
            "legacy_handoff_marker_copies": console[
                "legacy_handoff_marker_copies"
            ],
            "legacy_result_marker_copies": console[
                "legacy_result_marker_copies"
            ],
            "runtime_marker_copies": console["runtime_marker_copies"],
            "snapshot_sync_marker_copies": console["snapshot_sync_marker_copies"],
            "utf8_replacement_characters": console[
                "utf8_replacement_characters"
            ],
            "valid_pass_marker_copies": console["valid_pass_marker_copies"],
            "valid_raw_marker_copies": console["valid_raw_marker_copies"],
        },
        "gates": {
            "all_device_times_positive": True,
            "all_outputs_finite": True,
            "all_perf_queries_succeeded": True,
            "bounded_stream_success_marker": True,
            "compiled_rknn_executed_on_physical_npu": True,
            "embedded_artifacts_match_frozen_sources": True,
            "fp16_oracle_error": True,
            "guest_exclusive_handoff": True,
            "linux_restored": True,
            "native_f32_actuator_delta": True,
            "native_f32_error": True,
            "raw_manifest": True,
            "snapshot_matches_board": True,
        },
    }
    reference.write_json(output_path, report)
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw", type=Path, required=True)
    parser.add_argument("--raw-manifest", type=Path, required=True)
    parser.add_argument("--console", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--board-facts", type=Path, required=True)
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--embedded-runner", type=Path, required=True)
    parser.add_argument("--embedded-model", type=Path, required=True)
    parser.add_argument("--embedded-corpus", type=Path, required=True)
    parser.add_argument("--embedded-runtime", type=Path, required=True)
    parser.add_argument("--built-runner", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-branch", required=True)
    parser.add_argument(
        "--source-provenance",
        choices=sorted(SOURCE_PROVENANCE_VALUES),
        required=True,
    )
    parser.add_argument("--source-dirty", choices=("true", "false"), required=True)
    parser.add_argument("--tracked-change-count", type=int, required=True)
    parser.add_argument("--untracked-file-count", type=int, required=True)
    parser.add_argument("--started-at", required=True)
    parser.add_argument("--finished-at", required=True)
    parser.add_argument("--require-clean-source", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    evidence = StarryEvidenceInputs(
        raw_path=args.raw.resolve(),
        raw_manifest_path=args.raw_manifest.resolve(),
        console_path=args.console.resolve(),
        profile_path=args.profile.resolve(),
        board_facts_path=args.board_facts.resolve(),
        snapshot_path=args.snapshot.resolve(),
        embedded_runner_path=args.embedded_runner.resolve(),
        embedded_model_path=args.embedded_model.resolve(),
        embedded_corpus_path=args.embedded_corpus.resolve(),
        embedded_runtime_path=args.embedded_runtime.resolve(),
        built_runner_path=args.built_runner.resolve(),
        run_id=args.run_id,
        source_commit=args.source_commit,
        source_branch=args.source_branch,
        source_provenance=args.source_provenance,
        source_dirty=args.source_dirty == "true",
        tracked_change_count=args.tracked_change_count,
        untracked_file_count=args.untracked_file_count,
        started_at=args.started_at,
        finished_at=args.finished_at,
        require_clean_source=args.require_clean_source,
    )
    report = analyze(evidence, args.output.resolve())
    print(
        "THERMAL_RKNN_STARRY_REFERENCE_PASS "
        + json.dumps(
            {
                "device_p99_us": report["latency"]["device"]["p99"],
                "maximum_absolute_error": report["comparison_to_native_f32"][
                    "maximum_absolute_error"
                ],
                "raw_sha256": report["artifacts"]["raw_sha256"],
                "vectors": report["vectors"],
                "wall_p99_ns": report["latency"]["wall"]["p99"],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, reference.ReferenceError, ValueError) as error:
        print(f"thermal RKNN StarryOS reference failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
