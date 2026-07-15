#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
nix_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/nixpkgs-cache"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

# Map STARRY_ARCH to the correct qemu-user-static binary.
# Defaults to qemu-x86_64-static when STARRY_ARCH is unset or empty
# (the prebuild runs on the host, and x86_64 is the most common host).
qemu_user_static_binary() {
    case "${STARRY_ARCH:-x86_64}" in
        x86_64)      echo "qemu-x86_64-static" ;;
        aarch64)     echo "qemu-aarch64-static" ;;
        riscv64)     echo "qemu-riscv64-static" ;;
        loongarch64) echo "qemu-loongarch64-static" ;;
        *)
            echo "error: unsupported STARRY_ARCH '${STARRY_ARCH}' for nix prebuild" >&2
            exit 1
            ;;
    esac
}

ensure_host_packages() {
    local missing=()

    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v curl >/dev/null 2>&1 || missing+=(curl)
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
prepare_nix_conf
if [[ "${STARRY_NIX_SKIP_NIXPKGS:-0}" == "1" ]]; then
    echo "skipping nixpkgs source injection for sandbox diagnostics"
else
    resize_rootfs
    prepare_nixpkgs_tarball
fi
populate_overlay

echo "nix prebuild complete"
