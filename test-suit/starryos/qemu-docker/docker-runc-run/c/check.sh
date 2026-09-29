#!/bin/sh
# Runs INSIDE the runc container (ns bundle) as `/busybox sh /check.sh` and
# verifies the namespace isolation the Phase 2 acceptance requires: uts
# hostname, a private pid namespace with our own init at pid 1, and populated
# /proc/1/ns entries. The bundle rootfs contains only /busybox (no applet
# symlinks and no /tmp), so every applet is invoked explicitly and scratch
# state lives at /.
BB=/busybox
fails=0
mark() {
    echo "DOCKER_RUNC_RUN_CONTAINER_FAIL $1"
    fails=$((fails + 1))
}

[ "$($BB hostname)" = "runc-ns" ] || mark hostname

$BB test -d /proc/1/ns || mark ns-dir
for ns in pid mnt uts ipc; do
    ( exec 3< "/proc/1/ns/$ns" ) || mark "ns-$ns"
done

# Our pid 1 is the bundle's busybox init, not the host init.
[ "$($BB cat /proc/1/comm)" = "busybox" ] || mark pid1-comm
[ "$$" = 1 ] || mark pid1-self

if [ "$fails" -eq 0 ]; then
    echo DOCKER_RUNC_RUN_NS_OK
else
    exit 1
fi
