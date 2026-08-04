#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
python_command=${IVC_MODEL_PYTHON:-python}
first=$(mktemp -d)
second=$(mktemp -d)

cleanup() {
    rm -rf -- "$first" "$second"
}
trap cleanup EXIT HUP INT TERM

"$python_command" "$script_dir/export_thermal_onnx.py" --check
"$python_command" "$script_dir/export_thermal_onnx.py" --output-dir "$first"
"$python_command" "$script_dir/export_thermal_onnx.py" --output-dir "$second"

artifacts=(
    neural_model_generated.rs
    thermal-4x6x1-v1.onnx
    golden-vectors.json
    model-manifest.json
)
for artifact in "${artifacts[@]}"; do
    cmp "$first/$artifact" "$second/$artifact"
done
cmp "$first/neural_model_generated.rs" "$workspace/tools/ivcproto/src/neural_model_generated.rs"
for artifact in thermal-4x6x1-v1.onnx golden-vectors.json model-manifest.json; do
    cmp "$first/$artifact" "$script_dir/$artifact"
done

sha256sum \
    "$workspace/tools/ivcproto/src/neural_model_generated.rs" \
    "$script_dir/thermal-4x6x1-v1.onnx" \
    "$script_dir/golden-vectors.json" \
    "$script_dir/model-manifest.json"
echo THERMAL_MODEL_REBUILD_PASS
