#!/bin/bash
set -euo pipefail

case "${1:-}" in
    -h|--help)
        echo "Usage: apps/starry/orangepi-5-plus-selfbuild/connect_serial.sh [SERIAL_DEVICE]"
        exit 0
        ;;
esac
[ "$#" -le 1 ] || { echo "expected at most one serial device" >&2; exit 2; }

serial_device="${1:-/dev/ttyACM0}"
[ -c "$serial_device" ] || { echo "serial device is missing: $serial_device" >&2; exit 1; }
[ -t 0 ] && [ -t 1 ] || { echo "run this command in an interactive terminal" >&2; exit 1; }
command -v picocom >/dev/null || { echo "picocom not found" >&2; exit 1; }

# Raw picocom input cannot consume wrappers left enabled by the host terminal.
# Reset the terminal mode on stdout before picocom opens the serial device.
printf '\033[?2004l'
exec picocom -b 1500000 "$serial_device"
