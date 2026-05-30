#!/usr/bin/env bash
set -euo pipefail

CLAW_REPO="https://github.com/ultraworkers/claw-code"
CACHE_DIR="${CLAW_CACHE_DIR:-${HOME}/.cache/claw-code-build}"
CLAW_SRC="$CACHE_DIR/repo"
TARGET="x86_64-unknown-linux-musl"
CLAW_BIN="$CACHE_DIR/claw"

echo "=== 1. Copy base rootfs ==="
cp "$STARRY_BASE_ROOTFS" "$STARRY_OUTPUT_ROOTFS"
echo "Copied: $STARRY_OUTPUT_ROOTFS"

echo "=== 2. Build claw from source ==="
if [ -f "$CLAW_BIN" ]; then
    echo "claw binary cached at $CLAW_BIN"
else
    mkdir -p "$CACHE_DIR"
    rustup target add "$TARGET" 2>/dev/null || true
    if [ ! -d "$CLAW_SRC" ]; then
        echo "Cloning $CLAW_REPO ..."
        git clone --depth 1 "$CLAW_REPO" "$CLAW_SRC"
    fi
    echo "Building claw for $TARGET (this may take a while)..."
    (
        cd "$CLAW_SRC/rust"
        cargo build --workspace --target "$TARGET" --target-dir "$CACHE_DIR/target"
    )
    cp "$CACHE_DIR/target/$TARGET/debug/claw" "$CLAW_BIN"
    chmod +x "$CLAW_BIN"
    echo "claw binary built: $CLAW_BIN"
fi

echo "=== 3. Inject claw into rootfs ==="
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "rm /usr/bin/claw" 2>/dev/null || true
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "write $CLAW_BIN /usr/bin/claw"
debugfs -w "$STARRY_OUTPUT_ROOTFS" -R "sif /usr/bin/claw mode 0100755"
echo "Injected claw into rootfs"

# Place a marker so the overlay is never empty (app framework requires it).
touch "${STARRY_OVERLAY_DIR}/.claw-injected"
