#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for script in "$app_dir"/*.sh "$app_dir"/tests/*.sh; do
    bash -n "$script"
done
bash "$app_dir/tests/selfbuild_contract.sh"
bash "$app_dir/tests/build_log.sh"
python3 "$app_dir/tests/image_transfer.py"
echo "orangepi5plus_selfbuild_script_smoke=PASS"
