#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source_image="${YOLO_INPUT_IMAGE:-$repo_root/apps/starry/aka00-tennis-yolo/validation/tennis-ball-plant.jpg}"
out_file="${YOLO_INPUT_PPM:-$repo_root/tmp/task3-yolo/ncnn-model/input.ppm}"

if [[ ! -f "$source_image" ]]; then
    printf 'error: missing input image: %s\n' "$source_image" >&2
    exit 2
fi
mkdir -p "$(dirname "$out_file")"
python3 - "$source_image" "$out_file" <<'PY'
import sys
from PIL import Image

source, output = sys.argv[1:]
Image.open(source).convert("RGB").save(output, format="PPM")
PY
sha256sum "$out_file"
