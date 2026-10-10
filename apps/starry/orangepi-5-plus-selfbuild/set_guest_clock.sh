#!/bin/sh
set -eu

marker=${1:-}
requested_epoch=${2:-}
case "$marker" in
    ''|*[!A-Za-z0-9._-]*) exit 2 ;;
esac
case "$requested_epoch" in
    ''|*[!0-9]*) exit 2 ;;
esac

if date -u -s "@$requested_epoch" >/dev/null 2>&1; then
    current_epoch="$(date -u +%s 2>/dev/null || printf unknown)"
    printf '===%s-CLOCK requested_epoch=%s current_epoch=%s===\n' \
        "$marker" "$requested_epoch" "$current_epoch"
else
    printf '===%s-CLOCK-UNAVAILABLE requested_epoch=%s===\n' \
        "$marker" "$requested_epoch"
fi
