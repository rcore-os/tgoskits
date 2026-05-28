#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/pip-apk-cache"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

ensure_host_packages() {
    local missing=()

    command -v apk >/dev/null 2>&1 || missing+=(apk-tools)
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)

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

install_pip_packages() {
    mkdir -p "$apk_cache"
    echo "[pip prebuild] installing python3 and py3-pip via host apk..."
    apk --root "$staging_root" \
        --cache-dir "$apk_cache" \
        --update-cache \
        --no-progress \
        --no-scripts \
        add python3 py3-pip
}

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "error: missing guest file after package install: $guest_path" >&2
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
            readelf -d "$staging_root$guest_path" 2>/dev/null |
                sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p'
        )
    done
}

populate_overlay() {
    copy_file_to_overlay /usr/bin/python3 0755
    copy_file_to_overlay /usr/bin/pip3 0755

    if [[ -e "$staging_root/usr/bin/pip" ]]; then
        copy_file_to_overlay /usr/bin/pip 0755
    fi

    copy_runtime_dependencies /usr/bin/python3

    # Copy Python standard library
    local pyver
    pyver="$(ls "$staging_root/usr/lib/python3"* -d 2>/dev/null | head -1 | xargs basename)"
    if [[ -n "$pyver" && -d "$staging_root/usr/lib/$pyver" ]]; then
        mkdir -p "$overlay_dir/usr/lib/$pyver"
        cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    fi

    # Copy pip site-packages
    if [[ -d "$staging_root/usr/lib/python3" ]]; then
        mkdir -p "$overlay_dir/usr/lib/python3"
        cp -a "$staging_root/usr/lib/python3/." "$overlay_dir/usr/lib/python3/"
    fi

    install -Dm0755 "$app_dir/test_pip.sh" "$overlay_dir/usr/bin/test_pip.sh"
}

require_env STARRY_BASE_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
install_pip_packages
populate_overlay
