#!/bin/sh
# Restore Linux recovery before entering the persistent build environment.
set -eu
trap 'rc=$?; if [ "$rc" -ne 0 ]; then printf "===STARRY-ORANGEPI5PLUS-SELFBUILD-FAIL rc=%s===\n" "$rc"; fi' EXIT

app_dir=/opt/starry-orangepi5plus-selfbuild
rootfs=$app_dir/rootfs
run_id=${1:-kernel-cold}
"$app_dir/restore_linux_boot.sh"
sha256sum /boot/boot.scr /boot/boot.scr.tgoskits-backup
[ -f "$rootfs/opt/tgoskits/Cargo.toml" ]
[ -x "$rootfs/usr/local/bin/tg-xtask" ]
[ -x "$rootfs/usr/bin/timeout" ]
build_epoch=$(sed -n 's/^build_epoch=//p' "$rootfs/etc/starry-selfbuild/run.conf")
case "$build_epoch" in
    ''|*[!0-9]*) exit 2 ;;
esac
[ "$build_epoch" -ge 1609459200 ]
"$app_dir/set_guest_clock.sh" STARRY-ORANGEPI5PLUS-SELFBUILD "$build_epoch"
for directory in proc dev sys; do
    [ -d "$rootfs/$directory" ]
    if mountpoint -q "$rootfs/$directory"; then
        printf 'build_mount_present=%s\n' "$directory"
    else
        mount --bind "/$directory" "$rootfs/$directory"
    fi
done
chroot "$rootfs" /usr/bin/timeout --signal=TERM --kill-after=60 21600 \
    /bin/bash /guest-kernel-selfbuild.sh "$run_id"
