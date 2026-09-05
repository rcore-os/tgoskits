#!/usr/bin/env bash
# Stage sysbench onto the board's ext4 rootfs — run this ON THE BOARD LINUX
# (e.g. `ssh orangepi@192.168.50.2 'bash -s' < deploy-sysbench.sh`) BEFORE
# booting StarryOS.
#
# StarryOS on this board runs against the SAME ext4 rootfs (mmcblk1p2) as the
# board Linux, so a glibc sysbench installed here is what StarryOS executes
# (StarryOS runs glibc dynamic binaries — cf. the glibc-dynamic-smoke app).
#
# The `sync` is mandatory: the root is mounted commit=600, so a freshly written
# binary lives only in page cache for up to 10 min. Power-cycling into StarryOS
# re-mounts the ext4 and would NOT see an unsynced file → `sysbench: not found`.
set -euo pipefail

SUDO="sudo"
# Non-interactive sudo on the OrangePi (password: orangepi).
if ! sudo -n true 2>/dev/null; then
  SUDO="sudo -S"
  export SUDO_ASKPASS=/bin/false
fi

echo orangepi | $SUDO apt-get update
echo orangepi | $SUDO apt-get install -y sysbench

command -v sysbench
echo orangepi | $SUDO sync
echo "sysbench staged and synced: $(sysbench --version 2>&1 | head -1)"
