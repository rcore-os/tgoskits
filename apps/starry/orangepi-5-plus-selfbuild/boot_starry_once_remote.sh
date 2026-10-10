#!/bin/bash
set -euo pipefail

app_root=/opt/starry-orangepi5plus-selfbuild
expected_fit_sha=${1:-}
expected_boot_sha=${2:-}
expected_root_partuuid=${3:-}

fail() {
    echo "boot-starry-once: $*" >&2
    exit 1
}

is_starry_boot_script() {
    local identity_rc
    set +e
    "$app_root/boot_script_is_starry.sh" "$1"
    identity_rc=$?
    set -e
    case "$identity_rc" in
        0) return 0 ;;
        1) return 1 ;;
        *) fail "cannot inspect U-Boot script: $1" ;;
    esac
}

[ "$(id -u)" = 0 ] || fail "must run as root"
"$app_root/validate_sha256.sh" "$expected_fit_sha" \
    || fail "invalid FIT SHA-256"
"$app_root/validate_sha256.sh" "$expected_boot_sha" \
    || fail "invalid boot-script SHA-256"
case "$expected_root_partuuid" in
    ''|*[!0-9A-Fa-f-]*) fail "invalid root PARTUUID" ;;
esac

[ -s /boot/boot.scr.tgoskits-backup ] || fail "Linux boot-script backup is missing"
[ -s /boot/boot-starryos-emmc.scr ] || fail "StarryOS boot script is missing"
[ -s /image.fit ] || fail "StarryOS FIT is missing"
cmp -s /boot/boot.scr /boot/boot.scr.tgoskits-backup \
    || fail "current boot script is not the verified Linux default"
if is_starry_boot_script /boot/boot.scr.tgoskits-backup; then
    fail "Linux boot-script backup appears to select StarryOS"
fi
"$app_root/ensure_linux_fsck.sh" /boot/armbianEnv.txt \
    || fail "cannot arm the forced Linux filesystem check"

sha256sum -c - <<< "$expected_fit_sha  /image.fit"
sha256sum -c - <<< "$expected_boot_sha  /boot/boot-starryos-emmc.scr"
grep -qx 'starry_fit=/image.fit' /boot/starryEnv.txt \
    || fail "starryEnv.txt has an unexpected FIT path"
grep -qx "starry_root=PARTUUID=$expected_root_partuuid" /boot/starryEnv.txt \
    || fail "starryEnv.txt has an unexpected root partition"

root_device=$(findmnt -n -o SOURCE /)
root_partuuid=$(blkid -s PARTUUID -o value "$root_device")
[ "$root_partuuid" = "$expected_root_partuuid" ] \
    || fail "Linux root PARTUUID changed: $root_partuuid"
root_options=$(findmnt -n -o OPTIONS /)
case ",$root_options," in
    *,rw,*) ;;
    *) fail "Linux root filesystem is not writable" ;;
esac

install -m 0644 /boot/boot-starryos-emmc.scr /boot/boot.scr.new
cmp -s /boot/boot.scr.new /boot/boot-starryos-emmc.scr \
    || fail "temporary StarryOS boot selector differs from its source"
sync
mv -f /boot/boot.scr.new /boot/boot.scr
sync
cmp -s /boot/boot.scr /boot/boot-starryos-emmc.scr \
    || fail "failed to activate the verified StarryOS boot selector"

echo "starry_boot_previous_linux_boot_id=$(cat /proc/sys/kernel/random/boot_id)"
echo "starry_boot_selector_sha256=$expected_boot_sha"
echo "starry_boot_linux_fsck=armed"
echo "starry_boot_reboot=scheduled"
systemctl --force --force reboot
