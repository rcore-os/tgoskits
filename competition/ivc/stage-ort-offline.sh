#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
guest_dir=${ORANGEPI_IVC_GUEST_DIR:-/home/orangepi/axvisor-guest}
output_dir=$workspace/tmp/competition/ivc/starry
lease_dir=$workspace/tmp/competition/ivc/board-lease

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

artifact_sources=(
    "$output_dir/starryos-ort.bin"
    "$output_dir/starry-orangepi-5-plus-ort.dtb"
    "$output_dir/starry-ort-rootfs-smoke.img"
)
artifact_names=(
    starryos-ort.bin
    starry-orangepi-5-plus-ort.dtb
    starry-ort-rootfs-smoke.img
)
for artifact in "${artifact_sources[@]}" "$ssh_identity"; do
    if [[ ! -r "$artifact" ]]; then
        echo "Required ORT staging input is not readable: $artifact" >&2
        exit 1
    fi
done
for command_name in cargo cut grep rsync sha256sum ssh; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required ORT staging command not found: $command_name" >&2
        exit 1
    fi
done

mkdir -p "$lease_dir"
lease_log=$(mktemp "$lease_dir/ort-connect.XXXXXX.log")
manifest=$(mktemp "$lease_dir/ort-stage.XXXXXX.sha256")
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
    hash=$(sha256sum "${artifact_sources[index]}" | cut -d ' ' -f 1)
    printf '%s  %s\n' "$hash" "${artifact_names[index]}" >>"$manifest"
done

cd "$workspace"
cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$lease_log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "Board lease ended before ORT staging allocation" >&2
        sed -n '1,160p' "$lease_log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "Timed out while acquiring the $board_type staging lease" >&2
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
rsync -a -e "$rsync_shell" \
    "$manifest" "$ssh_target:$guest_dir/.ort-stage.sha256.new"

ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$guest_dir" "${artifact_names[@]}" <<'REMOTE'
set -eu
guest_dir=$1
shift
for artifact_name in "$@"; do
    mv -f -- "$guest_dir/$artifact_name.new" "$guest_dir/$artifact_name"
done
mv -f -- "$guest_dir/.ort-stage.sha256.new" "$guest_dir/.ort-stage.sha256"
rm -f -- /home/orangepi/ort-result.img /home/orangepi/ort-result.img.new
sync
(
    cd "$guest_dir"
    sha256sum -c .ort-stage.sha256
)
findmnt -n -o SOURCE,FSTYPE,OPTIONS /
echo BOARD_ORT_STAGE_HASHES_VERIFIED
REMOTE

echo "BOARD_ORT_STAGE_PASS destination=$guest_dir artifacts=${#artifact_names[@]}"
