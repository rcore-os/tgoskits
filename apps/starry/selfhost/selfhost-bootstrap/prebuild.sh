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

# ── Stage the full source tree as a single tarball (git archive) ───────────────
# One overlay-injection write; extracting it in-guest is far cheaper than
# injecting the whole tree file-by-file via debugfs.
info "Staging source tarball (git archive HEAD)..."
git -C "$repo_root" archive --format=tar HEAD -o "$overlay_dir/opt/starryos-src.tar"
git -C "$repo_root" rev-parse HEAD > "$overlay_dir/opt/.source-commit" 2>/dev/null || true

# ── Stage AIC8800 firmware blobs (gitignored; absent from git archive) ─────────
# Firmware is NOT optional: the in-guest inner script needs all 8 AIC8800 blobs
# for the offline self-compile.  Stage host copies (when present) directly into
# the guest cache path the inner script checks — /opt/starryos/components/aic8800/
# firmware/ — so its SHA-256 cache check hits and the download is skipped.  The
# source tarball (extracted on top later) does not carry these gitignored blobs,
# so the staged files survive.  When the host has none, the inner script
# downloads them from the pinned upstream commit and SHA-256-verifies.  Bootstrap
# does not COMPILE the kernel; it only provisions the toolchain + firmware.
fw_stage_dir="$overlay_dir/opt/starryos/components/aic8800/firmware"
if ls "$repo_root"/components/aic8800/firmware/*.bin >/dev/null 2>&1; then
    mkdir -p "$fw_stage_dir"
    cp "$repo_root"/components/aic8800/firmware/*.bin "$fw_stage_dir/"
    info "Staged $(ls "$fw_stage_dir"/*.bin | wc -l) AIC8800 firmware blob(s) into the guest cache path."
else
    info "AIC8800 firmware blobs not staged from host (gitignored) — the in-guest inner script will download + SHA-256-verify them."
fi

# ── Stage Rust DATA components into the overlay ────────────────────────────────
# THE KEY CONSTRAINT: the host's rustc/cargo/rustup binaries are glibc-linked;
# the Alpine guest uses musl.  We CANNOT simply cp the host's toolchain bin/
# directory — the binaries won't execute on the guest.  The guest MUST install
# rustup natively via curl|sh (which downloads musl-compatible binaries).
#
# What we CAN pre-stage are DATA-ONLY components that don't depend on libc:
#   - rust-src: pure source tree, the 500+ MiB component that takes longest to
#     download and times out most reliably over QEMU user-net
#   - llvm-tools-preview binaries: pre-compiled host tools like llvm-objcopy
#     placed into CARGO_HOME/bin so kallsyms finds them without cargo install
#   - x86_64-unknown-none target: pre-compiled std libs, linked against the
#     guest's target, not the host's libc — safe to copy from host
# This keeps ~1 GiB of downloads off QEMU's anemic network while letting the
# runtime-dependent binaries stay in the guest's native musl path.
TOOLCHAIN_SRC="$HOME/.rustup/toolchains/nightly-2026-05-28-x86_64-unknown-linux-gnu"
TOOLCHAIN_NAME="nightly-2026-05-28-x86_64-unknown-linux-gnu"
STAGED=""

if [ -d "$TOOLCHAIN_SRC/lib/rustlib/src/rust" ]; then
    info "Pre-staging rust-src into overlay (~500 MiB, pure source — musl-safe)..."
    mkdir -p "$overlay_dir/root/.rustup/toolchains/$TOOLCHAIN_NAME/lib/rustlib/src"
    cp -a "$TOOLCHAIN_SRC/lib/rustlib/src/rust" \
          "$overlay_dir/root/.rustup/toolchains/$TOOLCHAIN_NAME/lib/rustlib/src/"
    STAGED="rust-src"
fi

if [ -d "$TOOLCHAIN_SRC/lib/rustlib/x86_64-unknown-none" ]; then
    info "Pre-staging x86_64-unknown-none target libs into overlay..."
    mkdir -p "$overlay_dir/root/.rustup/toolchains/$TOOLCHAIN_NAME/lib/rustlib"
    cp -a "$TOOLCHAIN_SRC/lib/rustlib/x86_64-unknown-none" \
          "$overlay_dir/root/.rustup/toolchains/$TOOLCHAIN_NAME/lib/rustlib/"
    STAGED="$STAGED target"
fi

# Pre-populate the llvm-tools binaries so kallsyms tools (cargo-binutils,
# which provides rust-nm/rust-objcopy) find them without needing a network
# `cargo install`.  These are host-compiled binaries but rust-nm/rust-objcopy
# are statically-linked tools that generally work fine across libc.
if [ -d "$TOOLCHAIN_SRC/lib/rustlib/x86_64-unknown-linux-gnu/bin" ]; then
    info "Pre-staging llvm-tools binaries into overlay..."
    mkdir -p "$overlay_dir/root/.cargo/bin"
    cp -a "$TOOLCHAIN_SRC/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy" \
          "$overlay_dir/root/.cargo/bin/rust-objcopy" 2>/dev/null || true
    cp -a "$TOOLCHAIN_SRC/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-nm" \
          "$overlay_dir/root/.cargo/bin/rust-nm" 2>/dev/null || true
    STAGED="$STAGED llvm-tools"
fi

if [ -n "$STAGED" ]; then
    info "Rust data components staged into overlay: $STAGED"
else
    info "Host Rust components not found; in-guest downloads will run (may be slow)."
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

# Idempotent: if the toolchain was already installed by a previous run,
# skip apk entirely.  The apk upgrade of libssl/libcrypto can produce ELF
# files that rsext4 cannot read after remount, so avoid re-running apk.
# Also require `tar` (a busybox applet) to be genuinely executable: a base
# image with bash/gcc/git but a broken busybox symlink would otherwise skip
# apk here and then fail on the source untar below ("can't open 'tar'").
if [ -f /bin/bash ] && [ -f /usr/bin/gcc ] && [ -f /usr/bin/git ] && tar --version >/dev/null 2>&1; then
    echo "[bootstrap] Build toolchain already installed — skipping apk."
else
echo "[bootstrap] apk add build toolchain (--no-cache, fresh index each run)..."
apk add --no-cache --no-scripts \
    build-base clang clang-dev cmake pkgconf git curl python3 \
    linux-headers openssl-dev perl bash tar xz musl-dev \
    || true
# Verify the packages that matter actually installed (apk may return non-zero
# when a post-install trigger such as busybox-suid segfaults, but the packages
# themselves are installed).  Only fail if a critical binary is missing.
[ -x /bin/bash ] || fail "bash missing after apk"
[ -x /usr/bin/gcc ] || fail "gcc missing after apk"
[ -x /usr/bin/git ] || fail "git missing after apk"
fi  # end of toolchain-already-installed guard

# --no-scripts skips ALL triggers, including busybox.  When apk was
# skipped (idempotent re-run), the previous bootstrap may have lost
# the busybox symlinks during the failed remount.  Re-create them
# unconditionally so tar/readlink/dirname are available.
echo "[bootstrap] Ensuring busybox symlinks..."
/bin/busybox --install -s /bin 2>/dev/null || true

# Both inner scripts now run under busybox '#!/bin/sh' (the rebased kernel
# cannot load dynamically-linked bash as a shebang interpreter).  The kernel
# init process (/bin/sh -c init.sh) MUST stay on busybox — replacing it with
# bash breaks init loading.  /usr/bin/bash is only needed for the `bash -c`
# heartbeat subprocess in the full-kernel inner script, so just ensure the
# symlink exists.
[ -x /bin/bash ] || fail "bash missing after install"
ln -sf /bin/bash /usr/bin/bash 2>/dev/null || true
	[ -x /usr/bin/bash ] || fail "/usr/bin/bash symlink missing after bootstrap"

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

# rustup/cargo/rustc from the host are glibc-linked — they CANNOT run on the
# Alpine musl guest.  The guest MUST install rustup natively via curl|sh.
# What the host-side prebuild CAN pre-stage are DATA components (rust-src,
# target libs, llvm-tools binaries) placed into the toolchain directory so
# that once rustup is installed natively, it finds the components already in
# place and skips the network-dependent downloads.
TOOLCHAIN_DIR="$HOME/.rustup/toolchains/nightly-2026-05-28-x86_64-unknown-linux-gnu"

if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "$CHANNEL" --profile minimal \
        || fail "rustup install failed"
fi
. "$HOME/.cargo/env"

# Pre-staged data components (rust-src source tree, target libraries) are
# placed under $TOOLCHAIN_DIR by the host-side prebuild overlay.  If a
# component is already on disk, skip the network download.
# rust-src: check for the library source tree
if [ -d "$TOOLCHAIN_DIR/lib/rustlib/src/rust" ]; then
    echo "[bootstrap] rust-src pre-staged from host — skipping download."
else
    for _ in 1 2 3 4 5; do
        rustup component add rust-src && break
        echo "[bootstrap] rustup component add rust-src failed, retrying..."
        sleep 5
    done || fail "rustup component add rust-src failed after 5 attempts"
fi

# llvm-tools-preview: check for the llvm-objcopy binary in the toolchain
if [ -f "$TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy" ]; then
    echo "[bootstrap] llvm-tools-preview pre-staged from host — skipping download."
else
    for _ in 1 2 3 4 5; do
        rustup component add llvm-tools-preview && break
        echo "[bootstrap] rustup component add llvm-tools-preview failed, retrying..."
        sleep 5
    done || fail "rustup component add llvm-tools-preview failed after 5 attempts"
fi

# x86_64-unknown-none target
if rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-none; then
    echo "[bootstrap] target x86_64-unknown-none already installed."
elif [ -d "$TOOLCHAIN_DIR/lib/rustlib/x86_64-unknown-none" ]; then
    echo "[bootstrap] target x86_64-unknown-none pre-staged from host."
else
    for _ in 1 2 3 4 5; do
        rustup target add x86_64-unknown-none && break
        echo "[bootstrap] rustup target add failed, retrying..."
        sleep 5
    done || fail "rustup target add failed after 5 attempts"
fi
echo "[bootstrap] $(rustc --version)"


# kallsyms tools: rust-nm / rust-objcopy (from cargo-binutils or llvm-tools-preview)
# + gen_ksym (ksym).  If the host-side prebuild pre-staged llvm-tools binaries
# into ~/.cargo/bin/ (as rust-nm/rust-objcopy), use them directly.  Otherwise
# fall back to `cargo install` which requires QEMU user-net (slow but retried).
echo "[bootstrap] Installing kallsyms tools..."
if ! command -v rust-nm >/dev/null 2>&1 || ! command -v rust-objcopy >/dev/null 2>&1; then
    if command -v cargo >/dev/null 2>&1; then
        cargo install --locked cargo-binutils 2>&1 || cargo install cargo-binutils 2>&1 \
            || fail "cargo install cargo-binutils failed"
    else
        fail "cargo not found — cannot install kallsyms tools; ensure rustup installed correctly"
    fi
fi
if ! command -v gen_ksym >/dev/null 2>&1; then
    if command -v cargo >/dev/null 2>&1; then
        cargo install --locked ksym 2>&1 || cargo install ksym 2>&1 \
            || fail "cargo install ksym failed"
    else
        fail "cargo not found — cannot install ksym; ensure rustup installed correctly"
    fi
fi
command -v gen_ksym >/dev/null 2>&1 || fail "gen_ksym missing after install"
command -v rust-nm >/dev/null 2>&1 || fail "rust-nm missing after install"
command -v rust-objcopy >/dev/null 2>&1 || fail "rust-objcopy missing after install"
echo "[bootstrap] kallsyms tools ready."

# Warm the offline dependency cache so the subsequent self-compile (which runs
# with --offline) finds all crate sources pre-fetched.  cargo fetch downloads
# every workspace dependency into CARGO_HOME (order of ~1 GB on disk); no compilation
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
