#!/usr/bin/env bash
set -euo pipefail

CLAW_REPO="https://github.com/MuZhao2333/claw-code"
CACHE_DIR="${CLAW_CACHE_DIR:-${HOME}/.cache/claw-code-build}"
CLAW_SRC="$CACHE_DIR/repo"
TARGET="x86_64-unknown-linux-musl"
CLAW_BIN="$CACHE_DIR/claw"

WORKSPACE="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
ROOTFS_DIR="$WORKSPACE/tmp/axbuild/rootfs"
OVERLAY="${STARRY_OVERLAY_DIR:-$WORKSPACE/tmp/axbuild/starry-app/claw-code/overlay}"

rootfs_image_file() {
    local path="$1"
    if [ -d "$path" ]; then
        local name
        name="$(basename "$path")"
        if [ -f "$path/$name" ]; then
            printf '%s\n' "$path/$name"
            return
        fi
    fi
    printf '%s\n' "$path"
}

echo "=== 1. Build claw from source ==="
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
        cargo build --workspace --release --target "$TARGET" --target-dir "$CACHE_DIR/target"
    )
    cp "$CACHE_DIR/target/$TARGET/release/claw" "$CLAW_BIN"
    chmod +x "$CLAW_BIN"
    echo "claw binary built: $CLAW_BIN"
fi

echo "=== 2. Prepare rootfs ==="
# The app framework passes the canonical per-app image path through
# STARRY_ROOTFS. Older cached rootfs storage may leave a directory at the
# legacy tmp/axbuild/rootfs/*.img path, so resolve image-storage directories to
# their contained image file instead of assuming flat files.
BASE_ROOTFS="${STARRY_BASE_ROOTFS:-$ROOTFS_DIR/rootfs-${STARRY_ARCH:-x86_64}-alpine.img}"
APP_ROOTFS="${STARRY_ROOTFS:-$ROOTFS_DIR/rootfs-${STARRY_ARCH:-x86_64}-claw-code.img}"
ALPINE_IMG="$(rootfs_image_file "$BASE_ROOTFS")"
CLAW_IMG="$(rootfs_image_file "$APP_ROOTFS")"
if [ "$ALPINE_IMG" != "$CLAW_IMG" ]; then
    mkdir -p "$(dirname "$CLAW_IMG")"
    rm -rf "$CLAW_IMG"
    cp "$ALPINE_IMG" "$CLAW_IMG"
fi

echo "=== 3. Inject claw into rootfs ==="
inject_claw() {
    local img="$1"
    echo "  injecting into $img ..."
    debugfs -w "$img" -R "rm /usr/bin/claw" 2>/dev/null || true
    debugfs -w "$img" -R "write $CLAW_BIN /usr/bin/claw"
    debugfs -w "$img" -R "sif /usr/bin/claw mode 0100755"
}
inject_claw "$CLAW_IMG"

echo "Injected claw into rootfs"

# Place a marker so the overlay is never empty (app framework requires it).
mkdir -p "$OVERLAY"
touch "${OVERLAY}/.claw-injected"
