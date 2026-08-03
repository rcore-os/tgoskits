#!/usr/bin/env bash

set -euo pipefail

source_dtb=${1:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}
target_dtb=${2:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}
root_selector=${3:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}

if [[ ! -r "$source_dtb" ]]; then
    echo "Source DTB is not readable: $source_dtb" >&2
    exit 1
fi
if [[ "$root_selector" == *[[:space:]]* ]]; then
    echo "Root selector must not contain whitespace: $root_selector" >&2
    exit 1
fi
for command_name in fdtget fdtput; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required DTB tool not found: $command_name" >&2
        exit 1
    fi
done

mkdir -p -- "$(dirname -- "$target_dtb")"
cp -- "$source_dtb" "$target_dtb"

bootargs=$(fdtget "$target_dtb" /chosen bootargs)
read -r -a bootarg_tokens <<<"$bootargs"
root_replaced=0
for index in "${!bootarg_tokens[@]}"; do
    if [[ "${bootarg_tokens[index]}" == root=* ]]; then
        bootarg_tokens[index]="root=$root_selector"
        root_replaced=1
        break
    fi
done
if ((root_replaced == 0)); then
    bootarg_tokens+=("root=$root_selector")
fi

fdtput -t s "$target_dtb" /chosen bootargs "${bootarg_tokens[*]}"
echo "BOARD_SERVICE_DTB_READY path=$target_dtb root=$root_selector"
