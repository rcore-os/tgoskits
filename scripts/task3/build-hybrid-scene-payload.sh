#!/usr/bin/env bash
set -euo pipefail

# Assemble the raw cpio consumed by StarryOS /proc/initrd.  The RKNN runtime,
# model and glibc libraries are external board assets and are intentionally not
# checked into Git; callers provide their already-staged bundle explicitly.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mode="${1:-}"
output="${2:-}"
busybox="${BUSYBOX_STATIC:-}"
task2_binary="${TASK2_BINARY:-}"
rknn_bundle="${RKNN_BUNDLE:-}"
manifest="$repo_root/scripts/task3/task3-continuous-manifest.tsv"

usage() {
    printf 'usage: BUSYBOX_STATIC=... TASK2_BINARY=... %s <fixed|rknn> <output.cpio>\n' \
        "${BASH_SOURCE[0]##*/}" >&2
    printf '       RKNN mode also requires RKNN_BUNDLE=<directory containing bin/model/lib/validation assets>\n' >&2
}

if [[ "$mode" == -h || "$mode" == --help ]]; then
    usage
    exit 0
fi
if [[ "$mode" != fixed && "$mode" != rknn ]] || [[ -z "$output" ]]; then
    usage
    exit 2
fi
if [[ -z "$busybox" || ! -f "$busybox" ]]; then
    printf 'error: BUSYBOX_STATIC must name a static AArch64 BusyBox binary\n' >&2
    exit 1
fi
if [[ -z "$task2_binary" || ! -f "$task2_binary" ]]; then
    printf 'error: TASK2_BINARY must name the matching AArch64 task2-net controller\n' >&2
    exit 1
fi
for executable in cpio file install sha256sum; do
    command -v "$executable" >/dev/null 2>&1 || {
        printf 'error: required executable is unavailable: %s\n' "$executable" >&2
        exit 1
    }
done
case "$mode" in
    fixed) model_marker='continuous-scene-v1' ;;
    rknn) model_marker='external:rknn-control-v2' ;;
esac
if ! grep -aFq "$model_marker" "$task2_binary"; then
    printf 'error: TASK2_BINARY does not contain the %s model marker: %s\n' \
        "$mode" "$model_marker" >&2
    exit 1
fi
for binary in "$busybox" "$task2_binary"; do
    if ! file "$binary" | grep -q 'ARM aarch64'; then
        printf 'error: expected an AArch64 executable: %s\n' "$binary" >&2
        exit 1
    fi
done

root_dir="$(mktemp -d "${TMPDIR:-/tmp}/task3-hybrid-payload.XXXXXX")"
trap 'rm -rf -- "$root_dir"' EXIT
install -D -m 0755 "$busybox" "$root_dir/bin/busybox"
install -D -m 0755 "$task2_binary" "$root_dir/bin/task2-net"
install -m 0755 "$repo_root/scripts/task3/hybrid-scene-$mode-init.sh" "$root_dir/init"

if [[ "$mode" == rknn ]]; then
    if [[ -z "$rknn_bundle" || ! -d "$rknn_bundle" ]]; then
        printf 'error: RKNN_BUNDLE is required for the rknn payload\n' >&2
        exit 1
    fi
    for relative in \
        rknn_yolov8_bench \
        lib/ld-linux-aarch64.so.1 \
        lib/libc.so.6 \
        lib/libm.so.6 \
        lib/libstdc++.so.6 \
        lib/librknnrt.so \
        model/yolov8.rknn \
        model/coco_80_labels_list.txt; do
        if [[ ! -f "$rknn_bundle/$relative" ]]; then
            printf 'error: RKNN bundle asset is missing: %s\n' "$relative" >&2
            exit 1
        fi
    done
    install -d "$root_dir/rknn"
    cp -a "$rknn_bundle/." "$root_dir/rknn/"
    install -d "$root_dir/rknn/validation"
    install -m 0644 "$repo_root/scripts/task3/hybrid-scene-images.txt" \
        "$root_dir/rknn/validation/scene-images.txt"

    while IFS=$'\t' read -r _sequence _image_id event _source _source_frame \
        _timestamp_ms jpeg jpeg_sha256 _ppm _ppm_sha256 _expected_class \
        _truth_target _expected_decision; do
        [[ -z "$_sequence" || "$_sequence" == \#* || "$event" == reset ]] && continue
        image="$root_dir/rknn/validation/$jpeg"
        if [[ ! -f "$image" ]]; then
            printf 'error: RKNN validation image is missing: %s\n' "$jpeg" >&2
            exit 1
        fi
        actual_sha256="$(sha256sum "$image" | awk '{print $1}')"
        if [[ "$actual_sha256" != "$jpeg_sha256" ]]; then
            printf 'error: RKNN validation image hash mismatch for %s\n' "$jpeg" >&2
            exit 1
        fi
    done < "$manifest"
fi

mkdir -p "$(dirname "$output")"
(cd "$root_dir" && find . -print0 | sort -z | cpio --null -o -H newc --quiet) > "$output"
sha256sum "$output" > "$output.sha256"
printf 'payload=%s\nsha256=%s\nmode=%s\n' \
    "$(realpath "$output")" "$(awk '{print $1}' "$output.sha256")" "$mode"
