#!/bin/sh
# Peer guest init for Task 2 dual-guest vsw smoke (Guest B = 10.0.9.3).
# Installed into Alpine rootfs as /icpc-peer-init.sh by setup-peer-init.sh.
set -eu
ip link set lo up || true
ip link set eth0 up
ip addr flush dev eth0 2>/dev/null || true
ip addr add 10.0.9.3/24 dev eth0
# UDP echo on icpc port 9527 (busybox nc). Falls back to listen-only.
(
  while true; do
    if nc -u -l -p 9527 -e /bin/cat >/dev/null 2>&1; then
      :
    else
      nc -u -l -p 9527 >/dev/null 2>&1 || sleep 1
    fi
  done
) &
echo ICPC_PEER_READY
# Stay alive so the peer remains reachable for ping/UDP.
exec sleep infinity
