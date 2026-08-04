#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
python_command=${IVC_RKNN_PYTHON:-python}
first=$(mktemp -d)
second=$(mktemp -d)

cleanup() {
    rm -rf -- "$first" "$second"
}
trap cleanup EXIT HUP INT TERM

"$python_command" "$script_dir/convert_thermal_rknn.py" --check
"$python_command" "$script_dir/convert_thermal_rknn.py" --output-dir "$first"
"$python_command" "$script_dir/convert_thermal_rknn.py" --output-dir "$second"

artifacts=(
    thermal-4x6x1-v1-rk3588-fp16.rknn
    rknn-conversion.log
    rknn-conversion-report.json
)
for artifact in "${artifacts[@]}"; do
    cmp "$first/$artifact" "$second/$artifact"
    cmp "$first/$artifact" "$script_dir/$artifact"
done

"$python_command" "$script_dir/verify_thermal_rknn.py" --check

sha256sum \
    "$script_dir/thermal-4x6x1-v1-rk3588-fp16.rknn" \
    "$script_dir/rknn-conversion.log" \
    "$script_dir/rknn-conversion-report.json" \
    "$script_dir/rknn-simulator-report.json"
echo THERMAL_RKNN_REBUILD_PASS
