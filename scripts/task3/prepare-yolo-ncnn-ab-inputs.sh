#!/usr/bin/env bash
set -euo pipefail

# Build the frozen Task-3 A/B image set from a repository-owned photograph.
# The accepted samples preserve the photographed flowerpot pixels and move
# them horizontally; the rejection samples contain either only photographed
# background or a deliberately small copy of the same real scene.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source_image="${YOLO_AB_SOURCE_IMAGE:-$repo_root/apps/starry/aka00-tennis-yolo/validation/tennis-ball-plant.jpg}"
small_source_image="$repo_root/apps/starry/aka00-tennis-yolo/validation/tennis-ball-close.jpg"
out_dir="${YOLO_AB_OUT_DIR:-$repo_root/tmp/task3-yolo/ncnn-model/task3-ab}"
manifest="$repo_root/scripts/task3/task3-ab-manifest.tsv"
expected_source_sha256="6a3e4bb91eb93fd85b66a6d0e16dbba1185287e777a5ec93becdbb8ae8bb4614"
expected_small_source_sha256="73060d738a991c8a93d04a43ff228b7c200472e58605575ff9adce224af66507"

if [[ ! -f "$source_image" ]]; then
    printf 'error: missing Task-3 A/B source image: %s\n' "$source_image" >&2
    exit 2
fi
actual_source_sha256="$(sha256sum "$source_image" | awk '{print $1}')"
if [[ "$actual_source_sha256" != "$expected_source_sha256" ]]; then
    printf 'error: Task-3 A/B source hash mismatch: expected %s got %s\n' \
        "$expected_source_sha256" "$actual_source_sha256" >&2
    exit 1
fi
actual_small_source_sha256="$(sha256sum "$small_source_image" | awk '{print $1}')"
if [[ "$actual_small_source_sha256" != "$expected_small_source_sha256" ]]; then
    printf 'error: Task-3 A/B small-target source hash mismatch: expected %s got %s\n' \
        "$expected_small_source_sha256" "$actual_small_source_sha256" >&2
    exit 1
fi

mkdir -p "$out_dir"
python3 - "$source_image" "$small_source_image" "$out_dir" <<'PY'
import sys
from pathlib import Path

from PIL import Image


source_path, small_source_path, output_path = map(Path, sys.argv[1:])
source = Image.open(source_path).convert("RGB")
small_source = Image.open(small_source_path).convert("RGB")
width, height = source.size
fill = source.getpixel((0, 0))


def translated(offset_x: int) -> Image.Image:
    output = Image.new("RGB", source.size, fill)
    output.paste(source, (offset_x, 0))
    return output


samples = {
    "vase-left.ppm": translated(-128),
    "vase-center.ppm": source,
    "vase-right.ppm": translated(128),
    "no-target.ppm": source.crop((0, 3 * height // 4, width, height)).resize(source.size),
}

scale = 0.35
small_width = round(width * scale)
small_height = round(height * scale)
small_scene = small_source.resize((small_width, small_height))
small_target = Image.new("RGB", source.size, small_source.getpixel((0, 0)))
small_target.paste(small_scene, (420, (height - small_height) // 2))
samples["small-target.ppm"] = small_target

for name, image in samples.items():
    image.save(output_path / name, format="PPM")
PY

sample_count=0
while IFS=$'\t' read -r image_id filename expected_sha256 truth_target expected_behavior; do
    [[ -z "$image_id" || "$image_id" == \#* ]] && continue
    image_path="$out_dir/$filename"
    actual_sha256="$(sha256sum "$image_path" | awk '{print $1}')"
    if [[ "$actual_sha256" != "$expected_sha256" ]]; then
        printf 'error: Task-3 A/B image hash mismatch for %s: expected %s got %s\n' \
            "$image_id" "$expected_sha256" "$actual_sha256" >&2
        exit 1
    fi
    printf '%s  %s\n' "$actual_sha256" "$image_path"
    sample_count=$((sample_count + 1))
done < "$manifest"
if [[ "$sample_count" -ne 5 ]]; then
    printf 'error: Task-3 A/B manifest must contain five samples, got %s\n' "$sample_count" >&2
    exit 1
fi
cp "$manifest" "$out_dir/manifest.tsv"
