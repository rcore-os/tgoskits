#!/usr/bin/env bash
set -euo pipefail
case_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
runtime=$case_dir
[[ -x "$runtime/rknn_yolov8_images" ]] || runtime="$case_dir/install/rknn-yolov8-image"
if [[ ${1:-} == --help ]]; then
    echo 'Usage: ./yolov8_demo.sh [IMAGE_DIRECTORY]'
    echo 'Default: bundled images/. Optional: YOLO_MIN_CONFIDENCE=25 (percent).'
    exit 0
fi
[[ $# -le 1 ]] || { echo 'error: expected one image directory' >&2; exit 2; }
[[ -x "$runtime/rknn_yolov8_images" ]] || { echo 'error: build and deploy the runtime package first' >&2; exit 1; }
images=${1:-$runtime/images}
# Keep caller-relative image paths valid; do not change the working directory.
export LD_LIBRARY_PATH="$runtime/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$runtime/rknn_yolov8_images" "$runtime/model/yolov8.rknn" \
    "$runtime/model/coco_80_labels_list.txt" "$images" "${YOLO_MIN_CONFIDENCE:-25}"
