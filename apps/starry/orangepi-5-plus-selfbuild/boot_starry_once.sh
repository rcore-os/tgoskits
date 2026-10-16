#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
stage_meta="$repo_root/tmp/starry-orangepi5plus-selfbuild/staged-boot.meta"
remote_app=/opt/starry-orangepi5plus-selfbuild
linux_user="${BOARD_LINUX_USER:-orangepi}"
ssh_port="${BOARD_SSH_PORT:-22}"
host=""

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/orangepi-5-plus-selfbuild/boot_starry_once.sh --host BOARD_IP

Requires a serial monitor to be open already. The command verifies the staged
FIT metadata, atomically selects the verified StarryOS boot script from board
Linux, syncs the filesystem, and immediately reboots. It never interrupts
U-Boot and never sends U-Boot commands.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) host="${2:-}"; shift 2 ;;
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
[ -s "$stage_meta" ] || { echo "staged boot metadata is missing: $stage_meta" >&2; exit 1; }

meta_value() {
    local key=$1
    sed -n "s/^${key}=//p" "$stage_meta"
}

staged_host=$(meta_value host)
fit_sha=$(meta_value fit_sha256)
boot_sha=$(meta_value boot_sha256)
root_partuuid=$(meta_value root_partuuid)
[ "$staged_host" = "$host" ] || {
    echo "staged artifacts target $staged_host, not $host" >&2
    exit 1
}
"$app_dir/validate_sha256.sh" "$fit_sha" || { echo "invalid staged FIT hash" >&2; exit 1; }
"$app_dir/validate_sha256.sh" "$boot_sha" || { echo "invalid staged boot hash" >&2; exit 1; }
case "$root_partuuid" in
    ''|*[!0-9A-Fa-f-]*) echo "invalid staged root PARTUUID" >&2; exit 1 ;;
esac

remote="${linux_user}@${host}"
command -v timeout >/dev/null 2>&1 || {
    echo "timeout not found" >&2
    exit 1
}
ssh_args=(
    -p "$ssh_port"
    -o ServerAliveInterval=2
    -o ServerAliveCountMax=2
)
rsync -a -e "ssh -p $ssh_port" \
    "$app_dir/boot_script_is_starry.sh" \
    "$app_dir/boot_starry_once_remote.sh" \
    "$app_dir/ensure_linux_fsck.sh" \
    "$app_dir/validate_sha256.sh" \
    "$remote:$remote_app/incoming/"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/validate_sha256.sh" "$remote_app/validate_sha256.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/boot_script_is_starry.sh" \
    "$remote_app/boot_script_is_starry.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/boot_starry_once_remote.sh" \
    "$remote_app/boot_starry_once_remote.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/ensure_linux_fsck.sh" \
    "$remote_app/ensure_linux_fsck.sh"

set +e
timeout --signal=TERM --kill-after=2 30 \
    ssh "${ssh_args[@]}" "$remote" sudo -n \
    "$remote_app/boot_starry_once_remote.sh" \
    "$fit_sha" "$boot_sha" "$root_partuuid"
ssh_rc=$?
set -e
if [ "$ssh_rc" != 0 ] && [ "$ssh_rc" != 124 ] && [ "$ssh_rc" != 255 ]; then
    echo "board rejected the one-time StarryOS boot with rc=$ssh_rc" >&2
    exit "$ssh_rc"
fi
echo "starry_boot_once_ssh_rc=$ssh_rc"

reboot_observed=0
for _ in $(seq 1 20); do
    set +e
    ssh -o BatchMode=yes -o ConnectTimeout=1 "${ssh_args[@]}" "$remote" true \
        >/dev/null 2>&1
    probe_rc=$?
    set -e
    if [ "$probe_rc" != 0 ]; then
        reboot_observed=1
        break
    fi
    sleep 1
done

if [ "$reboot_observed" = 0 ]; then
    echo "board remained in Linux after selecting StarryOS; restoring Linux boot" >&2
    ssh "${ssh_args[@]}" "$remote" sudo -n "$remote_app/restore_linux_boot.sh"
    echo "board did not start the requested reboot; Linux boot was restored" >&2
    exit 1
fi
echo "starry_boot_reboot_transition=observed"
