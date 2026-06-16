#!/bin/sh
set -eu

MODEL=/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel
IMAGE=/usr/share/k230-nncase-runtime/images/bus.jpg

echo "K230_NNCASE_RUNTIME: minimal"
/usr/bin/kpu-nncase-minimal "$MODEL"

echo "K230_NNCASE_RUNTIME: yolov8n-demo"
/usr/bin/k230-yolov8n-demo "$MODEL" "$IMAGE"

echo "K230_NNCASE_RUNTIME_PASS"
