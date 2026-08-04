#!/usr/bin/env bash

set -euo pipefail

source_dtb=${1:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}
target_dtb=${2:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}
root_selector=${3:?usage: prepare-service-dtb.sh <source-dtb> <target-dtb> <root-selector>}
shift 3
rknpu_dma_carveout=0

while (($# > 0)); do
    case "$1" in
        --rknpu-dma-carveout)
            rknpu_dma_carveout=1
            shift
            ;;
        *)
            echo "Unknown service DTB option: $1" >&2
            exit 2
            ;;
    esac
done

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
if ((rknpu_dma_carveout == 1)); then
    reserved_memory=/reserved-memory
    carveout=$reserved_memory/axvisor-rknpu-dma@80000000
    if ! fdtget -p "$target_dtb" "$reserved_memory" >/dev/null 2>&1; then
        fdtput -c "$target_dtb" "$reserved_memory"
    fi
    fdtput -t x "$target_dtb" "$reserved_memory" '#address-cells' 2
    fdtput -t x "$target_dtb" "$reserved_memory" '#size-cells' 2
    fdtput "$target_dtb" "$reserved_memory" ranges
    if ! fdtget -p "$target_dtb" "$carveout" >/dev/null 2>&1; then
        fdtput -c "$target_dtb" "$carveout"
    fi
    fdtput -t x "$target_dtb" "$carveout" reg 0 0x80000000 0 0x10000000
    fdtput "$target_dtb" "$carveout" no-map
    echo "BOARD_SERVICE_DTB_RKNPU_DMA_RESERVED base=0x80000000 size=0x10000000 no-map=true"
fi
echo "BOARD_SERVICE_DTB_READY path=$target_dtb root=$root_selector"
