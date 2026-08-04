#!/usr/bin/env python3
"""Build the deterministic fixed-weight thermal-controller artifacts."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import struct
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Sequence


SCRIPT_PATH = Path(__file__).resolve()
MODEL_DIR = SCRIPT_PATH.parent
WORKSPACE = MODEL_DIR.parents[2]
WEIGHTS_PATH = MODEL_DIR / "thermal-4x6x1-v1.weights.json"
REQUIREMENTS_LOCK_PATH = MODEL_DIR / "requirements-lock.txt"
DEFAULT_RUST_PATH = WORKSPACE / "tools/ivcproto/src/neural_model_generated.rs"
EXPECTED_PYTHON = "3.10.12"
EXPECTED_PACKAGES = {
    "numpy": "1.26.4",
    "onnx": "1.16.1",
    "protobuf": "4.25.4",
}
CORPUS_SIZE = 10_000
CORPUS_SEED = 0x5447_4F53
ONNX_OPSET = 13
ONNX_IR_VERSION = 8


class ModelSourceError(ValueError):
    """The canonical model source violates the frozen schema."""


@dataclass(frozen=True)
class Observation:
    label: str
    temperature_milli_c: int
    setpoint_milli_c: int
    previous_actuator_permille: int
    temperature_rate_milli_c_per_s: int


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def f32(value: float | int) -> float:
    return struct.unpack("<f", struct.pack("<f", float(value)))[0]


def f32_bits(value: float | int) -> int:
    return struct.unpack("<I", struct.pack("<f", f32(value)))[0]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ModelSourceError(message)


def require_keys(value: dict[str, Any], expected: set[str], context: str) -> None:
    actual = set(value)
    require(actual == expected, f"{context} keys: expected {sorted(expected)}, got {sorted(actual)}")


def require_number(value: Any, context: str) -> float:
    require(isinstance(value, (int, float)) and not isinstance(value, bool), f"{context} must be numeric")
    return f32(value)


def require_vector(value: Any, length: int, context: str) -> list[float]:
    require(isinstance(value, list) and len(value) == length, f"{context} must contain {length} values")
    return [require_number(item, f"{context}[{index}]") for index, item in enumerate(value)]


def require_matrix(value: Any, rows: int, columns: int, context: str) -> list[list[float]]:
    require(isinstance(value, list) and len(value) == rows, f"{context} must contain {rows} rows")
    return [require_vector(row, columns, f"{context}[{index}]") for index, row in enumerate(value)]


def load_and_validate_weights() -> tuple[dict[str, Any], bytes]:
    source_bytes = WEIGHTS_PATH.read_bytes()
    source = json.loads(source_bytes)
    require(isinstance(source, dict), "model source root must be an object")
    require_keys(
        source,
        {"schema_version", "model_id", "float_format", "graph", "layers", "normalization", "actuator"},
        "model source",
    )
    require(source["schema_version"] == 1, "schema_version must be 1")
    require(source["model_id"] == "thermal-4x6x1-v1", "unexpected model_id")
    require(source["float_format"].startswith("IEEE-754 binary32"), "float_format must freeze binary32")

    graph = source["graph"]
    require(isinstance(graph, dict), "graph must be an object")
    require_keys(
        graph,
        {
            "input_name",
            "input_shape",
            "hidden_activation",
            "output_activation",
            "output_name",
            "output_shape",
        },
        "graph",
    )
    require(graph["input_name"] == "normalized_observation", "unexpected input name")
    require(graph["input_shape"] == [1, 4], "input shape must be [1, 4]")
    require(graph["hidden_activation"] == "Relu", "hidden activation must be Relu")
    require(graph["output_name"] == "control_fraction", "unexpected output name")
    require(graph["output_shape"] == [1, 1], "output shape must be [1, 1]")
    output_activation = graph["output_activation"]
    require(
        output_activation == {"maximum": 1.0, "minimum": 0.0, "operator": "Clip"},
        "output activation must be Clip(0, 1)",
    )

    layers = source["layers"]
    require(isinstance(layers, list) and len(layers) == 2, "layers must contain hidden and output")
    hidden, output = layers
    for layer, name, inputs, outputs in ((hidden, "hidden", 4, 6), (output, "output", 6, 1)):
        require(isinstance(layer, dict), f"{name} layer must be an object")
        require_keys(
            layer,
            {"name", "input_features", "output_features", "weight_layout", "weights", "bias"},
            f"{name} layer",
        )
        require(layer["name"] == name, f"unexpected {name} layer name")
        require(layer["input_features"] == inputs, f"{name} input feature count changed")
        require(layer["output_features"] == outputs, f"{name} output feature count changed")
        require(
            layer["weight_layout"] == "output_features_by_input_features",
            f"{name} weight layout changed",
        )
        require_matrix(layer["weights"], outputs, inputs, f"{name}.weights")
        require_vector(layer["bias"], outputs, f"{name}.bias")

    normalization = source["normalization"]
    require(isinstance(normalization, dict), "normalization must be an object")
    require_keys(normalization, {"features", "raw_ranges"}, "normalization")
    features = normalization["features"]
    require(isinstance(features, list) and len(features) == 4, "normalization must contain four features")
    require([feature["name"] for feature in features] == [
        "temperature_error",
        "setpoint",
        "temperature_rate",
        "previous_actuator",
    ], "normalization feature order changed")
    require_number(features[0]["denominator"], "temperature_error denominator")
    require_number(features[1]["denominator"], "setpoint denominator")
    require(features[1]["offset_milli_c"] == 20_000, "setpoint offset changed")
    require_number(features[2]["denominator"], "temperature_rate denominator")
    require_number(features[3]["denominator"], "previous_actuator denominator")
    require(normalization["raw_ranges"] == {
        "previous_actuator_permille": [0, 1000],
        "setpoint_milli_c": [-40000, 150000],
        "temperature_milli_c": [-40000, 150000],
        "temperature_rate_milli_c_per_s": [-100000, 100000],
    }, "raw input ranges changed")

    actuator = source["actuator"]
    require(isinstance(actuator, dict), "actuator must be an object")
    require_keys(actuator, {"maximum_permille", "rounding", "rounding_half", "scale"}, "actuator")
    require(actuator["maximum_permille"] == 1000, "actuator maximum changed")
    require(actuator["rounding"] == "truncate(control_fraction * scale + half)", "rounding rule changed")
    require_number(actuator["rounding_half"], "rounding_half")
    require_number(actuator["scale"], "actuator scale")
    return source, source_bytes


def layer_values(source: dict[str, Any]) -> tuple[list[list[float]], list[float], list[float], float]:
    hidden, output = source["layers"]
    hidden_weights = [[f32(value) for value in row] for row in hidden["weights"]]
    hidden_bias = [f32(value) for value in hidden["bias"]]
    output_weights = [f32(value) for value in output["weights"][0]]
    output_bias = f32(output["bias"][0])
    return hidden_weights, hidden_bias, output_weights, output_bias


def normalization_values(source: dict[str, Any]) -> tuple[float, int, float, float, float]:
    features = source["normalization"]["features"]
    return (
        f32(features[0]["denominator"]),
        int(features[1]["offset_milli_c"]),
        f32(features[1]["denominator"]),
        f32(features[2]["denominator"]),
        f32(features[3]["denominator"]),
    )


def normalize(source: dict[str, Any], observation: Observation) -> list[float]:
    error_scale, setpoint_offset, setpoint_scale, rate_scale, actuator_scale = normalization_values(source)
    return [
        f32(f32(observation.setpoint_milli_c - observation.temperature_milli_c) / error_scale),
        f32(f32(observation.setpoint_milli_c - setpoint_offset) / setpoint_scale),
        f32(f32(observation.temperature_rate_milli_c_per_s) / rate_scale),
        f32(f32(observation.previous_actuator_permille) / actuator_scale),
    ]


def infer(source: dict[str, Any], inputs: Sequence[float]) -> float:
    hidden_weights, hidden_bias, output_weights, output_bias = layer_values(source)
    hidden_outputs: list[float] = []
    for weights, bias in zip(hidden_weights, hidden_bias, strict=True):
        total = bias
        for weight, input_value in zip(weights, inputs, strict=True):
            total = f32(total + f32(weight * f32(input_value)))
        hidden_outputs.append(f32(max(total, 0.0)))
    output = output_bias
    for weight, activation in zip(output_weights, hidden_outputs, strict=True):
        output = f32(output + f32(weight * activation))
    clip = source["graph"]["output_activation"]
    return f32(min(max(output, f32(clip["minimum"])), f32(clip["maximum"])))


def actuator_command(source: dict[str, Any], output: float) -> int:
    actuator = source["actuator"]
    scaled = f32(f32(output * f32(actuator["scale"])) + f32(actuator["rounding_half"]))
    command = int(scaled)
    require(0 <= command <= actuator["maximum_permille"], "generated actuator is out of range")
    return command


class Lcg:
    def __init__(self, seed: int) -> None:
        self.state = seed

    def next_u32(self) -> int:
        self.state = (1_664_525 * self.state + 1_013_904_223) & 0xFFFF_FFFF
        return self.state

    def inclusive(self, minimum: int, maximum: int) -> int:
        return minimum + self.next_u32() % (maximum - minimum + 1)


def curated_observations() -> list[Observation]:
    return [
        Observation("nominal", 40_000, 55_000, 400, 1_000),
        Observation("all-zero-normalized", 20_000, 20_000, 0, 0),
        Observation("temperature-minimum", -40_000, 20_000, 0, 0),
        Observation("temperature-maximum", 150_000, 20_000, 0, 0),
        Observation("setpoint-minimum", 20_000, -40_000, 0, 0),
        Observation("setpoint-maximum", 20_000, 150_000, 0, 0),
        Observation("rate-minimum", 20_000, 20_000, 0, -100_000),
        Observation("rate-maximum", 20_000, 20_000, 0, 100_000),
        Observation("actuator-maximum", 20_000, 20_000, 1_000, 0),
        Observation("positive-error", 25_000, 60_000, 250, 2_500),
        Observation("negative-error", 70_000, 45_000, 750, -2_500),
        Observation("clip-low", 150_000, -40_000, 0, 100_000),
        Observation("clip-high", -40_000, 150_000, 1_000, -100_000),
        Observation("cooling", 65_000, 50_000, 650, -12_345),
        Observation("heating", 35_000, 65_000, 350, 12_345),
        Observation("near-equilibrium", 49_999, 50_000, 500, 1),
    ]


def build_observations() -> list[Observation]:
    observations = curated_observations()
    generator = Lcg(CORPUS_SEED)
    while len(observations) < CORPUS_SIZE:
        index = len(observations) - len(curated_observations())
        observations.append(
            Observation(
                f"lcg-{index:05d}",
                generator.inclusive(-40_000, 150_000),
                generator.inclusive(-40_000, 150_000),
                generator.inclusive(0, 1_000),
                generator.inclusive(-100_000, 100_000),
            )
        )
    return observations


def build_golden(source: dict[str, Any], source_sha256: str) -> tuple[bytes, list[dict[str, Any]]]:
    vectors = []
    for observation in build_observations():
        inputs = normalize(source, observation)
        output = infer(source, inputs)
        vectors.append(
            {
                "actuator_permille": actuator_command(source, output),
                "label": observation.label,
                "normalized_input": inputs,
                "normalized_input_f32_bits": [f"{f32_bits(value):08x}" for value in inputs],
                "observation": {
                    "previous_actuator_permille": observation.previous_actuator_permille,
                    "setpoint_milli_c": observation.setpoint_milli_c,
                    "temperature_milli_c": observation.temperature_milli_c,
                    "temperature_rate_milli_c_per_s": observation.temperature_rate_milli_c_per_s,
                },
                "output": output,
                "output_f32_bits": f"{f32_bits(output):08x}",
            }
        )
    document = {
        "corpus": {
            "algorithm": "u32 LCG: state = (1664525 * state + 1013904223) mod 2^32; inclusive range uses state modulo width",
            "curated_prefix_count": len(curated_observations()),
            "seed_hex": f"0x{CORPUS_SEED:08x}",
            "total_vectors": len(vectors),
        },
        "model_id": source["model_id"],
        "schema_version": 1,
        "weights_sha256": source_sha256,
        "vectors": vectors,
    }
    return canonical_json(document), vectors


def rust_f32(value: float | int) -> str:
    return f"f32::from_bits(0x{f32_bits(value):08x})"


def rust_array(values: Iterable[float | int]) -> str:
    return "[" + ", ".join(rust_f32(value) for value in values) + "]"


def build_rust_source(
    source: dict[str, Any], source_sha256: str, vectors: list[dict[str, Any]]
) -> bytes:
    hidden_weights, hidden_bias, output_weights, output_bias = layer_values(source)
    error_scale, setpoint_offset, setpoint_scale, rate_scale, actuator_scale = normalization_values(source)
    clip = source["graph"]["output_activation"]
    actuator = source["actuator"]
    lines = [
        "// @generated by competition/ivc/model/export_thermal_onnx.py; do not edit.",
        f'pub const MODEL_ID: &str = "{source["model_id"]}";',
        "pub const MODEL_SOURCE_SHA256: &str =",
        f'    "{source_sha256}";',
        "pub(crate) const INPUTS: usize = 4;",
        "pub(crate) const HIDDEN: usize = 6;",
        "#[rustfmt::skip]",
        "pub(crate) const HIDDEN_WEIGHTS: [[f32; INPUTS]; HIDDEN] = [",
    ]
    lines.extend(f"    {rust_array(row)}," for row in hidden_weights)
    lines.extend(
        [
            "];",
            "#[rustfmt::skip]",
            f"pub(crate) const HIDDEN_BIASES: [f32; HIDDEN] = {rust_array(hidden_bias)};",
            "#[rustfmt::skip]",
            f"pub(crate) const OUTPUT_WEIGHTS: [f32; HIDDEN] = {rust_array(output_weights)};",
            f"pub(crate) const OUTPUT_BIAS: f32 = {rust_f32(output_bias)};",
            f"pub(crate) const OUTPUT_MIN: f32 = {rust_f32(clip['minimum'])};",
            f"pub(crate) const OUTPUT_MAX: f32 = {rust_f32(clip['maximum'])};",
            f"pub(crate) const ERROR_SCALE: f32 = {rust_f32(error_scale)};",
            f"pub(crate) const SETPOINT_OFFSET_MILLI_C: i32 = {setpoint_offset};",
            f"pub(crate) const SETPOINT_SCALE: f32 = {rust_f32(setpoint_scale)};",
            f"pub(crate) const RATE_SCALE: f32 = {rust_f32(rate_scale)};",
            f"pub(crate) const ACTUATOR_INPUT_SCALE: f32 = {rust_f32(actuator_scale)};",
            f"pub(crate) const ACTUATOR_OUTPUT_SCALE: f32 = {rust_f32(actuator['scale'])};",
            f"pub(crate) const ACTUATOR_ROUNDING_HALF: f32 = {rust_f32(actuator['rounding_half'])};",
            "",
            "#[cfg(test)]",
            "#[rustfmt::skip]",
            "pub(crate) const GOLDEN_CASES: [([u32; INPUTS], u32, u16); 32] = [",
        ]
    )
    for vector in vectors[:32]:
        input_bits = ", ".join(f"0x{value}" for value in vector["normalized_input_f32_bits"])
        lines.append(
            f"    ([{input_bits}], 0x{vector['output_f32_bits']}, {vector['actuator_permille']}),"
        )
    lines.extend([
        "];",
        "",
    ])
    return "\n".join(lines).encode()


def raw_float_tensor(onnx: Any, name: str, dimensions: Sequence[int], values: Iterable[float]) -> Any:
    tensor = onnx.TensorProto()
    tensor.name = name
    tensor.data_type = onnx.TensorProto.FLOAT
    tensor.dims.extend(dimensions)
    tensor.raw_data = b"".join(struct.pack("<f", f32(value)) for value in values)
    return tensor


def build_onnx(source: dict[str, Any], source_sha256: str) -> bytes:
    import onnx
    from onnx import checker, helper

    require(onnx.__version__ == EXPECTED_PACKAGES["onnx"], f"onnx must be {EXPECTED_PACKAGES['onnx']}")
    hidden_weights, hidden_bias, output_weights, output_bias = layer_values(source)
    graph_source = source["graph"]
    initializers = [
        raw_float_tensor(onnx, "hidden_weight", [6, 4], (value for row in hidden_weights for value in row)),
        raw_float_tensor(onnx, "hidden_bias", [6], hidden_bias),
        raw_float_tensor(onnx, "output_weight", [1, 6], output_weights),
        raw_float_tensor(onnx, "output_bias", [1], [output_bias]),
        raw_float_tensor(onnx, "clip_min", [], [graph_source["output_activation"]["minimum"]]),
        raw_float_tensor(onnx, "clip_max", [], [graph_source["output_activation"]["maximum"]]),
    ]
    nodes = [
        helper.make_node(
            "Gemm",
            [graph_source["input_name"], "hidden_weight", "hidden_bias"],
            ["hidden_affine"],
            name="hidden_gemm",
            alpha=1.0,
            beta=1.0,
            transB=1,
        ),
        helper.make_node("Relu", ["hidden_affine"], ["hidden_relu"], name="hidden_relu"),
        helper.make_node(
            "Gemm",
            ["hidden_relu", "output_weight", "output_bias"],
            ["output_affine"],
            name="output_gemm",
            alpha=1.0,
            beta=1.0,
            transB=1,
        ),
        helper.make_node(
            "Clip",
            ["output_affine", "clip_min", "clip_max"],
            [graph_source["output_name"]],
            name="output_clip",
        ),
    ]
    graph = helper.make_graph(
        nodes,
        source["model_id"],
        [helper.make_tensor_value_info(graph_source["input_name"], onnx.TensorProto.FLOAT, [1, 4])],
        [helper.make_tensor_value_info(graph_source["output_name"], onnx.TensorProto.FLOAT, [1, 1])],
        initializer=initializers,
    )
    model = helper.make_model(
        graph,
        producer_name="tgoskits-ivc-model-exporter",
        producer_version="1",
        domain="org.tgoskits.competition",
        model_version=1,
        doc_string="Deterministic fixed-weight thermal controller; no training pipeline.",
        opset_imports=[helper.make_operatorsetid("", ONNX_OPSET)],
    )
    model.ir_version = ONNX_IR_VERSION
    for key, value in (("model_id", source["model_id"]), ("weights_sha256", source_sha256)):
        metadata = model.metadata_props.add()
        metadata.key = key
        metadata.value = value
    checker.check_model(model, full_check=True)
    encoded = model.SerializeToString(deterministic=True)
    checker.check_model(onnx.load_model_from_string(encoded), full_check=True)
    return encoded


def package_versions() -> dict[str, str]:
    actual = {name: importlib.metadata.version(name) for name in EXPECTED_PACKAGES}
    for name, expected in EXPECTED_PACKAGES.items():
        require(actual[name] == expected, f"{name} must be {expected}, got {actual[name]}")
    python_version = ".".join(str(value) for value in sys.version_info[:3])
    require(python_version == EXPECTED_PYTHON, f"Python must be {EXPECTED_PYTHON}, got {python_version}")
    return actual


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def file_record(path: str, data: bytes) -> dict[str, Any]:
    return {"path": path, "sha256": sha256_bytes(data), "size_bytes": len(data)}


def build_manifest(
    source: dict[str, Any],
    source_bytes: bytes,
    rust_bytes: bytes,
    onnx_bytes: bytes,
    golden_bytes: bytes,
    versions: dict[str, str],
) -> bytes:
    lock_bytes = REQUIREMENTS_LOCK_PATH.read_bytes()
    script_bytes = SCRIPT_PATH.read_bytes()
    manifest = {
        "artifacts": {
            "golden_vectors": file_record("competition/ivc/model/golden-vectors.json", golden_bytes),
            "native_rust": file_record("tools/ivcproto/src/neural_model_generated.rs", rust_bytes),
            "onnx": {
                **file_record("competition/ivc/model/thermal-4x6x1-v1.onnx", onnx_bytes),
                "dtype": "float32",
                "ir_version": ONNX_IR_VERSION,
                "opset": ONNX_OPSET,
            },
            "ort": {"status": "pending-m4-plus"},
            "rknn": {"status": "pending-m4-2"},
        },
        "graph": {
            "input": {"dtype": "float32", "name": "normalized_observation", "shape": [1, 4]},
            "nodes": ["Gemm", "Relu", "Gemm", "Clip"],
            "output": {"dtype": "float32", "name": "control_fraction", "shape": [1, 1]},
        },
        "model_id": source["model_id"],
        "policy": {
            "quantization": "disabled",
            "training": "none",
            "weight_tuning_after_freeze": "forbidden",
        },
        "schema_version": 1,
        "sources": {
            "exporter": file_record("competition/ivc/model/export_thermal_onnx.py", script_bytes),
            "requirements_lock": file_record("competition/ivc/model/requirements-lock.txt", lock_bytes),
            "weights": file_record("competition/ivc/model/thermal-4x6x1-v1.weights.json", source_bytes),
        },
        "toolchain": {
            "build_host": "WSL2 Ubuntu 22.04 x86_64",
            "numpy": versions["numpy"],
            "onnx": versions["onnx"],
            "protobuf": versions["protobuf"],
            "python": EXPECTED_PYTHON,
            "rknn_toolkit2": {
                "candidate_version": "2.3.2",
                "source": "https://github.com/airockchip/rknn-toolkit2/releases/tag/v2.3.2",
                "status": "not-frozen-before-m4-2-abi-gate",
            },
        },
        "runtime_compatibility_target": {
            "librknnrt": {
                "build_id": "4f5001b81d147d0db1f48e68fe87a6029caa2ccb",
                "reported_version": "2.3.2 (429f97ae6b@2025-04-09T09:09:27)",
                "sha256": "d31fc19c85b85f6091b2bd0f6af9d962d5264a4e410bfb536402ec92bac738e8",
            },
            "rknpu_driver_abi": "0.9.8",
            "target_platform": "rk3588",
        },
    }
    return canonical_json(manifest)


def write_or_check(path: Path, expected: bytes, check: bool, mismatches: list[str]) -> None:
    if check:
        if not path.is_file():
            mismatches.append(f"missing: {path}")
        elif path.read_bytes() != expected:
            mismatches.append(f"stale: {path}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(expected)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify committed outputs without changing them")
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="write every generated artifact below this directory for an independent rebuild",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    source, source_bytes = load_and_validate_weights()
    versions = package_versions()
    source_sha256 = sha256_bytes(source_bytes)
    golden_bytes, vectors = build_golden(source, source_sha256)
    rust_bytes = build_rust_source(source, source_sha256, vectors)
    onnx_bytes = build_onnx(source, source_sha256)
    manifest_bytes = build_manifest(source, source_bytes, rust_bytes, onnx_bytes, golden_bytes, versions)

    if args.output_dir is None:
        outputs = {
            DEFAULT_RUST_PATH: rust_bytes,
            MODEL_DIR / "thermal-4x6x1-v1.onnx": onnx_bytes,
            MODEL_DIR / "golden-vectors.json": golden_bytes,
            MODEL_DIR / "model-manifest.json": manifest_bytes,
        }
    else:
        outputs = {
            args.output_dir / "neural_model_generated.rs": rust_bytes,
            args.output_dir / "thermal-4x6x1-v1.onnx": onnx_bytes,
            args.output_dir / "golden-vectors.json": golden_bytes,
            args.output_dir / "model-manifest.json": manifest_bytes,
        }

    mismatches: list[str] = []
    for path, data in outputs.items():
        write_or_check(path, data, args.check, mismatches)
    if mismatches:
        for mismatch in mismatches:
            print(mismatch, file=sys.stderr)
        return 1
    for path, data in outputs.items():
        print(f"{sha256_bytes(data)}  {path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ModelSourceError, json.JSONDecodeError, OSError) as error:
        print(f"thermal model export failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
