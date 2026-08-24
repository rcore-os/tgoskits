#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source_dir="${YOLO_CONTINUOUS_SOURCE_DIR:-$repo_root/scripts/task3/fixtures/continuous-scene}"
out_dir="${YOLO_CONTINUOUS_OUT_DIR:-$repo_root/tmp/task3-yolo/ncnn-model/continuous-scene}"
manifest="$repo_root/scripts/task3/task3-continuous-manifest.tsv"

mkdir -p "$out_dir"

if [[ ! -d "$source_dir" ]]; then
    printf 'error: continuous-scene JPEG directory is missing: %s\n' "$source_dir" >&2
    printf 'error: set YOLO_CONTINUOUS_SOURCE_DIR to the hash-pinned JPEG fixture directory\n' >&2
    exit 1
fi

while IFS=$'\t' read -r sequence image_id event source source_frame timestamp_ms \
    jpeg jpeg_sha256 ppm ppm_sha256 expected_class truth_target expected_decision; do
    [[ -z "$sequence" || "$sequence" == \#* || "$event" == "reset" ]] && continue
    actual_jpeg_sha256="$(sha256sum "$source_dir/$jpeg" | awk '{print $1}')"
    if [[ "$actual_jpeg_sha256" != "$jpeg_sha256" ]]; then
        printf 'error: continuous-scene JPEG hash mismatch for %s: expected %s got %s\n' \
            "$image_id" "$jpeg_sha256" "$actual_jpeg_sha256" >&2
        exit 1
    fi
done < "$manifest"

python3 - "$source_dir" "$out_dir" <<'PY'
import sys
from pathlib import Path

from PIL import Image

source_dir, out_dir = map(Path, sys.argv[1:])
for source in sorted(source_dir.glob("*.jpg")):
    with Image.open(source) as image:
        image.convert("RGB").save(out_dir / f"{source.stem}.ppm", format="PPM")
PY

sample_count=0
while IFS=$'\t' read -r sequence image_id event source source_frame timestamp_ms \
    jpeg jpeg_sha256 ppm ppm_sha256 expected_class truth_target expected_decision; do
    [[ -z "$sequence" || "$sequence" == \#* || "$event" == "reset" ]] && continue
    actual_ppm_sha256="$(sha256sum "$out_dir/$ppm" | awk '{print $1}')"
    if [[ "$actual_ppm_sha256" != "$ppm_sha256" ]]; then
        printf 'error: continuous-scene PPM hash mismatch for %s: expected %s got %s\n' \
            "$image_id" "$ppm_sha256" "$actual_ppm_sha256" >&2
        exit 1
    fi
    printf '%s  %s\n' "$actual_ppm_sha256" "$out_dir/$ppm"
    sample_count=$((sample_count + 1))
done < "$manifest"

if [[ "$sample_count" -ne 11 ]]; then
    printf 'error: continuous-scene manifest must contain 11 frame samples, got %s\n' \
        "$sample_count" >&2
    exit 1
fi
cp "$manifest" "$out_dir/manifest.tsv"
