#!/usr/bin/env python3
"""Validate physical AxVisor/StarryOS ONNX Runtime CPU evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import struct
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any


MODEL_DIR = Path(__file__).resolve().parent
EXPECTED_VECTORS = 10_000
EXPECTED_SNAPSHOT_BYTES = 160 * 1024 * 1024
EXPECTED_RUNTIME_VERSION = "1.25.0"
EXPECTED_LIFECYCLE_CYCLES = 5
MAXIMUM_ABSOLUTE_ERROR = 1.0e-6
ROUNDING_BOUNDARY_TOLERANCE = 1.0e-6
MAXIMUM_POST_DESTROY_GROWTH_KIB = 16_384
MAXIMUM_PEAK_RSS_KIB = 131_072
MINIMUM_ROOTFS_AVAILABLE_PERCENT_X100 = 2_000
EXPECTED_EXACT_COMMANDS = 9_999
EXPECTED_ROUNDING_EQUIVALENCES = 1
CORPUS_HEADER = (
    "index,input0_f32_bits,input1_f32_bits,input2_f32_bits,input3_f32_bits,"
    "expected_output_f32_bits,expected_actuator_permille"
)
RAW_HEADER = (
    CORPUS_HEADER
    + ",ort_output_f32_bits,ort_actuator_permille,wall_ns"
)
SOURCE_COMMIT = re.compile(r"[0-9a-f]{40}")
RUN_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


class EvidenceError(ValueError):
    """Raised when harvested evidence violates the frozen contract."""


@dataclass(frozen=True)
class EvidenceInputs:
    """Physical artifacts and source provenance for one ORT board run."""

    raw_path: Path
    raw_manifest_path: Path
    resource_path: Path
    resource_manifest_path: Path
    console_path: Path
    profile_path: Path
    board_facts_path: Path
    snapshot_path: Path
    embedded_runner_path: Path
    embedded_model_path: Path
    embedded_corpus_path: Path
    embedded_runtime_path: Path
    embedded_provider_path: Path
    built_runner_path: Path
    built_corpus_path: Path
    run_id: str
    source_commit: str
    source_branch: str
    source_dirty: bool
    tracked_change_count: int
    untracked_file_count: int
    started_at: str
    finished_at: str
    require_clean_source: bool


def require(condition: bool, message: str) -> None:
    if not condition:
        raise EvidenceError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_record(path: Path, label: str) -> dict[str, Any]:
    require(path.is_file(), f"missing {label}: {path}")
    return {
        "path": label,
        "sha256": sha256_file(path),
        "size_bytes": path.stat().st_size,
    }


def parse_key_value_file(path: Path) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8").splitlines(),
        start=1,
    ):
        require("=" in line, f"invalid key/value line {line_number} in {path}")
        name, value = line.split("=", maxsplit=1)
        require(name != "" and value != "", f"empty key/value in {path}")
        require(name not in fields, f"duplicate key {name} in {path}")
        fields[name] = value
    return fields


def parse_nonnegative_integer(value: str, context: str) -> int:
    require(re.fullmatch(r"[0-9]+", value) is not None, f"invalid {context}")
    return int(value)


def parse_positive_integer(value: str, context: str) -> int:
    parsed = parse_nonnegative_integer(value, context)
    require(parsed > 0, f"{context} must be positive")
    return parsed


def parse_finite_float(value: str, context: str) -> float:
    try:
        parsed = float(value)
    except ValueError as error:
        raise EvidenceError(f"invalid {context}") from error
    require(math.isfinite(parsed), f"non-finite {context}")
    return parsed


def parse_f32_bits(value: str, context: str) -> tuple[int, float]:
    require(re.fullmatch(r"[0-9a-f]{8}", value) is not None, f"invalid {context}")
    bits = int(value, 16)
    return bits, struct.unpack("!f", bytes.fromhex(value))[0]


def parse_timestamp(value: str, context: str) -> datetime:
    require(value.endswith("Z"), f"{context} is not UTC")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise EvidenceError(f"invalid {context}") from error


def parse_manifest(path: Path, guest_path: str) -> str:
    text = path.read_text(encoding="utf-8").strip()
    match = re.fullmatch(rf"([0-9a-f]{{64}})  {re.escape(guest_path)}", text)
    require(match is not None, f"manifest differs for {guest_path}")
    return match.group(1)


def percentile(values: list[int], percentage: int) -> int:
    require(values and 0 <= percentage <= 100, "invalid percentile input")
    ordered = sorted(values)
    index = ((len(ordered) - 1) * percentage + 99) // 100
    return ordered[index]


def marker_candidates(console: str, marker: str) -> list[dict[str, str]]:
    """Return intact key/value marker copies from a potentially noisy UART log."""

    candidates: list[dict[str, str]] = []
    for source_line in console.splitlines():
        line = ANSI_CSI.sub("", source_line)
        offsets: list[int] = []
        search_offset = 0
        while (offset := line.find(marker, search_offset)) >= 0:
            offsets.append(offset)
            search_offset = offset + len(marker)
        for index, offset in enumerate(offsets):
            end = offsets[index + 1] if index + 1 < len(offsets) else len(line)
            fields: dict[str, str] = {}
            valid = True
            for token in line[offset + len(marker) : end].strip().split():
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


def matching_marker_copies(
    console: str,
    marker: str,
    expected: dict[str, str],
) -> int:
    copies = sum(
        all(fields.get(name) == value for name, value in expected.items())
        for fields in marker_candidates(console, marker)
    )
    require(copies > 0, f"no valid {marker} marker satisfies the frozen contract")
    return copies


def analyze_raw(raw_path: Path, corpus_path: Path) -> dict[str, Any]:
    corpus_lines = corpus_path.read_text(encoding="utf-8").splitlines()
    raw_lines = raw_path.read_text(encoding="utf-8").splitlines()
    require(corpus_lines and corpus_lines[0] == CORPUS_HEADER, "corpus header differs")
    require(raw_lines and raw_lines[0] == RAW_HEADER, "raw CSV header differs")
    require(len(corpus_lines) == EXPECTED_VECTORS + 1, "corpus vector count differs")
    require(len(raw_lines) == EXPECTED_VECTORS + 1, "raw vector count differs")

    maximum_error = 0.0
    exact_commands = 0
    rounding_equivalences = 0
    material_mismatches = 0
    wall_times: list[int] = []
    output_f32le = hashlib.sha256()
    for expected_index, (corpus_line, raw_line) in enumerate(
        zip(corpus_lines[1:], raw_lines[1:], strict=True)
    ):
        corpus_fields = corpus_line.split(",")
        raw_fields = raw_line.split(",")
        require(len(corpus_fields) == 7, "corpus field count differs")
        require(len(raw_fields) == 10, "raw field count differs")
        require(raw_fields[:7] == corpus_fields, "raw row is not bound to the corpus")
        require(
            parse_nonnegative_integer(raw_fields[0], "raw index") == expected_index,
            "raw indices are not contiguous",
        )
        _, expected_output = parse_f32_bits(raw_fields[5], "expected output bits")
        actual_bits, actual_output = parse_f32_bits(raw_fields[7], "ORT output bits")
        require(
            math.isfinite(expected_output) and math.isfinite(actual_output),
            "raw output is non-finite",
        )
        require(0.0 <= expected_output <= 1.0, "expected output is outside [0,1]")
        require(0.0 <= actual_output <= 1.0, "ORT output is outside [0,1]")
        expected_command = parse_nonnegative_integer(raw_fields[6], "expected command")
        actual_command = parse_nonnegative_integer(raw_fields[8], "ORT command")
        require(expected_command <= 1000 and actual_command <= 1000, "invalid command")
        command_delta = actual_command - expected_command
        if command_delta == 0:
            exact_commands += 1
        else:
            lower_command = min(actual_command, expected_command)
            boundary = (lower_command + 0.5) / 1000.0
            if (
                abs(command_delta) == 1
                and abs(actual_output - boundary) <= ROUNDING_BOUNDARY_TOLERANCE
                and abs(expected_output - boundary) <= ROUNDING_BOUNDARY_TOLERANCE
            ):
                rounding_equivalences += 1
            else:
                material_mismatches += 1
        maximum_error = max(maximum_error, abs(actual_output - expected_output))
        wall_times.append(parse_positive_integer(raw_fields[9], "wall latency"))
        output_f32le.update(struct.pack("<I", actual_bits))

    require(maximum_error <= MAXIMUM_ABSOLUTE_ERROR, "ORT numerical error exceeds gate")
    require(material_mismatches == 0, "material command mismatch found")
    require(exact_commands == EXPECTED_EXACT_COMMANDS, "exact command count differs")
    require(
        rounding_equivalences == EXPECTED_ROUNDING_EQUIVALENCES,
        "rounding-equivalence count differs",
    )
    return {
        "vectors": len(wall_times),
        "maximum_absolute_error": maximum_error,
        "exact_actuator_matches": exact_commands,
        "rounding_boundary_equivalences": rounding_equivalences,
        "material_actuator_mismatches": material_mismatches,
        "output_f32le_sha256": output_f32le.hexdigest(),
        "latency": {
            "unit": "ns",
            "minimum": min(wall_times),
            "p50": percentile(wall_times, 50),
            "p95": percentile(wall_times, 95),
            "p99": percentile(wall_times, 99),
            "maximum": max(wall_times),
        },
    }


def validate_resources(path: Path, profile: dict[str, str], raw: dict[str, Any]) -> dict[str, Any]:
    fields = parse_key_value_file(path)
    expected_fields = {
        "schema",
        "backend",
        "runtime_version",
        "lifecycle_cycles",
        "rss_before_kib",
        "rss_first_after_destroy_kib",
        "rss_lifecycle_final_kib",
        "rss_after_main_destroy_kib",
        "peak_rss_kib",
        "initialization_us",
        "wall_p50_ns",
        "wall_p95_ns",
        "wall_p99_ns",
        "wall_max_ns",
        "maximum_absolute_error",
        "exact_actuator_matches",
        "rounding_boundary_equivalences",
        "material_actuator_mismatches",
        "post_destroy_growth_kib",
        "rootfs_total_kib",
        "rootfs_available_before_kib",
        "rootfs_available_after_kib",
        "rootfs_available_percent_x100",
    }
    require(set(fields) == expected_fields, "resource fields differ from schema 1")
    require(fields["schema"] == "1", "resource schema differs")
    require(fields["backend"] == "onnxruntime-cpu", "resource backend differs")
    require(fields["runtime_version"] == EXPECTED_RUNTIME_VERSION, "runtime differs")
    cycles = parse_positive_integer(fields["lifecycle_cycles"], "lifecycle cycles")
    require(cycles == EXPECTED_LIFECYCLE_CYCLES, "lifecycle cycle count differs")
    require(profile.get("lifecycle_cycles") == str(cycles), "profile lifecycle differs")

    memory_names = (
        "rss_before_kib",
        "rss_first_after_destroy_kib",
        "rss_lifecycle_final_kib",
        "rss_after_main_destroy_kib",
        "peak_rss_kib",
    )
    memory = {
        name: parse_positive_integer(fields[name], name) for name in memory_names
    }
    growth = parse_nonnegative_integer(
        fields["post_destroy_growth_kib"],
        "post-destroy growth",
    )
    expected_growth = max(
        0,
        memory["rss_after_main_destroy_kib"]
        - memory["rss_first_after_destroy_kib"],
    )
    require(growth == expected_growth, "post-destroy growth is inconsistent")
    require(
        memory["peak_rss_kib"] >= max(memory.values()),
        "peak RSS is below an observed RSS value",
    )
    require(growth <= MAXIMUM_POST_DESTROY_GROWTH_KIB, "RSS growth exceeds gate")
    require(memory["peak_rss_kib"] <= MAXIMUM_PEAK_RSS_KIB, "peak RSS exceeds gate")

    latency = raw["latency"]
    for resource_name, report_name in (
        ("wall_p50_ns", "p50"),
        ("wall_p95_ns", "p95"),
        ("wall_p99_ns", "p99"),
        ("wall_max_ns", "maximum"),
    ):
        require(
            parse_positive_integer(fields[resource_name], resource_name)
            == latency[report_name],
            f"{resource_name} differs from raw evidence",
        )
    initialization_us = parse_positive_integer(fields["initialization_us"], "init time")
    require(
        parse_finite_float(fields["maximum_absolute_error"], "maximum error")
        == raw["maximum_absolute_error"],
        "resource maximum error differs from raw evidence",
    )
    for name, report_name in (
        ("exact_actuator_matches", "exact_actuator_matches"),
        ("rounding_boundary_equivalences", "rounding_boundary_equivalences"),
        ("material_actuator_mismatches", "material_actuator_mismatches"),
    ):
        require(
            parse_nonnegative_integer(fields[name], name) == raw[report_name],
            f"resource {name} differs from raw evidence",
        )

    rootfs_total = parse_positive_integer(fields["rootfs_total_kib"], "rootfs total")
    rootfs_before = parse_positive_integer(
        fields["rootfs_available_before_kib"],
        "rootfs available before",
    )
    rootfs_after = parse_positive_integer(
        fields["rootfs_available_after_kib"],
        "rootfs available after",
    )
    rootfs_percent = parse_nonnegative_integer(
        fields["rootfs_available_percent_x100"],
        "rootfs available percent",
    )
    require(
        rootfs_after <= rootfs_before <= rootfs_total,
        "rootfs available space is inconsistent",
    )
    require(
        rootfs_percent == rootfs_after * 10_000 // rootfs_total,
        "rootfs available percentage is inconsistent",
    )
    require(
        rootfs_percent >= MINIMUM_ROOTFS_AVAILABLE_PERCENT_X100,
        "rootfs available space is below gate",
    )
    return {
        "fields": fields,
        "initialization_us": initialization_us,
        "lifecycle_cycles": cycles,
        "memory": {**memory, "post_destroy_growth_kib": growth},
        "rootfs": {
            "total_kib": rootfs_total,
            "available_before_kib": rootfs_before,
            "available_after_kib": rootfs_after,
            "available_percent": rootfs_percent / 100,
            "available_percent_x100": rootfs_percent,
        },
    }


def validate_source(evidence: EvidenceInputs) -> dict[str, Any]:
    require(RUN_ID.fullmatch(evidence.run_id) is not None, "invalid run ID")
    require(SOURCE_COMMIT.fullmatch(evidence.source_commit) is not None, "invalid commit")
    require(evidence.source_branch != "", "source branch is empty")
    require(
        evidence.tracked_change_count >= 0 and evidence.untracked_file_count >= 0,
        "source change count is negative",
    )
    dirty_from_counts = (
        evidence.tracked_change_count > 0 or evidence.untracked_file_count > 0
    )
    require(evidence.source_dirty == dirty_from_counts, "source dirty flag differs")
    if evidence.require_clean_source:
        require(not evidence.source_dirty, "formal evidence requires a clean tree")
    started = parse_timestamp(evidence.started_at, "start time")
    finished = parse_timestamp(evidence.finished_at, "finish time")
    require(finished >= started, "finish time precedes start time")
    return {
        "branch": evidence.source_branch,
        "commit": evidence.source_commit,
        "dirty": evidence.source_dirty,
        "clean_source_required": evidence.require_clean_source,
        "tracked_change_count": evidence.tracked_change_count,
        "untracked_file_count": evidence.untracked_file_count,
    }


def validate_artifacts(evidence: EvidenceInputs) -> dict[str, Any]:
    profile = parse_key_value_file(evidence.profile_path)
    expected_profile = {
        "schema": "1",
        "vectors": str(EXPECTED_VECTORS),
        "warmup": "32",
        "lifecycle_cycles": str(EXPECTED_LIFECYCLE_CYCLES),
        "runtime_version": EXPECTED_RUNTIME_VERSION,
        "maximum_post_destroy_growth_kib": str(MAXIMUM_POST_DESTROY_GROWTH_KIB),
        "maximum_peak_rss_kib": str(MAXIMUM_PEAK_RSS_KIB),
        "minimum_rootfs_available_percent_x100": str(
            MINIMUM_ROOTFS_AVAILABLE_PERCENT_X100
        ),
    }
    for name, expected in expected_profile.items():
        require(profile.get(name) == expected, f"profile {name} differs")

    model_path = MODEL_DIR / "thermal-4x6x1-v1.ort"
    runtime_source = json.loads(
        (MODEL_DIR / "onnxruntime-1.25.0-source.json").read_text(encoding="utf-8")
    )
    runtime_files = runtime_source["runtime_files"]
    expected_hashes = {
        "runner_sha256": sha256_file(evidence.built_runner_path),
        "model_sha256": sha256_file(model_path),
        "corpus_sha256": sha256_file(evidence.built_corpus_path),
        "runtime_sha256": runtime_files["libonnxruntime.so.1.25.0"]["sha256"],
        "provider_shared_sha256": runtime_files[
            "libonnxruntime_providers_shared.so"
        ]["sha256"],
    }
    require(set(profile) == set(expected_profile) | set(expected_hashes), "profile fields differ")
    for name, expected in expected_hashes.items():
        require(profile.get(name) == expected, f"profile {name} differs")

    embedded_paths = {
        "runner": evidence.embedded_runner_path,
        "model": evidence.embedded_model_path,
        "corpus": evidence.embedded_corpus_path,
        "runtime": evidence.embedded_runtime_path,
        "provider_shared": evidence.embedded_provider_path,
    }
    profile_names = {
        "runner": "runner_sha256",
        "model": "model_sha256",
        "corpus": "corpus_sha256",
        "runtime": "runtime_sha256",
        "provider_shared": "provider_shared_sha256",
    }
    embedded: dict[str, Any] = {}
    for name, path in embedded_paths.items():
        embedded[name] = file_record(path, f"snapshot/{name}")
        require(
            embedded[name]["sha256"] == profile[profile_names[name]],
            f"snapshot {name} hash differs from profile",
        )

    facts = parse_key_value_file(evidence.board_facts_path)
    require(facts.get("machine") == "aarch64", "unexpected board architecture")
    require(facts.get("root_fstype") == "ext4", "restored root is not ext4")
    snapshot = file_record(evidence.snapshot_path, "starry-ort-result.img")
    require(snapshot["size_bytes"] == EXPECTED_SNAPSHOT_BYTES, "snapshot size differs")
    require(
        facts.get("snapshot_size") == str(EXPECTED_SNAPSHOT_BYTES),
        "board snapshot size differs",
    )
    require(facts.get("snapshot_sha256") == snapshot["sha256"], "snapshot hash differs")
    return {
        "board_facts": facts,
        "embedded": embedded,
        "expected_hashes": expected_hashes,
        "profile": profile,
        "snapshot": snapshot,
    }


def validate_console(
    path: Path,
    artifacts: dict[str, Any],
    resources: dict[str, Any],
    raw_sha256: str,
    resource_sha256: str,
) -> dict[str, Any]:
    console = path.read_bytes().decode(encoding="utf-8", errors="replace")
    for forbidden in (
        "THERMAL_ORT_STARRY_FAIL",
        "IVC_ORT_ERROR",
        "AXVISOR_HOST_FILESYSTEM_SYNC_FAILED",
        "AXVISOR_VM_BLOCK_SNAPSHOT_FAILED",
        "Unhandled synchronous exception from current EL:",
        "panicked at",
    ):
        require(forbidden not in console, f"console contains failure marker: {forbidden}")
    for completed in range(1000, EXPECTED_VECTORS + 1, 1000):
        require(
            f"IVC_ORT_PROGRESS completed={completed}" in console,
            f"progress marker {completed} is missing",
        )

    profile = artifacts["profile"]
    resource_fields = resources["fields"]
    begin_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_BEGIN",
        {
            "schema": "1",
            "vectors": str(EXPECTED_VECTORS),
            "warmup": profile["warmup"],
            "backend": "onnxruntime-cpu",
        },
    )
    pass_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_PASS",
        {
            "schema": "1",
            "vectors": str(EXPECTED_VECTORS),
            "backend": "onnxruntime-cpu",
        },
    )
    raw_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_RAW",
        {"schema": "1", "vectors": str(EXPECTED_VECTORS), "sha256": raw_sha256},
    )
    resource_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_RESOURCE",
        {
            "schema": "1",
            "cycles": str(EXPECTED_LIFECYCLE_CYCLES),
            "sha256": resource_sha256,
        },
    )
    runtime_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_RUNTIME",
        {
            "version": EXPECTED_RUNTIME_VERSION,
            "model_sha256": profile["model_sha256"],
        },
    )
    result_copies = matching_marker_copies(
        console,
        "THERMAL_ORT_STARRY_RESULT",
        {
            "schema": "1",
            "vectors": str(EXPECTED_VECTORS),
            "max_abs_error": resource_fields["maximum_absolute_error"],
            "exact_commands": resource_fields["exact_actuator_matches"],
            "rounding_equivalences": resource_fields[
                "rounding_boundary_equivalences"
            ],
            "material_mismatches": resource_fields["material_actuator_mismatches"],
            "init_us": resource_fields["initialization_us"],
            "wall_p99_ns": resource_fields["wall_p99_ns"],
            "wall_max_ns": resource_fields["wall_max_ns"],
        },
    )
    snapshot_sync_copies = console.count("AXVISOR_SNAPSHOT_SYNC_OK")
    host_sync_copies = console.count("AXVISOR_HOST_FILESYSTEM_SYNCED")
    require(snapshot_sync_copies > 0, "snapshot sync marker is missing")
    require(host_sync_copies > 0, "host filesystem sync marker is missing")
    return {
        "begin_marker_copies": begin_copies,
        "host_sync_marker_copies": host_sync_copies,
        "pass_marker_copies": pass_copies,
        "raw_marker_copies": raw_copies,
        "resource_marker_copies": resource_copies,
        "result_marker_copies": result_copies,
        "runtime_marker_copies": runtime_copies,
        "snapshot_sync_marker_copies": snapshot_sync_copies,
        "utf8_replacement_characters": console.count("\ufffd"),
    }


def analyze(evidence: EvidenceInputs, output_path: Path) -> dict[str, Any]:
    source = validate_source(evidence)
    artifacts = validate_artifacts(evidence)
    raw = analyze_raw(evidence.raw_path, evidence.embedded_corpus_path)
    conversion_report = json.loads(
        (MODEL_DIR / "ort-conversion-report.json").read_text(encoding="utf-8")
    )
    require(
        raw["output_f32le_sha256"]
        == conversion_report["verification"]["output_f32le_sha256"],
        "physical output fingerprint differs from the host ORT gate",
    )
    resources = validate_resources(evidence.resource_path, artifacts["profile"], raw)
    raw_sha256 = sha256_file(evidence.raw_path)
    resource_sha256 = sha256_file(evidence.resource_path)
    require(
        parse_manifest(evidence.raw_manifest_path, "/var/lib/ort/raw.csv")
        == raw_sha256,
        "raw hash differs from guest manifest",
    )
    require(
        parse_manifest(evidence.resource_manifest_path, "/var/lib/ort/resources.txt")
        == resource_sha256,
        "resource hash differs from guest manifest",
    )
    console = validate_console(
        evidence.console_path,
        artifacts,
        resources,
        raw_sha256,
        resource_sha256,
    )

    report = {
        "schema_version": 1,
        "status": "pass",
        "model_id": "thermal-4x6x1-v1",
        "platform": "AxVisor/StarryOS on OrangePi 5 Plus",
        "run": {
            "id": evidence.run_id,
            "started_at": evidence.started_at,
            "finished_at": evidence.finished_at,
        },
        "source": source,
        "board": {
            "facts": artifacts["board_facts"],
            "linux_restored": True,
        },
        "backend": {
            "kind": "onnxruntime-cpu",
            "runtime_version": EXPECTED_RUNTIME_VERSION,
            "execution_provider": "CPUExecutionProvider",
            "physical_ort_executed": True,
        },
        "artifacts": {
            "board_facts_sha256": sha256_file(evidence.board_facts_path),
            "console_sha256": sha256_file(evidence.console_path),
            "embedded": artifacts["embedded"],
            "expected_hashes": artifacts["expected_hashes"],
            "profile_sha256": sha256_file(evidence.profile_path),
            "raw_manifest_sha256": sha256_file(evidence.raw_manifest_path),
            "raw_sha256": raw_sha256,
            "resource_manifest_sha256": sha256_file(evidence.resource_manifest_path),
            "resource_sha256": resource_sha256,
            "snapshot": artifacts["snapshot"],
        },
        "vectors": EXPECTED_VECTORS,
        "numerical": {
            "exact_actuator_matches": raw["exact_actuator_matches"],
            "material_actuator_mismatches": raw["material_actuator_mismatches"],
            "maximum_absolute_error": raw["maximum_absolute_error"],
            "maximum_absolute_error_gate": MAXIMUM_ABSOLUTE_ERROR,
            "output_f32le_sha256": raw["output_f32le_sha256"],
            "rounding_boundary_equivalences": raw[
                "rounding_boundary_equivalences"
            ],
        },
        "latency": {
            **raw["latency"],
            "initialization_us": resources["initialization_us"],
            "warmup_vectors": int(artifacts["profile"]["warmup"]),
        },
        "resources": {
            "lifecycle_cycles": resources["lifecycle_cycles"],
            "memory": resources["memory"],
            "rootfs": resources["rootfs"],
        },
        "console_evidence": console,
        "gates": {
            "clean_source": not source["dirty"],
            "embedded_artifacts_match_frozen_sources": True,
            "host_output_fingerprint": True,
            "linux_restored": True,
            "numerical_equivalence": True,
            "physical_ort_cpu_executed": True,
            "raw_manifest": True,
            "repeated_session_lifecycle": True,
            "resource_manifest": True,
            "rootfs_available_space": True,
            "rss_growth": True,
            "rss_peak": True,
            "snapshot_matches_board": True,
        },
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "raw",
        "raw_manifest",
        "resource",
        "resource_manifest",
        "console",
        "profile",
        "board_facts",
        "snapshot",
        "embedded_runner",
        "embedded_model",
        "embedded_corpus",
        "embedded_runtime",
        "embedded_provider",
        "built_runner",
        "built_corpus",
        "output",
    ):
        parser.add_argument("--" + name.replace("_", "-"), type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-branch", required=True)
    parser.add_argument("--source-dirty", choices=("true", "false"), required=True)
    parser.add_argument("--tracked-change-count", type=int, required=True)
    parser.add_argument("--untracked-file-count", type=int, required=True)
    parser.add_argument("--started-at", required=True)
    parser.add_argument("--finished-at", required=True)
    parser.add_argument("--require-clean-source", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    evidence = EvidenceInputs(
        raw_path=args.raw.resolve(),
        raw_manifest_path=args.raw_manifest.resolve(),
        resource_path=args.resource.resolve(),
        resource_manifest_path=args.resource_manifest.resolve(),
        console_path=args.console.resolve(),
        profile_path=args.profile.resolve(),
        board_facts_path=args.board_facts.resolve(),
        snapshot_path=args.snapshot.resolve(),
        embedded_runner_path=args.embedded_runner.resolve(),
        embedded_model_path=args.embedded_model.resolve(),
        embedded_corpus_path=args.embedded_corpus.resolve(),
        embedded_runtime_path=args.embedded_runtime.resolve(),
        embedded_provider_path=args.embedded_provider.resolve(),
        built_runner_path=args.built_runner.resolve(),
        built_corpus_path=args.built_corpus.resolve(),
        run_id=args.run_id,
        source_commit=args.source_commit,
        source_branch=args.source_branch,
        source_dirty=args.source_dirty == "true",
        tracked_change_count=args.tracked_change_count,
        untracked_file_count=args.untracked_file_count,
        started_at=args.started_at,
        finished_at=args.finished_at,
        require_clean_source=args.require_clean_source,
    )
    report = analyze(evidence, args.output.resolve())
    print(
        "THERMAL_ORT_STARRY_REFERENCE_PASS "
        + json.dumps(
            {
                "initialization_us": report["latency"]["initialization_us"],
                "maximum_absolute_error": report["numerical"][
                    "maximum_absolute_error"
                ],
                "peak_rss_kib": report["resources"]["memory"]["peak_rss_kib"],
                "raw_sha256": report["artifacts"]["raw_sha256"],
                "rootfs_available_percent": report["resources"]["rootfs"][
                    "available_percent"
                ],
                "vectors": report["vectors"],
                "wall_p99_ns": report["latency"]["p99"],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (EvidenceError, KeyError, OSError, ValueError) as error:
        print(f"thermal ORT StarryOS reference failed: {error}")
        raise SystemExit(1) from error
