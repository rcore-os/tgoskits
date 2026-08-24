#!/usr/bin/env bash

set -euo pipefail

case_dir=$(cd "$(dirname "$0")" && pwd)
workspace=$(cd "$case_dir/../../.." && pwd)
source_dtb="$workspace/os/StarryOS/configs/board/orangepi-5-plus.dtb"
target_dtb="$workspace/tmp/starry/vision/orangepi-5-plus-starry.dtb"
prepare_dtb="$workspace/scripts/benchmark/axvisor-rt/board/prepare-service-dtb.sh"

# This is the observed SD-card root on the competition board. Override it fo
# another card; a stable GPT identifier prevents the eMMC `misc` partition from
# being selected when both RK3588 MMC controllers are present.
root_selector=${ORANGEPI_STARRY_ROOT:-PARTUUID=5874edd8-1582-a144-a298-b139acd7b0e6}

bash <(sed 's/\r$//' "$prepare_dtb") \
    "$source_dtb" \
    "$target_dtb" \
    "$root_selector"
