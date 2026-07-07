#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
nix_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/nix-binary-cache"
nix_version="2.34.0"

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

install_nix_package() {
    local system sha256 nix_store_path cacert_store_path
    case "${STARRY_ARCH:-}" in
        x86_64)
            system="x86_64-linux"
            sha256="5676b0887f1274e62edd175b6611af49aa8170c69c16877aa9bc6cebceb19855"
            nix_store_path="1kxmbqsah0bszi95w1ii633vzciqnanx-nix-${nix_version}"
            cacert_store_path="cv910ahrajv1p2lvy6phasy2b1d9nxcd-nss-cacert-3.117"
            ;;
        aarch64)
            system="aarch64-linux"
            sha256="cfddd4008b57a71464a16d5232cba79b1c76ae9dc81bbf71b4972b0118bc29c5"
            nix_store_path="xs9inr2d17npz3y05y4kqvk5qikq5yp6-nix-${nix_version}"
            cacert_store_path="2g4zfwsrkydpisqk3lz42cf9ak2lfvnc-nss-cacert-3.117"
            ;;
        *)
            echo "error: unsupported STARRY_ARCH='${STARRY_ARCH:-}'" >&2
            exit 1
            ;;
    esac

    local archive="$nix_cache/nix-${nix_version}-${system}.tar.xz"
    local extracted="$nix_cache/nix-${nix_version}-${system}"
    local url="https://releases.nixos.org/nix/nix-${nix_version}/nix-${nix_version}-${system}.tar.xz"

    mkdir -p "$nix_cache"
    if [[ ! -f "$archive" ]] || ! echo "$sha256  $archive" | sha256sum -c - >/dev/null 2>&1; then
        curl --fail --location --retry 3 --output "$archive.tmp" "$url"
        echo "$sha256  $archive.tmp" | sha256sum -c -
        mv "$archive.tmp" "$archive"
    fi

    rm -rf "$extracted"
    mkdir -p "$extracted"
    tar -xJf "$archive" --strip-components=1 -C "$extracted"

    mkdir -p "$overlay_dir/nix/store" "$overlay_dir/nix/var/nix" "$overlay_dir/usr/bin"
    cp -a "$extracted/store/." "$overlay_dir/nix/store/"
    install -Dm0644 "$extracted/.reginfo" "$overlay_dir/nix/.reginfo"
    ln -sfn "/nix/store/$nix_store_path/bin/nix" "$overlay_dir/usr/bin/nix"

    mkdir -p "$overlay_dir/etc/nix"
    cat > "$overlay_dir/etc/nix/nix.conf" <<NIXCONF
sandbox = false
build-users-group =
substituters = https://mirrors.cernet.edu.cn/nix-channels/store https://cache.nixos.org
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
ssl-cert-file = /nix/store/$cacert_store_path/etc/ssl/certs/ca-bundle.crt
NIXCONF

    echo "Nix ${nix_version} official closure prepared for ${STARRY_ARCH}"
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
    # NOTE: nix.sh (sandbox test) is intentionally NOT injected — sandbox
    # requires mount namespace isolation not yet available in StarryOS.
    # The nix binary must be kept as /usr/bin/nix (already copied above).
    install -Dm0755 "$app_dir/nix-nosandbox.sh" "$overlay_dir/usr/bin/nix-nosandbox"
    install -Dm0755 "$app_dir/nix-nixpkgs.sh" "$overlay_dir/usr/bin/nix-nixpkgs"
    install -Dm0755 "$app_dir/test_nix.sh" "$overlay_dir/usr/bin/test_nix.sh"

    # NIXPKGS diagnostic: inject builder-init regression test binary
    echo "NIXPKGS_DIAG: app_dir=$app_dir test_bin=$app_dir/test-nix-builder-init exists=$([[ -f "$app_dir/test-nix-builder-init" ]] && echo yes || echo no) exec=$([[ -x "$app_dir/test-nix-builder-init" ]] && echo yes || echo no)"
    if [[ -x "$app_dir/test-nix-builder-init" ]]; then
        mkdir -p "$overlay_dir/usr/bin/starry-test-suit"
        install -Dm0755 "$app_dir/test-nix-builder-init" \
            "$overlay_dir/usr/bin/starry-test-suit/test-nix-builder-init"
        echo "NIXPKGS_DIAG: test binary injected"
    else
        echo "NIXPKGS_DIAG: test binary not found or not executable, skipping"
    fi

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
resize_rootfs
install_nix_package
prepare_nixpkgs_tarball
populate_overlay

echo "nix prebuild complete"
