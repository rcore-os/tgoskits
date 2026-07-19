#!/bin/sh
# run-tun-tap.sh - StarryOS /dev/net/tun carpet driver.
#
# Runs both the TUN echo probe and the TAP L2-framing carpet, then reports a
# single verdict the qemu success_regex keys on.
set -u

echo "== tun-tap carpet =="

if [ ! -c /dev/net/tun ]; then
    echo "MISSING /dev/net/tun character device"
    echo "TEST FAILED"
    exit 1
fi
echo "found /dev/net/tun"

echo ""
echo "--- TUN (L3 IP datapath) ---"
/usr/bin/tun-echo
tun_rc=$?

echo ""
echo "--- TAP (L2 Ethernet framing) ---"
/usr/bin/tap-carpet
tap_rc=$?

echo ""
if [ "$tun_rc" -eq 0 ] && [ "$tap_rc" -eq 0 ]; then
    echo "TEST PASSED"
    exit 0
else
    echo "TEST FAILED (tun_rc=$tun_rc tap_rc=$tap_rc)"
    exit 1
fi
