#!/usr/bin/env bash
set -euo pipefail

# Convert the pinned YOLO11n ONNX artifact to ncnn's param/bin format.
# PNNX is a host-side conversion tool; the resulting pinned param/bin files are
# loaded by both the Linux and StarryOS Guest runtimes.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
model="${YOLO_ONNX:-$repo_root/tmp/task3-yolo/yolo11n.onnx}"
out_dir="${OUT_DIR:-$repo_root/tmp/task3-yolo/ncnn-model}"
pnnx_bin="${PNNX:-}"
expected_sha256="634279b40c07c6391472c51ad45b81ebc48706a9a1fe72dd3396322acd0c053b"

if [[ ! -f "$model" ]]; then
    printf 'error: missing YOLO ONNX model: %s\n' "$model" >&2
    exit 2
fi
actual_sha256="$(sha256sum "$model" | awk '{print $1}')"
if [[ "$actual_sha256" != "$expected_sha256" ]]; then
    printf 'error: YOLO ONNX hash mismatch: expected %s got %s\n' \
        "$expected_sha256" "$actual_sha256" >&2
    exit 2
fi
if [[ -z "$pnnx_bin" || ! -x "$pnnx_bin" ]]; then
    printf 'error: PNNX must point to the host converter executable\n' >&2
    exit 2
fi

mkdir -p "$out_dir"
cp "$model" "$out_dir/yolo11n.onnx"
(
    cd "$out_dir"
    "$pnnx_bin" yolo11n.onnx \
        inputshape='[1,3,640,640]f32' \
        inputshape2='[1,3,640,640]f32' \
        optlevel=2
)

test -s "$out_dir/yolo11n.ncnn.param"
test -s "$out_dir/yolo11n.ncnn.bin"
model_sha256="$(sha256sum "$out_dir/yolo11n.ncnn.bin" | awk '{print $1}')"
{
    printf 'source_onnx_sha256 = "%s"\n' "$actual_sha256"
    printf 'input_shape = "1,3,640,640"\n'
    printf 'ncnn_param = "yolo11n.ncnn.param"\n'
    printf 'ncnn_bin = "yolo11n.ncnn.bin"\n'
    printf 'ncnn_bin_sha256 = "%s"\n' "$model_sha256"
} > "$out_dir/manifest.toml"
printf 'param=%s\nbin=%s\nmanifest=%s\n' \
    "$out_dir/yolo11n.ncnn.param" "$out_dir/yolo11n.ncnn.bin" "$out_dir/manifest.toml"
