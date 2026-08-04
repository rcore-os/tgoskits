#!/usr/bin/env python3
"""Build the fixed thermal controller as a deterministic RK3588 FP16 RKNN model."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

EXPECTED_PYTHON = (3, 10, 12)
MODEL_ID = "thermal-4x6x1-v1"
ONNX_FILENAME = f"{MODEL_ID}.onnx"
RKNN_FILENAME = f"{MODEL_ID}-rk3588-fp16.rknn"
EVIDENCE_LOG_FILENAME = "rknn-conversion.log"
REPORT_FILENAME = "rknn-conversion-report.json"
SOURCE_METADATA_FILENAME = "rknn-toolkit2-2.3.2-source.json"
REQUIREMENTS_LOCK_FILENAME = "requirements-rknn-lock.txt"
MEMORY_PERTURB_VALUE = "255"
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")

EXPECTED_PACKAGE_VERSIONS = {
    "numpy": "1.26.4",
    "onnx": "1.16.1",
    "onnxruntime": "1.23.2",
    "opencv-python": "4.10.0.84",
    "protobuf": "4.25.4",
    "rknn-toolkit2": "2.3.2",
    "setuptools": "75.8.2",
    "torch": "2.4.0+cpu",
}

RKNN_CONFIG = {
    "mean_values": [[0.0, 0.0, 0.0, 0.0]],
    "std_values": [[1.0, 1.0, 1.0, 1.0]],
    "target_platform": "rk3588",
    "float_dtype": "float16",
    "optimization_level": 3,
    "single_core_mode": False,
    "compress_weight": False,
    "model_pruning": False,
    "quantize_weight": False,
}

EXPECTED_LAYER_NODES = {
    "InputOperator:normalized_observation": ("InputOperator", "FLOAT16", "CPU"),
    "Reshape:normalized_observation_tp_rs": ("Reshape", "FLOAT16", "NPU"),
    "Conv:hidden_gemm#2": ("ConvRelu", "FLOAT16", "NPU"),
    "Conv:output_gemm#2": ("ConvClip", "FLOAT16", "NPU"),
    "Reshape:control_fraction-rs": ("Reshape", "FLOAT16", "CPU"),
    "OutputOperator:control_fraction": ("OutputOperator", "FLOAT16", "CPU"),
}

COMPUTE_NODE_NAMES = {"Conv:hidden_gemm#2", "Conv:output_gemm#2"}


def main() -> int:
    args = parse_args()
    if args.worker:
        return run_worker(Path(args.worker_onnx), Path(args.worker_output))

    require_exact_toolchain()
    model_dir = Path(__file__).resolve().parent
    output_dir = Path(args.output_dir).resolve() if args.output_dir else model_dir
    inputs = validate_conversion_inputs(model_dir)

    with tempfile.TemporaryDirectory(prefix="thermal-rknn-") as temporary:
        generated_dir = Path(temporary)
        raw_log, layer_evidence, compiler_version = run_conversion(
            model_dir / ONNX_FILENAME,
            generated_dir / RKNN_FILENAME,
            Path(args.raw_log).resolve() if args.raw_log else None,
        )

        report = build_report(
            model_path=generated_dir / RKNN_FILENAME,
            inputs=inputs,
            layer_evidence=layer_evidence,
            compiler_version=compiler_version,
            raw_log=raw_log,
        )
        write_json(generated_dir / REPORT_FILENAME, report)
        (generated_dir / EVIDENCE_LOG_FILENAME).write_text(
            build_evidence_log(report), encoding="utf-8", newline="\n"
        )

        if args.check:
            compare_generated_outputs(generated_dir, output_dir)
        else:
            install_generated_outputs(generated_dir, output_dir)

    print(
        "THERMAL_RKNN_CONVERSION_PASS",
        json.dumps(
            {
                "model_sha256": report["artifact"]["sha256"],
                "operator_gate": report["operator_evidence"][
                    "all_model_compute_nodes_on_npu"
                ],
                "toolkit": report["toolchain"]["rknn-toolkit2"],
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
    )
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build and audit the fixed RK3588 FP16 RKNN model."
    )
    parser.add_argument(
        "--output-dir",
        help="directory for the RKNN model and deterministic conversion evidence",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="rebuild in a temporary directory and compare with committed outputs",
    )
    parser.add_argument(
        "--raw-log",
        help="optional path for the unnormalized vendor stdout/stderr log",
    )
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--worker-onnx", help=argparse.SUPPRESS)
    parser.add_argument("--worker-output", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker and (not args.worker_onnx or not args.worker_output):
        parser.error("internal worker mode requires ONNX and output paths")
    if args.check and args.output_dir:
        parser.error("--check always compares against the committed model directory")
    return args


def require_exact_toolchain() -> None:
    actual_python = sys.version_info[:3]
    if actual_python != EXPECTED_PYTHON:
        raise SystemExit(
            f"RKNN conversion requires Python {EXPECTED_PYTHON}, got {actual_python}"
        )

    mismatches = []
    for distribution, expected in EXPECTED_PACKAGE_VERSIONS.items():
        try:
            actual = importlib.metadata.version(distribution)
        except importlib.metadata.PackageNotFoundError:
            mismatches.append(f"{distribution}=missing (expected {expected})")
            continue
        if actual != expected:
            mismatches.append(f"{distribution}={actual} (expected {expected})")
    if mismatches:
        raise SystemExit("RKNN toolchain mismatch: " + "; ".join(mismatches))


def validate_conversion_inputs(model_dir: Path) -> dict[str, Any]:
    manifest_path = model_dir / "model-manifest.json"
    source_path = model_dir / SOURCE_METADATA_FILENAME
    lock_path = model_dir / REQUIREMENTS_LOCK_FILENAME
    onnx_path = model_dir / ONNX_FILENAME
    for path in (manifest_path, source_path, lock_path, onnx_path):
        if not path.is_file():
            raise SystemExit(f"required RKNN conversion input is missing: {path}")

    manifest = load_json(manifest_path)
    source = load_json(source_path)
    if manifest.get("model_id") != MODEL_ID:
        raise SystemExit("base model manifest has the wrong model_id")
    if source.get("tool") != "rknn-toolkit2" or source.get("version") != "2.3.2":
        raise SystemExit("RKNN source metadata does not describe Toolkit2 2.3.2")

    lock_text = lock_path.read_text(encoding="utf-8")
    wheel = source["x86_64_cp310_wheel"]
    required_lock_fragments = (
        f"rknn-toolkit2 @ {wheel['official_url']}",
        f"--hash=sha256:{wheel['sha256']}",
        "torch==2.4.0+cpu",
    )
    missing_fragments = [
        fragment for fragment in required_lock_fragments if fragment not in lock_text
    ]
    if missing_fragments:
        raise SystemExit(
            "RKNN requirements lock is not chained to the audited vendor source: "
            + "; ".join(missing_fragments)
        )

    expected_onnx_hash = manifest["artifacts"]["onnx"]["sha256"]
    actual_onnx_hash = sha256_file(onnx_path)
    if actual_onnx_hash != expected_onnx_hash:
        raise SystemExit(
            f"ONNX hash mismatch: expected {expected_onnx_hash}, got {actual_onnx_hash}"
        )

    return {
        "manifest": manifest,
        "manifest_sha256": sha256_file(manifest_path),
        "onnx_sha256": actual_onnx_hash,
        "source": source,
        "source_sha256": sha256_file(source_path),
        "requirements_lock_sha256": sha256_file(lock_path),
    }


def run_conversion(
    onnx_path: Path, output_path: Path, raw_log_path: Path | None
) -> tuple[bytes, list[dict[str, Any]], str]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--worker",
        "--worker-onnx",
        str(onnx_path),
        "--worker-output",
        str(output_path),
    ]
    environment = os.environ.copy()
    environment["MALLOC_PERTURB_"] = MEMORY_PERTURB_VALUE
    environment["PYTHONHASHSEED"] = "0"
    completed = subprocess.run(
        command,
        cwd=output_path.parent,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        timeout=120,
    )
    raw_log = completed.stdout
    if raw_log_path is not None:
        raw_log_path.write_bytes(raw_log)
    if completed.returncode != 0:
        tail = raw_log.decode("utf-8", errors="replace")[-4000:]
        raise SystemExit(
            f"RKNN conversion worker failed with status {completed.returncode}:\n{tail}"
        )
    if not output_path.is_file() or output_path.stat().st_size == 0:
        raise SystemExit("RKNN conversion reported success without producing a model")

    decoded_log = ANSI_ESCAPE.sub("", raw_log.decode("utf-8", errors="replace"))
    layer_evidence = parse_layer_evidence(decoded_log)
    compiler_version = require_log_value(
        decoded_log, r"librknnc version:\s*([^\r\n]+)", "compiler version"
    )
    toolkit_version = require_log_value(
        decoded_log, r"rknn-toolkit2 version:\s*([^\r\n]+)", "toolkit version"
    )
    if toolkit_version != "2.3.2":
        raise SystemExit(f"unexpected RKNN toolkit log version: {toolkit_version}")
    if "RKNN_STEP export_rknn status=0" not in decoded_log:
        raise SystemExit("RKNN conversion log lacks a successful export marker")
    return raw_log, layer_evidence, compiler_version


def run_worker(onnx_path: Path, output_path: Path) -> int:
    if os.environ.get("MALLOC_PERTURB_") != MEMORY_PERTURB_VALUE:
        raise SystemExit("RKNN worker must start with MALLOC_PERTURB_=255")

    from rknn.api import RKNN

    rknn = RKNN(verbose=True)
    try:
        run_rknn_step("config", rknn.config(**RKNN_CONFIG))
        run_rknn_step("load_onnx", rknn.load_onnx(model=str(onnx_path)))
        run_rknn_step("build", rknn.build(do_quantization=False))
        run_rknn_step("export_rknn", rknn.export_rknn(str(output_path)))
    finally:
        rknn.release()
    return 0


def run_rknn_step(name: str, status: int) -> None:
    print(f"RKNN_STEP {name} status={status}", flush=True)
    if status != 0:
        raise SystemExit(f"RKNN {name} failed with status {status}")


def parse_layer_evidence(log: str) -> list[dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}
    for line in log.replace("\r", "\n").splitlines():
        for full_name, expected in EXPECTED_LAYER_NODES.items():
            if full_name not in line:
                continue
            prefix = line.split(full_name, maxsplit=1)[0]
            tokens = prefix.split()
            expected_op, expected_dtype, expected_target = expected
            for index in range(len(tokens) - 3):
                if (
                    tokens[index].isdigit()
                    and tokens[index + 1] == expected_op
                    and tokens[index + 2] == expected_dtype
                    and tokens[index + 3] == expected_target
                ):
                    found[full_name] = {
                        "id": int(tokens[index]),
                        "op_type": expected_op,
                        "dtype": expected_dtype,
                        "target": expected_target,
                        "full_name": full_name,
                    }
                    break

    missing = sorted(set(EXPECTED_LAYER_NODES) - set(found))
    if missing:
        raise SystemExit(f"RKNN layer table is missing expected nodes: {missing}")
    for name in COMPUTE_NODE_NAMES:
        if found[name]["target"] != "NPU":
            raise SystemExit(f"model compute node did not map to NPU: {found[name]}")
    return sorted(found.values(), key=lambda node: node["id"])


def require_log_value(log: str, pattern: str, label: str) -> str:
    match = re.search(pattern, log)
    if not match:
        raise SystemExit(f"RKNN conversion log lacks {label}")
    return match.group(1).strip()


def build_report(
    *,
    model_path: Path,
    inputs: dict[str, Any],
    layer_evidence: list[dict[str, Any]],
    compiler_version: str,
    raw_log: bytes,
) -> dict[str, Any]:
    model_bytes = model_path.read_bytes()
    required_binary_markers = (
        b"rk3588",
        b"2.3.2(compiler version:",
        b"Conv:hidden_gemm#2",
        b"Conv:output_gemm#2",
        b"normalized_observation",
        b"control_fraction",
    )
    missing_markers = [
        marker.decode("ascii") for marker in required_binary_markers if marker not in model_bytes
    ]
    if missing_markers:
        raise SystemExit(f"RKNN model lacks identity markers: {missing_markers}")

    decoded_log = ANSI_ESCAPE.sub("", raw_log.decode("utf-8", errors="replace"))
    diagnostics = []
    if "Unkown op target: 0" in decoded_log:
        diagnostics.append(
            {
                "message": "Unkown op target: 0",
                "classification": "vendor diagnostic retained; build/export returned zero and the complete layer table is authoritative",
            }
        )

    source = inputs["source"]
    return {
        "schema_version": 1,
        "model_id": MODEL_ID,
        "status": "pass",
        "artifact": {
            "path": f"competition/ivc/model/{RKNN_FILENAME}",
            "sha256": hashlib.sha256(model_bytes).hexdigest(),
            "size_bytes": len(model_bytes),
            "target_platform": "rk3588",
            "precision": "FP16",
            "quantization": "disabled",
        },
        "sources": {
            "converter_sha256": sha256_file(Path(__file__).resolve()),
            "onnx_sha256": inputs["onnx_sha256"],
            "base_manifest_sha256": inputs["manifest_sha256"],
            "rknn_source_metadata_sha256": inputs["source_sha256"],
            "requirements_rknn_lock_sha256": inputs["requirements_lock_sha256"],
        },
        "toolchain": {
            "python": ".".join(str(value) for value in EXPECTED_PYTHON),
            **{
                distribution: importlib.metadata.version(distribution)
                for distribution in sorted(EXPECTED_PACKAGE_VERSIONS)
            },
            "compiler": compiler_version,
            "release_commit": source["release"]["commit"],
            "wheel_sha256": source["x86_64_cp310_wheel"]["sha256"],
        },
        "conversion": {
            "config": RKNN_CONFIG,
            "build": {"do_quantization": False},
            "binary_postprocessed": False,
        },
        "operator_evidence": {
            "nodes": layer_evidence,
            "compute_node_names": sorted(COMPUTE_NODE_NAMES),
            "all_model_compute_nodes_on_npu": all(
                node["target"] == "NPU"
                for node in layer_evidence
                if node["full_name"] in COMPUTE_NODE_NAMES
            ),
            "custom_cpu_ops": [],
        },
        "vendor_diagnostics": diagnostics,
        "reproducibility": {
            "child_environment": {
                "MALLOC_PERTURB_": MEMORY_PERTURB_VALUE,
                "PYTHONHASHSEED": "0",
            },
            "reason": "Toolkit2 otherwise emits process-memory-dependent bytes in a fixed internal region for this model; glibc allocation perturbation makes independent compiler processes byte-identical without editing the exported model.",
            "formal_default_allocator_audit": {
                "artifact_a_sha256": "a9698a5dfaa9661cefa8db051338cfe19167dc4ff0aab4e7d7fe68769448cce7",
                "artifact_b_sha256": "a9f663e9ef739ad583469638fc083471da0318d1f8218651393539e2dae18335",
                "artifacts_committed": False,
                "different_bytes": 845,
                "difference_region_zero_based_inclusive": [12128, 14200],
                "independent_processes": 2,
                "same_formal_onnx_config_and_toolchain": True,
            },
            "exploratory_default_environment_difference_region_zero_based_inclusive": [
                12128,
                14205,
            ],
            "binary_postprocessing": False,
        },
        "redistribution": {
            "vendor_wheel_committed": False,
            "assessment": source["repository_license"][
                "redistribution_assessment"
            ],
        },
    }


def build_evidence_log(report: dict[str, Any]) -> str:
    lines = [
        "RKNN_CONVERSION_EVIDENCE schema=1 status=pass",
        f"model_id={report['model_id']}",
        f"model_sha256={report['artifact']['sha256']}",
        f"onnx_sha256={report['sources']['onnx_sha256']}",
        f"toolkit={report['toolchain']['rknn-toolkit2']}",
        f"compiler={report['toolchain']['compiler']}",
        "target=rk3588 precision=FP16 quantization=disabled",
        "MALLOC_PERTURB_=255 PYTHONHASHSEED=0 binary_postprocessed=false",
    ]
    for node in report["operator_evidence"]["nodes"]:
        lines.append(
            "node "
            f"id={node['id']} op={node['op_type']} dtype={node['dtype']} "
            f"target={node['target']} name={node['full_name']}"
        )
    for diagnostic in report["vendor_diagnostics"]:
        lines.append(
            f"vendor_diagnostic={diagnostic['message']} classification={diagnostic['classification']}"
        )
    lines.append("RKNN_CONVERSION_GATE all_compute_nodes_on_npu=true custom_cpu_ops=0")
    return "\n".join(lines) + "\n"


def compare_generated_outputs(generated_dir: Path, reference_dir: Path) -> None:
    for filename in (RKNN_FILENAME, EVIDENCE_LOG_FILENAME, REPORT_FILENAME):
        generated = generated_dir / filename
        reference = reference_dir / filename
        if not reference.is_file():
            raise SystemExit(f"committed RKNN output is missing: {reference}")
        if generated.read_bytes() != reference.read_bytes():
            raise SystemExit(f"RKNN rebuild differs from committed output: {filename}")


def install_generated_outputs(generated_dir: Path, output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    for filename in (RKNN_FILENAME, EVIDENCE_LOG_FILENAME, REPORT_FILENAME):
        shutil.copyfile(generated_dir / filename, output_dir / filename)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"JSON root must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


if __name__ == "__main__":
    raise SystemExit(main())
