#!/usr/bin/env bash
#
# Prepares the persistent rootfs overlay for the x86_64 StarryOS self-build app.
# The app runner owns rootfs injection and QEMU startup; this script only stages
# the exact current checkout and the guest-side runner.

set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:?STARRY_WORKSPACE is required}"
rootfs="${STARRY_ROOTFS:?STARRY_ROOTFS is required}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:?STARRY_ARCH is required}"
rootfs_size_mib="${SELFHOST_ROOTFS_SIZE_MIB:-32768}"
output_dir="$workspace/target/starry-selfhost-x86_64"

require_x86_64() {
    if [[ "$arch" != "x86_64" ]]; then
        echo "selfhost prebuild only supports x86_64, got $arch" >&2
        exit 2
    fi
}

git_value() {
    local fallback="$1"
    shift
    git -C "$workspace" "$@" 2>/dev/null || printf '%s\n' "$fallback"
}

source_dirty() {
    if [[ -n "$(git -C "$workspace" status --porcelain --untracked-files=all)" ]]; then
        printf '%s\n' true
    else
        printf '%s\n' false
    fi
}

resize_rootfs() {
    cargo xtask image resize "$rootfs" --size-mib "$rootfs_size_mib"
}

stage_source_archive() {
    local source_tar="$output_dir/tgoskits-src.tar"

    tar -C "$workspace" \
        --exclude=.git \
        --exclude=target \
        --exclude=tmp \
        --exclude=.tgos-images \
        --exclude=.cache \
        --exclude=download \
        --exclude=.idea \
        --exclude=.vscode \
        -cf "$source_tar" .
    install -m 0644 "$source_tar" "$overlay_dir/opt/tgoskits-src.tar"
}

stage_source_metadata() {
    local metadata="$output_dir/tgoskits-src.meta"

    cat >"$metadata" <<EOF
commit=$(git_value unknown rev-parse HEAD)
ref=$(git_value detached symbolic-ref --quiet --short HEAD)
dirty=$(source_dirty)
generated_by=apps/starry/selfhost/selfhost-full-kernel/prebuild.sh
EOF
    install -m 0644 "$metadata" "$overlay_dir/opt/tgoskits-src.meta"
}

stage_guest_resolver() {
    local resolver="$overlay_dir/etc/resolv.conf"
    local resolver_source

    mkdir -p "$(dirname "$resolver")"
    : >"$resolver"
    for resolver_source in /run/systemd/resolve/resolv.conf /etc/resolv.conf; do
        [[ -f "$resolver_source" ]] || continue
        awk '
            $1 == "nameserver" && $2 !~ /^127\./ && $2 !~ /:/ && $2 != "10.0.2.3" {
                print
            }
        ' "$resolver_source" >>"$resolver"
        if [[ -s "$resolver" ]]; then
            return
        fi
    done

    cat >"$resolver" <<'EOF'
nameserver 1.1.1.1
nameserver 8.8.8.8
EOF
}

stage_guest_runner() {
    install -m 0755 "$app_dir/guest-selfbuild.sh" "$overlay_dir/opt/starry-selfhost-run.sh"
}

stage_guest_reboot_guard() {
    local profile_dir="$overlay_dir/etc/profile.d"

    mkdir -p "$profile_dir"
    install -m 0644 \
        "$app_dir/guest-selfbuild-reboot-guard.sh" \
        "$profile_dir/starry-selfhost-reboot-guard.sh"
}

stage_run_state() {
    local run_id

    run_id="$(git_value unknown rev-parse --short=12 HEAD)-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    printf '%s\n' "$run_id" >"$overlay_dir/opt/starry-selfhost.run-id"
    printf 'ready %s prebuild\n' "$run_id" >"$overlay_dir/opt/starry-selfhost.state"
}

# Build a pre-extracted Rust toolchain tarball from host-downloaded components.
# The guest cannot reliably extract .tar.xz files because: (1) QEMU slirp
# degrades catastrophically for large downloads, and (2) StarryOS MemoryFs
# (/tmp) has a limited size — filling it during XZ extraction freezes the
# guest.  We download the six component tarballs on the host, extract them
# into a merged toolchain tree, and inject a single uncompressed tar so the
# guest only needs `tar xf` (no XZ, no network, no tmpfs pressure).
stage_rust_toolchain() {
    local rust_date="2026-05-28"
    local rust_dl="https://static.rust-lang.org/dist/${rust_date}"
    local toolchain_name="nightly-2026-05-28-x86_64-unknown-linux-musl"
    local toolchain_version="2"
    local version_file=".starry-selfhost-toolchain-version"
    local stage_dir="$output_dir/toolchain-stage"
    local toolchain_dir="$stage_dir/$toolchain_name"
    local component_stage="$stage_dir/component"
    local cache_dir="$output_dir/toolchain-downloads"
    local output_tar="$output_dir/rust-toolchain.tar"

    # component → sha256 from channel-rust-nightly.toml
    local pairs=(
        "rustc:b03dac6f955cf5e8075d4187e2579bad0737cbc96caaa7e76c9a949a47bae0ff"
        "cargo:4180435487dadf1593925f11e1dd4b02dbd5315d7a4813b8c214b96410957c3d"
        "rust-std:783e922fb28ff74488db25ef0c62ef8147ba509b7e7d19ac8adfadfc3924bf41"
        "rust-src:3ef29c6fe273c9c1fc210a53c461a1f984fc8857be508aa7aa3e8f82f23652b2"
        "llvm-tools:13bdcad985200f19188537e629bb80a7cd104237ad4469deebb53eb32b4a29ec"
        "rust-std-none:2e67b503d145f68ab474fc7070bac3a1d936d5dd78f96a8bc3a2c5d98baa190d"
    )

    if [[ -f "$output_tar" ]] \
        && [[ "$(tar -xOf "$output_tar" "$toolchain_name/$version_file" 2>/dev/null)" \
            == "$toolchain_version" ]]; then
        echo "[prebuild] rust toolchain tar already built ($(du -h "$output_tar" | cut -f1)) — skipping"
        install -m 0644 "$output_tar" "$overlay_dir/opt/rust-toolchain.tar"
        return
    fi

    rm -rf "$stage_dir"
    mkdir -p "$toolchain_dir" "$cache_dir"

    for pair in "${pairs[@]}"; do
        component="${pair%%:*}"
        hash="${pair##*:}"
        case "$component" in
            rust-src)        tarball="rust-src-nightly.tar.xz" ;;
            llvm-tools)      tarball="llvm-tools-nightly-x86_64-unknown-linux-musl.tar.xz" ;;
            rust-std-none)   tarball="rust-std-nightly-x86_64-unknown-none.tar.xz" ;;
            *)               tarball="${component}-nightly-x86_64-unknown-linux-musl.tar.xz" ;;
        esac
        url="${rust_dl}/${tarball}"
        dest="$cache_dir/$hash"

        if [[ -f "$dest" ]] \
            && printf '%s  %s\n' "$hash" "$dest" | sha256sum --check --status; then
            echo "[prebuild]   ${component} already downloaded ($(du -h "$dest" | cut -f1))"
        else
            rm -f "$dest"
            echo "[prebuild]   downloading ${tarball}..."
            curl -fsSL --retry 3 --connect-timeout 30 --max-time 600 \
                "$url" -o "${dest}.tmp" 2>/dev/null || {
                    rm -f "${dest}.tmp"
                    echo "[prebuild] ERROR: failed to download ${tarball}" >&2
                    exit 1
            }
            mv "${dest}.tmp" "$dest"
        fi

        if ! printf '%s  %s\n' "$hash" "$dest" | sha256sum --check --status; then
            rm -f "$dest"
            echo "[prebuild] ERROR: checksum mismatch for ${tarball}" >&2
            exit 1
        fi

        echo "[prebuild]   installing ${tarball}..."
        rm -rf "$component_stage"
        mkdir -p "$component_stage"
        tar xf "$dest" -C "$component_stage" 2>/dev/null || {
            echo "[prebuild] ERROR: failed to extract ${tarball}" >&2
            exit 1
        }
        installer="$(find "$component_stage" -mindepth 2 -maxdepth 2 -type f -name install.sh -print -quit)"
        if [[ -z "$installer" ]]; then
            echo "[prebuild] ERROR: installer missing from ${tarball}" >&2
            exit 1
        fi
        bash "$installer" --prefix="$toolchain_dir" --disable-ldconfig >/dev/null || {
            echo "[prebuild] ERROR: failed to install ${tarball}" >&2
            exit 1
        }
    done

    [[ -x "$toolchain_dir/bin/rustc" ]] \
        || { echo "[prebuild] ERROR: rustc missing from assembled toolchain" >&2; exit 1; }
    [[ -x "$toolchain_dir/bin/cargo" ]] \
        || { echo "[prebuild] ERROR: cargo missing from assembled toolchain" >&2; exit 1; }
    [[ -d "$toolchain_dir/lib/rustlib/src/rust/library" ]] \
        || { echo "[prebuild] ERROR: rust-src missing from assembled toolchain" >&2; exit 1; }
    [[ -d "$toolchain_dir/lib/rustlib/x86_64-unknown-none/lib" ]] \
        || { echo "[prebuild] ERROR: x86_64-unknown-none missing from assembled toolchain" >&2; exit 1; }
    printf '%s\n' "$toolchain_version" >"$toolchain_dir/$version_file"

    echo "[prebuild] creating uncompressed toolchain tar..."
    rm -f "$output_tar"
    tar -C "$stage_dir" -cf "$output_tar" "$toolchain_name"
    echo "[prebuild] rust toolchain tar built ($(du -h "$output_tar" | cut -f1))"

    rm -rf "$stage_dir"
    install -m 0644 "$output_tar" "$overlay_dir/opt/rust-toolchain.tar"
}

require_x86_64
mkdir -p "$output_dir" "$overlay_dir/opt"
resize_rootfs
stage_source_archive
stage_source_metadata
stage_guest_resolver
stage_guest_runner
stage_guest_reboot_guard
stage_run_state
stage_rust_toolchain

echo "selfhost x86_64 overlay ready in $overlay_dir"
echo "rootfs=$rootfs"
echo "rootfs_size_mib=$rootfs_size_mib"
