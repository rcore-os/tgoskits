#!/bin/sh
set -u

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo "network-smoke: usage: $0 <server-ip>"
    echo STARRY_NETWORK_SMOKE_FAILED
    exit 1
fi

if ! curl-smoke.sh "$1" 4 1 STARRY_NETWORK_SMOKE; then
    echo STARRY_NETWORK_SMOKE_FAILED
    exit 1
fi
