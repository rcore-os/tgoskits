#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:?set ORANGEPI_SSH_TARGET, for example orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:?set ORANGEPI_SSH_IDENTITY to the board SSH private key}
guest_dir=${ORANGEPI_IVC_GUEST_DIR:-/home/orangepi/axvisor-guest}
kernel=${IVC_LINUX_KERNEL:-$workspace/tmp/competition/ivc/linux/linux-qemu}
initramfs=$workspace/tmp/competition/ivc/linux/initramfs.cpio.gz
guest_dtb=$workspace/tmp/competition/ivc/linux/orangepi-5-plus.dtb
lease_dir=$workspace/tmp/competition/ivc/board-lease

if [[ "$kernel" != /* ]]; then
    kernel=$workspace/$kernel
fi

case "$guest_dir" in
    /home/orangepi/*) ;;
    *)
        echo "ORANGEPI_IVC_GUEST_DIR must remain below /home/orangepi: $guest_dir" >&2
        exit 1
        ;;
esac
if [[ "$guest_dir" =~ [^A-Za-z0-9_./-] ]]; then
    echo "ORANGEPI_IVC_GUEST_DIR contains unsupported characters: $guest_dir" >&2
    exit 1
fi
for artifact in "$kernel" "$initramfs" "$guest_dtb" "$ssh_identity"; do
    if [[ ! -r "$artifact" ]]; then
        echo "Required Orange Pi staging input is not readable: $artifact" >&2
        exit 1
    fi
done

mkdir -p "$lease_dir"
lease_log=$(mktemp "$lease_dir/connect.XXXXXX.log")
lease_pid=
cleanup() {
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    rm -f -- "$lease_log"
}
trap cleanup EXIT HUP INT TERM

cd "$workspace"
cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$lease_log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "Board lease ended before allocation" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "Timed out while acquiring the $board_type lease" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    sleep 0.2
done

ssh_options=(
    -i "$ssh_identity"
    -o IdentitiesOnly=yes
    -o BatchMode=yes
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=8
)
ssh "${ssh_options[@]}" "$ssh_target" mkdir -p -- "$guest_dir"
printf -v rsync_shell 'ssh -i %q -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8' "$ssh_identity"
rsync -a --info=stats1 -e "$rsync_shell" "$kernel" "$ssh_target:$guest_dir/linux-qemu.new"
rsync -a --info=stats1 -e "$rsync_shell" "$initramfs" "$ssh_target:$guest_dir/initramfs.cpio.gz.new"
rsync -a --info=stats1 -e "$rsync_shell" "$guest_dtb" "$ssh_target:$guest_dir/orangepi-5-plus.dtb.new"

ssh "${ssh_options[@]}" "$ssh_target" sh -s -- "$guest_dir" <<'REMOTE'
set -eu
guest_dir=$1
mv -f -- "$guest_dir/linux-qemu.new" "$guest_dir/linux-qemu"
mv -f -- "$guest_dir/initramfs.cpio.gz.new" "$guest_dir/initramfs.cpio.gz"
mv -f -- "$guest_dir/orangepi-5-plus.dtb.new" "$guest_dir/orangepi-5-plus.dtb"
sync
sha256sum \
    "$guest_dir/linux-qemu" \
    "$guest_dir/initramfs.cpio.gz" \
    "$guest_dir/orangepi-5-plus.dtb"
findmnt -n -o SOURCE,FSTYPE,OPTIONS /
REMOTE

echo "BOARD_STAGE_PASS destination=$guest_dir"
