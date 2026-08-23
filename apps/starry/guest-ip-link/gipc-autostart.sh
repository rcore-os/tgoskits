#!/bin/sh

set -eu

if [ "${GIPC_AUTORUN:-1}" != "1" ]; then
    exit 0
fi

mkdir -p /run
if [ -e /run/gipc-starry-client.done ]; then
    exit 0
fi
touch /run/gipc-starry-client.done
exec /usr/bin/gipc-starry-client "${GIPC_PEER_IP:-10.0.42.2}"
