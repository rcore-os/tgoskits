#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:-x86_64}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

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

    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)

    # qemu-user is needed to run apk inside the staging root on non-Alpine hosts
    case "$arch" in
        aarch64)     command -v qemu-aarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        riscv64)     command -v qemu-riscv64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        x86_64)      command -v qemu-x86_64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
        loongarch64) command -v qemu-loongarch64-static >/dev/null 2>&1 || missing+=(qemu-user-static) ;;
    esac

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
    # Set up apk repositories and DNS so apk can download packages
    mkdir -p "$staging_root/etc/apk"
    echo "https://dl-cdn.alpinelinux.org/alpine/v3.21/main" > "$staging_root/etc/apk/repositories"
    echo "https://dl-cdn.alpinelinux.org/alpine/v3.21/community" >> "$staging_root/etc/apk/repositories"
    cp /etc/resolv.conf "$staging_root/etc/resolv.conf"

    local qemu_user
    case "$arch" in
        aarch64)     qemu_user="qemu-aarch64-static" ;;
        riscv64)     qemu_user="qemu-riscv64-static" ;;
        x86_64)      qemu_user="qemu-x86_64-static" ;;
        loongarch64) qemu_user="qemu-loongarch64-static" ;;
        *)           echo "error: unsupported arch: $arch" >&2; exit 1 ;;
    esac

    echo "[pip prebuild] installing python3 and py3-pip via qemu-user apk..."
    export QEMU_LD_PREFIX="$staging_root"
    export LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib"
    "$qemu_user" -L "$staging_root" "$staging_root/sbin/apk" \
        add --no-cache --root "$staging_root" python3 py3-pip
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
