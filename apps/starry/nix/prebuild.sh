#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/nix-apk-cache"

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
    command -v qemu-x86_64-static >/dev/null 2>&1 || missing+=(qemu-user-static)

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

extract_base_rootfs() {
    debugfs -R "rdump / $staging_root" "$base_rootfs"
}

install_nix_package() {
    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    mkdir -p "$apk_cache"
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        qemu-x86_64-static -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --update-cache \
            --no-progress \
            --no-scripts \
            add nix

    mkdir -p "$staging_root/etc/nix"
    cat > "$staging_root/etc/nix/nix.conf" <<'NIXCONF'
sandbox = false
build-users-group =
# sandbox = false is required because StarryOS does not yet support
# unshare(CLONE_NEWNS).  Without it, builtins.fetchTarball fails in its
# download thread with "unsharing filesystem state: Invalid argument".
# Flip to sandbox = true once mount namespace isolation is available.
NIXCONF

    echo "Nix installed from Alpine apk"
}

# The official Nix tarball path is intentionally left disabled.
#
# It installs Nix as a /nix/store closure with many symlinks.  The current
# debugfs overlay injector accepts only regular files and directories, so
# preserving symlinks fails injection.  Dereferencing the full closure is not
# reliable either because the official closure currently contains at least one
# broken symlink (for example libgcc_s.so in the GCC lib output), causing a
# plain `cp -aL` or tar dereference copy to fail or skip files unpredictably.
# Use Alpine's `apk add nix` path above until the overlay injector supports
# symlinks or the tarball closure is copied through a Nix-aware path.
#
# install_nix_from_official_tarball() {
#     local nix_ver="2.31.5"
#     local nix_url="https://releases.nixos.org/nix/nix-${nix_ver}/nix-${nix_ver}-x86_64-linux.tar.xz"
#     ...
# }

copy_file_to_overlay() {
    local guest_path="$1"
    local mode="$2"
    local source="$staging_root${guest_path}"
    local target="$overlay_dir${guest_path}"

    if [[ ! -e "$source" ]]; then
        echo "error: missing guest file after Nix package install: $guest_path" >&2
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

        if [[ -x "$staging_root$guest_path" ]]; then
            copy_file_to_overlay "$guest_path" 0755
        else
            copy_file_to_overlay "$guest_path" 0644
        fi

        while IFS= read -r library; do
            local library_path
            if ! library_path="$(find_library_path "$library")"; then
                continue
            fi
            pending+=("$library_path")
        done < <(
            readelf -d "$staging_root$guest_path" 2>/dev/null |
                sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p'
        )
    done
}

prepare_nixpkgs_tarball() {
    local nixpkgs_rev="06278c77b5d162e62df170fec307e83f1812d94b"
    local tarball_url="https://github.com/NixOS/nixpkgs/archive/${nixpkgs_rev}.tar.gz"
    local tarball_dest="$overlay_dir/nixpkgs.tar.gz"

    if [[ -f "$tarball_dest" ]]; then
        echo "nixpkgs tarball already cached at $tarball_dest"
        return 0
    fi

    echo "downloading nixpkgs tarball from $tarball_url"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 5 --retry-all-errors --connect-timeout 30 -o "$tarball_dest" "$tarball_url" || {
            echo "curl download failed, retrying..."
            curl -fL --retry 5 --retry-all-errors --connect-timeout 30 -o "$tarball_dest" "$tarball_url"
        }
    elif command -v wget >/dev/null 2>&1; then
        wget --tries=5 --timeout=30 -O "$tarball_dest" "$tarball_url"
    else
        echo "error: no download tool available (curl or wget required)" >&2
        exit 1
    fi

    echo "nixpkgs tarball downloaded ($(du -h "$tarball_dest" | cut -f1))"
}

populate_overlay() {
    copy_runtime_dependencies /usr/bin/nix

    install -Dm0644 "$staging_root/etc/nix/nix.conf" "$overlay_dir/etc/nix/nix.conf"

    mkdir -p "$overlay_dir/nix/store"

    # Install test scripts.
    # NOTE: nix.sh (sandbox test) is intentionally NOT injected — sandbox
    # requires mount namespace isolation not yet available in StarryOS.
    # The nix binary must be kept as /usr/bin/nix (already copied above).
    install -Dm0755 "$app_dir/nix-nosandbox.sh" "$overlay_dir/usr/bin/nix-nosandbox"
    install -Dm0755 "$app_dir/nix-nixpkgs.sh" "$overlay_dir/usr/bin/nix-nixpkgs"
    install -Dm0755 "$app_dir/test_nix.sh" "$overlay_dir/usr/bin/test_nix.sh"

    # Inject nixpkgs tarball for the nixpkgs test.
    # The guest cannot use builtins.fetchTarball because Nix's download
    # subsystem requires unshare(CLONE_NEWNS) which StarryOS does not
    # support.  Instead we download on the host during prebuild and inject
    # the tarball; the guest extracts it and imports locally.
    prepare_nixpkgs_tarball

    echo "overlay populated"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
install_nix_package
populate_overlay

echo "nix prebuild complete"
