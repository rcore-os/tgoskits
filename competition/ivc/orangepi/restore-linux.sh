#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
serial_command=$script_dir/serial-command.sh
power_tool=$workspace/.claude/skills/board-power-control/scripts/board_power.py
serial_path=${ORANGEPI_SERIAL:-/dev/serial/by-path/platform-vhci_hcd.0-usb-0:1:1.0-port0}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
linux_boot_wait_seconds=${ORANGEPI_LINUX_BOOT_WAIT_SECONDS:-120}
axvisor_sync_confirmed=${ORANGEPI_AXVISOR_SYNC_CONFIRMED:-0}
axvisor_no_host_fs=${ORANGEPI_AXVISOR_NO_HOST_FS:-0}
power_python=${ORANGEPI_POWER_PYTHON:-}

ssh_options=(
    -i "$ssh_identity"
    -o IdentitiesOnly=yes
    -o BatchMode=yes
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=4
)

linux_probe() {
    ssh "${ssh_options[@]}" "$ssh_target" \
        'test "$(hostname)" = orangepi5plus; findmnt -no SOURCE,FSTYPE,OPTIONS /; echo BOARD_LINUX_RESTORED'
}

run_power_tool() {
    local tool_path=$power_tool

    if [[ "$power_python" == *.exe ]]; then
        tool_path=$(wslpath -w "$power_tool")
    fi
    "$power_python" "$tool_path" "$@"
}

has_exact_line() {
    local expected=$1
    local output=$2
    local line

    while IFS= read -r line; do
        line=${line%$'\r'}
        if [[ "$line" == "$expected" ]]; then
            return 0
        fi
    done <<<"$output"
    return 1
}

validate_boolean() {
    local name=$1
    local value=$2

    if [[ "$value" != 0 && "$value" != 1 ]]; then
        echo "$name must be 0 or 1" >&2
        exit 2
    fi
}

if linux_probe; then
    echo "Orange Pi is already running Linux"
    exit 0
fi

validate_boolean ORANGEPI_AXVISOR_SYNC_CONFIRMED "$axvisor_sync_confirmed"
validate_boolean ORANGEPI_AXVISOR_NO_HOST_FS "$axvisor_no_host_fs"
case "$linux_boot_wait_seconds" in
    ''|*[!0-9]*)
        echo "ORANGEPI_LINUX_BOOT_WAIT_SECONDS must be a positive integer" >&2
        exit 2
        ;;
esac
if ((linux_boot_wait_seconds == 0)); then
    echo "ORANGEPI_LINUX_BOOT_WAIT_SECONDS must be a positive integer" >&2
    exit 2
fi
for input_path in "$serial_command" "$power_tool" "$ssh_identity"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required Linux restore input is not readable: $input_path" >&2
        exit 1
    fi
done
if [[ -z "$power_python" ]]; then
    if python3 -c 'import miio' >/dev/null 2>&1; then
        power_python=python3
    elif command -v python.exe >/dev/null 2>&1 \
        && python.exe -c 'import miio' >/dev/null 2>&1; then
        power_python=$(command -v python.exe)
    else
        echo "python-miio is unavailable in both WSL and Windows Python" >&2
        exit 1
    fi
fi
if [[ ! -e "$serial_path" ]]; then
    echo "Serial device not found: $serial_path" >&2
    exit 1
fi
for command_name in fuser pgrep ssh "$power_python"; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required Linux restore tool not found: $command_name" >&2
        exit 1
    fi
done
if [[ "$power_python" == *.exe ]] && ! command -v wslpath >/dev/null 2>&1; then
    echo "wslpath is required when smart-plug control uses Windows Python" >&2
    exit 1
fi
if pgrep -f 'target/debug/tg-xtask (board connect|starry test board|axvisor board)' >/dev/null; then
    echo "A board-service client still holds or is requesting a lease" >&2
    exit 1
fi
if fuser "$serial_path" >/dev/null 2>&1; then
    echo "Serial device is busy: $serial_path" >&2
    exit 1
fi

if [[ "$axvisor_no_host_fs" == 1 ]]; then
    echo "Axvisor was built without host filesystem support; no filesystem sync is required"
elif [[ "$axvisor_sync_confirmed" == 1 ]]; then
    echo "Axvisor filesystem sync marker was confirmed by the board test"
else
    set +e
    console_probe_output=$(bash "$serial_command" help 2>&1)
    console_probe_status=$?
    set -e
    printf '%s\n' "$console_probe_output"

    if ((console_probe_status != 0)); then
        echo "Console did not respond; refusing to remove power without a filesystem sync marker" >&2
        exit 1
    fi

    if [[ "$console_probe_output" == *"ArceOS Shell - Available Commands:"* ]]; then
        set +e
        host_sync_output=$(bash "$serial_command" sync-host 2>&1)
        host_sync_status=$?
        set -e
        printf '%s\n' "$host_sync_output"
        if ((host_sync_status != 0)) \
            || ! has_exact_line "AXVISOR_HOST_FILESYSTEM_SYNCED" "$host_sync_output"; then
            echo "Axvisor did not confirm host filesystem sync; refusing to remove power" >&2
            exit 1
        fi
    elif [[ "$console_probe_output" == *"=> help"* ]]; then
        echo "U-Boot prompt confirmed; no host filesystem is mounted"
    else
        set +e
        sync_output=$(
            bash "$serial_command" 'sync && echo BOARD_PRE_CYCLE_SYNC_DONE' 2>&1
        )
        sync_status=$?
        set -e
        printf '%s\n' "$sync_output"
        if ((sync_status != 0)) \
            || ! has_exact_line "BOARD_PRE_CYCLE_SYNC_DONE" "$sync_output"; then
            echo "Live shell did not confirm filesystem sync; refusing to remove power" >&2
            exit 1
        fi
    fi
fi

cd "$workspace"
run_power_tool status
run_power_tool cycle --yes

# The SPI U-Boot intentionally stops at its prompt, so source the TF-card boot
# script with a transient environment and leave the persistent environment alone.
sleep 20
uboot_boot_command='setenv devtype mmc; setenv devnum 1; setenv mmc_bootdev 1; setenv distro_bootpart 1; setenv prefix /; echo UBOOT_LINUX_BOOT_BEGIN; fatload mmc 1:1 0x00c00000 boot.scr; source 0x00c00000'
bash "$serial_command" "$uboot_boot_command"

attempts=$(((linux_boot_wait_seconds + 4) / 5))
for ((attempt = 1; attempt <= attempts; attempt++)); do
    if linux_probe; then
        exit 0
    fi
    sleep 5
done

echo "Orange Pi did not return to Linux within $linux_boot_wait_seconds seconds" >&2
exit 1
