#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

READELF="${READELF:-readelf}"

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
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v "$READELF" >/dev/null 2>&1 || missing+=("readelf (binutils)")

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "error: missing required host packages and apt-get is unavailable: ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

ensure_musl_ld() {
    local musl_ld="$staging_root/lib/ld-musl-x86_64.so.1"

    if [[ ! -x "$musl_ld" ]]; then
        echo "error: musl dynamic linker not found: $musl_ld" >&2
        exit 1
    fi
}

install_gdb_package() {
    local musl_ld="$staging_root/lib/ld-musl-x86_64.so.1"
    local guest_apk="$staging_root/sbin/apk"
    local apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/gdb-apk-cache"

    mkdir -p "$apk_cache"

    if [[ ! -x "$guest_apk" ]]; then
        echo "error: staging root is missing guest apk: $guest_apk" >&2
        exit 1
    fi

    echo "Installing gdb via musl ld (Alpine x86_64)..."
    "$musl_ld" \
        --library-path "$staging_root/lib:$staging_root/usr/lib:$staging_root/usr/local/lib" \
        "$guest_apk" \
        --root "$staging_root" \
        --repositories-file "$staging_root/etc/apk/repositories" \
        --keys-dir "$staging_root/etc/apk/keys" \
        --cache-dir "$apk_cache" \
        --update-cache \
        --timeout 60 \
        --no-interactive \
        --force-no-chroot \
        --scripts=no \
        add gdb
}

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "error: missing guest file after gdb package install: $guest_path" >&2
        exit 1
    fi

    if [[ -L "$source" ]]; then
        source="$(readlink -f "$source")"
    fi

    install -Dm"$mode" "$source" "$target"
}

find_library_path() {
    local library="$1"
    local dir

    for dir in lib usr/lib usr/local/lib; do
        if [[ -e "$staging_root/$dir/$library" ]]; then
            printf '/%s/%s\n' "$dir" "$library"
            return 0
        fi
    done

    return 1
}

copy_runtime_dependencies() {
    local pending=("$@")
    local seen=" "
    local guest_path library

    while [[ ${#pending[@]} -gt 0 ]]; do
        guest_path="${pending[0]}"
        pending=("${pending[@]:1}")

        if [[ "$seen" == *" $guest_path "* ]]; then
            continue
        fi
        seen+="$guest_path "

        while IFS= read -r library; do
            local library_path
            if ! library_path="$(find_library_path "$library")"; then
                continue
            fi
            copy_file_to_overlay "$library_path" 0644
            pending+=("$library_path")
        done < <(
            "$READELF" -d "$staging_root$guest_path" 2>/dev/null |
                sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p'
        )
    done
}

populate_overlay() {
    copy_file_to_overlay /usr/bin/gdb 0755
    copy_runtime_dependencies /usr/bin/gdb
}

require_env STARRY_BASE_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_tools
extract_base_rootfs
ensure_musl_ld
install_gdb_package
populate_overlay
