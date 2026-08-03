#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
guest_dir=${ORANGEPI_RT_GUEST_DIR:-/home/orangepi/axvisor-guest}
kernel=$workspace/tmp/axvisor-rt/starryos-rt.bin
dtb=$workspace/tmp/competition/ivc/starry/starry-orangepi-5-plus.dtb
rootfs=$workspace/tmp/axvisor-rt/starry-rt-capture-rootfs.img
rootfs_name=starry-rt-capture-rootfs.img
noise_guest=

usage() {
    cat <<'EOF'
usage: stage-starry-board.sh [options]

Options:
  --kernel PATH          StarryOS kernel binary
  --dtb PATH             StarryOS guest DTB
  --rootfs PATH          StarryOS RT rootfs image
  --rootfs-name NAME     Remote rootfs basename
  --noise-guest PATH     Optional bounded AArch64 noise guest image
  --guest-dir PATH       Remote directory below /home/orangepi
EOF
}

while (($# > 0)); do
    case "$1" in
        --kernel) kernel=${2:?--kernel requires a value}; shift 2 ;;
        --dtb) dtb=${2:?--dtb requires a value}; shift 2 ;;
        --rootfs) rootfs=${2:?--rootfs requires a value}; shift 2 ;;
        --rootfs-name) rootfs_name=${2:?--rootfs-name requires a value}; shift 2 ;;
        --noise-guest) noise_guest=${2:?--noise-guest requires a value}; shift 2 ;;
        --guest-dir) guest_dir=${2:?--guest-dir requires a value}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$guest_dir" in
    /home/orangepi/*) ;;
    *)
        echo "--guest-dir must remain below /home/orangepi: $guest_dir" >&2
        exit 1
        ;;
esac
case "$rootfs_name" in
    ''|*/*|*[!A-Za-z0-9_.-]*)
        echo "--rootfs-name must be a safe basename: $rootfs_name" >&2
        exit 1
        ;;
esac
if [[ "$guest_dir" =~ [^A-Za-z0-9_./-] ]]; then
    echo "--guest-dir contains unsupported characters: $guest_dir" >&2
    exit 1
fi

artifact_sources=("$kernel" "$dtb" "$rootfs")
artifact_names=(starryos.bin starry-orangepi-5-plus.dtb "$rootfs_name")
if [[ -n "$noise_guest" ]]; then
    artifact_sources+=("$noise_guest")
    artifact_names+=(aarch64-rt-noise.bin)
fi
for input in "${artifact_sources[@]}" "$ssh_identity"; do
    if [[ ! -r "$input" ]]; then
        echo "required staging input is not readable: $input" >&2
        exit 1
    fi
done
for command in cargo cut grep mktemp rsync sha256sum ssh; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "required staging command not found: $command" >&2
        exit 1
    }
done

lease_dir=$workspace/tmp/axvisor-rt/board-lease
mkdir -p "$lease_dir"
lease_log=$(mktemp "$lease_dir/connect.XXXXXX.log")
manifest=$(mktemp "$lease_dir/stage.XXXXXX.sha256")
lease_pid=
cleanup() {
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    rm -f -- "$lease_log" "$manifest"
}
trap cleanup EXIT HUP INT TERM

for index in "${!artifact_sources[@]}"; do
    hash=$(sha256sum "${artifact_sources[index]}")
    hash=${hash%% *}
    printf '%s  %s\n' "$hash" "${artifact_names[index]}" >>"$manifest"
done

cd "$workspace"
cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$lease_log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "board lease ended before staging allocation" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "timed out acquiring $board_type for staging" >&2
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
printf -v rsync_shell \
    'ssh -i %q -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8' \
    "$ssh_identity"
for index in "${!artifact_sources[@]}"; do
    rsync -a --info=stats1 -e "$rsync_shell" \
        "${artifact_sources[index]}" \
        "$ssh_target:$guest_dir/${artifact_names[index]}.new"
done
rsync -a -e "$rsync_shell" "$manifest" "$ssh_target:$guest_dir/.rt-stage.sha256.new"

ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$guest_dir" "${artifact_names[@]}" <<'REMOTE'
set -eu
guest_dir=$1
shift
for artifact_name in "$@"; do
    mv -f -- "$guest_dir/$artifact_name.new" "$guest_dir/$artifact_name"
done
mv -f -- "$guest_dir/.rt-stage.sha256.new" "$guest_dir/.rt-stage.sha256"
sync
(
    cd "$guest_dir"
    sha256sum -c .rt-stage.sha256
)
findmnt -n -o SOURCE,FSTYPE,OPTIONS /
echo AXVISOR_RT_BOARD_STAGE_PASS
REMOTE

kill -TERM "$lease_pid" 2>/dev/null || true
wait "$lease_pid" 2>/dev/null || true
lease_pid=

noise_artifact=none
if [[ -n "$noise_guest" ]]; then
    noise_artifact=aarch64-rt-noise.bin
fi
echo "AXVISOR_RT_BOARD_STAGE_COMPLETE destination=$guest_dir rootfs=$rootfs_name noise=$noise_artifact"
