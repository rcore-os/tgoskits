#!/bin/bash
set -euo pipefail

app_root=/opt/starry-orangepi5plus-selfbuild
incoming="$app_root/incoming"
expected_fit_sha=${1:-}
expected_boot_sha=${2:-}
expected_env_sha=${3:-}
expected_root_partuuid=${4:-}

fail() {
    echo "deploy-starry-boot: $*" >&2
    exit 1
}

validate_sha256() {
    "$app_root/validate_sha256.sh" "$1" \
        || fail "invalid SHA-256: $1"
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
validate_sha256 "$expected_fit_sha"
validate_sha256 "$expected_boot_sha"
validate_sha256 "$expected_env_sha"
case "$expected_root_partuuid" in
    ''|*[!0-9A-Fa-f-]*) fail "invalid root PARTUUID" ;;
esac

for artifact in image.fit boot-starryos-emmc.scr starryEnv.txt; do
    [ -s "$incoming/$artifact" ] || fail "missing staged artifact: $artifact"
done
[ -s /boot/boot.scr ] || fail "Linux boot script is missing"
[ -s /boot/boot.scr.tgoskits-backup ] || fail "Linux boot-script backup is missing"
cmp -s /boot/boot.scr /boot/boot.scr.tgoskits-backup \
    || fail "persistent boot script is not the verified Linux default"
if is_starry_boot_script /boot/boot.scr.tgoskits-backup; then
    fail "Linux boot-script backup appears to select StarryOS"
fi

root_device=$(findmnt -n -o SOURCE /)
case "$root_device" in
    /dev/mmcblk[0-9]p[0-9]*) ;;
    *) fail "unexpected Linux root device: $root_device" ;;
esac
root_partuuid=$(blkid -s PARTUUID -o value "$root_device")
[ "$root_partuuid" = "$expected_root_partuuid" ] \
    || fail "root PARTUUID changed: $root_partuuid"
root_options=$(findmnt -n -o OPTIONS /)
case ",$root_options," in
    *,rw,*) ;;
    *) fail "Linux root filesystem is not writable" ;;
esac

grep -qx 'starry_fit=/image.fit' "$incoming/starryEnv.txt" \
    || fail "staged starryEnv.txt has an unexpected FIT path"
grep -qx "starry_root=PARTUUID=$root_partuuid" "$incoming/starryEnv.txt" \
    || fail "staged starryEnv.txt has an unexpected root partition"

install -m 0644 "$incoming/image.fit" /image.fit.new
install -m 0644 "$incoming/boot-starryos-emmc.scr" /boot/boot-starryos-emmc.scr.new
install -m 0644 "$incoming/starryEnv.txt" /boot/starryEnv.txt.new
sha256sum -c - <<< "$expected_fit_sha  /image.fit.new"
sha256sum -c - <<< "$expected_boot_sha  /boot/boot-starryos-emmc.scr.new"
sha256sum -c - <<< "$expected_env_sha  /boot/starryEnv.txt.new"

# Flush every temporary file before its same-filesystem atomic rename. The
# persistent Linux boot selector remains untouched throughout deployment.
sync
mv -f /image.fit.new /image.fit
mv -f /boot/boot-starryos-emmc.scr.new /boot/boot-starryos-emmc.scr
mv -f /boot/starryEnv.txt.new /boot/starryEnv.txt
sync

sha256sum -c - <<< "$expected_fit_sha  /image.fit"
sha256sum -c - <<< "$expected_boot_sha  /boot/boot-starryos-emmc.scr"
sha256sum -c - <<< "$expected_env_sha  /boot/starryEnv.txt"
cmp -s /boot/boot.scr /boot/boot.scr.tgoskits-backup \
    || fail "deployment changed the persistent Linux boot script"

echo "starry_boot_remote_fit_sha256=$expected_fit_sha"
echo "starry_boot_remote_root_partuuid=$root_partuuid"
echo "starry_boot_remote_linux_default=verified"
