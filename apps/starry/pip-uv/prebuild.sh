#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:-x86_64}"
base_rootfs="${STARRY_ROOTFS:-${STARRY_BASE_ROOTFS:-}}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
apk_cache="$workspace/target/pip-uv-apk-cache"

# Offline asset locations (downloaded ahead of time; no network at build time).
# Override via env if the assets live elsewhere.
#   PIPUV_DOWNLOAD_DIR : pip wheel + per-arch uv binaries (download/pip-uv/)
#   PIPUV_WHEELS_DIR   : offline build-backend wheels (setuptools/wheel/packaging/six/pip)
pipuv_download_dir="${PIPUV_DOWNLOAD_DIR:-$HOME/rcore/download/pip-uv}"
pipuv_wheels_dir="${PIPUV_WHEELS_DIR:-$HOME/rcore/pipuv-work/offline-wheels}"
pipuv_uvbins_dir="${PIPUV_UVBINS_DIR:-$HOME/rcore/pipuv-work/uvbins}"

require_env() {
    local name="$1"
    local value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

# First path matching a glob, or empty. Runs in the caller's `$(...)` subshell so
# `nullglob` does not leak, and returns 0 even on no-match — so a missing asset
# under `set -euo pipefail` does not abort the script before the explicit
# "error: missing ..." check below can print a clear, locatable message.
first_glob() {
    local pattern="$1" matches
    shopt -s nullglob
    # shellcheck disable=SC2206  # intentional word-split + glob of the pattern
    matches=($pattern)
    [[ ${#matches[@]} -gt 0 ]] && printf '%s\n' "${matches[0]}"
    return 0
}

ensure_host_packages() {
    local missing=()

    command -v debugfs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)
    command -v tar >/dev/null 2>&1 || missing+=(tar)

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

install_python_packages() {
    local qemu_runner
    case "$arch" in
        aarch64)     qemu_runner="qemu-aarch64-static" ;;
        riscv64)     qemu_runner="qemu-riscv64-static" ;;
        x86_64)      qemu_runner="qemu-x86_64-static" ;;
        loongarch64) qemu_runner="qemu-loongarch64-static" ;;
        *)           echo "error: unsupported arch: $arch" >&2; exit 1 ;;
    esac

    if ! command -v "$qemu_runner" >/dev/null 2>&1; then
        echo "error: $qemu_runner not found" >&2
        exit 1
    fi

    # Copy host DNS config so apk can resolve hostnames inside qemu-user.
    if [[ -f /etc/resolv.conf ]]; then
        cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    fi

    mkdir -p "$apk_cache"
    echo "[pip-uv prebuild] installing python3 via qemu-user apk..."
    QEMU_LD_PREFIX="$staging_root" \
    LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --update-cache \
            --no-progress \
            --no-scripts \
            add python3
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

# Resolve the offline uv binary for this arch and stage it into a temp file.
# x86_64/aarch64/riscv64: astral-sh static-musl release tarballs.
# loongarch64: Alpine edge community apk (astral-sh ships no loong binary).
extract_uv_binary() {
    local out="$1"
    rm -f "$out"

    case "$arch" in
        x86_64)  uv_triple="x86_64-unknown-linux-musl" ;;
        aarch64) uv_triple="aarch64-unknown-linux-musl" ;;
        riscv64) uv_triple="riscv64gc-unknown-linux-musl" ;;  # NOTE: riscv64 -> riscv64gc
        loongarch64) uv_triple="" ;;
        *) echo "error: unsupported arch for uv: $arch" >&2; exit 1 ;;
    esac

    # Fast path: a pre-extracted per-arch uv binary in pipuv-work/uvbins/.
    local prebuilt="$pipuv_uvbins_dir/uv-$arch"
    if [[ -s "$prebuilt" ]]; then
        cp "$prebuilt" "$out"
        chmod 0755 "$out"
        echo "[pip-uv prebuild] using prebuilt uv: $prebuilt"
        return
    fi

    if [[ "$arch" == "loongarch64" ]]; then
        # Alpine apk is a gzip tar; uv lives at usr/bin/uv.
        local apk
        apk="$(first_glob "$pipuv_download_dir/uv-loongarch64-*.apk")"
        [[ -n "$apk" ]] || { echo "error: missing loongarch64 uv apk in $pipuv_download_dir" >&2; exit 1; }
        local tmpd; tmpd="$(mktemp -d)"
        tar -xzf "$apk" -C "$tmpd" 2>/dev/null || true
        local found
        found="$(find "$tmpd" -name uv -type f 2>/dev/null | head -1)"
        [[ -n "$found" ]] || { echo "error: uv not found inside $apk" >&2; exit 1; }
        cp "$found" "$out"
        rm -rf "$tmpd"
    else
        local tgz="$pipuv_download_dir/uv-$uv_triple.tar.gz"
        [[ -f "$tgz" ]] || { echo "error: missing uv tarball: $tgz" >&2; exit 1; }
        local tmpd; tmpd="$(mktemp -d)"
        tar -xzf "$tgz" -C "$tmpd"
        local found
        found="$(find "$tmpd" -name uv -type f 2>/dev/null | head -1)"
        [[ -n "$found" ]] || { echo "error: uv not found inside $tgz" >&2; exit 1; }
        cp "$found" "$out"
        rm -rf "$tmpd"
    fi

    chmod 0755 "$out"
    [[ -s "$out" ]] || { echo "error: extracted uv binary is empty for $arch" >&2; exit 1; }
}

inject_offline_assets() {
    local wheels_dst="$overlay_dir/opt/wheels"
    mkdir -p "$wheels_dst"

    # 1) pip wheel (26.1.2) -> /opt/wheels and ensurepip bundled dir (for self-bootstrap)
    local pip_whl
    pip_whl="$(first_glob "$pipuv_download_dir/pip-*.whl")"
    [[ -n "$pip_whl" ]] || { echo "error: missing pip wheel in $pipuv_download_dir" >&2; exit 1; }
    install -Dm0644 "$pip_whl" "$wheels_dst/$(basename "$pip_whl")"

    # Replace the ensurepip bundled pip wheel so the guest self-bootstraps pip 26.1.2.
    local pyver pydir
    pydir="$(first_glob "$staging_root/usr/lib/python3*")"
    pyver=""
    [[ -n "$pydir" ]] && pyver="$(basename "$pydir")"
    if [[ -n "$pyver" && -d "$staging_root/usr/lib/$pyver/ensurepip/_bundled" ]]; then
        # The stdlib was already copied into the overlay below; drop the stale wheel and add ours.
        mkdir -p "$overlay_dir/usr/lib/$pyver/ensurepip/_bundled"
        rm -f "$overlay_dir/usr/lib/$pyver/ensurepip/_bundled"/pip-*.whl
        install -Dm0644 "$pip_whl" "$overlay_dir/usr/lib/$pyver/ensurepip/_bundled/$(basename "$pip_whl")"
    fi

    # 2) build-backend wheels (setuptools/wheel/packaging/six, + a redundant pip) -> /opt/wheels.
    # Guest stages 5 and 15 install `setuptools wheel` offline from /opt/wheels, so validate the
    # required wheels here and fail BEFORE entering the guest with the missing item — otherwise a
    # missing/empty/incomplete PIPUV_WHEELS_DIR would only surface as a late in-QEMU failure,
    # breaking the app workflow's reproducibility.
    if [[ ! -d "$pipuv_wheels_dir" ]]; then
        echo "error: offline build-backend wheels dir not found: $pipuv_wheels_dir" >&2
        echo "       set PIPUV_WHEELS_DIR (must contain at least setuptools-*.whl and wheel-*.whl)" >&2
        exit 1
    fi
    local w
    for w in "$pipuv_wheels_dir"/*.whl; do
        [[ -e "$w" ]] || continue
        install -Dm0644 "$w" "$wheels_dst/$(basename "$w")"
    done
    local required
    for required in setuptools wheel; do
        if [[ -z "$(first_glob "$wheels_dst/${required}-*.whl")" ]]; then
            echo "error: required build-backend wheel '${required}-*.whl' missing from \
$pipuv_wheels_dir" >&2
            echo "       (the guest installs '${required}' offline from /opt/wheels at stages 5/15)" >&2
            exit 1
        fi
    done

    # 3) per-arch uv binary -> /usr/local/bin/uv
    local uv_tmp="$workspace/target/uv-$arch.bin"
    extract_uv_binary "$uv_tmp"
    install -Dm0755 "$uv_tmp" "$overlay_dir/usr/local/bin/uv"
    rm -f "$uv_tmp"
}

populate_overlay() {
    copy_file_to_overlay /usr/bin/python3 0755
    copy_runtime_dependencies /usr/bin/python3

    # Copy Python standard library (versioned dir, e.g. python3.14)
    local pyver pydir
    pydir="$(first_glob "$staging_root/usr/lib/python3*")"
    pyver=""
    [[ -n "$pydir" ]] && pyver="$(basename "$pydir")"
    if [[ -n "$pyver" && -d "$staging_root/usr/lib/$pyver" ]]; then
        mkdir -p "$overlay_dir/usr/lib/$pyver"
        cp -a "$staging_root/usr/lib/$pyver/." "$overlay_dir/usr/lib/$pyver/"
    fi

    # Copy any unversioned python3 site dir if present.
    if [[ -d "$staging_root/usr/lib/python3" ]]; then
        mkdir -p "$overlay_dir/usr/lib/python3"
        cp -a "$staging_root/usr/lib/python3/." "$overlay_dir/usr/lib/python3/"
    fi

    inject_offline_assets

    install -Dm0755 "$app_dir/test_pipuv.sh" "$overlay_dir/usr/bin/test_pipuv.sh"
}

require_env STARRY_ROOTFS "$base_rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR "$overlay_dir"

ensure_host_packages
extract_base_rootfs
install_python_packages
populate_overlay
