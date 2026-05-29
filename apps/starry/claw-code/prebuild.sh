#!/usr/bin/env bash
set -euo pipefail

CLAW_URL="https://github.com/rcore-os/tgoskits/releases/download/claw-code-binary/claw"
CLAW_BIN="/tmp/claw"

if [ ! -f "$CLAW_BIN" ]; then
    echo "Downloading claw binary..."
    curl -sL -o "$CLAW_BIN" "$CLAW_URL"
    chmod +x "$CLAW_BIN"
    echo "Downloaded: $CLAW_BIN"
fi

install -Dm0755 "$CLAW_BIN" "${STARRY_OVERLAY_DIR}/usr/bin/claw"
echo "claw injected into overlay"
