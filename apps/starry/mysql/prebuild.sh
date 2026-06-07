#!/usr/bin/env bash
set -eo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
rootfs="${STARRY_ROOTFS:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
arch="${STARRY_ARCH:-}"

mysql_version="${MYSQL_VERSION:-8.4.6}"
mysql_package_dir="mysql-${mysql_version}-linux-glibc2.28-x86_64"
mysql_tarball_name="${mysql_package_dir}.tar.xz"
mysql_tarball_url="${MYSQL_TARBALL_URL:-https://dev.mysql.com/get/Downloads/MySQL-8.4/$mysql_tarball_name}"
mysql_cache_dir="${MYSQL_CACHE_DIR:-$workspace/target/mysql}"
mysql_rootfs_size="${MYSQL_ROOTFS_SIZE:-5G}"
rootfs_release="${MYSQL_ROOTFS_RELEASE:-v0.0.5}"
base_rootfs_archive_name="rootfs-${arch}-debian.img.tar.xz"
base_rootfs_archive="${MYSQL_BASE_ROOTFS_ARCHIVE:-$workspace/tmp/axbuild/rootfs/$base_rootfs_archive_name}"
base_rootfs_archive_url="${MYSQL_BASE_ROOTFS_ARCHIVE_URL:-https://github.com/rcore-os/tgosimages/releases/download/$rootfs_release/$base_rootfs_archive_name}"
base_rootfs="${MYSQL_BASE_ROOTFS:-}"
apt_lists_ready=0

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

run_e2fsck() {
    local status

    set +e
    e2fsck -f -y "$1" >/dev/null
    status=$?
    set -e

    if (( (status & 4) != 0 || status >= 8 )); then
        echo "error: e2fsck failed for $1 with status $status" >&2
        exit 1
    fi
}

ensure_host_tools() {
    local missing=()

    for tool in apt-get chmod cp dirname dpkg-deb e2fsck find head id install ln losetup mkdir mktemp mount mountpoint mv numfmt resize2fs rm rmdir sort stat sync tar truncate umount wget; do
        command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done

    if [[ ${#missing[@]} -ne 0 ]]; then
        echo "error: missing required host tools: ${missing[*]}" >&2
        exit 1
    fi

    if [[ "$(id -u)" -ne 0 ]]; then
        if ! command -v sudo >/dev/null 2>&1; then
            echo "error: mysql prebuild requires root or sudo for loop mount" >&2
            exit 1
        fi
        if ! sudo -n true 2>/dev/null; then
            echo "error: mysql prebuild requires passwordless sudo for loop mount" >&2
            exit 1
        fi
    fi
}

run_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

download_mysql_tarball() {
    mkdir -p "$mysql_cache_dir"

    if [[ -n "${MYSQL_TARBALL:-}" ]]; then
        if [[ ! -f "$MYSQL_TARBALL" ]]; then
            echo "error: MYSQL_TARBALL not found: $MYSQL_TARBALL" >&2
            exit 1
        fi
        printf '%s\n' "$MYSQL_TARBALL"
        return
    fi

    local workspace_tarball="$workspace/mysql.tar.xz"
    if [[ -s "$workspace_tarball" ]]; then
        printf '%s\n' "$workspace_tarball"
        return
    fi

    local tarball="$mysql_cache_dir/$mysql_tarball_name"
    if [[ -s "$tarball" ]]; then
        printf '%s\n' "$tarball"
        return
    fi

    local tmp="$tarball.tmp"
    rm -f "$tmp"
    wget --no-check-certificate -O "$tmp" "$mysql_tarball_url"
    mv "$tmp" "$tarball"
    printf '%s\n' "$tarball"
}

extract_mysql_tarball() {
    local tarball="$1"
    local extract_root="$mysql_cache_dir/package"
    local package_path="$extract_root/$mysql_package_dir"

    if [[ -x "$package_path/bin/mysqld" && -x "$package_path/bin/mysql" ]]; then
        printf '%s\n' "$package_path"
        return
    fi

    rm -rf "$extract_root"
    mkdir -p "$extract_root"
    tar -xf "$tarball" -C "$extract_root"

    if [[ ! -x "$package_path/bin/mysqld" || ! -x "$package_path/bin/mysql" ]]; then
        echo "error: unexpected MySQL package layout under $extract_root" >&2
        exit 1
    fi

    printf '%s\n' "$package_path"
}

download_deb() {
    local deb_cache="$mysql_cache_dir/debs"
    local package

    mkdir -p "$deb_cache"
    for package in "$@"; do
        local existing=()
        while IFS= read -r path; do
            existing+=("$path")
        done < <(find "$deb_cache" -maxdepth 1 -type f -name "${package}_*.deb" | sort)

        if [[ ${#existing[@]} -ne 0 ]]; then
            printf '%s\n' "${existing[0]}"
            return
        fi

        if (cd "$deb_cache" && apt-get download "$package" >/dev/null); then
            local downloaded
            downloaded="$(find "$deb_cache" -maxdepth 1 -type f -name "${package}_*.deb" | sort | head -n 1)"
            if [[ -n "$downloaded" ]]; then
                printf '%s\n' "$downloaded"
                return
            fi
        fi

        if [[ "$apt_lists_ready" -eq 0 ]]; then
            run_root apt-get update >/dev/null
            apt_lists_ready=1
        fi

        if (cd "$deb_cache" && apt-get download "$package" >/dev/null); then
            local downloaded
            downloaded="$(find "$deb_cache" -maxdepth 1 -type f -name "${package}_*.deb" | sort | head -n 1)"
            if [[ -n "$downloaded" ]]; then
                printf '%s\n' "$downloaded"
                return
            fi
        fi
    done

    echo "error: failed to download any of: $*" >&2
    exit 1
}

prepare_debian_rootfs_archive() {
    if [[ -n "${MYSQL_BASE_ROOTFS_ARCHIVE:-}" ]]; then
        return
    fi

    mkdir -p "$(dirname "$base_rootfs_archive")"
    wget --no-check-certificate -c -O "$base_rootfs_archive" "$base_rootfs_archive_url"
}

prepare_mysql_rootfs_image() {
    local extract_dir extracted fallback

    if [[ -n "$base_rootfs" && "$rootfs" == "$base_rootfs" ]]; then
        return
    fi

    mkdir -p "$(dirname "$rootfs")"
    rm -f "$rootfs"

    if [[ -n "$base_rootfs" && -f "$base_rootfs" ]]; then
        cp --reflink=auto --sparse=always "$base_rootfs" "$rootfs" 2>/dev/null \
            || cp "$base_rootfs" "$rootfs"
        chmod 0644 "$rootfs"
        return
    fi

    prepare_debian_rootfs_archive

    if [[ -f "$base_rootfs_archive" ]]; then
        extract_dir="$(mktemp -d "${TMPDIR:-/tmp}/starry-mysql-base.XXXXXX")"
        tar -xf "$base_rootfs_archive" -C "$extract_dir"

        extracted="$extract_dir/rootfs-${arch}-debian.img"
        if [[ ! -f "$extracted" ]]; then
            extracted="$(find "$extract_dir" -maxdepth 1 -type f -name '*.img' | sort | head -n 1)"
        fi

        if [[ -z "$extracted" || ! -f "$extracted" ]]; then
            rm -rf "$extract_dir"
            echo "error: no rootfs image found in $base_rootfs_archive" >&2
            exit 1
        fi

        mv "$extracted" "$rootfs"
        rm -rf "$extract_dir"
        chmod 0644 "$rootfs"
        return
    fi

    fallback="$workspace/tmp/axbuild/rootfs/rootfs-${arch}-debian.img.tar.xz"
    echo "error: Debian base rootfs archive not found: $base_rootfs_archive" >&2
    echo "error: expected archive path: $fallback" >&2
    echo "error: download URL: $base_rootfs_archive_url" >&2
    exit 1
}

resize_rootfs_if_needed() {
    local current_size target_size
    current_size="$(stat -c '%s' "$rootfs")"
    target_size="$(numfmt --from=iec "$mysql_rootfs_size")"

    if (( current_size >= target_size )); then
        return
    fi

    truncate -s "$target_size" "$rootfs"
    run_e2fsck "$rootfs"
    resize2fs "$rootfs" >/dev/null
}

repair_rootfs_image() {
    run_e2fsck "$rootfs"
}

populate_rootfs() {
    local mnt="$1"
    local mysql_package="$2"
    local libaio_deb="$3"
    local libnuma_deb="$4"
    local libncurses_deb="$5"
    local libdir="$mnt/usr/lib/x86_64-linux-gnu"

    if [[ ! -f "$mnt/etc/debian_version" ]]; then
        echo "error: target rootfs is not Debian: $rootfs" >&2
        exit 1
    fi
    if [[ ! -e "$mnt/lib64/ld-linux-x86-64.so.2" && ! -e "$mnt/usr/lib64/ld-linux-x86-64.so.2" ]]; then
        echo "error: Debian rootfs is missing ld-linux-x86-64.so.2" >&2
        exit 1
    fi

    run_root rm -rf "$mnt/opt/mysql"
    run_root mkdir -p "$mnt/opt"
    run_root cp -a "$mysql_package" "$mnt/opt/mysql"

    run_root dpkg-deb -x "$libaio_deb" "$mnt"
    run_root dpkg-deb -x "$libnuma_deb" "$mnt"
    run_root dpkg-deb -x "$libncurses_deb" "$mnt"

    if [[ -e "$libdir/libaio.so.1t64" ]]; then
        run_root ln -sf libaio.so.1t64 "$libdir/libaio.so.1"
    fi
    if [[ -e "$libdir/libnuma.so.1.0.0" ]]; then
        run_root ln -sf libnuma.so.1.0.0 "$libdir/libnuma.so.1"
    fi

cat >"$mysql_cache_dir/mysql-env.sh" <<'EOF'
export MYSQL_HOME=/opt/mysql
export PATH=/opt/mysql/bin:$PATH
export LD_LIBRARY_PATH=/opt/mysql/lib:/opt/mysql/lib/private:/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}
EOF
    run_root install -m 0644 "$mysql_cache_dir/mysql-env.sh" "$mnt/root/mysql-env.sh"
}

populate_overlay() {
    mkdir -p "$overlay_dir/usr/bin" "$overlay_dir/root/mysql"
    install -m 0755 "$app_dir/mysql-test.sh" "$overlay_dir/usr/bin/mysql-test.sh"
    install -m 0755 "$app_dir/mysql-interactive.sh" "$overlay_dir/usr/bin/mysql-interactive.sh"
    cat >"$overlay_dir/root/mysql/prebuild-info.txt" <<EOF
mysql package: /opt/mysql
mysql version: $mysql_version
base rootfs archive: $base_rootfs_archive
rootfs size target: $mysql_rootfs_size
EOF
}

require_env STARRY_ROOTFS "$rootfs"
require_env STARRY_OVERLAY_DIR "$overlay_dir"
require_env STARRY_ARCH "$arch"

if [[ "$arch" != "x86_64" ]]; then
    echo "error: mysql prebuild currently supports only x86_64, got: $arch" >&2
    exit 1
fi

ensure_host_tools
mysql_tarball="$(download_mysql_tarball)"
mysql_package="$(extract_mysql_tarball "$mysql_tarball")"
libaio_deb="$(download_deb libaio1t64 libaio1)"
libnuma_deb="$(download_deb libnuma1)"
libncurses_deb="$(download_deb libncurses6)"

prepare_mysql_rootfs_image

if [[ ! -f "$rootfs" ]]; then
    echo "error: rootfs image not found: $rootfs" >&2
    exit 1
fi

repair_rootfs_image
resize_rootfs_if_needed

mnt="$(mktemp -d "${TMPDIR:-/tmp}/starry-mysql-rootfs.XXXXXX")"
loop="$(run_root losetup -f --show "$rootfs")"

cleanup() {
    if mountpoint -q "$mnt"; then
        run_root sync
        run_root umount "$mnt"
    fi
    if [[ -n "${loop:-}" ]]; then
        run_root losetup -d "$loop" 2>/dev/null || true
    fi
    rmdir "$mnt" 2>/dev/null || true
}
trap cleanup EXIT

run_root mount "$loop" "$mnt"
populate_rootfs "$mnt" "$mysql_package" "$libaio_deb" "$libnuma_deb" "$libncurses_deb"
run_root sync
run_root umount "$mnt"
run_root losetup -d "$loop"
loop=""

populate_overlay
