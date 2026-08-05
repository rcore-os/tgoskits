#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
python_command=${IVC_ORT_PYTHON:-python}

"$python_command" "$script_dir/export_thermal_ort.py" --check
"$python_command" "$script_dir/export_thermal_ort.py" --check

sha256sum \
    "$script_dir/thermal-4x6x1-v1.ort" \
    "$script_dir/thermal-4x6x1-v1.required_operators_and_types.config" \
    "$script_dir/ort-conversion-report.json" \
    "$script_dir/onnxruntime-1.25.0-source.json"
echo THERMAL_ORT_REBUILD_PASS
