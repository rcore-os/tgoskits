#!/usr/bin/env bash
#
# prebuild.sh — Bootstrap overlay generator for QEMU-based selfhost rootfs.
#
# Generates an Alpine-compatible bootstrap inner script that creates a
# selfhost rootfs from the Alpine base (the only x86_64 managed rootfs
# available without host sudo).  The guest:
#   1. Installs build tools (apk add)
#   2. Installs bash (for inner script compatibility)
#   3. Installs Rust nightly (rustup)
#   4. Fetches cargo dependencies
#   5. Writes success marker and powers off
#
# The resulting selfhost rootfs is Alpine-based (musl libc); this does not
# affect self-compilation because the bare-metal target (x86_64-unknown-none)
# does not link any libc.
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
info "Generating bootstrap overlay for $arch"

# ── bootstrap inner script (Alpine-compatible: #!/bin/sh) ──────────────────────
mkdir -p "$overlay_dir/usr/bin"
cat > "$overlay_dir/usr/bin/self-compile-inner.sh" << 'INNER_EOF'
#!/bin/sh
set -e

fail() {
    echo "SELFHOST_BOOTSTRAP_FAILED: $1"
    echo ""
    sync
    poweroff
    exit 1
}

echo "[bootstrap] Resizing root filesystem..."
resize2fs /dev/vda 2>/dev/null || resize2fs /dev/sda 2>/dev/null || true

echo "[bootstrap] Updating apk..."
apk update || fail "apk update failed"

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
    perl \
    bash \
    || fail "apk add failed"

# Ensure /usr/bin/bash exists for Debian-compatible inner scripts
[ -f /usr/bin/bash ] || ln -sf /bin/bash /usr/bin/bash 2>/dev/null || true
[ -x /usr/bin/bash ] || [ -x /bin/bash ] || fail "bash not found after install"

echo "[bootstrap] Build tools installed."

echo "[bootstrap] Installing Rust via rustup..."
if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain $(grep -oP 'channel\s*=\s*"\K[^"]+' rust-toolchain.toml 2>/dev/null || echo nightly-2026-05-28) --profile minimal \
        || fail "rustup install failed"
fi
. "$HOME/.cargo/env" 2>/dev/null || . "$HOME/.cargo/env"

echo "[bootstrap] Rust: $(rustc --version 2>&1)"
echo "[bootstrap] Cargo: $(cargo --version 2>&1)"

echo "[bootstrap] Adding bare-metal targets..."
rustup target add x86_64-unknown-none \
    || fail "rustup target add failed"

# Pre-fetch cargo dependencies so subsequent --offline builds work.
# Cargo.toml and Cargo.lock are injected via the overlay.
if [ -f /opt/starryos/Cargo.toml ] && [ -f /opt/starryos/Cargo.lock ]; then
    echo "[bootstrap] Fetching cargo dependencies..."
    cd /opt/starryos
    cargo fetch || fail "cargo fetch failed"
    echo "[bootstrap] Cargo dependencies fetched."
else
    echo "[bootstrap] No Cargo.toml — skipping cargo fetch."
fi

echo ""
echo "SELFHOST_BOOTSTRAP_SUCCESS"
echo ""
sync
poweroff
INNER_EOF
chmod +x "$overlay_dir/usr/bin/self-compile-inner.sh"
info "Bootstrap inner script generated."

# ── Cargo.toml + Cargo.lock for dependency fetch ───────────────────────────────
mkdir -p "$overlay_dir/opt/starryos"
if [ -f "$repo_root/Cargo.toml" ]; then
    cp "$repo_root/Cargo.toml" "$overlay_dir/opt/starryos/Cargo.toml"
    info "Cargo.toml staged."
fi
if [ -f "$repo_root/Cargo.lock" ]; then
    cp "$repo_root/Cargo.lock" "$overlay_dir/opt/starryos/Cargo.lock"
    info "Cargo.lock staged."
fi

# ── Source commit marker ───────────────────────────────────────────────────────
if command -v git >/dev/null 2>&1; then
    git -C "$repo_root" rev-parse HEAD \
        > "$overlay_dir/opt/starryos/.source-commit" 2>/dev/null || true
fi

info "Bootstrap overlay ready: $overlay_dir"
