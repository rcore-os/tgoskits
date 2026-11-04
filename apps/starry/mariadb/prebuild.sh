#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:-}"
rootfs="${STARRY_ROOTFS:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
rootfs_size="${MARIADB_ROOTFS_SIZE:-4G}"

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
    command -v e2fsck >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v numfmt >/dev/null 2>&1 || missing+=(coreutils)
    command -v stat >/dev/null 2>&1 || missing+=(coreutils)
    command -v truncate >/dev/null 2>&1 || missing+=(coreutils)
    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    echo "error: missing required host packages: ${missing[*]}" >&2
    exit 1
}

run_e2fsck() {
    local status=0
    e2fsck -f -y "$1" >/dev/null || status=$?
    if (( status > 1 )); then
        echo "error: e2fsck failed for $1 with status $status" >&2
        exit 2
    fi
}

grow_rootfs() {
    [[ -f "$rootfs" ]] || { echo "error: rootfs image missing: $rootfs" >&2; exit 2; }

    local before target after
    before=$(stat -c %s "$rootfs")
    target=$(numfmt --from=iec "$rootfs_size")
    if (( before >= target )); then
        echo "prebuild: rootfs is $((before / 1024 / 1024)) MiB; target $rootfs_size already satisfied"
        return
    fi

    echo "prebuild: growing rootfs from $((before / 1024 / 1024)) MiB to $rootfs_size"
    truncate -s "$rootfs_size" "$rootfs"
    run_e2fsck "$rootfs"
    resize2fs "$rootfs" >/dev/null
    after=$(stat -c %s "$rootfs")
    echo "prebuild: rootfs grown to $((after / 1024 / 1024)) MiB"
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
    install -m 0755 "$app_dir/mariadb-test.sh" "$overlay_dir/usr/bin/mariadb-test.sh"

    copy_base_text_file_to_overlay /etc/apk/repositories
    copy_base_text_file_to_overlay /etc/resolv.conf
}

require_env STARRY_ARCH "$arch"
require_env STARRY_ROOTFS "$rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
grow_rootfs
populate_overlay
