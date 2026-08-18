#!/bin/sh
set -u

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo "iperf-smoke: usage: $0 <server-ip>"
    echo STARRY_IPERF_SMOKE_FAILED
    exit 1
fi

command -v iperf3 >/dev/null 2>&1 || {
    echo "iperf-smoke: iperf3 is not installed"
    echo STARRY_IPERF_SMOKE_FAILED
    exit 1
}

server_ip=$1

printf '\n=== iperf3 network smoke ===\n\n'
printf 'iperf3 -c %s -t 3 -O 1 -P 1 -l 128K\n\n' "$server_ip"

if iperf3 -c "$server_ip" -t 3 -O 1 -P 1 -l 128K; then
    printf '\nSTARRY_IPERF_SMOKE_PASSED\n'
else
    printf '\nSTARRY_IPERF_SMOKE_FAILED\n'
    exit 1
fi
