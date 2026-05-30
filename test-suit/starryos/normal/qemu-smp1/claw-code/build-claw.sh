#!/usr/bin/env bash
# Build claw from source and cache the binary.
# Called by build.rs files; idempotent — builds only once.
set -euo pipefail

CLAW_REPO="https://github.com/MuZhao2333/claw-code"
CACHE_DIR="${CLAW_CACHE_DIR:-${HOME}/.cache/claw-code-build}"
CLAW_SRC="$CACHE_DIR/repo"
# Statically-linked musl binary so it runs inside Alpine-based StarryOS rootfs.
TARGET="x86_64-unknown-linux-musl"
CLAW_BIN="$CACHE_DIR/claw"

if [ -f "$CLAW_BIN" ]; then
    echo "claw binary cached at $CLAW_BIN"
    echo -n "$CLAW_BIN"
    exit 0
fi

echo "=== Building claw from source (target: $TARGET) ==="
mkdir -p "$CACHE_DIR"

# Install musl target if needed.
rustup target add "$TARGET" 2>/dev/null || true

if [ ! -d "$CLAW_SRC" ]; then
    echo "Cloning $CLAW_REPO ..."
    git clone --depth 1 "$CLAW_REPO" "$CLAW_SRC"
fi

echo "Building claw (this may take a while)..."
(
    cd "$CLAW_SRC/rust"
    cargo build --workspace --release --target "$TARGET" --target-dir "$CACHE_DIR/target"
)

cp "$CACHE_DIR/target/$TARGET/release/claw" "$CLAW_BIN"
chmod +x "$CLAW_BIN"
echo "claw binary built: $CLAW_BIN"
echo -n "$CLAW_BIN"
