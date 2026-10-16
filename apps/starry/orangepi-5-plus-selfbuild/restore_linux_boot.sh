#!/bin/sh
set -eu

marker=STARRY-ORANGEPI5PLUS-SELFBUILD
linux_boot=/boot/boot.scr.tgoskits-backup
starry_boot=/boot/boot-starryos-emmc.scr
active_boot=/boot/boot.scr

fail() {
    echo "===${marker}-LINUX-BOOT-RESTORE-FAIL reason=$1==="
    exit 1
}

[ -s "$linux_boot" ] || fail linux-boot-backup-missing
[ -s "$starry_boot" ] || fail starry-boot-script-missing
if cmp -s "$active_boot" "$linux_boot"; then
    echo "===${marker}-LINUX-BOOT-RESTORED state=already-linux==="
    exit 0
fi
cmp -s "$active_boot" "$starry_boot" || fail active-boot-script-unknown
[ "$(stat -c %s "$linux_boot")" = "$(stat -c %s "$active_boot")" ] \
    || fail boot-slot-size-mismatch
# Preserve the extents U-Boot already sees; it cannot replay journal-only
# inode changes made by truncating and extending the active boot script.
dd if="$linux_boot" of="$active_boot" conv=notrunc status=none || fail linux-boot-copy
cmp -s "$active_boot" "$linux_boot" || fail linux-boot-copy-verify
sync "$active_boot" || fail linux-boot-copy-sync
sync || fail linux-boot-activate-sync
cmp -s "$active_boot" "$linux_boot" || fail linux-boot-activate-verify
echo "===${marker}-LINUX-BOOT-RESTORED state=switched==="
