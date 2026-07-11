#!/bin/sh
# run-starrywrt.sh - on-target boot gate for the StarryWRT distribution.
#
# Invoked as the entire shell_init_cmd. It prints the StarryWRT banner (the distribution
# identity) and then confirms every shipped software stack runs correctly on the single-core
# StarryOS kernel, by running three carpets to their pinned OK lines:
#
#   uci-carpet.sh        - the config stack  (70 assertions: full uci command + option surface)
#   opkg-carpet.sh       - the package stack (42 assertions: full opkg .ipk lifecycle offline)
#   starrywrt-carpet.sh  - the distribution  (identity + busybox base + shipped /etc/config +
#                          OpenWrt init framework + dropbear SSH stack + dnsmasq DNS stack)
#
# TEST PASSED is printed only when all three report their OK line, so a single failed or
# skipped assertion anywhere fails the gate.
set -u
export PATH=/usr/local/bin:/usr/bin:/usr/sbin:/bin:/sbin
export HOME=/root
export UCI_BIN=/usr/bin/uci

[ -f /etc/banner ] && cat /etc/banner
echo "boot: $(. /etc/openwrt_release 2>/dev/null; echo "${DISTRIB_DESCRIPTION:-StarryWRT}")"
echo "arch: $(uname -m)   hostname: $(uci -c /etc/config get system.@system[0].hostname 2>/dev/null || echo StarryWRT)"
echo "================================================================"

rc=0
run() { name="$1"; shift; echo "== $name =="; if "$@"; then echo "$name: OK"; else echo "$name: FAIL"; rc=1; fi; }

run "uci"       sh /usr/bin/uci-carpet.sh /usr/bin/uci
run "opkg"      sh /usr/bin/opkg-carpet.sh /usr/bin/opkg
run "starrywrt" sh /usr/bin/starrywrt-carpet.sh

echo "================================================================"
if [ "$rc" -eq 0 ]; then
    echo "All StarryWRT software stacks verified on single-core StarryOS."
    echo "TEST PASSED"
else
    echo "TEST FAILED"
fi
