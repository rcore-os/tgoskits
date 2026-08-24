#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ncnn_prefix="${NCNN_PREFIX:-$repo_root/tmp/task3-yolo/ncnn-aarch64/install}"
cross_cxx="${CROSS_CXX:-/home/huhu/.local/toolchains/aarch64-linux-musl-cross/bin/aarch64-linux-musl-g++}"
cross_qemu="${QEMU_AARCH64:-/home/huhu/.local/bin/qemu-aarch64}"
out_dir="${OUT_DIR:-$repo_root/tmp/task3-yolo/ncnn-smoke}"
input_path="${1:-$repo_root/tmp/task3-yolo/ncnn-model/input.ppm}"
if [[ ! -f "$input_path" ]]; then
    printf 'error: ncnn smoke input is missing: %s\n' "$input_path" >&2
    exit 2
fi
mkdir -p "$out_dir"

"$cross_cxx" -std=c++11 -O2 -static \
    -I"$ncnn_prefix/include" \
    "$repo_root/components/task3-ncnn/src/adapter.cc" \
    "$repo_root/scripts/task3/ncnn-smoke.cc" \
    "$ncnn_prefix/lib/libncnn.a" -lstdc++ -lgcc -lm -lpthread \
    -o "$out_dir/ncnn-smoke"

if [[ ! -x "$cross_qemu" ]]; then
    printf 'error: qemu-aarch64 is missing: %s\n' "$cross_qemu" >&2
    exit 1
fi
"$cross_qemu" "$out_dir/ncnn-smoke" \
    "$repo_root/tmp/task3-yolo/ncnn-model/yolo11n.ncnn.param" \
    "$repo_root/tmp/task3-yolo/ncnn-model/yolo11n.ncnn.bin" \
    "$input_path"
