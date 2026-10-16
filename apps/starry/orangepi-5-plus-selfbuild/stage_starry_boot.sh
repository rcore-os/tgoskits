#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
out_dir="$repo_root/tmp/starry-orangepi5plus-selfbuild"
remote_app=/opt/starry-orangepi5plus-selfbuild
linux_user="${BOARD_LINUX_USER:-orangepi}"
ssh_port="${BOARD_SSH_PORT:-22}"
host=""
build_config="$app_dir/build-aarch64-unknown-none-softfloat.toml"
skip_build=0

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/orangepi-5-plus-selfbuild/stage_starry_boot.sh --host BOARD_IP \
    [--config BUILD_CONFIG] [--skip-build]

Build a StarryOS kernel, package it with the device tree exported by the
currently running board Linux, and atomically deploy the verified FIT and
one-time U-Boot script. The command does not change /boot/boot.scr and does not
reboot the board.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) host="${2:-}"; shift 2 ;;
        --config) build_config="${2:-}"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[[ "$host" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid --host" >&2; exit 2; }
[[ "$linux_user" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]] \
    || { echo "invalid BOARD_LINUX_USER" >&2; exit 2; }
if ! [[ "$ssh_port" =~ ^[0-9]+$ ]] \
    || ((10#$ssh_port < 1 || 10#$ssh_port > 65535)); then
    echo "invalid BOARD_SSH_PORT" >&2
    exit 2
fi
case "$build_config" in
    /*) ;;
    *) build_config="$repo_root/$build_config" ;;
esac
[ -f "$build_config" ] || { echo "build config is missing: $build_config" >&2; exit 2; }

for command in cargo cmp dumpimage fdtget mkimage rsync sha256sum ssh stat truncate; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "$command not found" >&2
        exit 1
    }
done

if [ "$skip_build" = 0 ]; then
    (
        cd "$repo_root"
        cargo xtask starry build --config "$build_config"
    )
fi

kernel="$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin"
[ -s "$kernel" ] || { echo "StarryOS binary is missing: $kernel" >&2; exit 1; }

mkdir -p "$out_dir"
stage_dir="$(mktemp -d -p "$out_dir" direct-boot.XXXXXX)"
cleanup() {
    rm -rf "$stage_dir"
}
trap cleanup EXIT

remote="${linux_user}@${host}"
ssh_args=(-p "$ssh_port" -o BatchMode=yes -o ConnectTimeout=8)
if [ -n "${BOARD_SSH_HOST_KEY_ALIAS:-}" ]; then
    [[ "$BOARD_SSH_HOST_KEY_ALIAS" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid BOARD_SSH_HOST_KEY_ALIAS" >&2; exit 2; }
    ssh_args+=(-o "HostKeyAlias=$BOARD_SSH_HOST_KEY_ALIAS")
fi
rsync_ssh="ssh ${ssh_args[*]}"
ssh "${ssh_args[@]}" "$remote" true
ssh "${ssh_args[@]}" "$remote" sudo -n cat /boot/boot.scr \
    > "$stage_dir/linux-boot.scr"
[ -s "$stage_dir/linux-boot.scr" ] || {
    echo "/boot/boot.scr is empty" >&2
    exit 1
}
if "$app_dir/boot_script_is_starry.sh" "$stage_dir/linux-boot.scr"; then
    echo "/boot/boot.scr is not the verified Linux default" >&2
    exit 1
fi
linux_boot_bytes=$(stat -c %s "$stage_dir/linux-boot.scr")

cp "$kernel" "$stage_dir/starryos.bin"
cp "$app_dir/orangepi5plus-selfbuild.its" "$stage_dir/image.its"
cp "$app_dir/boot-starryos-emmc.cmd" "$stage_dir/boot.cmd"
ssh "${ssh_args[@]}" "$remote" sudo -n cat /sys/firmware/fdt \
    > "$stage_dir/board.dtb"
fdtget "$stage_dir/board.dtb" / compatible >/dev/null
(
    cd "$stage_dir"
    mkimage -f image.its image.fit
    mkimage -A arm64 -O linux -T script -C none \
        -n 'TGOSKits StarryOS one-time eMMC boot' \
        -d boot.cmd boot-starryos-emmc.scr
)
# U-Boot does not replay ext4's journal. Keep the Linux boot slot's geometry
# so Starry can restore its bytes without truncating or reallocating extents.
starry_boot_bytes=$(stat -c %s "$stage_dir/boot-starryos-emmc.scr")
[ "$starry_boot_bytes" -le "$linux_boot_bytes" ] || {
    echo "StarryOS boot script does not fit the Linux boot slot" >&2
    exit 1
}
truncate -s "$linux_boot_bytes" "$stage_dir/boot-starryos-emmc.scr"
dumpimage -T flat_dt -p 0 -o "$stage_dir/fit-kernel.bin" "$stage_dir/image.fit"
dumpimage -T flat_dt -p 1 -o "$stage_dir/fit-board.dtb" "$stage_dir/image.fit"
cmp -s "$stage_dir/starryos.bin" "$stage_dir/fit-kernel.bin" || {
    echo "FIT-embedded kernel differs from the current seed kernel" >&2
    exit 1
}
cmp -s "$stage_dir/board.dtb" "$stage_dir/fit-board.dtb" || {
    echo "FIT-embedded device tree differs from the board-exported DTB" >&2
    exit 1
}

root_device="$(ssh "${ssh_args[@]}" "$remote" findmnt -n -o SOURCE /)"
case "$root_device" in
    /dev/mmcblk[0-9]p[0-9]*) ;;
    *) echo "unexpected board Linux root device: $root_device" >&2; exit 1 ;;
esac
root_partuuid="$(ssh "${ssh_args[@]}" "$remote" \
    sudo -n blkid -s PARTUUID -o value "$root_device")"
case "$root_partuuid" in
    ''|*[!0-9A-Fa-f-]*)
        echo "invalid board Linux root PARTUUID: $root_partuuid" >&2
        exit 1
        ;;
esac
printf 'starry_fit=/image.fit\nstarry_root=PARTUUID=%s\n' "$root_partuuid" \
    > "$stage_dir/starryEnv.txt"
fit_sha=$(sha256sum "$stage_dir/image.fit")
fit_sha=${fit_sha%% *}
boot_sha=$(sha256sum "$stage_dir/boot-starryos-emmc.scr")
boot_sha=${boot_sha%% *}
env_sha=$(sha256sum "$stage_dir/starryEnv.txt")
env_sha=${env_sha%% *}
fdt_sha=$(sha256sum "$stage_dir/board.dtb")
fdt_sha=${fdt_sha%% *}

ssh "${ssh_args[@]}" "$remote" sudo -n install -d -m 0755 "$remote_app/incoming"
rsync -a -e "$rsync_ssh" \
    "$stage_dir/image.fit" \
    "$stage_dir/boot-starryos-emmc.scr" \
    "$stage_dir/starryEnv.txt" \
    "$app_dir/boot_script_is_starry.sh" \
    "$app_dir/deploy_starry_boot_remote.sh" \
    "$app_dir/ensure_linux_fsck.sh" \
    "$app_dir/init.sh" \
    "$app_dir/init-kernel-selfbuild.sh" \
    "$app_dir/guest-kernel-selfbuild.sh" \
    "$app_dir/restore_linux_boot.sh" \
    "$app_dir/set_guest_clock.sh" \
    "$app_dir/validate_sha256.sh" \
    "$remote:$remote_app/incoming/"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/validate_sha256.sh" "$remote_app/validate_sha256.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/boot_script_is_starry.sh" \
    "$remote_app/boot_script_is_starry.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/deploy_starry_boot_remote.sh" \
    "$remote_app/deploy_starry_boot_remote.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/ensure_linux_fsck.sh" \
    "$remote_app/ensure_linux_fsck.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/init.sh" "$remote_app/init.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/restore_linux_boot.sh" "$remote_app/restore_linux_boot.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/set_guest_clock.sh" "$remote_app/set_guest_clock.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/init-kernel-selfbuild.sh" "$remote_app/init-kernel-selfbuild.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/guest-kernel-selfbuild.sh" "$remote_app/rootfs/guest-kernel-selfbuild.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n \
    "$remote_app/deploy_starry_boot_remote.sh" \
    "$fit_sha" "$boot_sha" "$env_sha" "$root_partuuid"

mkdir -p "$out_dir"
cat > "$out_dir/staged-boot.meta" <<EOF
host=$host
fit_sha256=$fit_sha
boot_sha256=$boot_sha
env_sha256=$env_sha
fdt_sha256=$fdt_sha
root_partuuid=$root_partuuid
EOF

echo "starry_boot_fit_sha256=$fit_sha"
echo "starry_boot_fit_bytes=$(stat -c %s "$stage_dir/image.fit")"
kernel_sha=$(sha256sum "$kernel")
echo "starry_boot_kernel_sha256=${kernel_sha%% *}"
echo "starry_boot_linux_fdt_sha256=$fdt_sha"
echo "starry_boot_root_partuuid=$root_partuuid"
echo "starry_boot_linux_default=verified_unchanged"
