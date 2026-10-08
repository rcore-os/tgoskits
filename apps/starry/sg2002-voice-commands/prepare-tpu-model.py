#!/usr/bin/env python3
"""Split and compile the pinned Zipformer2 frontend for SG2002 CV181x BF16."""

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess
import sys

import numpy as np
import onnx
from onnx import helper
from onnx.utils import Extractor
import onnxruntime as ort

ENCODER_SHA256 = "540ff509ed89bd22afe04bf7049a54bb1c95c6d8a18742ea9691910cdb5f859e"
FRONT_INPUTS = ["x", "embed_states"]
FRONT_OUTPUTS = ["/out_norm/Mul_1_output_0", "new_embed_states"]
OUTPUT_SHAPES = [[1, 16, 128], [1, 128, 3, 19]]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def session(path):
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    return ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])


def split(source, output):
    if digest(source) != ENCODER_SHA256:
        raise ValueError("The TPU split requires the pinned chunk-16/left-64 FP32 encoder")
    model = onnx.load(source)
    for value in model.graph.input:
        for dim in value.type.tensor_type.shape.dim:
            if dim.dim_param == "N":
                dim.dim_value = 1
    model = onnx.shape_inference.infer_shapes(model)
    for name, shape in zip(FRONT_OUTPUTS, OUTPUT_SHAPES):
        for value in [*model.graph.value_info, *model.graph.output]:
            if value.name == name:
                value.type.tensor_type.shape.Clear()
                for size in shape:
                    value.type.tensor_type.shape.dim.add().dim_value = size
    # Extractor also needs this original graph output as an intermediate input.
    model.graph.value_info.append(next(v for v in model.graph.output
                                       if v.name == "new_embed_states"))
    frontend = Extractor(model).extract_model(FRONT_INPUTS, FRONT_OUTPUTS)
    onnx.save(frontend, output / "frontend-original.onnx")

    # The compiler mislowers the stable Log/Exp expansion in these four Swoosh
    # activations. ONNX Softplus expresses the same function without that pattern.
    prefixes = ("/conv/conv.3", "/conv/conv.6", "/conv/conv.9", "/activation")
    replaced = 0
    for node in frontend.graph.node:
        prefix = node.name.rsplit("/", 1)[0]
        if prefix in prefixes and node.name == prefix + "/Add_1":
            node.CopyFrom(helper.make_node("Softplus", [prefix + "/Sub_output_0"],
                                           list(node.output), name=prefix + "/Softplus"))
            replaced += 1
    if replaced != 4:
        raise ValueError("Unexpected activation graph")
    frontend = Extractor(frontend).extract_model(FRONT_INPUTS, FRONT_OUTPUTS)
    onnx.checker.check_model(frontend)
    onnx.save(frontend, output / "frontend.onnx")

    # Retain x for downstream Shape nodes, but cut all frontend computation.
    names = [v.name for v in model.graph.input if v.name != "embed_states"] + FRONT_OUTPUTS
    tail = Extractor(model).extract_model(names, [v.name for v in model.graph.output])
    onnx.checker.check_model(tail)
    onnx.save(tail, output / "encoder-tail.onnx")
    accelerated = onnx.ModelProto()
    accelerated.CopyFrom(tail)
    accelerated.graph.ClearField("input")
    accelerated.graph.input.extend(model.graph.input)
    accelerated.graph.node.insert(0, helper.make_node(
        "VoiceFrontend", FRONT_INPUTS, FRONT_OUTPUTS,
        name="voice_frontend_tpu", domain="voice.sg2002"))
    accelerated.opset_import.append(helper.make_opsetid("voice.sg2002", 1))
    accelerated.metadata_props.extend(model.metadata_props)
    onnx.checker.check_model(accelerated)
    onnx.save(accelerated, output / "encoder.onnx")

    bridge = helper.make_model(helper.make_graph(
        [helper.make_node("VoiceFrontend", FRONT_INPUTS, FRONT_OUTPUTS, domain="voice.sg2002")],
        "frontend-bridge", list(frontend.graph.input), list(frontend.graph.output)),
        opset_imports=[*frontend.opset_import, helper.make_opsetid("voice.sg2002", 1)])
    bridge.ir_version = model.ir_version
    onnx.checker.check_model(bridge)
    onnx.save(bridge, output / "frontend-bridge.onnx")


def validate_split(source, output):
    original = session(output / "frontend-original.onnx")
    front = session(output / "frontend.onnx")
    rng = np.random.default_rng(1)
    errors = []
    for mean in (-20, -3, 5, 15):
        values = {"x": rng.normal(mean, 2, (1, 45, 80)).astype(np.float32),
                  "embed_states": np.zeros((1, 128, 3, 19), np.float32)}
        for a, b in zip(original.run(None, values), front.run(None, values)):
            np.testing.assert_allclose(a, b, rtol=1e-4, atol=1e-4)
            errors.append(float(np.max(np.abs(a - b))))
    values["x"] = rng.normal(-3, 2, (1, 45, 80)).astype(np.float32)
    np.savez(output / "input.npz", **values)

    full = session(source)
    tail = session(output / "encoder-tail.onnx")
    inputs = {i.name: np.zeros([1 if isinstance(d, str) else d for d in i.shape],
                              np.int64 if i.type == "tensor(int64)" else np.float32)
              for i in full.get_inputs()}
    for _ in range(3):
        inputs["x"] = rng.normal(0, 2, (1, 45, 80)).astype(np.float32)
        reference = full.run(None, inputs)
        features = front.run(None, {k: inputs[k] for k in FRONT_INPUTS})
        arguments = {k: v for k, v in inputs.items() if k != "embed_states"}
        arguments.update(zip(FRONT_OUTPUTS, features))
        for a, b in zip(reference, tail.run(None, arguments)):
            np.testing.assert_allclose(a, b, rtol=1e-3, atol=2e-4)
            errors.append(float(np.max(np.abs(a - b))))
        for key, value in zip(list(inputs)[1:], reference[1:]):
            inputs[key] = value
    print("FLOAT32_SPLIT_EQUIVALENT", max(errors), flush=True)
    return max(errors)


def compile_frontend(output):
    wrapper = Path(__file__).with_name("tpu-tool.py")
    commands = [
        ["model_transform.py", "--model_name", "voice_frontend", "--model_def", "frontend.onnx",
         "--input_shapes", "[[1,45,80],[1,128,3,19]]", "--test_input", "input.npz",
         "--test_result", "ref.npz", "--mlir", "frontend.mlir"],
        ["model_deploy.py", "--mlir", "frontend.mlir", "--quantize", "BF16", "--chip", "cv181x",
         "--test_input", "voice_frontend_in_f32.npz", "--test_reference", "ref.npz",
         "--model", "frontend.cvimodel"],
    ]
    for command in commands:
        log = output / (Path(command[0]).stem + ".log")
        with log.open("w") as stream:
            subprocess.run([sys.executable, str(wrapper), *command], cwd=output,
                           stdout=stream, stderr=subprocess.STDOUT, check=True, timeout=600)
        print(f"COMPILER_CHECK_PASSED {log}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("encoder", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    split(args.encoder, output)
    error = validate_split(args.encoder, output)
    compile_frontend(output)
    manifest = {"source_encoder_sha256": digest(args.encoder), "chip": "cv181x",
                "precision": "BF16", "tpu_mlir": importlib.metadata.version("tpu-mlir"),
                "float32_check_max_absolute_error": error,
                "sha256": {name: digest(output / name)
                           for name in ("encoder.onnx", "frontend.cvimodel", "frontend.onnx")}}
    (output / "tpu-model.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
