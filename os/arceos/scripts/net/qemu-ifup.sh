#!/usr/bin/env bash
set -euo pipefail

TAP_IF="${1:?missing tap interface name}"
BRIDGE="${BRIDGE:-xkbr0}"

ip link set "$TAP_IF" up
ip link set "$TAP_IF" master "$BRIDGE"
