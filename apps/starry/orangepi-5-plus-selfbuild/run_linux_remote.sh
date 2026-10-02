#!/bin/bash
set -euo pipefail

rootfs=${1:-}
run_id=${2:-}
profile=${3:-off}
marker=LINUX-ORANGEPI5PLUS-SELFBUILD
guest=/opt/tgoskits/apps/starry/orangepi-5-plus-selfbuild/guest-selfbuild.sh

[ "$(id -u)" = 0 ] || { echo "run-linux: must run as root" >&2; exit 1; }
[ "$rootfs" = /opt/starry-orangepi5plus-selfbuild/rootfs ] \
    || { echo "run-linux: unexpected rootfs" >&2; exit 1; }
case "$run_id" in ''|*[!A-Za-z0-9._-]*) echo "run-linux: invalid run id" >&2; exit 2 ;; esac
case "$profile" in off|stat|record) ;; *) echo "run-linux: invalid profile" >&2; exit 2 ;; esac

mounted_proc=0
mounted_dev=0
mounted_sys=0
cleanup() {
    if [ "$mounted_sys" = 1 ]; then umount -R "$rootfs/sys" || true; fi
    if [ "$mounted_dev" = 1 ]; then umount -R "$rootfs/dev" || true; fi
    if [ "$mounted_proc" = 1 ]; then umount "$rootfs/proc" || true; fi
}
trap cleanup EXIT

if ! mountpoint -q "$rootfs/proc"; then
    mount --bind /proc "$rootfs/proc"
    mount --make-rslave "$rootfs/proc"
    mounted_proc=1
fi
if ! mountpoint -q "$rootfs/dev"; then
    mount --rbind /dev "$rootfs/dev"
    mount --make-rslave "$rootfs/dev"
    mounted_dev=1
fi
if ! mountpoint -q "$rootfs/sys"; then
    mount --rbind /sys "$rootfs/sys"
    mount --make-rslave "$rootfs/sys"
    mounted_sys=1
fi

sync
echo 3 > /proc/sys/vm/drop_caches
chroot "$rootfs" /usr/bin/timeout --signal=TERM --kill-after=60 21600 \
    /usr/bin/env \
    STARRY_SELFBUILD_MARKER="$marker" \
    STARRY_SELFBUILD_RUN_ID="$run_id" \
    STARRY_SELFBUILD_PROFILE="$profile" \
    /bin/bash "$guest"
