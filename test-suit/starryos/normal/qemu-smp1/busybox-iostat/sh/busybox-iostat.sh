#!/bin/sh
set -eu

OUT="$(busybox iostat 1 1 2>&1 || true)"
printf '%s\n' "$OUT"

if printf '%s\n' "$OUT" | busybox grep -qF "avg-cpu"; then
    echo "TEST PASSED"
else
    echo "TEST FAILED"
    exit 1
fi
