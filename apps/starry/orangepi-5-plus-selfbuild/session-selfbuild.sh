#!/bin/sh
set -eu

base=/opt/tgoskits-selfbuild-app
url=${STARRY_SELFBUILD_URL:?}
marker=STARRY-ORANGEPI5PLUS-SELFBUILD
lock=/dev/shm/tgoskits-selfbuild-active
run_id=board-$(cat "$base/session/epoch")
run="$base/runs/$run_id"
root="$run/root"
loop=""
mounted_root=0
mounted_work=0
mounted_workspace_tmp=0
native_tmpfs=0
mounted_output=0
mounted_proc=0
mounted_dev=0
mounted_sys=0
locked=0
# Use the mount interface implemented by StarryOS for ext4 and bind mounts.
export LIBMOUNT_FORCE_MOUNT2=always

cleanup() {
    status=$?
    released=1
    if [ "$mounted_sys" = 1 ]; then umount "$root/sys" || { status=1; released=0; }; fi
    if [ "$mounted_dev" = 1 ]; then umount "$root/dev" || { status=1; released=0; }; fi
    if [ "$mounted_proc" = 1 ]; then umount "$root/proc" || { status=1; released=0; }; fi
    if [ "$mounted_output" = 1 ]; then umount "$root/output" || { status=1; released=0; }; fi
    if [ "$native_tmpfs" = 1 ] && [ ! -f "$run/work/native-target-released" ]; then
        umount "$root/work/targets" || { status=1; released=0; }
    fi
    if [ "$mounted_workspace_tmp" = 1 ]; then umount "$root/opt/tgoskits/tmp" || { status=1; released=0; }; fi
    if [ "$mounted_work" = 1 ]; then umount "$root/work" || { status=1; released=0; }; fi
    if [ "$mounted_root" = 1 ]; then umount "$root" || { status=1; released=0; }; fi
    if [ -n "$loop" ]; then losetup -d "$loop" || { status=1; released=0; }; fi
    if [ "$locked" = 1 ] && [ "$released" = 1 ]; then rmdir "$lock" || status=1; fi
    if [ "$status" != 0 ]; then
        printf '===%s-FAIL reason=session-runner rc=%s===\n' "$marker" "$status"
    else
        printf '===%s-SESSION-PASS run=%s===\n' "$marker" "$run_id"
    fi
    trap - EXIT
    exit "$status"
}
trap cleanup EXIT

fail() {
    printf '===%s-FAIL reason=%s===\n' "$marker" "$1"
    exit 1
}

case "$run_id" in ''|*[!A-Za-z0-9._-]*) fail invalid-run-id ;; esac
[ -d /dev/shm ] || fail shared-memory-missing
mkdir "$lock" || fail session-already-active
locked=1
mkdir -p "$base/runs" "$base/output/runs"
mkdir "$run" || fail run-already-exists
date -u -s "@$(cat "$base/session/epoch")" || fail clock
python3 "$base/session/receive_image.py" "$url" "$base" || fail image-transfer
mkdir "$root" "$run/work"
mkdir "$run/work/workspace-tmp"
mkdir "$run/work/targets"
loop_candidate=$(losetup -f) || fail no-free-loop-device
losetup -r "$loop_candidate" "$base/image.ext4" || fail loop-setup
loop=$loop_candidate
mount -t ext4 -o ro "$loop" "$root" || fail mount-image
mounted_root=1
mount --bind "$run/work" "$root/work" || fail mount-work
mounted_work=1
if [ "${KEEP_TARGET:-0}" != 1 ]; then
    mount -t tmpfs -o size=4G tmpfs "$root/work/targets" || fail mount-native-target
    native_tmpfs=1
fi
mount --bind "$run/work/workspace-tmp" "$root/opt/tgoskits/tmp" || fail mount-workspace-tmp
mounted_workspace_tmp=1
mount --bind "$base/output" "$root/output" || fail mount-output
mounted_output=1
mount --bind /proc "$root/proc" || fail mount-proc
mounted_proc=1
mount --bind /dev "$root/dev" || fail mount-dev
mounted_dev=1
mount --bind /sys "$root/sys" || fail mount-sys
mounted_sys=1
chroot "$root" /usr/bin/env HOME=/root CARGO_HOME=/root/.cargo CARGO_NET_OFFLINE=true \
    STARRY_SELFBUILD_RUN_ID="$run_id" \
    STARRY_SELFBUILD_NATIVE_TMPFS="$native_tmpfs" \
    /usr/bin/timeout --signal=TERM --kill-after=60 21600 \
    /bin/bash /opt/tgoskits/apps/starry/orangepi-5-plus-selfbuild/guest-selfbuild.sh
