#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
base_initramfs="${BASE_INITRAMFS:-/home/huhu/tgoskits-realtime/tmp/initramfs-custom}"
task2_binary="${TASK2_BINARY:-$repo_root/tmp/net-dual-guest/linux-task2/controller/task2-net}"
model_dir="${TASK3_NCNN_MODEL_DIR:-$repo_root/tmp/task3-yolo/ncnn-model}"
ab_manifest="$repo_root/scripts/task3/task3-ab-manifest.tsv"
ab_model_dir="$model_dir/task3-ab"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest/linux-task2}"
out_file="$out_dir/task2-linux-initramfs.cpio.gz"
template="$repo_root/scripts/test/net-dual-guest/linux-init.sh"
expected_param_sha256="d2c0adf8939dc9ce02964ce8ada104447768ffd8e3bffad8fa11e2e61e709c1f"
expected_bin_sha256="0ae562447923999779b12b4f91f96b9ef263add8c9902d10e22e6dd6a2932c12"
expected_input_sha256="608c8a61ff0bb43e5a8613f1f6f8aa08af74b084363610ed2b526ad925e4cb6f"

for input in "$base_initramfs" "$task2_binary" "$template"; do
    if [[ ! -f "$input" ]]; then
        printf 'error: required input is missing: %s\n' "$input" >&2
        exit 1
    fi
done
mkdir -p "$out_dir"
root_dir="$(mktemp -d /tmp/task2-initramfs-root.XXXXXX)"
trap 'rm -rf "$root_dir"' EXIT

gzip -dc "$base_initramfs" | (cd "$root_dir" && cpio -idm --quiet)
install -m 0755 "$task2_binary" "$root_dir/bin/task2-net"
install -m 0755 "$template" "$root_dir/init"
if [[ "${TASK3_MODEL:-}" == "yolo" ]]; then
    "$repo_root/scripts/task3/prepare-yolo-ncnn-ab-inputs.sh" >/dev/null
    for model_file in yolo11n.ncnn.param yolo11n.ncnn.bin input.ppm; do
        if [[ ! -f "$model_dir/$model_file" ]]; then
            printf 'error: YOLO ncnn asset is missing: %s\n' "$model_dir/$model_file" >&2
            exit 1
        fi
    done
    for asset in \
        "yolo11n.ncnn.param:$expected_param_sha256" \
        "yolo11n.ncnn.bin:$expected_bin_sha256" \
        "input.ppm:$expected_input_sha256"; do
        asset_name="${asset%%:*}"
        expected_sha256="${asset#*:}"
        actual_sha256="$(sha256sum "$model_dir/$asset_name" | awk '{print $1}')"
        if [[ "$actual_sha256" != "$expected_sha256" ]]; then
            printf 'error: YOLO ncnn asset hash mismatch for %s: expected %s got %s\n' \
                "$asset_name" "$expected_sha256" "$actual_sha256" >&2
            exit 1
        fi
    done
    install -d "$root_dir/usr/share/task3-yolo"
    install -m 0644 "$model_dir/yolo11n.ncnn.param" "$root_dir/usr/share/task3-yolo/yolo11n.ncnn.param"
    install -m 0644 "$model_dir/yolo11n.ncnn.bin" "$root_dir/usr/share/task3-yolo/yolo11n.ncnn.bin"
    install -m 0644 "$model_dir/input.ppm" "$root_dir/usr/share/task3-yolo/input.ppm"
    install -d "$root_dir/usr/share/task3-yolo/task3-ab"
    install -m 0644 "$ab_manifest" "$root_dir/usr/share/task3-yolo/task3-ab/manifest.tsv"
    while IFS=$'\t' read -r image_id filename expected_sha256 truth_target expected_behavior; do
        [[ -z "$image_id" || "$image_id" == \#* ]] && continue
        actual_sha256="$(sha256sum "$ab_model_dir/$filename" | awk '{print $1}')"
        if [[ "$actual_sha256" != "$expected_sha256" ]]; then
            printf 'error: Task-3 A/B image hash mismatch for %s: expected %s got %s\n' \
                "$image_id" "$expected_sha256" "$actual_sha256" >&2
            exit 1
        fi
        install -m 0644 "$ab_model_dir/$filename" \
            "$root_dir/usr/share/task3-yolo/task3-ab/$filename"
    done < "$ab_manifest"
fi

(cd "$root_dir" && find . -print | cpio -o -H newc --quiet) | gzip -n -9 > "$out_file"
sha256sum "$out_file" | awk '{print $1}' > "$out_file.sha256"
printf 'initramfs=%s sha256=%s\n' "$out_file" "$(cat "$out_file.sha256")"
