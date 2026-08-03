#!/usr/bin/env bash

set -euo pipefail

serial_path=${ORANGEPI_SERIAL:-/dev/serial/by-path/platform-vhci_hcd.0-usb-0:1:1.0-port0}
serial_timeout=${ORANGEPI_SERIAL_COMMAND_TIMEOUT:-10}
serial_settle_seconds=${ORANGEPI_SERIAL_COMMAND_SETTLE_SECONDS:-5}
serial_command=${*:-echo BOARD_LINUX_CHECK_BEGIN; hostname; id; findmnt -no SOURCE,FSTYPE,OPTIONS /; lsblk -f; ip -brief addr; sync; echo BOARD_LINUX_CHECK_DONE}

case "$serial_timeout" in
    ''|*[!0-9]*)
        echo "ORANGEPI_SERIAL_COMMAND_TIMEOUT must be a positive integer" >&2
        exit 2
        ;;
esac
case "$serial_settle_seconds" in
    ''|*[!0-9]*)
        echo "ORANGEPI_SERIAL_COMMAND_SETTLE_SECONDS must be a nonnegative integer" >&2
        exit 2
        ;;
esac
if ((serial_timeout == 0)); then
    echo "ORANGEPI_SERIAL_COMMAND_TIMEOUT must be a positive integer" >&2
    exit 2
fi
for command_name in fuser picocom timeout; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required serial tool not found: $command_name" >&2
        exit 1
    fi
done
if [[ ! -e "$serial_path" ]]; then
    echo "Serial device not found: $serial_path" >&2
    exit 1
fi
if fuser "$serial_path" >/dev/null 2>&1; then
    echo "Serial device is busy: $serial_path" >&2
    exit 1
fi

{
    sleep 1
    printf '\r'
    sleep 1
    printf '%s\r' "$serial_command"
    sleep "$serial_settle_seconds"
} | timeout "$serial_timeout" picocom --quiet --baud 1500000 "$serial_path"
