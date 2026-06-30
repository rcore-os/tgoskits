#!/usr/bin/env bash
#
# prebuild.sh — Bootstrap overlay generator: PROVISION a complete toolchain
# selfhost rootfs from the Alpine base entirely inside QEMU, with NO host sudo.
#
# What this DOES (verified): the Alpine guest (which has network) installs, under
# StarryOS's Linux compat, everything needed to build StarryOS — mirroring the
# privileged prepare-selfhost-rootfs.sh but unprivileged:
#   1. Build toolchain (apk) + bash
#   2. x86_64-linux-musl-{cc,gcc,ar} symlinks -> Alpine's native musl gcc/ar
#   3. Full source tree (git archive, staged via overlay) -> /opt/starryos
#   4. AIC8800 firmware blobs (gitignored; staged via overlay)
#   5. Rust nightly + rust-src + llvm-tools-preview + bare-metal target
#   6. kallsyms tools (cargo-binutils -> rust-nm/rust-objcopy, ksym -> gen_ksym)
#
# What this does NOT do, and WHY: it does not warm the offline dependency cache
# (an in-guest `-Zbuild-std` build). That step needs a download-during-build that
# does not fit StarryOS's resources at usable rootfs sizes (tmpfs target -> RAM
# OOM; disk target -> rsext4 size limits), so a self-contained OFFLINE-buildable
# blueprint cannot be produced under StarryOS. For a verified offline self-compile
# use the downloadable pre-baked blueprint (curl + SHA-256, no sudo) or the
# privileged prepare-selfhost-rootfs.sh — see docs/starryos-self-compilation.md.
#
# Env vars (set by the axbuild app runner): STARRY_OVERLAY_DIR, STARRY_WORKSPACE,
# STARRY_ARCH.

set -euo pipefail

overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR required}"
repo_root="${STARRY_WORKSPACE:?STARRY_WORKSPACE required}"
arch="${STARRY_ARCH:-x86_64}"

info() { printf "[prebuild:bootstrap] %s\n" "$*"; }
info "Generating toolchain-provisioning overlay for $arch"

mkdir -p "$overlay_dir/usr/bin" "$overlay_dir/opt"

# ── Stage the full source tree as a single tarball (git archive, ~58 MB) ───────
# One overlay-injection write; extracting it in-guest is far cheaper than
# injecting the whole tree file-by-file via debugfs.
info "Staging source tarball (git archive HEAD)..."
git -C "$repo_root" archive --format=tar HEAD -o "$overlay_dir/opt/starryos-src.tar"
git -C "$repo_root" rev-parse HEAD > "$overlay_dir/opt/.source-commit" 2>/dev/null || true

# ── Stage AIC8800 firmware blobs (gitignored; absent from git archive) ─────────
if ls "$repo_root"/components/aic8800/firmware/*.bin >/dev/null 2>&1; then
    mkdir -p "$overlay_dir/opt/firmware-blobs"
    cp "$repo_root"/components/aic8800/firmware/*.bin "$overlay_dir/opt/firmware-blobs/"
    info "Staged $(ls "$overlay_dir"/opt/firmware-blobs/*.bin | wc -l) AIC8800 firmware blob(s)."
else
    printf "[prebuild:bootstrap] ERROR: AIC8800 firmware blobs not found at %s/components/aic8800/firmware/\n" "$repo_root" >&2
    printf "The bootstrap inner script requires these blobs and will fail ~15 min into QEMU.\n" >&2
    printf "Obtain the firmware files and place them at that path, then re-run.\n" >&2
    exit 1
fi

# ── In-guest provisioning inner script (Alpine /bin/sh) ─────────────────────────
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

# The root filesystem is grown to the full image size on the host (resize2fs,
# before boot); the StarryOS guest cannot reliably resize it, so no in-guest
# resize is attempted here.

echo "[bootstrap] apk update + install build toolchain..."
apk update || fail "apk update failed"
apk add --no-cache \
    build-base clang clang-dev cmake pkgconf git curl python3 \
    linux-headers openssl-dev perl bash tar xz musl-dev \
    || fail "apk add failed"

# Inner scripts and the kernel linker wrapper use bash arrays; make /bin/sh bash.
[ -x /bin/bash ] || fail "bash missing after install"
ln -sf /bin/bash /bin/sh
ln -sf /bin/bash /usr/bin/bash 2>/dev/null || true

# musl cross-toolchain names the std build expects (Alpine's native gcc IS musl).
gcc_path="$(command -v gcc)" || fail "gcc not found"
ar_path="$(command -v ar)" || fail "ar not found"
ln -sf "$gcc_path" /usr/local/bin/x86_64-linux-musl-cc
ln -sf "$gcc_path" /usr/local/bin/x86_64-linux-musl-gcc
ln -sf "$ar_path"  /usr/local/bin/x86_64-linux-musl-ar
echo "[bootstrap] musl toolchain symlinks ready."

echo "[bootstrap] Extracting source tree to /opt/starryos..."
mkdir -p /opt/starryos
tar xf /opt/starryos-src.tar -C /opt/starryos || fail "source untar failed"
[ -f /opt/starryos/Cargo.toml ] || fail "Cargo.toml missing after untar"
[ -f /opt/.source-commit ] && cp /opt/.source-commit /opt/starryos/.source-commit 2>/dev/null || true

# AIC8800 firmware blobs (xtask hashes them before every Starry build).
if ls /opt/firmware-blobs/*.bin >/dev/null 2>&1; then
    mkdir -p /opt/starryos/components/aic8800/firmware
    cp /opt/firmware-blobs/*.bin /opt/starryos/components/aic8800/firmware/
    echo "[bootstrap] AIC8800 firmware staged."
else
    fail "AIC8800 firmware blobs missing in overlay"
fi

echo "[bootstrap] Installing Rust toolchain..."
CHANNEL=$(awk -F'"' '/channel[[:space:]]*=/{print $2; exit}' /opt/starryos/rust-toolchain.toml 2>/dev/null) || true
[ -n "$CHANNEL" ] || CHANNEL=nightly-2026-05-28
if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "$CHANNEL" --profile minimal \
        || fail "rustup install failed"
fi
. "$HOME/.cargo/env"
rustup component add rust-src llvm-tools-preview || fail "rustup component add failed"
rustup target add x86_64-unknown-none || fail "rustup target add failed"
echo "[bootstrap] $(rustc --version)"

# kallsyms tools: rust-nm / rust-objcopy (cargo-binutils) + gen_ksym (ksym).
# Guarded so re-runs skip the (slow) rebuild when the tools already persist.
echo "[bootstrap] Installing kallsyms tools..."
if ! command -v rust-nm >/dev/null 2>&1 || ! command -v rust-objcopy >/dev/null 2>&1; then
    cargo install --locked cargo-binutils 2>&1 || cargo install cargo-binutils 2>&1 \
        || fail "cargo install cargo-binutils failed"
fi
if ! command -v gen_ksym >/dev/null 2>&1; then
    cargo install --locked ksym 2>&1 || cargo install ksym 2>&1 \
        || fail "cargo install ksym failed"
fi
command -v gen_ksym >/dev/null 2>&1 || fail "gen_ksym missing after install"
command -v rust-nm >/dev/null 2>&1 || fail "rust-nm missing after install"
command -v rust-objcopy >/dev/null 2>&1 || fail "rust-objcopy missing after install"
echo "[bootstrap] kallsyms tools ready."

# NOTE: the offline dependency-cache warm-up (an in-guest -Zbuild-std build) is
# intentionally NOT performed here. Under StarryOS the download-during-build hits
# resource limits (tmpfs target -> RAM OOM; disk target -> rsext4 size limits),
# so a self-contained offline-buildable blueprint cannot be produced under
# StarryOS. This rootfs is fully PROVISIONED (toolchain + Rust + kallsyms tools +
# source + firmware); for a verified offline self-compile use the downloadable
# pre-baked blueprint or the privileged prepare-selfhost-rootfs.sh (see docs).
rm -f /opt/starryos-src.tar 2>/dev/null || true
sync

echo ""
echo "[bootstrap] Toolchain provisioning complete (no host sudo)."
echo "SELFHOST_BOOTSTRAP_SUCCESS"
echo ""
sync
poweroff
INNER_EOF
chmod +x "$overlay_dir/usr/bin/self-compile-inner.sh"
info "Provisioning inner script generated."
info "Bootstrap overlay ready: $overlay_dir"
