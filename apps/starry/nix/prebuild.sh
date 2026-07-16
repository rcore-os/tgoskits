#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
nix_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/nixpkgs-cache"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/nix-apk-cache/${STARRY_ARCH:-x86_64}"
qemu_runner=""

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

# Map STARRY_ARCH to the matching qemu-user binary. The prebuild runs on the
# host but installs Alpine packages into the target rootfs.
qemu_user_binary_names() {
    case "${STARRY_ARCH:-x86_64}" in
        x86_64)      printf '%s\n' qemu-x86_64-static qemu-x86_64 ;;
        aarch64)     printf '%s\n' qemu-aarch64-static qemu-aarch64 ;;
        riscv64)     printf '%s\n' qemu-riscv64-static qemu-riscv64 ;;
        loongarch64) printf '%s\n' qemu-loongarch64-static qemu-loongarch64 ;;
        *)
            echo "error: unsupported STARRY_ARCH '${STARRY_ARCH}' for nix prebuild" >&2
            exit 1
            ;;
    esac
}

find_qemu_runner() {
    local candidate

    while IFS= read -r candidate; do
        if command -v "$candidate" >/dev/null 2>&1; then
            qemu_runner="$(command -v "$candidate")"
            return
        fi
    done < <(qemu_user_binary_names)

    if command -v apt-get >/dev/null 2>&1; then
        echo "installing missing host package: qemu-user-static"
        apt-get update
        apt-get install -y --no-install-recommends qemu-user-static
        while IFS= read -r candidate; do
            if command -v "$candidate" >/dev/null 2>&1; then
                qemu_runner="$(command -v "$candidate")"
                return
            fi
        done < <(qemu_user_binary_names)
    fi

    echo "error: missing qemu-user runner for ${STARRY_ARCH:-x86_64}" >&2
    exit 1
}

ensure_host_packages() {
    local missing=()

    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v curl >/dev/null 2>&1 || missing+=(curl)
    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v e2fsck >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v sha256sum >/dev/null 2>&1 || missing+=(coreutils)
    command -v tar >/dev/null 2>&1 || missing+=(tar)

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "error: missing required host packages and no supported package manager is available: ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

prepare_nix_conf() {
    mkdir -p "$overlay_dir/etc/nix"
    cat > "$overlay_dir/etc/nix/nix.conf" <<NIXCONF
sandbox = false
build-users-group =
substituters = https://mirrors.tuna.tsinghua.edu.cn/nix-channels/store https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
NIXCONF
    echo "nix.conf prepared"
}

extract_base_rootfs() {
    rm -rf "$staging_root"
    mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    if [[ ! -x "$staging_root/sbin/apk" ]]; then
        echo "error: staging root is missing guest apk: $staging_root/sbin/apk" >&2
        exit 1
    fi
}

run_guest_apk_with_retry() {
    local attempt
    local max_attempts=4

    for attempt in $(seq 1 "$max_attempts"); do
        if env -u LD_LIBRARY_PATH \
            QEMU_LD_PREFIX="$staging_root" \
            "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" "$@"; then
            return 0
        fi

        if [[ "$attempt" -eq "$max_attempts" ]]; then
            return 1
        fi

        echo "apk command failed, retrying ($attempt/$max_attempts)..." >&2
        sleep $((attempt * 3))
    done
}

install_nix_package() {
    mkdir -p "$apk_cache"
    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    echo "installing Alpine-packaged Nix into staging root via $qemu_runner"
    run_guest_apk_with_retry \
        --root "$staging_root" \
        --repositories-file "$staging_root/etc/apk/repositories" \
        --keys-dir "$staging_root/etc/apk/keys" \
        --cache-dir "$apk_cache" \
        --update-cache \
        --timeout 60 \
        --no-interactive \
        --force-no-chroot \
        --scripts=no \
        add nix

    if [[ ! -x "$staging_root/usr/bin/nix" ]]; then
        echo "error: apk add nix did not produce /usr/bin/nix" >&2
        exit 1
    fi
    copy_nix_closure_to_overlay
    verify_nix_overlay
}

copy_nix_closure_to_overlay() {
    local package

    while IFS= read -r package; do
        copy_package_files_to_overlay "$package"
    done < <(
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --cache-dir "$apk_cache" \
            info --recursive --format json nix |
            sed -n 's/^[[:space:]]*"name": "\(.*\)",$/\1/p'
    )

    copy_path_to_overlay /etc/apk/world
    copy_path_to_overlay /lib/apk/db/installed
}

verify_nix_overlay() {
    local path

    for path in \
        /usr/bin/nix \
        /usr/lib/libnixutil.so \
        /usr/lib/libnixstore.so \
        /usr/lib/libnixexpr.so \
        /usr/lib/libgc.so.1 \
        /usr/lib/libarchive.so.13 \
        /usr/lib/libsqlite3.so.0; do
        if [[ ! -e "$overlay_dir/${path#/}" && ! -L "$overlay_dir/${path#/}" ]]; then
            echo "error: Nix overlay is missing $path" >&2
            exit 1
        fi
    done

    echo "Nix package closure copied into overlay"
}

copy_package_files_to_overlay() {
    local package="$1"
    local listed_path

    while IFS= read -r listed_path; do
        case "$listed_path" in
            bin/*|etc/*|lib/*|sbin/*|usr/*|var/*|nix/*)
                copy_path_to_overlay "/$listed_path"
                ;;
        esac
    done < <(
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --cache-dir "$apk_cache" \
            info -L "$package"
    )
}

copy_path_to_overlay() {
    local guest_path="$1"
    local relative="${guest_path#/}"
    local source="$staging_root/$relative"
    local target="$overlay_dir/$relative"

    if [[ ! -e "$source" && ! -L "$source" ]]; then
        return
    fi

    if [[ -d "$source" && ! -L "$source" ]]; then
        mkdir -p "$target"
    elif [[ -L "$source" ]]; then
        mkdir -p "$(dirname "$target")"
        ln -sfn "$(readlink "$source")" "$target"
    else
        mkdir -p "$(dirname "$target")"
        cp -a "$source" "$target"
    fi
}

prepare_nixpkgs_tarball() {
    local rev="714a5f8c4ead6b31148d829288440ed033ccc041"
    local sha256="96009df77ed2339619ddc93fd99e7a2aeea13299bc5e0620314b6e475e015b36"
    local archive="$nix_cache/nixpkgs-$rev.tar.gz"
    local url="https://github.com/NixOS/nixpkgs/archive/$rev.tar.gz"

    mkdir -p "$nix_cache"
    if [[ ! -f "$archive" ]] || ! echo "$sha256  $archive" | sha256sum -c - >/dev/null 2>&1; then
        curl --fail --location --retry 3 --output "$archive.tmp" "$url"
        echo "$sha256  $archive.tmp" | sha256sum -c -
        mv "$archive.tmp" "$archive"
    fi

    rm -rf "$overlay_dir/opt/nixpkgs"
    mkdir -p "$overlay_dir/opt/nixpkgs"
    tar -xzf "$archive" --strip-components=1 -C "$overlay_dir/opt/nixpkgs"
    echo "nixpkgs $rev source prepared"
}

populate_overlay() {
    # Install test scripts.
    install -Dm0755 "$app_dir/nix-nosandbox.sh" "$overlay_dir/usr/bin/nix-nosandbox"
    install -Dm0755 "$app_dir/nix-nixpkgs.sh" "$overlay_dir/usr/bin/nix-nixpkgs"
    install -Dm0755 "$app_dir/nix.sh" "$overlay_dir/usr/bin/nix-sandbox"
    install -Dm0755 "$app_dir/test_nix.sh" "$overlay_dir/usr/bin/test_nix.sh"

    # The pinned source tree is injected by prepare_nixpkgs_tarball. Keeping
    # extraction host-side avoids Nix's metadata-heavy Git-cache import on the
    # guest filesystem while still exercising nixpkgs evaluation and builds.

    echo "overlay populated"
}

# Resize the rootfs ext4 image so it can hold the full nixpkgs stdenv closure
# (~393 MiB unpacked) plus the nixpkgs source tree (~322 MiB) and NAR download
# overhead. The default tgosimages rootfs is 3 GiB which overflows during
# stdenv substitution. 8 GiB leaves ~5 GiB free after the base system + overlay.
resize_rootfs() {
    local img="$base_rootfs"
    local target_mib=8192
    local current_size
    current_size=$(stat -c %s "$img" 2>/dev/null || echo 0)
    local target_bytes=$((target_mib * 1024 * 1024))
    if [[ "$current_size" -ge "$target_bytes" ]]; then
        echo "rootfs already >= ${target_mib} MiB ($current_size bytes), skip resize"
        return 0
    fi
    echo "resizing rootfs from $current_size to ${target_mib} MiB"
    truncate -s "${target_mib}M" "$img"
    e2fsck -fy "$img" >/dev/null 2>&1 || true
    resize2fs "$img" >/dev/null 2>&1
    echo "rootfs resized to ${target_mib} MiB"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
find_qemu_runner
extract_base_rootfs
install_nix_package
prepare_nix_conf
if [[ "${STARRY_NIX_SKIP_NIXPKGS:-0}" == "1" ]]; then
    echo "skipping nixpkgs source injection for sandbox diagnostics"
else
    resize_rootfs
    prepare_nixpkgs_tarball
fi
populate_overlay

echo "nix prebuild complete"
