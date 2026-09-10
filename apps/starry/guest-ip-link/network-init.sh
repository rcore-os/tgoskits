#!/bin/sh

# Configure the StarryOS side of the private VirtIO-net link before the
# application starts.  The peer (ArceOS) uses 10.0.42.2/24.
set -eu

interface=${GIPC_INTERFACE:-eth0}
address=${GIPC_STARRY_IP:-10.0.42.1/24}
peer=${GIPC_PEER_IP:-10.0.42.2}

command -v ip >/dev/null 2>&1 || {
    echo "GIPC_STARRY_NET_ERROR missing ip command" >&2
    exit 1
}

ip link show "$interface" >/dev/null 2>&1 || {
    echo "GIPC_STARRY_NET_ERROR interface=$interface unavailable" >&2
    exit 1
}
ip link set "$interface" up
if ! ip addr show dev "$interface" | grep -q "$(printf '%s' "$address" | cut -d/ -f1)"; then
    ip addr add "$address" dev "$interface"
fi
if ! ip route show dev "$interface" | grep -q "10.0.42.0/24"; then
    ip route add 10.0.42.0/24 dev "$interface" 2>/dev/null || true
fi
ip addr show dev "$interface" | grep -q "$(printf '%s' "$address" | cut -d/ -f1)" || {
    echo "GIPC_STARRY_NET_ERROR address=$address not configured" >&2
    exit 1
}
echo "GIPC_STARRY_NET_READY interface=$interface address=$address peer=$peer"
