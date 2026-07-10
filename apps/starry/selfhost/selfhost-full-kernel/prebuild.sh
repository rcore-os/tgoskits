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
rootfs_size_mib="${SELFHOST_ROOTFS_SIZE_MIB:-16384}"
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

require_x86_64
mkdir -p "$output_dir" "$overlay_dir/opt"
resize_rootfs
stage_source_archive
stage_source_metadata
stage_guest_resolver
stage_guest_runner

echo "selfhost x86_64 overlay ready in $overlay_dir"
echo "rootfs=$rootfs"
echo "rootfs_size_mib=$rootfs_size_mib"
