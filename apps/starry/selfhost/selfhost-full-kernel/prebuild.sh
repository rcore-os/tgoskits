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

# Pre-download rustup component tarballs on the host so the guest can
# populate the rustup download cache in-RAM without touching QEMU user-
# mode networking.  QEMU slirp degrades catastrophically for large
# downloads (TCP throughput collapses from ~100 KiB/s to <1 KiB/s
# after ~100 MiB), making a ~300 MiB toolchain download impossible.
stage_rust_download_cache() {
    local rust_date="2026-05-28"
    local rust_dl="https://static.rust-lang.org/dist/${rust_date}"
    local cache_dir="$overlay_dir/root/.rustup-dl-cache"
    mkdir -p "$cache_dir"

    # component → sha256 from channel-rust-nightly.toml (x86_64-unknown-linux-musl)
    for pair in \
        "rustc:b03dac6f955cf5e8075d4187e2579bad0737cbc96caaa7e76c9a949a47bae0ff" \
        "cargo:4180435487dadf1593925f11e1dd4b02dbd5315d7a4813b8c214b96410957c3d" \
        "rust-std:783e922fb28ff74488db25ef0c62ef8147ba509b7e7d19ac8adfadfc3924bf41"
    do
        component="${pair%%:*}"
        hash="${pair##*:}"
        url="${rust_dl}/${component}-nightly-x86_64-unknown-linux-musl.tar.xz"
        dest="$cache_dir/$hash"
        if [ -f "$dest" ] && [ "$(stat -c%s "$dest" 2>/dev/null)" -gt 10000000 ]; then
            echo "[prebuild] rust ${component} tarball already cached ($(du -h "$dest" | cut -f1))"
            continue
        fi
        echo "[prebuild] downloading rust ${component} tarball (~$( \
            curl -sI "$url" 2>/dev/null | awk '/content-length/ {printf "%.0f", $2/1024/1024}') MiB)..."
        if curl -fsSL --retry 3 --connect-timeout 30 --max-time 600 \
            "$url" -o "${dest}.tmp" 2>/dev/null; then
            mv "${dest}.tmp" "$dest"
            echo "[prebuild]   ${component} cached ($(du -h "$dest" | cut -f1))"
        else
            rm -f "${dest}.tmp"
            echo "[prebuild] WARNING: failed to download ${component} tarball (guest will fall back to network)" >&2
        fi
    done
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
stage_rust_download_cache

echo "selfhost x86_64 overlay ready in $overlay_dir"
echo "rootfs=$rootfs"
echo "rootfs_size_mib=$rootfs_size_mib"
