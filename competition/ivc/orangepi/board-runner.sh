#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
prepare_service_dtb=$script_dir/prepare-service-dtb.sh
restore_linux=$script_dir/restore-linux.sh
harvest_result=$script_dir/harvest-result.sh
serial_path=${ORANGEPI_SERIAL:-/dev/serial/by-path/platform-vhci_hcd.0-usb-0:1:1.0-port0}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
lease_wait_seconds=${ORANGEPI_LEASE_WAIT_SECONDS:-600}
run_timeout_seconds=${ORANGEPI_RUN_TIMEOUT_SECONDS:-720}
build_config=${ORANGEPI_AXVISOR_BUILD_CONFIG:-os/axvisor/configs/board/orangepi-5-plus.toml}
board_config=${ORANGEPI_AXVISOR_BOARD_CONFIG:-test-suit/axvisor/normal/board-orangepi-5-plus/smoke/board-orangepi-5-plus-linux.toml}
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
host_root_selector=${ORANGEPI_AXVISOR_HOST_ROOT:-/dev/mmcblk0p2}
shutdown_marker_required=${ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED:-0}
no_host_fs=${ORANGEPI_AXVISOR_NO_HOST_FS:-0}
restore_linux_required=${ORANGEPI_RESTORE_LINUX:-1}
raw_output=${ORANGEPI_IVC_RAW_CSV:-}
pre_reset_raw_output=${ORANGEPI_IVC_PRE_RESET_RAW_CSV:-}
expected_pre_reset_count=${ORANGEPI_IVC_EXPECTED_PRE_RESET_COUNT:-0}
guest_image=${ORANGEPI_IVC_GUEST_IMAGE:-}
result_image=${ORANGEPI_IVC_RESULT_IMAGE:-}
expected_count=${ORANGEPI_IVC_EXPECTED_COUNT:-}
service_dtb=${ORANGEPI_SERVICE_DTB:-${HOME}/.local/share/ostool-server/dtbs/orangepi-5-plus-starry.dtb}
axvisor_dtb=${ORANGEPI_AXVISOR_DTB:-$workspace/os/axvisor/configs/board/orangepi-5-plus.dtb}
starry_dtb=${ORANGEPI_STARRY_DTB:-$workspace/os/StarryOS/configs/board/orangepi-5-plus.dtb}
ostool_patch=${OSTOOL_UBOOT_PATCH:-${HOME}/.config/ostool-server/uboot-shell-patch.toml}
cargo_lock=$workspace/Cargo.lock
cargo_lock_backup=

validate_boolean() {
    local name=$1
    local value=$2

    if [[ "$value" != 0 && "$value" != 1 ]]; then
        echo "$name must be 0 or 1" >&2
        exit 2
    fi
}

resolve_workspace_path() {
    local path=$1

    if [[ "$path" == /* ]]; then
        printf '%s\n' "$path"
    else
        printf '%s\n' "$workspace/$path"
    fi
}

configure_system_libclang() {
    local libclang

    if [[ -n "${LIBCLANG_PATH:-}" ]]; then
        echo "BOARD_HOST_LIBCLANG path=$LIBCLANG_PATH source=environment"
        return
    fi
    for libclang in /usr/lib/llvm-*/lib/libclang.so*; do
        if [[ -r "$libclang" ]]; then
            LIBCLANG_PATH=${libclang%/*}
            export LIBCLANG_PATH
            echo "BOARD_HOST_LIBCLANG path=$LIBCLANG_PATH source=system"
            return
        fi
    done
}

backup_cargo_lock() {
    if [[ ! -r "$ostool_patch" ]]; then
        return
    fi
    if [[ ! -r "$cargo_lock" ]]; then
        echo "Cargo lockfile is not readable before applying the local ostool patch: $cargo_lock" >&2
        return 1
    fi
    cargo_lock_backup=$(mktemp "${TMPDIR:-/tmp}/orangepi-cargo-lock.XXXXXX")
    cp -- "$cargo_lock" "$cargo_lock_backup"
}

restore_cargo_lock() {
    if [[ -z "$cargo_lock_backup" ]]; then
        return
    fi
    if ! cp -- "$cargo_lock_backup" "$cargo_lock"; then
        echo "Failed to restore Cargo lockfile after the local ostool patch" >&2
        return 1
    fi
    rm -f -- "$cargo_lock_backup"
    cargo_lock_backup=
}

prepare_linux_state() (
    local lease_log
    local lease_pid
    local lease_deadline
    local ssh_status

    cleanup_linux_lease() {
        if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
            kill -TERM "$lease_pid" 2>/dev/null || true
            wait "$lease_pid" 2>/dev/null || true
        fi
        if [[ -n "$lease_log" ]]; then
            rm -f -- "$lease_log"
        fi
    }
    trap cleanup_linux_lease EXIT
    trap 'exit 130' HUP INT TERM

    lease_pid=
    lease_log=
    lease_log=$(mktemp "${TMPDIR:-/tmp}/orangepi-linux-lease.XXXXXX.log")
    cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
    lease_pid=$!
    lease_deadline=$((SECONDS + 60))
    while ! grep -q '^Allocated board session:' "$lease_log"; do
        if ! kill -0 "$lease_pid" 2>/dev/null; then
            echo "Board lease ended before Linux preparation" >&2
            sed -n '1,160p' "$lease_log" >&2
            return 1
        fi
        if ((SECONDS >= lease_deadline)); then
            echo "Timed out while acquiring the $board_type Linux preparation lease" >&2
            return 1
        fi
        sleep 0.2
    done

    set +e
    if [[ -n "$raw_output" ]]; then
        ssh "${ssh_options[@]}" "$ssh_target" sh -s -- "$result_image" <<'REMOTE'
set -eu
result_image=$1
rm -f -- "$result_image" "$result_image.new"
hostname
sudo -n /usr/bin/sync
sudo -n -l /usr/sbin/reboot >/dev/null
REMOTE
        ssh_status=$?
    else
        ssh "${ssh_options[@]}" "$ssh_target" \
            'hostname; sudo -n /usr/bin/sync; sudo -n -l /usr/sbin/reboot >/dev/null'
        ssh_status=$?
    fi
    set -e

    kill -TERM "$lease_pid" 2>/dev/null || true
    wait "$lease_pid" 2>/dev/null || true
    lease_pid=
    rm -f -- "$lease_log"
    lease_log=
    return "$ssh_status"
)

validate_boolean ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED "$shutdown_marker_required"
validate_boolean ORANGEPI_AXVISOR_NO_HOST_FS "$no_host_fs"
validate_boolean ORANGEPI_RESTORE_LINUX "$restore_linux_required"
case "$lease_wait_seconds" in
    ''|*[!0-9]*)
        echo "ORANGEPI_LEASE_WAIT_SECONDS must be a positive integer" >&2
        exit 2
        ;;
esac
if ((lease_wait_seconds == 0)); then
    echo "ORANGEPI_LEASE_WAIT_SECONDS must be a positive integer" >&2
    exit 2
fi
case "$run_timeout_seconds" in
    ''|*[!0-9]*)
        echo "ORANGEPI_RUN_TIMEOUT_SECONDS must be a positive integer" >&2
        exit 2
        ;;
esac
if ((run_timeout_seconds == 0)); then
    echo "ORANGEPI_RUN_TIMEOUT_SECONDS must be a positive integer" >&2
    exit 2
fi
if [[ "$host_root_selector" == *[[:space:]]* ]]; then
    echo "AxVisor host root selector must not contain whitespace: $host_root_selector" >&2
    exit 1
fi

resolved_build_config=$(resolve_workspace_path "$build_config")
resolved_board_config=$(resolve_workspace_path "$board_config")
for input_path in \
    "$prepare_service_dtb" \
    "$restore_linux" \
    "$ssh_identity" \
    "$axvisor_dtb" \
    "$starry_dtb" \
    "$resolved_build_config" \
    "$resolved_board_config"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required Orange Pi runner input is not readable: $input_path" >&2
        exit 1
    fi
done
if [[ -n "$raw_output" ]]; then
    if [[ ! -r "$harvest_result" ]]; then
        echo "Required Orange Pi harvest script is not readable: $harvest_result" >&2
        exit 1
    fi
    if [[ -z "$guest_image" || -z "$result_image" || -z "$expected_count" ]]; then
        echo "Result harvest requires guest image, result image, and expected count" >&2
        exit 2
    fi
    case "$expected_pre_reset_count" in
        ''|*[!0-9]*)
            echo "ORANGEPI_IVC_EXPECTED_PRE_RESET_COUNT must be a nonnegative integer" >&2
            exit 2
            ;;
    esac
    if ((expected_pre_reset_count > 0)); then
        if [[ -z "$pre_reset_raw_output" || "$pre_reset_raw_output" != /* ]]; then
            echo "Pre-reset result harvest requires an absolute output path" >&2
            exit 2
        fi
        if [[ "$pre_reset_raw_output" == "$raw_output" ]]; then
            echo "Pre-reset and post-reset raw output paths must differ" >&2
            exit 2
        fi
    fi
    for remote_image in "$guest_image" "$result_image"; do
        case "$remote_image" in
            /home/orangepi/*) ;;
            *)
                echo "Board image must remain below /home/orangepi: $remote_image" >&2
                exit 1
                ;;
        esac
        if [[ "$remote_image" =~ [^A-Za-z0-9_./-] ]]; then
            echo "Board image contains unsupported characters: $remote_image" >&2
            exit 1
        fi
    done
    [[ "$guest_image" != "$result_image" ]] || {
        echo "Result image must not overwrite the staged guest image: $guest_image" >&2
        exit 1
    }
fi
for command_name in cargo cp fuser grep mktemp sed ssh tee timeout; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required Orange Pi runner tool not found: $command_name" >&2
        exit 1
    fi
done
if [[ ! -e "$serial_path" ]]; then
    echo "Serial device not found: $serial_path" >&2
    exit 1
fi
if [[ "$no_host_fs" == 1 ]] \
    && grep -Eq '"(fs|host-fs)"' "$resolved_build_config"; then
    echo "ORANGEPI_AXVISOR_NO_HOST_FS=1 conflicts with filesystem features in $resolved_build_config" >&2
    exit 1
fi
if fuser "$serial_path" >/dev/null 2>&1; then
    echo "Serial device is busy before the board run: $serial_path" >&2
    exit 1
fi

ssh_options=(
    -i "$ssh_identity"
    -o IdentitiesOnly=yes
    -o BatchMode=yes
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=8
)
cd "$workspace"
configure_system_libclang
prepare_linux_state

bash "$prepare_service_dtb" "$axvisor_dtb" "$service_dtb" "$host_root_selector"

runner_pid=
rebooter_pid=
runner_log=$(mktemp "${TMPDIR:-/tmp}/orangepi-axvisor-run.XXXXXX.log")
restore_service_dtb() {
    bash "$prepare_service_dtb" \
        "$starry_dtb" "$service_dtb" "$host_root_selector" >/dev/null 2>&1 || true
}
cleanup() {
    if [[ -n "$rebooter_pid" ]] && kill -0 "$rebooter_pid" 2>/dev/null; then
        kill -TERM "$rebooter_pid" 2>/dev/null || true
    fi
    if [[ -n "$runner_pid" ]] && kill -0 "$runner_pid" 2>/dev/null; then
        kill -INT "$runner_pid" 2>/dev/null || true
    fi
    rm -f -- "$runner_log"
    restore_service_dtb
    restore_cargo_lock || true
}
trap cleanup EXIT HUP INT TERM

backup_cargo_lock
cargo_command=(cargo)
if [[ -r "$ostool_patch" ]]; then
    cargo_command+=(--config "$ostool_patch")
fi

(
    set -o pipefail
    timeout --foreground --signal=INT --kill-after=30 "$run_timeout_seconds" \
        "${cargo_command[@]}" xtask axvisor board \
        -c "$build_config" \
        --board-config "$board_config" \
        -b "$board_type" \
        "$@" 2>&1 | tee "$runner_log"
) &
runner_pid=$!

(
    lease_deadline=$((SECONDS + lease_wait_seconds))
    while ! fuser "$serial_path" >/dev/null 2>&1; do
        if ! kill -0 "$runner_pid" 2>/dev/null; then
            exit 0
        fi
        if ((SECONDS >= lease_deadline)); then
            echo "Timed out waiting for the board service to acquire $serial_path" >&2
            kill -INT "$runner_pid" 2>/dev/null || true
            exit 1
        fi
        sleep 0.2
    done

    sleep 1
    set +e
    reboot_output=$(
        ssh "${ssh_options[@]}" "$ssh_target" \
            'sudo -n /usr/bin/sync; echo BOARD_REBOOT_ARMED; sudo -n /usr/sbin/reboot' \
            2>&1
    )
    reboot_status=$?
    set -e
    printf '%s\n' "$reboot_output"
    if ((reboot_status != 0)) && [[ "$reboot_output" != *BOARD_REBOOT_ARMED* ]]; then
        echo "Failed to reboot Orange Pi after the serial lease was acquired" >&2
        kill -INT "$runner_pid" 2>/dev/null || true
        exit 1
    fi
) &
rebooter_pid=$!

set +e
wait "$runner_pid"
runner_status=$?
wait "$rebooter_pid"
rebooter_status=$?
sync_marker_confirmed=0
snapshot_marker_confirmed=0
if grep -aEq $'(^|[^[:alnum:]_])AXVISOR_SNAPSHOT_SYNC_OK\r*$' "$runner_log"; then
    snapshot_marker_confirmed=1
    sync_marker_confirmed=1
fi
if grep -aEq $'(^|[^[:alnum:]_])AXVISOR_HOST_FILESYSTEM_SYNCED\r*$' "$runner_log"; then
    sync_marker_confirmed=1
fi
guest_marker_confirmed=0
if grep -aEq $'IVC-STARRY-DONE exit=0\r*$' "$runner_log"; then
    guest_marker_confirmed=1
fi
if ((runner_status == 0)) \
    && [[ "$shutdown_marker_required" == 1 ]] \
    && [[ "$no_host_fs" == 0 ]] \
    && [[ "$sync_marker_confirmed" == 0 ]]; then
    echo "Axvisor board run exited without the exact host filesystem sync marker" >&2
    runner_status=1
fi
if ((runner_status == 0)) \
    && [[ -n "$raw_output" ]] \
    && [[ "$snapshot_marker_confirmed" == 0 ]]; then
    echo "Axvisor board run exited without the expected volatile block snapshot marker" >&2
    runner_status=1
fi
if ((runner_status == 0)) \
    && [[ -n "$raw_output" ]] \
    && [[ "$guest_marker_confirmed" == 0 ]]; then
    echo "Axvisor board run exited without the StarryOS completion marker" >&2
    runner_status=1
fi
restore_status=0
if [[ "$restore_linux_required" == 1 ]]; then
    if [[ "$sync_marker_confirmed" == 1 ]]; then
        ORANGEPI_AXVISOR_SYNC_CONFIRMED=1 \
        ORANGEPI_AXVISOR_NO_HOST_FS="$no_host_fs" \
            bash "$restore_linux"
    else
        ORANGEPI_AXVISOR_NO_HOST_FS="$no_host_fs" bash "$restore_linux"
    fi
    restore_status=$?
fi
harvest_status=0
if ((runner_status == 0 && restore_status == 0)) && [[ -n "$raw_output" ]]; then
    ORANGEPI_IVC_RAW_CSV="$raw_output" \
    ORANGEPI_IVC_RESULT_IMAGE="$result_image" \
    ORANGEPI_IVC_EXPECTED_COUNT="$expected_count" \
    ORANGEPI_IVC_PRE_RESET_RAW_CSV="$pre_reset_raw_output" \
    ORANGEPI_IVC_EXPECTED_PRE_RESET_COUNT="$expected_pre_reset_count" \
        bash "$harvest_result"
    harvest_status=$?
fi
bash "$prepare_service_dtb" \
    "$starry_dtb" "$service_dtb" "$host_root_selector" >/dev/null
dtb_restore_status=$?
rm -f -- "$runner_log"
restore_cargo_lock
lock_restore_status=$?
set -e

runner_pid=
rebooter_pid=
trap - EXIT HUP INT TERM

if ((rebooter_status != 0)); then
    exit "$rebooter_status"
fi
if ((restore_status != 0)); then
    exit "$restore_status"
fi
if ((harvest_status != 0)); then
    exit "$harvest_status"
fi
if ((dtb_restore_status != 0)); then
    exit "$dtb_restore_status"
fi
if ((lock_restore_status != 0)); then
    exit "$lock_restore_status"
fi
exit "$runner_status"
