#!/bin/sh
set -u

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo "iperf-smoke: usage: $0 <server-ip>"
    echo STARRY_IPERF_SMOKE_FAILED
    exit 1
fi

if ! iperf2-smoke "$1" 4 1 STARRY_IPERF_SMOKE; then
    echo STARRY_IPERF_SMOKE_FAILED
    exit 1
fi
