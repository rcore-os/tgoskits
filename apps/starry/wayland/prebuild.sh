#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
arch="${STARRY_ARCH:-}"
rootfs="${STARRY_ROOTFS:-}"
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
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required host packages: ${missing[*]}" >&2
        exit 1
    fi
}

copy_base_text_file_to_overlay() {
    local guest_path="$1"
    local target="$overlay_dir$guest_path"
    mkdir -p "$(dirname "$target")"
    if ! debugfs -R "cat $guest_path" "$rootfs" >"$target" 2>/dev/null; then
        rm -f "$target"
        return
    fi
    chmod 0644 "$target"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/bin"
    cp "$app_dir/wayland-test.sh" "$overlay_dir/usr/bin/wayland-test.sh"
    chmod 0755 "$overlay_dir/usr/bin/wayland-test.sh"

    copy_base_text_file_to_overlay /etc/apk/repositories
    copy_base_text_file_to_overlay /etc/resolv.conf
}

require_env STARRY_ARCH "$arch"
require_env STARRY_ROOTFS "$rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
populate_overlay
