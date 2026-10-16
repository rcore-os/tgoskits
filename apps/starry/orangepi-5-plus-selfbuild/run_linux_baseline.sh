#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
remote_app=/opt/starry-orangepi5plus-selfbuild
remote_rootfs="$remote_app/rootfs"
linux_user="${BOARD_LINUX_USER:-orangepi}"
ssh_port="${BOARD_SSH_PORT:-22}"
host=""
run_id="linux-$(date -u +%Y%m%dT%H%M%SZ)"
profile=off
skip_provision=0

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/orangepi-5-plus-selfbuild/run_linux_baseline.sh --host BOARD_IP \
    [--run-id RUN_ID] [--profile off|stat|record] [--skip-provision]
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) host="${2:-}"; shift 2 ;;
        --run-id) run_id="${2:-}"; shift 2 ;;
        --profile) profile="${2:-}"; shift 2 ;;
        --skip-provision) skip_provision=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[[ "$host" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid --host" >&2; exit 2; }
[[ "$linux_user" =~ ^[A-Za-z_][A-Za-z0-9_-]*$ ]] \
    || { echo "invalid BOARD_LINUX_USER" >&2; exit 2; }
if ! [[ "$ssh_port" =~ ^[0-9]+$ ]]; then
    echo "invalid BOARD_SSH_PORT" >&2
    exit 2
fi
if ((10#$ssh_port < 1 || 10#$ssh_port > 65535)); then
    echo "invalid BOARD_SSH_PORT" >&2
    exit 2
fi
case "$run_id" in ''|*[!A-Za-z0-9._-]*) echo "invalid run id" >&2; exit 2 ;; esac
case "$profile" in off|stat|record) ;; *) echo "invalid profile" >&2; exit 2 ;; esac

if [ "$skip_provision" = 0 ]; then
    "$app_dir/provision_rootfs.sh" --host "$host"
fi

remote="${linux_user}@${host}"
ssh_args=(-p "$ssh_port")
rsync -a -e "ssh -p $ssh_port" "$app_dir/run_linux_remote.sh" \
    "$remote:$remote_app/incoming/run_linux_remote.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0755 \
    "$remote_app/incoming/run_linux_remote.sh" "$remote_app/run_linux_remote.sh"
ssh "${ssh_args[@]}" "$remote" sudo -n "$remote_app/run_linux_remote.sh" \
    "$remote_rootfs" "$run_id" "$profile"

"$app_dir/fetch_artifacts.sh" --host "$host" --run-id "$run_id"
