#!/bin/sh
set -u
exec </dev/ttyS0 >/dev/ttyS0 2>&1
payload_root="${G2_PAYLOAD_ROOT:-}"
busybox="${payload_root}/bin/busybox"
echo TASK3_HYBRID_SCENE_BEGIN source=rknn communication_cpu=0 ai_cpu=1
"$busybox" rm -f /rknn-control.txt /rknn-control.txt.tmp /tmp/scene-expected.txt
"$busybox" ip addr add 10.0.42.15/24 dev eth0
"$busybox" taskset -c 0 "${payload_root}/bin/task2-net" &
control_pid=$!
echo TASK3_CONTROLLER_STARTED cpu=0 pid="$control_pid"
cd "${payload_root}/rknn" || exit 100
export LD_LIBRARY_PATH="${payload_root}/rknn/lib"
"$busybox" taskset -c 1 \
    "${payload_root}/rknn/lib/ld-linux-aarch64.so.1" \
    --library-path "${payload_root}/rknn/lib" \
    "${payload_root}/rknn/rknn_yolov8_bench" \
    --validate-list validation/scene-images.txt \
    --write-expected /tmp/scene-expected.txt \
    --control-output /rknn-control.txt \
    --min-confidence 25 \
    --core-mask all \
    --profile --profile-frames \
    >/tmp/rknn-scene-profile.log 2>&1
rc=$?
"$busybox" sleep 2
echo TASK3_RKNN_SCENE_END cpu=1 images=11 rc="$rc"
"$busybox" cat /tmp/rknn-scene-profile.log
exec "$busybox" sleep 300
