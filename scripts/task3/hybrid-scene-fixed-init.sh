#!/bin/sh
set -u
exec </dev/ttyS0 >/dev/ttyS0 2>&1
payload_root="${G2_PAYLOAD_ROOT:-}"
busybox="${payload_root}/bin/busybox"
echo TASK3_HYBRID_SCENE_BEGIN source=fixed-perception communication_cpu=0 ai_cpu=1
"$busybox" ip addr add 10.0.42.15/24 dev eth0
"$busybox" taskset -c 0 "${payload_root}/bin/task2-net"
rc=$?
echo TASK3_HYBRID_SCENE_END source=fixed-perception rc="$rc"
exec "$busybox" sleep 300
