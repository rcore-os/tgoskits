#!/usr/bin/env bash
#
# prebuild.sh — Bootstrap overlay generator (no host sudo required).
#
# Generates a bootstrap inner script that runs inside StarryOS on an
# Alpine rootfs.  The guest installs build tools (apk), Rust (rustup),
# and caches cargo dependencies so the rootfs is ready for self-compilation.
#
# Env vars (set by axbuild app runner or self-compile.sh):
#   STARRY_OVERLAY_DIR   — staging directory for rootfs injection
#   STARRY_WORKSPACE     — repository root
#   STARRY_ARCH          — target architecture

set -euo pipefail

overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR required}"
repo_root="${STARRY_WORKSPACE:?STARRY_WORKSPACE required}"
arch="${STARRY_ARCH:-x86_64}"

info() { printf "[prebuild:bootstrap] %s\n" "$*"; }

# Map arch to Rust target for cargo fetch
case "$arch" in
    x86_64)  rust_target="x86_64-unknown-none" ;;
    riscv64) rust_target="riscv64gc-unknown-none-elf" ;;
    aarch64) rust_target="aarch64-unknown-none-softfloat" ;;
    *)       echo "Unsupported arch: $arch" >&2; exit 1 ;;
esac

info "Generating bootstrap overlay for $arch"

# ── bootstrap inner script (runs inside Alpine guest) ──────────────────────────
mkdir -p "$overlay_dir/usr/bin"
cat > "$overlay_dir/usr/bin/self-compile-inner.sh" << 'INNER_EOF'
#!/bin/sh
set -e

echo "[bootstrap] Resizing root filesystem..."
resize2fs /dev/vda 2>/dev/null || resize2fs /dev/sda 2>/dev/null || true

echo "[bootstrap] Updating apk..."
apk update

echo "[bootstrap] Installing build toolchain..."
apk add --no-cache \
    build-base \
    clang \
    clang-dev \
    cmake \
    pkgconf \
    git \
    curl \
    python3 \
    linux-headers \
    openssl-dev \
    perl

echo "[bootstrap] Build tools installed."

echo "[bootstrap] Installing Rust via rustup..."
if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain nightly-2026-04-27 --profile minimal
    . "$HOME/.cargo/env"
fi

echo "[bootstrap] Rust: $(rustc --version 2>&1)"
echo "[bootstrap] Cargo: $(cargo --version 2>&1)"

echo "[bootstrap] Adding bare-metal targets..."
rustup target add x86_64-unknown-none 2>/dev/null || true
rustup target add riscv64gc-unknown-none-elf 2>/dev/null || true
rustup target add aarch64-unknown-none-softfloat 2>/dev/null || true

# Pre-fetch cargo dependencies so subsequent --offline builds work.
# Cargo.toml and Cargo.lock are injected via the overlay.
if [ -f /opt/starryos/Cargo.toml ] && [ -f /opt/starryos/Cargo.lock ]; then
    echo "[bootstrap] Fetching cargo dependencies..."
    cd /opt/starryos
    cargo fetch
    echo "[bootstrap] Cargo dependencies cached."
else
    echo "[bootstrap] No Cargo.toml found — skipping dependency fetch."
    echo "[bootstrap] Dependencies will be fetched during first self-compilation."
fi

echo ""
echo "SELFHOST_BOOTSTRAP_SUCCESS"
echo ""
sync
poweroff
INNER_EOF
chmod +x "$overlay_dir/usr/bin/self-compile-inner.sh"

# ── Cargo.toml + Cargo.lock for dependency fetch ──────────────────────────────
mkdir -p "$overlay_dir/opt/starryos"
if [ -f "$repo_root/Cargo.toml" ]; then
    cp "$repo_root/Cargo.toml" "$overlay_dir/opt/starryos/Cargo.toml"
    info "Cargo.toml staged for overlay"
fi
if [ -f "$repo_root/Cargo.lock" ]; then
    cp "$repo_root/Cargo.lock" "$overlay_dir/opt/starryos/Cargo.lock"
    info "Cargo.lock staged for overlay"
fi

# ── .source-commit marker ─────────────────────────────────────────────────────
if command -v git >/dev/null 2>&1; then
    git -C "$repo_root" rev-parse HEAD > "$overlay_dir/opt/starryos/.source-commit" 2>/dev/null || true
fi

info "Bootstrap overlay ready: $overlay_dir"
