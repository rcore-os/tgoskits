#!/bin/sh
# run-openwrt.sh - on-target gate for the StarryOS OpenWrt-userland carpet (uci + opkg).
#
# Staged into the rootfs by prebuild.sh and invoked as the ENTIRE shell_init_cmd
# (`sh /usr/bin/run-openwrt.sh`). The gate lives in a staged script, not inline in the toml,
# so the harness never echoes a literal TEST PASSED back over the serial console and
# self-matches success_regex: TEST PASSED is printed ONLY here, ONLY when BOTH carpets report
# their pinned OK line (each carpet prints "<TOOL> CARPET OK <n>" only when every assertion
# passed AND the count equals its pinned total - a skipped assertion changes the total and
# fails the gate).
#
# uci  drives the full Unified Configuration Interface command + option surface against a
#      synthetic /etc/config tree (get/set/commit/add/add_list/del_list/delete/rename/reorder/
#      revert/changes/export/import/batch and -c/-d/-q/-s/-S/-X/-n/-N/-f).
# opkg drives the full .ipk package-manager surface against a hermetic offline feed the carpet
#      builds at runtime (update/list/install-with-deps/remove/upgrade/files/status/info/
#      depends/whatdepends/flag/compare-versions/print-architecture and the force/offline-root
#      options) - no network, no committed packages.
set -u
export PATH=/usr/local/bin:/usr/bin:/usr/sbin:/bin:/sbin
export HOME=/root

echo "----------------------------------------------------------------"
echo " StarryWRT userland carpet - OpenWrt uci + opkg on StarryOS"
echo " arch: $(uname -m)   kernel: $(uname -s)"
echo "----------------------------------------------------------------"

rc=0

echo "== uci carpet =="
if sh /usr/bin/uci-carpet.sh /usr/bin/uci; then
    echo "uci: OK"
else
    echo "uci: FAIL"; rc=1
fi

echo "== opkg carpet =="
if sh /usr/bin/opkg-carpet.sh /usr/bin/opkg; then
    echo "opkg: OK"
else
    echo "opkg: FAIL"; rc=1
fi

if [ "$rc" -eq 0 ]; then
    echo "TEST PASSED"
else
    echo "TEST FAILED"
fi
