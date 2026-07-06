#!/usr/bin/env bash
#
# prebuild.sh — Bootstrap overlay generator: PROVISION a complete toolchain
# selfhost rootfs from the Alpine base entirely inside QEMU, with NO host sudo.
#
# What this DOES (verified): the Alpine guest (which has network) installs, under
# StarryOS's Linux compat, everything needed for a fully offline self-compile:
#   1. Build toolchain (apk) + bash
#   2. x86_64-linux-musl-{cc,gcc,ar} symlinks -> Alpine's native musl gcc/ar
#   3. Full source tree (git archive, staged via overlay) -> /opt/starryos
#   4. AIC8800 firmware blobs (downloaded in-guest from pinned GitHub commit,
#      SHA-256 verified — identical to xtask's ensure_aic8800_firmware)
#   5. Rust nightly + rust-src + llvm-tools-preview + bare-metal target
#   6. kallsyms tools (cargo-binutils -> rust-nm/rust-objcopy, ksym -> gen_ksym)
#   7. Offline dependency cache warm-up (cargo fetch)
#
# After bootstrap, the rootfs IS fully self-compile-capable — no sudo,
# no pre-baked image download, no host-side firmware prerequisite.
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
# Bootstrap only provisions the toolchain — it does NOT compile the kernel, so
# firmware is optional here.  The full-kernel self-compile will check for firmware
# at build time and fail with a clear message if they are missing.
if ls "$repo_root"/components/aic8800/firmware/*.bin >/dev/null 2>&1; then
    mkdir -p "$overlay_dir/opt/firmware-blobs"
    cp "$repo_root"/components/aic8800/firmware/*.bin "$overlay_dir/opt/firmware-blobs/"
    info "Staged $(ls "$overlay_dir"/opt/firmware-blobs/*.bin | wc -l) AIC8800 firmware blob(s)."
else
    info "AIC8800 firmware blobs not found (gitignored) — skipping (bootstrap does not compile)."
    info "Place them at components/aic8800/firmware/ before running the full self-compile."
fi

# ── In-guest provisioning inner script (Alpine /bin/sh) ─────────────────────────
cat > "$overlay_dir/usr/bin/self-compile-inner.sh" << 'INNER_EOF'
#!/bin/sh
set -euo pipefail

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

# Inner scripts carry '#!/usr/bin/bash' shebangs and are invoked via
# shell_init_cmd; they do NOT depend on /bin/sh.  The kernel init process
# (/bin/sh -c init.sh) MUST stay on busybox — replacing it with bash
# breaks init loading.  Only ensure /usr/bin/bash exists for the inner
# scripts.
[ -x /bin/bash ] || fail "bash missing after install"
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

# AIC8800 Wi-Fi firmware — download from the pinned upstream commit so the
# subsequent offline self-compile finds the blobs xtask requires.
echo "[bootstrap] Downloading AIC8800 firmware blobs..."
FW_COMMIT=c56f910044cc854d6c553bcb9a644f3bca5a4c38
FW_BASE="https://raw.githubusercontent.com/lxowalle/aic8800-sdio-firmware/$FW_COMMIT"
mkdir -p /opt/starryos/components/aic8800/firmware

# Each entry: "remote_subpath|local_name|sha256"
FW_FILES="aic8800_and_aic8800D80/fmacfw.bin|fmacfw.bin|2c6e70726df10ef74d9b1a657c74fdcfaeb88855b96b2c9bc8e0e603ac7c4cc3
aic8800_and_aic8800D80/fmacfw_patch.bin|fmacfw_patch.bin|6c8126ad655e9971f05ca03dc60fa82cb6d48c3b02cf3ba960137566ce2e28d5
aic8800DC/fmacfw_patch_8800dc_u02.bin|fmacfw_patch_8800dc_u02.bin|69d3ac2038da3b8e652ed1ec5079598ceb6df51db7b87b1d33f6d3c820c86a6f
aic8800DC/fw_patch_8800dc_u02.bin|fw_patch_8800dc_u02.bin|c4087b95e788785df0fc55aa92152d214323ee028c70ba0ebb23944d4070340b
aic8800DC/fw_patch_table_8800dc_u02.bin|fw_patch_table_8800dc_u02.bin|e7eea12cc85fca5d8667182b4520b6a0929044c70c6d9e9a3d7ece8b16169688
aic8800_and_aic8800D80/fmacfw_8800d80_u02.bin|fmacfw_8800d80_u02.bin|ffb49ede6004e58453f01489edf28b888b509529c3173554c98aa94fbb33507d
aic8800_and_aic8800D80/fw_patch_8800d80_u02.bin|fw_patch_8800d80_u02.bin|f0e2f5bbc17bc327ca7f1574ff55370dfd863d931514347bb4abc18a74f6218f
aic8800_and_aic8800D80/fw_patch_table_8800d80_u02.bin|fw_patch_table_8800d80_u02.bin|9decb77435b7e9713e33e32da483d683b7329ed93b672b2d1b134031d7da5f67"

echo "$FW_FILES" | while IFS="|" read -r remote name expected; do
    dest="/opt/starryos/components/aic8800/firmware/$name"
    if [ -f "$dest" ] && echo "$expected  $dest" | sha256sum -c - >/dev/null 2>&1; then
        echo "[bootstrap]   $name (cached)"
        continue
    fi
    echo "[bootstrap]   fetching $name..."
    curl --retry 3 --retry-delay 2 --connect-timeout 30 --max-time 120 -fsSL "$FW_BASE/$remote" -o "$dest" || fail "failed to download $name"
    actual=$(sha256sum "$dest" | awk '{print $1}')
    if [ "$actual" != "$expected" ]; then
        fail "sha256 mismatch for $name: expected $expected, got $actual"
    fi
    echo "[bootstrap]   $name OK"
done
echo "[bootstrap] AIC8800 firmware ready."

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

# Warm the offline dependency cache so the subsequent self-compile (which runs
# with --offline) finds all crate sources pre-fetched.  cargo fetch downloads
# every workspace dependency into CARGO_HOME (~1–2 GB on disk); no compilation
# occurs, so tmpfs/RAM pressure is minimal.
echo "[bootstrap] Warming offline dependency cache (cargo fetch)..."
cd /opt/starryos
cargo fetch 2>&1 || fail "cargo fetch failed — cannot warm offline cache"
cd /
echo "[bootstrap] Offline dependency cache warmed."
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
