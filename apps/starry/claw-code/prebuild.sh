#!/usr/bin/env bash
set -euo pipefail

CLAW_URL="https://github.com/rcore-os/tgoskits/releases/download/claw-code-binary/claw"
CLAW_BIN="/tmp/claw"

echo "=== Download claw binary ==="
if [ ! -f "$CLAW_BIN" ]; then
    curl -sL -o "$CLAW_BIN" "$CLAW_URL"
    chmod +x "$CLAW_BIN"
    echo "Downloaded: $CLAW_BIN ($(du -h "$CLAW_BIN" | cut -f1))"
else
    echo "Already downloaded: $CLAW_BIN"
fi

echo "=== Inject claw into rootfs ==="
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "rm /usr/bin/claw" 2>/dev/null || true
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "write $CLAW_BIN /usr/bin/claw"
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "sif /usr/bin/claw mode 0100755"
echo "Injected claw into rootfs"

# Place a marker so the overlay is never empty (app framework requires it).
touch "${STARRY_OVERLAY_DIR}/.claw-injected"
