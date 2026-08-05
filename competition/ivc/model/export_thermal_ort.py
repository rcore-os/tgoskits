#!/usr/bin/env python3
"""Export and verify the frozen thermal controller in ORT format."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import struct
import sys
import tempfile
from pathlib import Path
from typing import Any, Sequence


SCRIPT_PATH = Path(__file__).resolve()
MODEL_DIR = SCRIPT_PATH.parent
MODEL_ID = "thermal-4x6x1-v1"
ONNX_PATH = MODEL_DIR / f"{MODEL_ID}.onnx"
GOLDEN_PATH = MODEL_DIR / "golden-vectors.json"
REQUIREMENTS_LOCK_PATH = MODEL_DIR / "requirements-ort-lock.txt"
RUNTIME_SOURCE_PATH = MODEL_DIR / "onnxruntime-1.25.0-source.json"
ORT_NAME = f"{MODEL_ID}.ort"
CONFIG_NAME = f"{MODEL_ID}.required_operators_and_types.config"
REPORT_NAME = "ort-conversion-report.json"
EXPECTED_PYTHON = "3.12.11"
EXPECTED_PACKAGE_VERSIONS = {
    "flatbuffers": "25.12.19",
    "numpy": "1.26.4",
    "onnx": "1.16.1",
    "onnxruntime": "1.25.0",
    "packaging": "26.2",
    "protobuf": "4.25.4",
}
EXPECTED_ONNX_SHA256 = (
    "b21b3441908040c796a7528e6d941ae1f15d0b6fad8362a9d6492cf5b479b5b6"
)
EXPECTED_GOLDEN_SHA256 = (
    "f7255de6c730924781bd3703ecac717bd222fa9ed8488e52dd87e20204278ef6"
)
CANONICAL_ORT_SHA256 = (
    "3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887"
)
AUDITED_ORT_VARIANT_SHA256 = frozenset(
    {
        CANONICAL_ORT_SHA256,
        "63ccf6753965138723ea88b5e801754c30f5e398567081e0ff9580f57da92ebf",
    }
)
EXPECTED_OPERATOR_RECORDS = (
    'ai.onnx;13;Clip{"inputs": {"0": ["float"]}},Gemm{"inputs": {"0": ["float"]}}',
    "com.microsoft;1;FusedGemm",
)
EXPECTED_VECTORS = 10_000
ORT_MAX_ABSOLUTE_ERROR = 1e-6
ORT_ROUNDING_BOUNDARY_TOLERANCE = 1e-6


class OrtExportError(RuntimeError):
    """The ORT conversion environment or output violated the frozen contract."""


def normalize_operator_config(generated: str, expected_model_name: str) -> str:
    """Remove the temporary output directory from the generated comment."""
    lines = generated.splitlines()
    if len(lines) < 3 or lines[0] != "# Generated from model/s:":
        raise OrtExportError("operator config header is malformed")
    model_records = [line for line in lines if line.startswith("# - ")]
    if len(model_records) != 1:
        raise OrtExportError("operator config must identify exactly one model")
    generated_model = Path(model_records[0][4:]).name
    if generated_model != expected_model_name:
        raise OrtExportError("operator config model identity is unexpected")
    model_record_index = lines.index(model_records[0])
    lines[model_record_index] = f"# - {expected_model_name}"
    return "\n".join(lines) + "\n"


def require_audited_ort_variant(digest: str) -> None:
    """Reject new serializer byte layouts until they receive a semantic audit."""
    if digest not in AUDITED_ORT_VARIANT_SHA256:
        raise OrtExportError(f"ORT converter emitted an unaudited byte variant: {digest}")


def canonical_write_source(
    regenerated_digest: str,
    existing_digest: str | None,
) -> str:
    """Select bytes without replacing the canonical artifact by an alternate layout."""
    require_audited_ort_variant(regenerated_digest)
    if regenerated_digest == CANONICAL_ORT_SHA256:
        return "regenerated"
    if existing_digest == CANONICAL_ORT_SHA256:
        return "existing"
    raise OrtExportError(
        "fresh conversion produced an audited alternate layout and no existing "
        "canonical artifact is available"
    )


def validate_operator_config(config_bytes: bytes) -> list[str]:
    operators = [
        line
        for line in config_bytes.decode().splitlines()
        if line and not line.startswith("#")
    ]
    if tuple(operators) != EXPECTED_OPERATOR_RECORDS:
        raise OrtExportError(f"required operator contract changed: {operators}")
    return operators


def load_toolchain() -> tuple[Any, Any, Any]:
    actual_python = platform.python_version()
    if actual_python != EXPECTED_PYTHON:
        raise OrtExportError(
            f"Python {EXPECTED_PYTHON} is required, got {actual_python}"
        )
    for package, expected in EXPECTED_PACKAGE_VERSIONS.items():
        try:
            actual = importlib.metadata.version(package)
        except importlib.metadata.PackageNotFoundError as error:
            raise OrtExportError(f"required package is not installed: {package}") from error
        if actual != expected:
            raise OrtExportError(f"{package} {expected} is required, got {actual}")
    if os.environ.get("ORT_CONVERT_ONNX_MODELS_TO_ORT_OPTIMIZATION_LEVEL", "all") != "all":
        raise OrtExportError("ORT conversion optimization level must remain 'all'")
    if sha256_bytes(ONNX_PATH.read_bytes()) != EXPECTED_ONNX_SHA256:
        raise OrtExportError("frozen ONNX SHA-256 differs from the conversion contract")
    if sha256_bytes(GOLDEN_PATH.read_bytes()) != EXPECTED_GOLDEN_SHA256:
        raise OrtExportError("frozen golden corpus differs from the conversion contract")
    if not REQUIREMENTS_LOCK_PATH.is_file():
        raise OrtExportError(f"requirements lock is missing: {REQUIREMENTS_LOCK_PATH}")
    try:
        import numpy as np
        import onnxruntime as ort
        import onnxruntime.tools.convert_onnx_models_to_ort as converter
    except ImportError as error:
        raise OrtExportError("ONNX Runtime conversion modules are unavailable") from error
    return converter, ort, np


def load_vectors() -> list[dict[str, Any]]:
    document = json.loads(GOLDEN_PATH.read_text(encoding="utf-8"))
    if document.get("model_id") != MODEL_ID:
        raise OrtExportError("golden corpus model identity differs")
    vectors = document.get("vectors")
    if not isinstance(vectors, list) or len(vectors) != EXPECTED_VECTORS:
        raise OrtExportError(
            f"expected {EXPECTED_VECTORS} golden vectors, got "
            f"{len(vectors) if isinstance(vectors, list) else 'invalid document'}"
        )
    return vectors


def verify_ort_path(path: Path, ort: Any, np: Any) -> dict[str, Any]:
    """Run the complete frozen corpus through one CPU EP ORT artifact."""
    options = ort.SessionOptions()
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    options.add_session_config_entry("session.intra_op.allow_spinning", "0")
    options.add_session_config_entry("session.inter_op.allow_spinning", "0")
    try:
        session = ort.InferenceSession(
            str(path),
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
    except Exception as error:
        raise OrtExportError(f"cannot load ORT artifact {path}: {error}") from error
    if session.get_providers() != ["CPUExecutionProvider"]:
        raise OrtExportError(f"unexpected execution providers: {session.get_providers()}")
    inputs = session.get_inputs()
    outputs = session.get_outputs()
    if len(inputs) != 1 or (
        inputs[0].name,
        inputs[0].shape,
        inputs[0].type,
    ) != ("normalized_observation", [1, 4], "tensor(float)"):
        raise OrtExportError("ORT input contract differs")
    if len(outputs) != 1 or (
        outputs[0].name,
        outputs[0].shape,
        outputs[0].type,
    ) != ("control_fraction", [1, 1], "tensor(float)"):
        raise OrtExportError("ORT output contract differs")

    maximum_error = 0.0
    exact_command_matches = 0
    rounding_boundary_equivalences: list[dict[str, Any]] = []
    material_command_mismatches: list[dict[str, Any]] = []
    output_digest = hashlib.sha256()
    vectors = load_vectors()
    for index, vector in enumerate(vectors):
        input_bits = np.asarray(
            [int(value, 16) for value in vector["normalized_input_f32_bits"]],
            dtype=np.uint32,
        )
        model_input = input_bits.view(np.float32).reshape(1, 4)
        try:
            output = float(
                session.run(
                    ["control_fraction"],
                    {"normalized_observation": model_input},
                )[0][0, 0]
            )
        except Exception as error:
            raise OrtExportError(f"ORT inference {index} failed: {error}") from error
        if not math.isfinite(output):
            raise OrtExportError(f"ORT output {index} is not finite")
        output_digest.update(struct.pack("<f", output))
        expected_output = float(vector["output"])
        maximum_error = max(maximum_error, abs(output - expected_output))
        command = int(
            float(np.float32(np.float32(output) * np.float32(1000.0)) + np.float32(0.5))
        )
        expected_command = vector["actuator_permille"]
        if command == expected_command:
            exact_command_matches += 1
            continue
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
    if maximum_error > ORT_MAX_ABSOLUTE_ERROR:
        raise OrtExportError(f"ORT maximum error {maximum_error} exceeds the gate")
    if material_command_mismatches:
        raise OrtExportError(
            "ORT produced material actuator mismatches: "
            f"{material_command_mismatches[:5]}"
        )
    return {
        "backend": "onnxruntime-cpu",
        "exact_actuator_matches": exact_command_matches,
        "material_actuator_mismatches": len(material_command_mismatches),
        "maximum_absolute_error": maximum_error,
        "output_f32le_sha256": output_digest.hexdigest(),
        "providers": session.get_providers(),
        "rounding_boundary_equivalences": len(rounding_boundary_equivalences),
        "rounding_boundary_examples": rounding_boundary_equivalences[:5],
        "rounding_boundary_tolerance": ORT_ROUNDING_BOUNDARY_TOLERANCE,
        "vectors": len(vectors),
    }


def export_artifacts(converter: Any, ort: Any, np: Any) -> tuple[bytes, bytes, dict[str, Any]]:
    """Run the pinned converter and verify the regenerated model semantically."""
    with tempfile.TemporaryDirectory(prefix="ivc-thermal-ort-") as temporary:
        output_dir = Path(temporary)
        try:
            converter.convert_onnx_models_to_ort(
                ONNX_PATH,
                output_dir=output_dir,
                optimization_styles=[converter.OptimizationStyle.Fixed],
                target_platform="arm",
                enable_type_reduction=True,
            )
        except Exception as error:
            raise OrtExportError(f"ONNX Runtime conversion failed: {error}") from error
        ort_path = output_dir / ORT_NAME
        config_path = output_dir / CONFIG_NAME
        if not ort_path.is_file() or not config_path.is_file():
            raise OrtExportError("ONNX Runtime converter did not create both artifacts")
        ort_bytes = ort_path.read_bytes()
        require_audited_ort_variant(sha256_bytes(ort_bytes))
        normalized_config = normalize_operator_config(
            config_path.read_text(encoding="utf-8"),
            ORT_NAME,
        ).encode()
        validate_operator_config(normalized_config)
        verification = verify_ort_path(ort_path, ort, np)
        return ort_bytes, normalized_config, verification


def build_report(
    ort_bytes: bytes,
    config_bytes: bytes,
    verification: dict[str, Any],
) -> dict[str, Any]:
    """Describe the frozen conversion inputs, options, outputs, and gates."""
    return {
        "schema_version": 2,
        "status": "pass",
        "model_id": MODEL_ID,
        "source": file_record(
            ONNX_PATH.read_bytes(),
            f"competition/ivc/model/{ONNX_PATH.name}",
        ),
        "artifacts": {
            "ort": file_record(ort_bytes, f"competition/ivc/model/{ORT_NAME}"),
            "required_operators_and_types": file_record(
                config_bytes,
                f"competition/ivc/model/{CONFIG_NAME}",
            ),
        },
        "conversion": {
            "enable_type_reduction": True,
            "operators": validate_operator_config(config_bytes),
            "optimization_level": "all",
            "optimization_style": "Fixed",
            "target_platform": "arm",
        },
        "byte_reproducibility": {
            "canonical_artifact_sha256": CANONICAL_ORT_SHA256,
            "observed_semantically_equivalent_sha256": sorted(
                AUDITED_ORT_VARIANT_SHA256
            ),
            "status": "two-upstream-serializer-layouts-observed",
            "validation_policy": (
                "freeze the canonical bytes; accept only audited regenerated layouts "
                "after the normalized operator and 10000-vector semantic gates pass"
            ),
        },
        "verification": verification,
        "toolchain": {
            "host": "WSL2 Ubuntu 22.04 x86_64",
            "packages": dict(sorted(EXPECTED_PACKAGE_VERSIONS.items())),
            "python": EXPECTED_PYTHON,
            "requirements_lock": file_record(
                REQUIREMENTS_LOCK_PATH.read_bytes(),
                "competition/ivc/model/requirements-ort-lock.txt",
            ),
        },
        "target_runtime": {
            "source": file_record(
                RUNTIME_SOURCE_PATH.read_bytes(),
                "competition/ivc/model/onnxruntime-1.25.0-source.json",
            ),
            "status": "pending-starry-physical-gate",
        },
        "exporter": file_record(
            SCRIPT_PATH.read_bytes(),
            "competition/ivc/model/export_thermal_ort.py",
        ),
    }


def read_required(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError as error:
        raise OrtExportError(f"cannot read committed artifact {path}: {error}") from error


def check_artifacts(
    output_dir: Path,
    regenerated_ort: bytes,
    regenerated_config: bytes,
    regenerated_verification: dict[str, Any],
    ort: Any,
    np: Any,
) -> bytes:
    """Check immutable bytes plus semantic equivalence of a fresh conversion."""
    committed_ort_path = output_dir / ORT_NAME
    committed_ort = read_required(committed_ort_path)
    committed_digest = sha256_bytes(committed_ort)
    if committed_digest != CANONICAL_ORT_SHA256:
        raise OrtExportError(
            f"committed ORT artifact is not canonical: {committed_digest}"
        )
    committed_config = read_required(output_dir / CONFIG_NAME)
    if committed_config != regenerated_config:
        raise OrtExportError("committed operator config is stale")
    validate_operator_config(committed_config)
    require_audited_ort_variant(sha256_bytes(regenerated_ort))
    committed_verification = verify_ort_path(committed_ort_path, ort, np)
    if committed_verification != regenerated_verification:
        raise OrtExportError("regenerated ORT semantics differ from the canonical artifact")
    expected_report = encode_json(
        build_report(committed_ort, committed_config, committed_verification)
    )
    if read_required(output_dir / REPORT_NAME) != expected_report:
        raise OrtExportError("committed ORT conversion report is stale")
    return committed_ort


def write_artifacts(
    output_dir: Path,
    ort_bytes: bytes,
    config_bytes: bytes,
    verification: dict[str, Any],
    ort: Any,
    np: Any,
) -> None:
    digest = sha256_bytes(ort_bytes)
    existing_path = output_dir / ORT_NAME
    existing_bytes = existing_path.read_bytes() if existing_path.is_file() else None
    existing_digest = sha256_bytes(existing_bytes) if existing_bytes is not None else None
    source = canonical_write_source(digest, existing_digest)
    selected_ort = ort_bytes if source == "regenerated" else existing_bytes
    if selected_ort is None:
        raise OrtExportError("canonical ORT artifact selection failed")
    if source == "existing":
        existing_verification = verify_ort_path(existing_path, ort, np)
        if existing_verification != verification:
            raise OrtExportError(
                "regenerated ORT semantics differ from the existing canonical artifact"
            )
    report = build_report(selected_ort, config_bytes, verification)
    output_dir.mkdir(parents=True, exist_ok=True)
    for path, contents in {
        output_dir / ORT_NAME: selected_ort,
        output_dir / CONFIG_NAME: config_bytes,
        output_dir / REPORT_NAME: encode_json(report),
    }.items():
        path.write_bytes(contents)


def file_record(contents: bytes, path: str) -> dict[str, object]:
    return {
        "path": path,
        "sha256": sha256_bytes(contents),
        "size_bytes": len(contents),
    }


def sha256_bytes(contents: bytes) -> str:
    return hashlib.sha256(contents).hexdigest()


def encode_json(value: object) -> bytes:
    return (
        json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n"
    ).encode()


def parse_arguments(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=MODEL_DIR,
        help="directory for the .ort, operator config, and conversion report",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify frozen bytes and a fresh conversion through semantic gates",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    converter, ort, np = load_toolchain()
    ort_bytes, config_bytes, verification = export_artifacts(converter, ort, np)
    regenerated_digest = sha256_bytes(ort_bytes)
    if arguments.check:
        canonical_bytes = check_artifacts(
            arguments.output_dir,
            ort_bytes,
            config_bytes,
            verification,
            ort,
            np,
        )
    else:
        write_artifacts(
            arguments.output_dir,
            ort_bytes,
            config_bytes,
            verification,
            ort,
            np,
        )
        canonical_bytes = read_required(arguments.output_dir / ORT_NAME)
    print(
        "THERMAL_ORT_EXPORT_PASS "
        + json.dumps(
            {
                "canonical_ort_sha256": sha256_bytes(canonical_bytes),
                "check": arguments.check,
                "config_sha256": sha256_bytes(config_bytes),
                "onnxruntime": EXPECTED_PACKAGE_VERSIONS["onnxruntime"],
                "regenerated_ort_sha256": regenerated_digest,
                "vectors": verification["vectors"],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (json.JSONDecodeError, OSError, OrtExportError, UnicodeDecodeError) as error:
        print(f"thermal ORT export failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
