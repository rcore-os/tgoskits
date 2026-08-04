#!/usr/bin/env python3
"""Prepare and analyze the physical Linux RKNN reference corpus."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import re
import struct
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from pathlib import PurePosixPath
from typing import Any

import numpy as np


MODEL_DIR = Path(__file__).resolve().parent
if str(MODEL_DIR) not in sys.path:
    sys.path.insert(0, str(MODEL_DIR))

from verify_thermal_rknn import (
    ACTUATOR_MAX_ABSOLUTE_DELTA_GATE,
    F32_MAX_ABSOLUTE_ERROR_GATE,
    FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
    MODEL_ID,
    fp16_oracle,
)


WEIGHTS_PATH = MODEL_DIR / f"{MODEL_ID}.weights.json"
GOLDEN_PATH = MODEL_DIR / "golden-vectors.json"
MANIFEST_PATH = MODEL_DIR / "model-manifest.json"
CONVERSION_REPORT_PATH = MODEL_DIR / "rknn-conversion-report.json"
RKNN_PATH = MODEL_DIR / f"{MODEL_ID}-rk3588-fp16.rknn"
RUNTIME_PATH = (
    MODEL_DIR.parents[2]
    / "apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image"
    / "3rdparty/rknpu2/Linux/aarch64/librknnrt.so"
)
EXPECTED_VECTORS = 10_000
CORPUS_HEADER = (
    "index",
    "input0_f32_bits",
    "input1_f32_bits",
    "input2_f32_bits",
    "input3_f32_bits",
    "expected_output_f32_bits",
    "expected_actuator_permille",
)
RAW_HEADER = CORPUS_HEADER + (
    "rknn_output_f32_bits",
    "rknn_actuator_permille",
    "wall_ns",
    "device_us",
)
HEX32 = re.compile(r"[0-9a-f]{8}")
SHA256 = re.compile(r"[0-9a-f]{64}")
SOURCE_COMMIT = re.compile(r"[0-9a-f]{40}")
RUN_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")


class ReferenceError(RuntimeError):
    """Physical reference input or evidence violated its frozen contract."""


@dataclass(frozen=True)
class EvidenceInputs:
    """Paths and source provenance captured by the physical-board runner."""

    board_facts_path: Path
    ldd_path: Path
    deployed_runtime_path: Path
    deployed_model_path: Path
    runner_binary_path: Path
    board_type: str
    remote_dir: str
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
        raise ReferenceError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def file_record(path: Path, display_path: str | None = None) -> dict[str, Any]:
    require(path.is_file(), f"evidence artifact is missing: {path}")
    return {
        "path": display_path or path.name,
        "sha256": sha256_file(path),
        "size_bytes": path.stat().st_size,
    }


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReferenceError(f"cannot read JSON {path}: {error}") from error
    require(isinstance(value, dict), f"JSON root must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def load_documents() -> dict[str, Any]:
    weights = load_json(WEIGHTS_PATH)
    golden = load_json(GOLDEN_PATH)
    manifest = load_json(MANIFEST_PATH)
    conversion = load_json(CONVERSION_REPORT_PATH)
    vectors = golden.get("vectors")
    require(isinstance(vectors, list), "golden vectors must be a list")
    require(len(vectors) == EXPECTED_VECTORS, "golden corpus must contain 10000 vectors")
    require(
        weights.get("model_id")
        == golden.get("model_id")
        == manifest.get("model_id")
        == conversion.get("model_id")
        == MODEL_ID,
        "model IDs differ",
    )
    require(
        manifest["sources"]["weights"]["sha256"] == sha256_file(WEIGHTS_PATH),
        "weights hash differs from base manifest",
    )
    require(
        manifest["artifacts"]["golden_vectors"]["sha256"] == sha256_file(GOLDEN_PATH),
        "golden hash differs from base manifest",
    )
    require(
        conversion["artifact"]["sha256"] == sha256_file(RKNN_PATH),
        "RKNN hash differs from conversion report",
    )
    require(conversion.get("status") == "pass", "RKNN conversion did not pass")
    return {
        "conversion": conversion,
        "manifest": manifest,
        "vectors": vectors,
        "weights": weights,
    }


def corpus_bytes(vectors: list[dict[str, Any]]) -> bytes:
    lines = [",".join(CORPUS_HEADER)]
    for index, vector in enumerate(vectors):
        input_bits = vector.get("normalized_input_f32_bits")
        require(
            isinstance(input_bits, list)
            and len(input_bits) == 4
            and all(isinstance(value, str) and HEX32.fullmatch(value) for value in input_bits),
            f"vector {index} has invalid input bits",
        )
        output_bits = vector.get("output_f32_bits")
        command = vector.get("actuator_permille")
        require(
            isinstance(output_bits, str) and HEX32.fullmatch(output_bits) is not None,
            f"vector {index} has invalid output bits",
        )
        require(isinstance(command, int) and 0 <= command <= 1000, f"vector {index} has invalid command")
        lines.append(
            ",".join(
                [str(index), *input_bits, output_bits, str(command)]
            )
        )
    return ("\n".join(lines) + "\n").encode()


def prepare_corpus(output_path: Path, check: bool) -> dict[str, Any]:
    documents = load_documents()
    encoded = corpus_bytes(documents["vectors"])
    if check:
        require(output_path.is_file(), f"prepared corpus is missing: {output_path}")
        require(output_path.read_bytes() == encoded, "prepared corpus is stale")
    else:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_bytes(encoded)
    return {
        "model_id": MODEL_ID,
        "path": str(output_path),
        "sha256": hashlib.sha256(encoded).hexdigest(),
        "size_bytes": len(encoded),
        "vectors": len(documents["vectors"]),
    }


def parse_hex32(value: str, context: str) -> int:
    require(HEX32.fullmatch(value) is not None, f"{context} must be canonical lowercase hex32")
    return int(value, 16)


def f32_from_bits(value: int) -> float:
    return struct.unpack("<f", struct.pack("<I", value))[0]


def decode_inputs(vectors: list[dict[str, Any]]) -> np.ndarray[Any, np.dtype[np.float32]]:
    encoded = np.asarray(
        [[int(value, 16) for value in vector["normalized_input_f32_bits"]] for vector in vectors],
        dtype=np.uint32,
    )
    return encoded.view(np.float32)


def read_raw(path: Path, vectors: list[dict[str, Any]]) -> dict[str, Any]:
    outputs: list[float] = []
    commands: list[int] = []
    wall_ns: list[int] = []
    device_us: list[int] = []
    with path.open("r", encoding="utf-8", newline="") as source:
        reader = csv.DictReader(source)
        require(tuple(reader.fieldnames or ()) == RAW_HEADER, "raw CSV header differs from schema 1")
        for expected_index, row in enumerate(reader):
            require(expected_index < len(vectors), "raw CSV has extra rows")
            require(int(row["index"]) == expected_index, "raw indices are not contiguous")
            vector = vectors[expected_index]
            for input_index in range(4):
                field = f"input{input_index}_f32_bits"
                parse_hex32(row[field], f"row {expected_index} {field}")
                require(
                    row[field] == vector["normalized_input_f32_bits"][input_index],
                    f"row {expected_index} input differs from golden corpus",
                )
            parse_hex32(row["expected_output_f32_bits"], f"row {expected_index} expected output")
            require(
                row["expected_output_f32_bits"] == vector["output_f32_bits"],
                f"row {expected_index} expected output differs from golden corpus",
            )
            require(
                int(row["expected_actuator_permille"]) == vector["actuator_permille"],
                f"row {expected_index} expected command differs from golden corpus",
            )
            output = f32_from_bits(
                parse_hex32(row["rknn_output_f32_bits"], f"row {expected_index} RKNN output")
            )
            require(math.isfinite(output), f"row {expected_index} output is not finite")
            command = int(row["rknn_actuator_permille"])
            expected_command_from_output = int(
                np.float32(np.float32(output) * np.float32(1000.0)) + np.float32(0.5)
            )
            require(
                command == expected_command_from_output,
                f"row {expected_index} command does not match its float output",
            )
            row_wall_ns = int(row["wall_ns"])
            row_device_us = int(row["device_us"])
            require(row_wall_ns > 0, f"row {expected_index} wall time is not positive")
            require(row_device_us > 0, f"row {expected_index} device time is not positive")
            outputs.append(output)
            commands.append(command)
            wall_ns.append(row_wall_ns)
            device_us.append(row_device_us)
    require(len(outputs) == len(vectors), f"raw CSV contains {len(outputs)} vectors")
    return {
        "commands": np.asarray(commands, dtype=np.int32),
        "device_us": device_us,
        "outputs": np.asarray(outputs, dtype=np.float32),
        "wall_ns": wall_ns,
    }


def marker_fields(console: str, prefix: str) -> dict[str, str]:
    matching = [line for line in console.splitlines() if line.startswith(prefix)]
    require(len(matching) == 1, f"expected exactly one {prefix} marker")
    fields: dict[str, str] = {}
    for token in matching[0][len(prefix) :].strip().split():
        require("=" in token, f"malformed token in {prefix}: {token}")
        name, value = token.split("=", maxsplit=1)
        require(name not in fields and name, f"duplicate or empty field in {prefix}")
        fields[name] = value
    return fields


def decode_hex_text(value: str, context: str) -> str:
    require(len(value) % 2 == 0 and re.fullmatch(r"[0-9a-f]*", value) is not None, f"invalid {context}")
    try:
        decoded = bytes.fromhex(value).decode("utf-8")
    except UnicodeDecodeError as error:
        raise ReferenceError(f"invalid UTF-8 in {context}") from error
    require("\x00" not in decoded, f"NUL in {context}")
    return decoded


def parse_console(path: Path, vector_count: int) -> dict[str, Any]:
    console = path.read_text(encoding="utf-8", errors="strict")
    require("IVC_RKNN_LINUX_ERROR" not in console, "runner emitted an error marker")
    begin = marker_fields(console, "IVC_RKNN_LINUX_BEGIN")
    runtime = marker_fields(console, "IVC_RKNN_RUNTIME")
    tensor = marker_fields(console, "IVC_RKNN_TENSOR")
    result = marker_fields(console, "IVC_RKNN_LINUX_RESULT")
    require(console.splitlines().count("IVC_RKNN_LINUX_DONE") == 1, "missing unique done marker")
    require(begin.get("schema") == "1", "runner schema differs")
    require(int(begin.get("vectors", "-1")) == vector_count, "begin vector count differs")
    require(begin.get("core_mask") == "0", "physical reference must use NPU core 0")
    require(result.get("status") == "pass", "runner result did not pass")
    require(int(result.get("vectors", "-1")) == vector_count, "result vector count differs")
    require(result.get("core_mask") == "0", "result core mask differs")
    require(result.get("perf_query_errors") == "0", "runner reported performance query errors")
    require(result.get("run_errors") == "0", "runner reported inference errors")
    require(tensor.get("input_type") == "FP16" and tensor.get("input_elems") == "4", "input tensor differs")
    require(tensor.get("submitted_input_type") == "FP32", "submitted input type differs")
    require(tensor.get("output_type") == "FP16" and tensor.get("output_elems") == "1", "output tensor differs")
    require(tensor.get("requested_output_type") == "FP32", "requested output type differs")
    require(
        decode_hex_text(tensor["input_name_hex"], "input tensor name") == "normalized_observation",
        "input tensor name differs",
    )
    require(
        decode_hex_text(tensor["output_name_hex"], "output tensor name") == "control_fraction",
        "output tensor name differs",
    )
    api_version = decode_hex_text(runtime["api_version_hex"], "API version")
    driver_version = decode_hex_text(runtime["driver_version_hex"], "driver version")
    require(api_version.startswith("2.3.2 "), f"unexpected RKNN Runtime API: {api_version}")
    require(driver_version == "0.9.6", f"unexpected Linux RKNPU driver: {driver_version}")
    return {
        "api_version": api_version,
        "begin": begin,
        "driver_version": driver_version,
        "result": result,
        "tensor": tensor,
    }


def parse_key_value_file(path: Path) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        require("=" in line, f"malformed board fact at line {line_number}")
        name, value = line.split("=", maxsplit=1)
        require(re.fullmatch(r"[a-z][a-z0-9_]*", name) is not None, f"invalid board fact name: {name}")
        require(name not in fields, f"duplicate board fact: {name}")
        require(value != "", f"empty board fact: {name}")
        fields[name] = value
    return fields


def parse_positive_integer(value: str, context: str) -> int:
    require(re.fullmatch(r"[0-9]+", value) is not None, f"invalid {context}")
    parsed = int(value)
    require(parsed > 0, f"{context} must be positive")
    return parsed


def parse_timestamp(value: str, context: str) -> datetime:
    require(value.endswith("Z"), f"{context} must use UTC Z notation")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise ReferenceError(f"invalid {context}: {value}") from error
    require(parsed.microsecond == 0, f"{context} must use whole seconds")
    return parsed


def build_physical_evidence(
    evidence: EvidenceInputs,
    corpus_path: Path,
) -> dict[str, Any]:
    require(evidence.board_type == "OrangePi-5-Plus", "unexpected physical board type")
    require(RUN_ID.fullmatch(evidence.run_id) is not None, "invalid run ID")
    require(SOURCE_COMMIT.fullmatch(evidence.source_commit) is not None, "invalid source commit")
    require(evidence.source_branch != "", "source branch is empty")
    require(evidence.tracked_change_count >= 0, "tracked change count is negative")
    require(evidence.untracked_file_count >= 0, "untracked file count is negative")
    require(
        evidence.source_dirty
        == (evidence.tracked_change_count > 0 or evidence.untracked_file_count > 0),
        "source dirty flag conflicts with change counts",
    )
    if evidence.require_clean_source:
        require(not evidence.source_dirty, "formal physical evidence requires a clean source tree")

    started_at = parse_timestamp(evidence.started_at, "run start time")
    finished_at = parse_timestamp(evidence.finished_at, "run finish time")
    require(finished_at >= started_at, "run finish time precedes start time")

    remote_dir = PurePosixPath(evidence.remote_dir)
    require(remote_dir.is_absolute(), "remote deployment directory must be absolute")
    require(
        len(remote_dir.parts) == 4
        and remote_dir.parts[:3] == ("/", "home", "orangepi")
        and remote_dir.name.startswith("ivc-rknn-reference-"),
        "remote deployment directory must be a direct child of /home/orangepi",
    )

    facts = parse_key_value_file(evidence.board_facts_path)
    required_facts = {
        "schema",
        "hostname",
        "machine",
        "kernel_release",
        "rknpu_version",
        "root_source",
        "root_fstype",
        "root_options",
        "machine_id_sha256",
        "cpu_temp_start_milli_c",
        "cpu_temp_finish_milli_c",
        "gxx_version_hex",
        "runtime_sha256",
        "rknn_sha256",
        "corpus_sha256",
        "runner_sha256",
    }
    require(required_facts <= facts.keys(), "board facts are incomplete")
    require(facts["schema"] == "1", "board facts schema differs")
    require(facts["hostname"] == "orangepi5plus", "unexpected board hostname")
    require(facts["machine"] == "aarch64", "unexpected board architecture")
    require(facts["kernel_release"] == "6.1.43-rockchip-rk3588", "unexpected Linux kernel")
    require(facts["rknpu_version"] == "0.9.6", "unexpected RKNPU driver module version")
    require(facts["root_fstype"] == "ext4", "board root filesystem is not ext4")
    require("rw" in facts["root_options"].split(","), "board root filesystem is not writable")
    require(SHA256.fullmatch(facts["machine_id_sha256"]) is not None, "invalid board identity hash")
    cpu_temp_start = parse_positive_integer(
        facts["cpu_temp_start_milli_c"], "start CPU temperature"
    )
    cpu_temp_finish = parse_positive_integer(
        facts["cpu_temp_finish_milli_c"], "finish CPU temperature"
    )
    gxx_version = decode_hex_text(facts["gxx_version_hex"], "g++ version")
    require(gxx_version.startswith("g++ "), "unexpected board C++ compiler identity")
    require("\n" not in gxx_version and "\r" not in gxx_version, "multiline C++ compiler identity")

    runtime = file_record(
        evidence.deployed_runtime_path,
        "apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/"
        "3rdparty/rknpu2/Linux/aarch64/librknnrt.so",
    )
    model = file_record(
        evidence.deployed_model_path,
        f"board/{MODEL_ID}-rk3588-fp16.rknn",
    )
    corpus = file_record(corpus_path, "board/corpus.csv")
    runner = file_record(evidence.runner_binary_path, "board/thermal_rknn_linux_reference")
    require(runtime["sha256"] == sha256_file(RUNTIME_PATH), "deployed RKNN Runtime differs from frozen runtime")
    require(model["sha256"] == sha256_file(RKNN_PATH), "deployed RKNN model differs from frozen model")
    require(corpus["sha256"] == facts["corpus_sha256"], "board corpus hash differs")
    require(runtime["sha256"] == facts["runtime_sha256"], "board runtime hash differs")
    require(model["sha256"] == facts["rknn_sha256"], "board RKNN hash differs")
    require(runner["sha256"] == facts["runner_sha256"], "board runner hash differs")

    expected_runtime_path = str(remote_dir / "lib/librknnrt.so")
    ldd_text = evidence.ldd_path.read_text(encoding="utf-8", errors="strict")
    resolved_runtime_lines = [
        line.strip() for line in ldd_text.splitlines() if line.strip().startswith("librknnrt.so =>")
    ]
    require(len(resolved_runtime_lines) == 1, "ldd must resolve exactly one RKNN Runtime")
    require(
        resolved_runtime_lines[0].startswith(f"librknnrt.so => {expected_runtime_path} "),
        "runner did not resolve the deployed RKNN Runtime",
    )

    return {
        "run": {
            "id": evidence.run_id,
            "started_at": evidence.started_at,
            "finished_at": evidence.finished_at,
        },
        "source": {
            "commit": evidence.source_commit,
            "branch": evidence.source_branch,
            "dirty": evidence.source_dirty,
            "tracked_change_count": evidence.tracked_change_count,
            "untracked_file_count": evidence.untracked_file_count,
            "clean_source_required": evidence.require_clean_source,
        },
        "board": {
            "type": evidence.board_type,
            "hostname": facts["hostname"],
            "machine": facts["machine"],
            "machine_id_sha256": facts["machine_id_sha256"],
            "kernel_release": facts["kernel_release"],
            "rknpu_version": facts["rknpu_version"],
            "root_filesystem": {
                "source": facts["root_source"],
                "type": facts["root_fstype"],
                "options": facts["root_options"],
            },
            "cpu_temp_milli_c": {
                "start": cpu_temp_start,
                "finish": cpu_temp_finish,
            },
        },
        "deployment": {
            "remote_dir": str(remote_dir),
            "compiler": gxx_version,
            "runtime": runtime,
            "model": model,
            "corpus": corpus,
            "runner": runner,
            "ldd": file_record(evidence.ldd_path, "board/ldd.log"),
            "resolved_runtime_path": expected_runtime_path,
            "uses_frozen_runtime": True,
        },
    }


def nearest_rank(values: list[int], percentile: int) -> int:
    require(values, "latency vector is empty")
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered) / 100))
    return ordered[rank - 1]


def latency_summary(values: list[int], unit: str) -> dict[str, Any]:
    return {
        "count": len(values),
        "maximum": max(values),
        "mean": sum(values) / len(values),
        "minimum": min(values),
        "p50": nearest_rank(values, 50),
        "p95": nearest_rank(values, 95),
        "p99": nearest_rank(values, 99),
        "unit": unit,
    }


def analyze(
    raw_path: Path,
    console_path: Path,
    corpus_path: Path,
    output_path: Path,
    evidence: EvidenceInputs,
) -> dict[str, Any]:
    documents = load_documents()
    expected_corpus = corpus_bytes(documents["vectors"])
    require(corpus_path.read_bytes() == expected_corpus, "deployed corpus differs from frozen source")
    raw = read_raw(raw_path, documents["vectors"])
    console = parse_console(console_path, len(documents["vectors"]))
    physical_evidence = build_physical_evidence(evidence, corpus_path)

    native_outputs = np.asarray([vector["output"] for vector in documents["vectors"]], dtype=np.float32)
    native_commands = np.asarray(
        [vector["actuator_permille"] for vector in documents["vectors"]], dtype=np.int32
    )
    output_errors = np.abs(raw["outputs"].astype(np.float64) - native_outputs.astype(np.float64))
    maximum_f32_error = float(np.max(output_errors))
    command_deltas = raw["commands"] - native_commands
    maximum_command_delta = int(np.max(np.abs(command_deltas)))

    inputs = decode_inputs(documents["vectors"])
    oracle_outputs = fp16_oracle(documents["weights"], inputs)
    fp16_errors = np.abs(raw["outputs"].astype(np.float64) - oracle_outputs.astype(np.float64))
    maximum_fp16_error = float(np.max(fp16_errors))
    exact_fp16 = raw["outputs"] == oracle_outputs.astype(np.float32)

    require(maximum_f32_error <= F32_MAX_ABSOLUTE_ERROR_GATE, "physical f32 error exceeds gate")
    require(maximum_command_delta <= ACTUATOR_MAX_ABSOLUTE_DELTA_GATE, "physical command delta exceeds gate")
    require(
        maximum_fp16_error <= FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
        "physical FP16-oracle error exceeds gate",
    )
    unique_deltas, counts = np.unique(command_deltas, return_counts=True)
    histogram = {
        str(int(delta)): int(count)
        for delta, count in zip(unique_deltas, counts, strict=True)
    }
    require(sum(histogram.values()) == EXPECTED_VECTORS, "command histogram count differs")

    report = {
        "schema_version": 1,
        "model_id": MODEL_ID,
        "status": "pass",
        "platform": "OrangePi 5 Plus Linux",
        "physical_evidence": physical_evidence,
        "backend": {
            "kind": "rknn-runtime-npu",
            "physical_compiled_rknn_executed": True,
            "core_mask": "0",
            "api_version": console["api_version"],
            "driver_version": console["driver_version"],
            "positive_device_time_samples": len(raw["device_us"]),
        },
        "artifacts": {
            "base_manifest_sha256": sha256_file(MANIFEST_PATH),
            "console_sha256": sha256_file(console_path),
            "conversion_report_sha256": sha256_file(CONVERSION_REPORT_PATH),
            "corpus_sha256": sha256_file(corpus_path),
            "golden_vectors_sha256": sha256_file(GOLDEN_PATH),
            "raw_sha256": sha256_file(raw_path),
            "rknn_sha256": sha256_file(RKNN_PATH),
            "weights_sha256": sha256_file(WEIGHTS_PATH),
            "board_facts_sha256": sha256_file(evidence.board_facts_path),
            "deployed_runtime_sha256": physical_evidence["deployment"]["runtime"]["sha256"],
            "runner_binary_sha256": physical_evidence["deployment"]["runner"]["sha256"],
        },
        "vectors": EXPECTED_VECTORS,
        "comparison_to_native_f32": {
            "actuator_command_delta_histogram": histogram,
            "exact_actuator_matches": int(np.count_nonzero(command_deltas == 0)),
            "maximum_absolute_actuator_command_delta": maximum_command_delta,
            "maximum_absolute_actuator_command_delta_gate": ACTUATOR_MAX_ABSOLUTE_DELTA_GATE,
            "maximum_absolute_error": maximum_f32_error,
            "maximum_absolute_error_gate": F32_MAX_ABSOLUTE_ERROR_GATE,
        },
        "comparison_to_fp16_oracle": {
            "exact_output_matches": int(np.count_nonzero(exact_fp16)),
            "maximum_absolute_error": maximum_fp16_error,
            "maximum_absolute_error_gate": FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
        },
        "latency": {
            "device": latency_summary(raw["device_us"], "us"),
            "wall": latency_summary(raw["wall_ns"], "ns"),
            "initialization_us": int(console["result"]["init_us"]),
            "warmup_vectors": int(console["result"]["warmup"]),
        },
        "gates": {
            "all_outputs_finite": True,
            "all_perf_queries_succeeded": True,
            "all_device_times_positive": True,
            "compiled_rknn_executed_on_physical_npu": True,
            "expected_output_count": True,
            "fp16_oracle_error": True,
            "native_f32_actuator_delta": True,
            "native_f32_error": True,
            "runtime_and_driver_versions": True,
        },
    }
    write_json(output_path, report)
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    prepare = subparsers.add_parser("prepare", help="write the deterministic board corpus CSV")
    prepare.add_argument("--output", type=Path, required=True)
    prepare.add_argument("--check", action="store_true")
    analyze_parser = subparsers.add_parser("analyze", help="validate physical RKNN raw evidence")
    analyze_parser.add_argument("--raw", type=Path, required=True)
    analyze_parser.add_argument("--console", type=Path, required=True)
    analyze_parser.add_argument("--corpus", type=Path, required=True)
    analyze_parser.add_argument("--output", type=Path, required=True)
    analyze_parser.add_argument("--board-facts", type=Path, required=True)
    analyze_parser.add_argument("--ldd", type=Path, required=True)
    analyze_parser.add_argument("--deployed-runtime", type=Path, required=True)
    analyze_parser.add_argument("--deployed-model", type=Path, required=True)
    analyze_parser.add_argument("--runner-binary", type=Path, required=True)
    analyze_parser.add_argument("--board-type", required=True)
    analyze_parser.add_argument("--remote-dir", required=True)
    analyze_parser.add_argument("--run-id", required=True)
    analyze_parser.add_argument("--source-commit", required=True)
    analyze_parser.add_argument("--source-branch", required=True)
    analyze_parser.add_argument("--source-dirty", choices=("true", "false"), required=True)
    analyze_parser.add_argument("--tracked-change-count", type=int, required=True)
    analyze_parser.add_argument("--untracked-file-count", type=int, required=True)
    analyze_parser.add_argument("--started-at", required=True)
    analyze_parser.add_argument("--finished-at", required=True)
    analyze_parser.add_argument("--require-clean-source", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "prepare":
        result = prepare_corpus(args.output.resolve(), args.check)
        print("THERMAL_RKNN_LINUX_CORPUS_PASS " + json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    evidence = EvidenceInputs(
        board_facts_path=args.board_facts.resolve(),
        ldd_path=args.ldd.resolve(),
        deployed_runtime_path=args.deployed_runtime.resolve(),
        deployed_model_path=args.deployed_model.resolve(),
        runner_binary_path=args.runner_binary.resolve(),
        board_type=args.board_type,
        remote_dir=args.remote_dir,
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
    report = analyze(
        args.raw.resolve(),
        args.console.resolve(),
        args.corpus.resolve(),
        args.output.resolve(),
        evidence,
    )
    print(
        "THERMAL_RKNN_LINUX_REFERENCE_PASS "
        + json.dumps(
            {
                "device_p99_us": report["latency"]["device"]["p99"],
                "maximum_absolute_error": report["comparison_to_native_f32"]["maximum_absolute_error"],
                "rknn_sha256": report["artifacts"]["rknn_sha256"],
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
    except (OSError, ReferenceError, ValueError) as error:
        print(f"thermal RKNN Linux reference failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
