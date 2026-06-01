#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
output_rootfs="${STARRY_OUTPUT_ROOTFS:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

ensure_host_tools() {
    local missing=()

    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v tar >/dev/null 2>&1 || missing+=(tar)

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    echo "error: missing required host packages: ${missing[*]}" >&2
    exit 1
}

rebuild_clean_rootfs_cache() {
    local archive="$1"

    echo "rebuilding clean rootfs cache: cargo xtask starry rootfs --arch $arch"
    rm -f "$archive" "$base_rootfs"
    (
        cd "$workspace"
        cargo xtask starry rootfs --arch "$arch"
    )

    if [[ ! -f "$archive" ]]; then
        echo "error: rootfs command did not produce clean archive: $archive" >&2
        exit 1
    fi
}

refresh_output_rootfs() {
    local base_dir image_name archive output_dir tmp_dir extracted

    base_dir="$(dirname "$base_rootfs")"
    image_name="$(basename "$base_rootfs")"
    archive="$base_dir/${image_name}.tar.xz"
    output_dir="$(dirname "$output_rootfs")"
    mkdir -p "$output_dir"

    if [[ ! -f "$archive" ]]; then
        rebuild_clean_rootfs_cache "$archive"
    fi

    echo "refreshing MariaDB app rootfs from clean archive: $archive"
    tmp_dir="$(mktemp -d "$output_dir/.mariadb-rootfs.XXXXXX")"
    (
        trap 'rm -rf "$tmp_dir"' EXIT
        if ! tar -xJf "$archive" -C "$tmp_dir" "$image_name"; then
            rm -f "$archive"
            rebuild_clean_rootfs_cache "$archive"
            tar -xJf "$archive" -C "$tmp_dir" "$image_name"
        fi
        extracted="$tmp_dir/$image_name"
        if [[ ! -s "$extracted" ]]; then
            echo "error: clean rootfs archive did not produce $image_name" >&2
            exit 1
        fi
        chmod 0644 "$extracted"
        mv -f "$extracted" "$output_rootfs"
    )
}

copy_base_text_file_to_overlay() {
    local guest_path="$1"
    local target="$overlay_dir$guest_path"

    mkdir -p "$(dirname "$target")"
    if ! debugfs -R "cat $guest_path" "$base_rootfs" >"$target" 2>/dev/null; then
        rm -f "$target"
        return
    fi
    chmod 0644 "$target"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/bin"
    install -m 0755 "$app_dir/mariadb-test.sh" "$overlay_dir/usr/bin/mariadb-test.sh"

    copy_base_text_file_to_overlay /etc/apk/repositories
    copy_base_text_file_to_overlay /etc/resolv.conf
}

require_env STARRY_ARCH "$arch"
require_env STARRY_BASE_ROOTFS "$base_rootfs"
require_env STARRY_OUTPUT_ROOTFS "$output_rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
refresh_output_rootfs
populate_overlay
