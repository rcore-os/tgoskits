#!/usr/bin/env python3
"""Verify the frozen RKNN artifact with the Toolkit2 simulator and FP16 oracle."""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

import numpy as np

from convert_thermal_rknn import (
    EXPECTED_PACKAGE_VERSIONS,
    EXPECTED_PYTHON,
    MEMORY_PERTURB_VALUE,
    MODEL_ID,
    ONNX_FILENAME,
    REPORT_FILENAME as CONVERSION_REPORT_FILENAME,
    RKNN_CONFIG,
    RKNN_FILENAME,
    require_exact_toolchain,
    sha256_file,
    write_json,
)


MODEL_DIR = Path(__file__).resolve().parent
WEIGHTS_FILENAME = f"{MODEL_ID}.weights.json"
GOLDEN_FILENAME = "golden-vectors.json"
BASE_MANIFEST_FILENAME = "model-manifest.json"
SIMULATOR_REPORT_FILENAME = "rknn-simulator-report.json"
EXPECTED_VECTORS = 10_000
F32_MAX_ABSOLUTE_ERROR_GATE = 0.002
ACTUATOR_MAX_ABSOLUTE_DELTA_GATE = 2
FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE = 0.0005


class VerificationError(RuntimeError):
    """The RKNN artifact or simulator violated the frozen model contract."""


def main() -> int:
    args = parse_args()
    if args.worker:
        return run_worker(
            Path(args.worker_onnx),
            Path(args.worker_inputs),
            Path(args.worker_output),
        )

    require_exact_toolchain()
    documents = load_documents()
    inputs = decode_inputs(documents["vectors"])

    with tempfile.TemporaryDirectory(prefix="thermal-rknn-simulator-") as temporary:
        temporary_dir = Path(temporary)
        simulator_outputs, _raw_log = run_simulator(
            documents["onnx_path"],
            inputs,
            temporary_dir,
            Path(args.raw_log).resolve() if args.raw_log else None,
        )

        report = build_report(documents, simulator_outputs)
        generated_report = temporary_dir / SIMULATOR_REPORT_FILENAME
        write_json(generated_report, report)
        committed_report = MODEL_DIR / SIMULATOR_REPORT_FILENAME
        if args.check:
            if not committed_report.is_file():
                raise VerificationError(
                    f"committed simulator report is missing: {committed_report}"
                )
            if generated_report.read_bytes() != committed_report.read_bytes():
                raise VerificationError(
                    f"RKNN simulator result differs from {SIMULATOR_REPORT_FILENAME}"
                )
        else:
            shutil.copyfile(generated_report, committed_report)

    print(
        "THERMAL_RKNN_SIMULATOR_PASS",
        json.dumps(
            {
                "command_delta_histogram": report["comparison_to_native_f32"][
                    "actuator_command_delta_histogram"
                ],
                "fp16_oracle_maximum_absolute_error": report[
                    "comparison_to_fp16_oracle"
                ]["maximum_absolute_error"],
                "native_f32_maximum_absolute_error": report[
                    "comparison_to_native_f32"
                ]["maximum_absolute_error"],
                "vectors": report["vectors"],
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
    )
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="rerun the simulator and compare with the committed deterministic report",
    )
    parser.add_argument(
        "--raw-log",
        help="optional path for the unnormalized vendor stdout/stderr log",
    )
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--worker-onnx", help=argparse.SUPPRESS)
    parser.add_argument("--worker-inputs", help=argparse.SUPPRESS)
    parser.add_argument("--worker-output", help=argparse.SUPPRESS)
    args = parser.parse_args()
    worker_paths = (args.worker_onnx, args.worker_inputs, args.worker_output)
    if args.worker and not all(worker_paths):
        parser.error("internal worker mode requires RKNN, input, and output paths")
    return args


def require(condition: bool, message: str) -> None:
    if not condition:
        raise VerificationError(message)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise VerificationError(f"cannot read JSON {path}: {error}") from error
    require(isinstance(value, dict), f"JSON root must be an object: {path}")
    return value


def load_documents() -> dict[str, Any]:
    paths = {
        "weights": MODEL_DIR / WEIGHTS_FILENAME,
        "golden": MODEL_DIR / GOLDEN_FILENAME,
        "manifest": MODEL_DIR / BASE_MANIFEST_FILENAME,
        "conversion_report": MODEL_DIR / CONVERSION_REPORT_FILENAME,
        "onnx": MODEL_DIR / ONNX_FILENAME,
        "rknn": MODEL_DIR / RKNN_FILENAME,
    }
    for path in paths.values():
        require(path.is_file(), f"required RKNN verification input is missing: {path}")

    weights = load_json(paths["weights"])
    golden = load_json(paths["golden"])
    manifest = load_json(paths["manifest"])
    conversion_report = load_json(paths["conversion_report"])
    vectors = golden.get("vectors")
    require(isinstance(vectors, list), "golden vectors must be a list")
    require(len(vectors) == EXPECTED_VECTORS, f"expected {EXPECTED_VECTORS} vectors")
    require(
        weights.get("model_id")
        == golden.get("model_id")
        == manifest.get("model_id")
        == conversion_report.get("model_id")
        == MODEL_ID,
        "model IDs differ",
    )
    require(
        manifest["sources"]["weights"]["sha256"] == sha256_file(paths["weights"]),
        "weights hash differs from the base manifest",
    )
    require(
        manifest["artifacts"]["golden_vectors"]["sha256"]
        == sha256_file(paths["golden"]),
        "golden-vector hash differs from the base manifest",
    )
    require(
        conversion_report["sources"]["base_manifest_sha256"]
        == sha256_file(paths["manifest"]),
        "conversion report is not chained to the base manifest",
    )
    require(
        conversion_report["sources"]["onnx_sha256"] == sha256_file(paths["onnx"]),
        "ONNX hash differs from the conversion report",
    )
    require(
        conversion_report["artifact"]["sha256"] == sha256_file(paths["rknn"]),
        "RKNN hash differs from the conversion report",
    )
    require(conversion_report.get("status") == "pass", "RKNN conversion did not pass")
    require(
        conversion_report["operator_evidence"]["all_model_compute_nodes_on_npu"],
        "conversion report does not map every model compute node to the NPU",
    )
    return {
        "conversion_report": conversion_report,
        "conversion_report_path": paths["conversion_report"],
        "golden_path": paths["golden"],
        "manifest_path": paths["manifest"],
        "onnx_path": paths["onnx"],
        "rknn_path": paths["rknn"],
        "vectors": vectors,
        "weights": weights,
        "weights_path": paths["weights"],
    }


def decode_inputs(vectors: list[dict[str, Any]]) -> np.ndarray[Any, np.dtype[np.float32]]:
    encoded = np.asarray(
        [
            [int(value, 16) for value in vector["normalized_input_f32_bits"]]
            for vector in vectors
        ],
        dtype=np.uint32,
    )
    inputs = encoded.view(np.float32)
    require(inputs.shape == (EXPECTED_VECTORS, 4), f"unexpected corpus shape: {inputs.shape}")
    require(np.isfinite(inputs).all(), "corpus contains non-finite inputs")
    return inputs


def run_simulator(
    onnx_path: Path,
    inputs: np.ndarray[Any, np.dtype[np.float32]],
    temporary_dir: Path,
    raw_log_path: Path | None,
) -> tuple[np.ndarray[Any, np.dtype[np.float32]], bytes]:
    inputs_path = temporary_dir / "inputs.f32le"
    outputs_path = temporary_dir / "outputs.f32le"
    inputs_path.write_bytes(inputs.astype("<f4", copy=False).tobytes(order="C"))
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--worker",
        "--worker-onnx",
        str(onnx_path),
        "--worker-inputs",
        str(inputs_path),
        "--worker-output",
        str(outputs_path),
    ]
    environment = os.environ.copy()
    environment["MALLOC_PERTURB_"] = MEMORY_PERTURB_VALUE
    environment["PYTHONHASHSEED"] = "0"
    completed = subprocess.run(
        command,
        cwd=temporary_dir,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        timeout=300,
    )
    raw_log = completed.stdout
    if raw_log_path is not None:
        raw_log_path.write_bytes(raw_log)
    if completed.returncode != 0:
        tail = raw_log.decode("utf-8", errors="replace")[-4000:]
        raise VerificationError(
            f"RKNN simulator worker failed with status {completed.returncode}:\n{tail}"
        )
    require(outputs_path.is_file(), "RKNN simulator did not write outputs")
    outputs = np.frombuffer(outputs_path.read_bytes(), dtype="<f4").copy()
    require(outputs.shape == (EXPECTED_VECTORS,), f"unexpected output shape: {outputs.shape}")
    require(np.isfinite(outputs).all(), "RKNN simulator produced non-finite outputs")
    decoded_log = raw_log.decode("utf-8", errors="replace")
    for marker in (
        "RKNN_SIM_STEP config status=0",
        "RKNN_SIM_STEP load_onnx status=0",
        "RKNN_SIM_STEP build status=0",
        "RKNN_SIM_STEP init_runtime status=0",
        f"RKNN_SIM_OUTPUTS count={EXPECTED_VECTORS}",
    ):
        require(marker in decoded_log, f"RKNN simulator log lacks marker: {marker}")
    return outputs, raw_log


def run_worker(onnx_path: Path, inputs_path: Path, output_path: Path) -> int:
    if os.environ.get("MALLOC_PERTURB_") != MEMORY_PERTURB_VALUE:
        raise SystemExit("RKNN simulator worker must start with MALLOC_PERTURB_=255")

    from rknn.api import RKNN

    inputs = np.frombuffer(inputs_path.read_bytes(), dtype="<f4")
    if inputs.size != EXPECTED_VECTORS * 4:
        raise SystemExit(f"RKNN simulator input has {inputs.size} elements")
    inputs = inputs.reshape(EXPECTED_VECTORS, 4)

    rknn = RKNN(verbose=True)
    outputs: list[float] = []
    try:
        run_rknn_step("config", rknn.config(**RKNN_CONFIG))
        run_rknn_step("load_onnx", rknn.load_onnx(model=str(onnx_path)))
        run_rknn_step("build", rknn.build(do_quantization=False))
        run_rknn_step("init_runtime", rknn.init_runtime())
        for index, input_row in enumerate(inputs):
            result = rknn.inference(inputs=[input_row.reshape(1, 4)])
            if not isinstance(result, list) or len(result) != 1:
                raise SystemExit(f"RKNN inference {index} returned an invalid result list")
            output = np.asarray(result[0])
            if output.size != 1:
                raise SystemExit(f"RKNN inference {index} returned shape {output.shape}")
            value = float(output.reshape(-1)[0])
            if not math.isfinite(value):
                raise SystemExit(f"RKNN inference {index} returned {value}")
            outputs.append(value)
    finally:
        rknn.release()

    output_path.write_bytes(np.asarray(outputs, dtype="<f4").tobytes(order="C"))
    print(f"RKNN_SIM_OUTPUTS count={len(outputs)}", flush=True)
    return 0


def run_rknn_step(name: str, status: int) -> None:
    print(f"RKNN_SIM_STEP {name} status={status}", flush=True)
    if status != 0:
        raise SystemExit(f"RKNN simulator {name} failed with status {status}")


def fp16_oracle(
    weights: dict[str, Any], inputs: np.ndarray[Any, np.dtype[np.float32]]
) -> np.ndarray[Any, np.dtype[np.float16]]:
    hidden_source, output_source = weights["layers"]
    hidden_weights = np.asarray(hidden_source["weights"], dtype=np.float16)
    hidden_bias = np.asarray(hidden_source["bias"], dtype=np.float16)
    output_weights = np.asarray(output_source["weights"][0], dtype=np.float16)
    output_bias = np.float16(output_source["bias"][0])
    outputs = np.empty(inputs.shape[0], dtype=np.float16)
    for index, input_row in enumerate(inputs):
        hidden = np.asarray(
            np.matmul(input_row.astype(np.float16), hidden_weights.T) + hidden_bias,
            dtype=np.float16,
        )
        hidden = np.maximum(hidden, np.float16(0.0))
        output = np.float16(np.matmul(hidden, output_weights) + output_bias)
        outputs[index] = np.clip(output, np.float16(0.0), np.float16(1.0))
    return outputs


def actuator_commands(outputs: np.ndarray[Any, Any]) -> np.ndarray[Any, np.dtype[np.int32]]:
    values = np.asarray(outputs, dtype=np.float32)
    scaled = np.float32(values * np.float32(1000.0))
    rounded = np.float32(scaled + np.float32(0.5))
    return np.trunc(rounded).astype(np.int32)


def build_report(
    documents: dict[str, Any], simulator_outputs: np.ndarray[Any, np.dtype[np.float32]]
) -> dict[str, Any]:
    vectors = documents["vectors"]
    inputs = decode_inputs(vectors)
    native_outputs = np.asarray([vector["output"] for vector in vectors], dtype=np.float32)
    native_commands = np.asarray(
        [vector["actuator_permille"] for vector in vectors], dtype=np.int32
    )
    simulator_commands = actuator_commands(simulator_outputs)
    command_deltas = simulator_commands - native_commands
    maximum_command_delta = int(np.max(np.abs(command_deltas)))
    f32_errors = np.abs(
        simulator_outputs.astype(np.float64) - native_outputs.astype(np.float64)
    )
    maximum_f32_error = float(np.max(f32_errors))

    oracle_outputs = fp16_oracle(documents["weights"], inputs)
    fp16_errors = np.abs(
        simulator_outputs.astype(np.float64) - oracle_outputs.astype(np.float64)
    )
    maximum_fp16_error = float(np.max(fp16_errors))
    exact_fp16 = simulator_outputs == oracle_outputs.astype(np.float32)

    require(
        maximum_f32_error <= F32_MAX_ABSOLUTE_ERROR_GATE,
        f"native-f32 maximum error {maximum_f32_error} exceeds gate",
    )
    require(
        maximum_command_delta <= ACTUATOR_MAX_ABSOLUTE_DELTA_GATE,
        f"actuator command delta {maximum_command_delta} exceeds gate",
    )
    require(
        maximum_fp16_error <= FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
        f"FP16-oracle maximum error {maximum_fp16_error} exceeds gate",
    )

    unique_deltas, counts = np.unique(command_deltas, return_counts=True)
    histogram = {
        str(int(delta)): int(count)
        for delta, count in zip(unique_deltas, counts, strict=True)
    }
    command_examples = [
        {
            "index": int(index),
            "label": vectors[index]["label"],
            "native_command": int(native_commands[index]),
            "native_output": float(native_outputs[index]),
            "rknn_command": int(simulator_commands[index]),
            "rknn_output": float(simulator_outputs[index]),
        }
        for index in np.flatnonzero(command_deltas)[:5]
    ]
    fp16_examples = [
        {
            "fp16_oracle_output": float(oracle_outputs[index]),
            "index": int(index),
            "label": vectors[index]["label"],
            "rknn_output": float(simulator_outputs[index]),
        }
        for index in np.flatnonzero(~exact_fp16)[:5]
    ]

    return {
        "schema_version": 1,
        "model_id": MODEL_ID,
        "status": "pass",
        "vectors": len(vectors),
        "artifacts": {
            "base_manifest_sha256": sha256_file(documents["manifest_path"]),
            "conversion_report_sha256": sha256_file(
                documents["conversion_report_path"]
            ),
            "golden_vectors_sha256": sha256_file(documents["golden_path"]),
            "onnx_sha256": sha256_file(documents["onnx_path"]),
            "rknn_sha256": sha256_file(documents["rknn_path"]),
            "verifier_sha256": sha256_file(Path(__file__).resolve()),
            "weights_sha256": sha256_file(documents["weights_path"]),
        },
        "backend": {
            "kind": "rknn-toolkit2-onnx-host-simulator",
            "target_platform": "rk3588",
            "precision": "FP16",
            "toolkit_version": EXPECTED_PACKAGE_VERSIONS["rknn-toolkit2"],
            "python": ".".join(str(value) for value in EXPECTED_PYTHON),
            "raw_vendor_log_committed": False,
            "compiled_rknn_executed": False,
            "conversion_config": RKNN_CONFIG,
            "scope": "Toolkit2 does not execute load_rknn artifacts in its host simulator; this run builds the same frozen ONNX with the same RK3588 FP16 configuration. The compiled artifact is covered by conversion hashes and must be executed on physical RK3588 hardware.",
        },
        "comparison_to_native_f32": {
            "maximum_absolute_error": maximum_f32_error,
            "maximum_absolute_error_gate": F32_MAX_ABSOLUTE_ERROR_GATE,
            "exact_actuator_matches": int(np.count_nonzero(command_deltas == 0)),
            "maximum_absolute_actuator_command_delta": maximum_command_delta,
            "maximum_absolute_actuator_command_delta_gate": ACTUATOR_MAX_ABSOLUTE_DELTA_GATE,
            "actuator_command_delta_histogram": histogram,
            "nonzero_command_delta_examples": command_examples,
        },
        "comparison_to_fp16_oracle": {
            "maximum_absolute_error": maximum_fp16_error,
            "maximum_absolute_error_gate": FP16_ORACLE_MAX_ABSOLUTE_ERROR_GATE,
            "exact_output_matches": int(np.count_nonzero(exact_fp16)),
            "nonexact_output_examples": fp16_examples,
        },
        "gates": {
            "all_outputs_finite": True,
            "expected_output_count": True,
            "fp16_oracle_error": True,
            "native_f32_actuator_delta": True,
            "native_f32_error": True,
        },
    }


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, VerificationError, json.JSONDecodeError) as error:
        print(f"thermal RKNN verification failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
