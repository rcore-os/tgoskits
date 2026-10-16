#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
out_dir="$repo_root/target/starry-orangepi5plus-selfbuild"
remote_app=/opt/starry-orangepi5plus-selfbuild
linux_user="${BOARD_LINUX_USER:-orangepi}"
ssh_port="${BOARD_SSH_PORT:-22}"
host=""
run_id="starry-$(date -u +%Y%m%dT%H%M%SZ)"
profile=off
skip_provision=0
skip_boot_build=0
serial="${BOARD_SERIAL:-/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0}"
serial_timeout=22800
linux_return_attempts=180
linux_return_interval=5

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/orangepi-5-plus-selfbuild/run_selfbuild.sh --host BOARD_IP \
    [--run-id RUN_ID] [--profile off|stat|record] [--skip-provision] \
    [--skip-boot-build]

The board must be in its default Linux. This command provisions the reusable
rootfs, stages a verified one-time image, opens the direct UART, temporarily
selects StarryOS from Linux, runs the self-build, waits for Linux to return, and
fetches the verified artifacts. It never interrupts U-Boot and does not use a
board runner.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --host) host="${2:-}"; shift 2 ;;
        --run-id) run_id="${2:-}"; shift 2 ;;
        --profile) profile="${2:-}"; shift 2 ;;
        --skip-provision) skip_provision=1; shift ;;
        --skip-boot-build) skip_boot_build=1; shift ;;
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
[ -c "$serial" ] || { echo "serial device is unavailable: $serial" >&2; exit 1; }
python3 -c 'import serial' || {
    echo "Python package pyserial is required for direct UART monitoring" >&2
    exit 1
}

if [ "$skip_provision" = 0 ]; then
    "$app_dir/provision_rootfs.sh" --host "$host"
fi

mkdir -p "$out_dir/tmp"
config_stage="$(mktemp "$out_dir/tmp/run-conf.XXXXXX")"
build_epoch=$(date +%s)
cleanup() {
    rm -f "$config_stage"
}
trap cleanup EXIT
cat > "$config_stage" <<EOF
run_id=$run_id
profile=$profile
build_epoch=$build_epoch
EOF

remote="${linux_user}@${host}"
ssh_args=(-p "$ssh_port")
rsync -a -e "ssh -p $ssh_port" "$config_stage" "$remote:$remote_app/incoming/run.conf"
ssh "${ssh_args[@]}" "$remote" sudo -n install -m 0644 \
    "$remote_app/incoming/run.conf" "$remote_app/rootfs/etc/starry-selfbuild/run.conf"
ssh "${ssh_args[@]}" "$remote" sudo -n sync

stage_args=(--host "$host")
if [ "$skip_boot_build" = 1 ]; then
    stage_args+=(--skip-build)
fi
"$app_dir/stage_starry_boot.sh" "${stage_args[@]}"
cleanup
trap - EXIT

run_dir="$out_dir/board-runs/$run_id"
[ ! -e "$run_dir" ] || {
    echo "board-run evidence directory already exists: $run_dir" >&2
    exit 1
}
mkdir -p "$run_dir"
serial_log="$run_dir/serial.log"
ready_file="$run_dir/serial.ready"
before_boot_id=$(ssh "${ssh_args[@]}" "$remote" cat /proc/sys/kernel/random/boot_id)
printf '%s\n' "$before_boot_id" > "$run_dir/linux-boot-id.before"

python3 "$app_dir/serial_selfbuild.py" \
    --serial "$serial" \
    --log "$serial_log" \
    --ready-file "$ready_file" \
    --run-id "$run_id" \
    --timeout "$serial_timeout" &
serial_pid=$!

cleanup_serial() {
    if kill -0 "$serial_pid" 2>/dev/null; then
        kill "$serial_pid" 2>/dev/null || true
        wait "$serial_pid" 2>/dev/null || true
    fi
}
trap cleanup_serial EXIT

for _ in $(seq 1 50); do
    [ -s "$ready_file" ] && break
    kill -0 "$serial_pid" 2>/dev/null || {
        echo "serial monitor exited before the board reboot" >&2
        exit 1
    }
    sleep 0.1
done
[ -s "$ready_file" ] || { echo "serial monitor did not become ready" >&2; exit 1; }

"$app_dir/boot_starry_once.sh" --host "$host"

set +e
wait "$serial_pid"
serial_rc=$?
set -e
trap - EXIT

linux_ready=0
for _ in $(seq 1 "$linux_return_attempts"); do
    if ssh -o ConnectTimeout=3 "${ssh_args[@]}" "$remote" true; then
        linux_ready=1
        break
    fi
    sleep "$linux_return_interval"
done
[ "$linux_ready" = 1 ] || {
    echo "board Linux did not return after the StarryOS run" >&2
    exit 1
}

after_boot_id=$(ssh "${ssh_args[@]}" "$remote" cat /proc/sys/kernel/random/boot_id)
printf '%s\n' "$after_boot_id" > "$run_dir/linux-boot-id.after"
[ "$after_boot_id" != "$before_boot_id" ] || {
    echo "board Linux boot ID did not change" >&2
    exit 1
}
ssh "${ssh_args[@]}" "$remote" sudo -n \
    cmp -s /boot/boot.scr /boot/boot.scr.tgoskits-backup || {
        echo "board did not restore the verified Linux boot script" >&2
        exit 1
    }
ssh "${ssh_args[@]}" "$remote" sudo -n sync

[ "$serial_rc" = 0 ] || {
    echo "StarryOS self-build serial monitor failed with rc=$serial_rc" >&2
    exit "$serial_rc"
}
"$app_dir/fetch_artifacts.sh" --host "$host" --run-id "$run_id"

echo "selfbuild_run_id=$run_id"
echo "selfbuild_serial_log=$serial_log"
echo "selfbuild_linux_boot_id_before=$before_boot_id"
echo "selfbuild_linux_boot_id_after=$after_boot_id"
echo "===STARRY-ORANGEPI5PLUS-SELFBUILD-END-TO-END-PASS run=$run_id==="
