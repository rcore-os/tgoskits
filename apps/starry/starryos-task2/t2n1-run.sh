#!/bin/sh
set -u
ip addr replace 10.0.42.15/24 dev eth0 2>/dev/null || true
ip link set eth0 up 2>/dev/null || true
echo "STARRY_T2N1_NET_CONFIGURED ip=10.0.42.15/24"
exec /usr/bin/starry-t2n1-endpoint "$@"
