#!/usr/bin/env python3
"""Verify the frozen corpus against the Rust oracle and optional ORT CPU EP."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import subprocess
import sys
from pathlib import Path
from typing import Any


MODEL_DIR = Path(__file__).resolve().parent
WEIGHTS_PATH = MODEL_DIR / "thermal-4x6x1-v1.weights.json"
ONNX_PATH = MODEL_DIR / "thermal-4x6x1-v1.onnx"
GOLDEN_PATH = MODEL_DIR / "golden-vectors.json"
MANIFEST_PATH = MODEL_DIR / "model-manifest.json"
EXPECTED_VECTORS = 10_000
ORT_MAX_ABSOLUTE_ERROR = 1e-6
ORT_ROUNDING_BOUNDARY_TOLERANCE = 1e-6


class VerificationError(RuntimeError):
    """A model artifact or backend violated the frozen contract."""


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--rust-oracle",
        type=Path,
        required=True,
        help="path to the built tools/ivcproto examples/thermal_oracle executable",
    )
    parser.add_argument(
        "--require-onnxruntime",
        action="store_true",
        help="fail instead of skipping when the Python onnxruntime package is unavailable",
    )
    return parser.parse_args()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise VerificationError(message)


def load_documents() -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    weights = json.loads(WEIGHTS_PATH.read_text(encoding="utf-8"))
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    golden = json.loads(GOLDEN_PATH.read_text(encoding="utf-8"))
    vectors = golden["vectors"]
    require(weights["model_id"] == manifest["model_id"] == golden["model_id"], "model IDs differ")
    require(len(vectors) == EXPECTED_VECTORS, f"expected {EXPECTED_VECTORS} vectors, got {len(vectors)}")
    require(golden["corpus"]["total_vectors"] == EXPECTED_VECTORS, "corpus count metadata differs")
    require(manifest["sources"]["weights"]["sha256"] == sha256(WEIGHTS_PATH), "weights hash differs")
    require(manifest["artifacts"]["onnx"]["sha256"] == sha256(ONNX_PATH), "ONNX hash differs")
    require(
        manifest["artifacts"]["golden_vectors"]["sha256"] == sha256(GOLDEN_PATH),
        "golden-vector hash differs",
    )
    return weights, manifest, vectors


def verify_rust_oracle(path: Path, vectors: list[dict[str, Any]]) -> dict[str, Any]:
    require(path.is_file(), f"Rust oracle not found: {path}")
    payload = "".join(
        ",".join(vector["normalized_input_f32_bits"]) + "\n" for vector in vectors
    ).encode()
    completed = subprocess.run(
        [str(path.resolve())],
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise VerificationError(
            f"Rust oracle exited {completed.returncode}: {completed.stderr.decode(errors='replace').strip()}"
        )
    outputs = completed.stdout.decode().splitlines()
    require(len(outputs) == len(vectors), f"Rust oracle returned {len(outputs)} outputs")
    mismatches = [
        (index, actual, vector["output_f32_bits"])
        for index, (actual, vector) in enumerate(zip(outputs, vectors, strict=True))
        if actual != vector["output_f32_bits"]
    ]
    require(not mismatches, f"Rust oracle mismatch: {mismatches[:5]}")
    return {"backend": "native-rust", "exact_f32_matches": len(outputs), "mismatches": 0}


def ort_available() -> bool:
    return importlib.util.find_spec("onnxruntime") is not None


def verify_onnxruntime(vectors: list[dict[str, Any]]) -> dict[str, Any]:
    import numpy as np
    import onnxruntime as ort

    options = ort.SessionOptions()
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    options.add_session_config_entry("session.intra_op.allow_spinning", "0")
    options.add_session_config_entry("session.inter_op.allow_spinning", "0")
    session = ort.InferenceSession(
        str(ONNX_PATH),
        sess_options=options,
        providers=["CPUExecutionProvider"],
    )
    require(session.get_providers() == ["CPUExecutionProvider"], f"unexpected ORT providers: {session.get_providers()}")

    maximum_error = 0.0
    exact_command_matches = 0
    rounding_boundary_equivalences: list[dict[str, Any]] = []
    material_command_mismatches: list[dict[str, Any]] = []
    for index, vector in enumerate(vectors):
        input_bits = np.asarray(
            [int(value, 16) for value in vector["normalized_input_f32_bits"]], dtype=np.uint32
        )
        inputs = input_bits.view(np.float32).reshape(1, 4)
        output = float(session.run(["control_fraction"], {"normalized_observation": inputs})[0][0, 0])
        require(math.isfinite(output), f"ORT output {index} is not finite")
        error = abs(output - float(vector["output"]))
        maximum_error = max(maximum_error, error)
        command = int(float(np.float32(np.float32(output) * np.float32(1000.0)) + np.float32(0.5)))
        expected_command = vector["actuator_permille"]
        if command == expected_command:
            exact_command_matches += 1
            continue
        expected_output = float(vector["output"])
        lower_command = min(command, expected_command)
        boundary = (lower_command + 0.5) / 1000.0
        mismatch = {
            "actual_command": command,
            "actual_output": output,
            "expected_command": expected_command,
            "expected_output": expected_output,
            "index": index,
            "label": vector["label"],
            "rounding_boundary": boundary,
        }
        if (
            abs(command - expected_command) == 1
            and abs(output - boundary) <= ORT_ROUNDING_BOUNDARY_TOLERANCE
            and abs(expected_output - boundary) <= ORT_ROUNDING_BOUNDARY_TOLERANCE
        ):
            rounding_boundary_equivalences.append(mismatch)
        else:
            material_command_mismatches.append(mismatch)
    require(maximum_error <= ORT_MAX_ABSOLUTE_ERROR, f"ORT maximum error {maximum_error} exceeds gate")
    require(
        not material_command_mismatches,
        f"ORT produced material actuator mismatches: {material_command_mismatches[:5]}",
    )
    return {
        "backend": "onnxruntime-cpu",
        "exact_actuator_matches": exact_command_matches,
        "material_actuator_mismatches": len(material_command_mismatches),
        "maximum_absolute_error": maximum_error,
        "providers": session.get_providers(),
        "rounding_boundary_equivalences": len(rounding_boundary_equivalences),
        "rounding_boundary_examples": rounding_boundary_equivalences[:5],
        "rounding_boundary_tolerance": ORT_ROUNDING_BOUNDARY_TOLERANCE,
        "vectors": len(vectors),
        "version": ort.__version__,
    }


def main() -> int:
    args = parse_args()
    _, manifest, vectors = load_documents()
    results: dict[str, Any] = {
        "manifest_sha256": sha256(MANIFEST_PATH),
        "model_id": manifest["model_id"],
        "onnx_sha256": sha256(ONNX_PATH),
        "rust": verify_rust_oracle(args.rust_oracle, vectors),
        "vectors": len(vectors),
        "weights_sha256": sha256(WEIGHTS_PATH),
    }
    if ort_available():
        results["onnxruntime"] = verify_onnxruntime(vectors)
    elif args.require_onnxruntime:
        raise VerificationError("onnxruntime is required but not installed")
    else:
        results["onnxruntime"] = {"status": "skipped-not-installed"}
    print("THERMAL_MODEL_VERIFY_PASS " + json.dumps(results, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, VerificationError, json.JSONDecodeError) as error:
        print(f"thermal model verification failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
