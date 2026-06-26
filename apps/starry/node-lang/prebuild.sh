#!/usr/bin/env bash
# prebuild.sh — provision a Node.js 22 (V8 + core API runtime) environment into
# the app rootfs and stage the language carpet suite.
#
# Portable model (mirrors the merged python-lang app): extract the base Alpine
# rootfs to a staging tree, `apk add nodejs icu-data-full` INTO it via
# qemu-user-static (so it works for every target arch on an x86 build host), then
# copy the `node` binary, its runtime shared-library closure, and the ICU data
# table into the app overlay, plus the carpet sources under /usr/bin. No
# host-absolute paths, no prebuilt images — the only inputs are the registered
# base rootfs and the app's own node/ sources.
#
# Node v22 (LTS) lives in Alpine v3.22 main (musl-native, no gcompat). The base
# Alpine image ships v3.22, but to pin the exact tested closure the prebuild
# points apk at v3.22 main+community. The `icu-data-full` package is required for
# `Intl.*` / `toLocaleString` (the stub libicudata in icu-libs is data-less).
#
# If the build host has no network, the prebuild falls back to a documented,
# pre-fetched apk cache at $NODE_APK_CACHE/<arch>/ (default
# <repo>/download/nodejs-apks/<arch>), installing the same closure offline.
#
# Env from the app runner: STARRY_ARCH, STARRY_ROOTFS (base alpine working copy),
# STARRY_STAGING_ROOT (scratch extraction tree), STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

# Optional offline apk cache (pre-fetched v3.22/main closure) — only used if the
# network apk path below fails. Default is the workspace download dir; overridable
# via NODE_APK_CACHE (explicit dir) or NODE_DL_ROOT (its parent root).
default_cache="$(cd "$app_dir/../../.." 2>/dev/null && pwd)/download/nodejs-apks"
apk_cache="${NODE_APK_CACHE:-${NODE_DL_ROOT:+$NODE_DL_ROOT/nodejs-apks}}"
apk_cache="${apk_cache:-$default_cache}"

case "$arch" in
    aarch64)     qemu_runner="qemu-aarch64-static" ;;
    riscv64)     qemu_runner="qemu-riscv64-static" ;;
    x86_64)      qemu_runner="qemu-x86_64-static" ;;
    loongarch64) qemu_runner="qemu-loongarch64-static" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v debugfs    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v readelf    >/dev/null 2>&1 || missing+=(binutils)
    command -v "$qemu_runner" >/dev/null 2>&1 || missing+=(qemu-user-static)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            echo "prebuild: installing host tools: ${missing[*]}"
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2
            exit 1
        fi
    fi
}

extract_base_rootfs() {
    rm -rf "$staging_root"; mkdir -p "$staging_root"
    debugfs -R "rdump / $staging_root" "$base_rootfs" >/dev/null 2>&1
    [[ -x "$staging_root/sbin/apk" ]] || { echo "prebuild: base rootfs has no apk" >&2; exit 2; }
}

# Install nodejs + icu-data-full + their runtime closure into the staging tree.
# Try the network first (apk add from v3.22 main+community); on failure, fall back
# to the pre-fetched offline apk cache by unpacking each .apk over the tree.
install_node() {
    [[ -f /etc/resolv.conf ]] && cp -f /etc/resolv.conf "$staging_root/etc/resolv.conf" || true
    local repo="https://dl-cdn.alpinelinux.org/alpine"
    printf '%s/v3.22/main\n%s/v3.22/community\n' "$repo" "$repo" > "$staging_root/etc/apk/repositories"
    echo "prebuild: apk add nodejs icu-data-full (Node 22 LTS) from Alpine v3.22 via $qemu_runner..."
    if QEMU_LD_PREFIX="$staging_root" \
       LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" \
            "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --update-cache --no-progress --no-scripts \
            add nodejs icu-data-full
    then
        echo "prebuild: provisioned nodejs via network apk"
    else
        echo "prebuild: network apk failed; falling back to offline apk cache $apk_cache/$arch"
        install_node_offline
    fi
}

# Offline path: unpack the pre-fetched v3.22/main closure apks over the staging
# tree (each apk is a gzip-tar whose payload expands under /usr,/lib). Excludes
# apk metadata members. Mirrors the documented prep-nodejs-rootfs injection.
install_node_offline() {
    local dir="$apk_cache/$arch"
    [[ -d "$dir" ]] || { echo "prebuild: offline apk cache missing: $dir" >&2; exit 3; }
    local apks=(
        musl libgcc "libstdc++" zlib zstd-libs brotli-libs c-ares nghttp2-libs
        ada-libs simdjson simdutf sqlite-libs libcrypto3 libssl3
        icu-libs icu-data-full nodejs
    )
    local pkg f
    for pkg in "${apks[@]}"; do
        f="$(ls "$dir/${pkg}"-[0-9]*.apk 2>/dev/null | head -1 || true)"
        [[ -n "$f" ]] || { echo "prebuild: offline apk missing for '$pkg' in $dir" >&2; exit 3; }
        tar -xzf "$f" -C "$staging_root" \
            --exclude='.PKGINFO' --exclude='.SIGN.*' --exclude='.pre-install' \
            --exclude='.post-install' --exclude='.trigger' 2>/dev/null || true
    done
    echo "prebuild: provisioned nodejs offline from ${#apks[@]} apks"
}

verify_node() {
    [[ -x "$staging_root/usr/bin/node" ]] || { echo "prebuild: no /usr/bin/node after install" >&2; exit 4; }
    # Hard version gate: the carpet's v22-gated checks must run on real Node 22.
    local nodever
    nodever="$(QEMU_LD_PREFIX="$staging_root" "$qemu_runner" -L "$staging_root" \
        "$staging_root/usr/bin/node" -p 'process.versions.node' 2>/dev/null || true)"
    case "$nodever" in
        22.*|2[3-9].*|[3-9][0-9].*) echo "prebuild: provisioned node v$nodever" ;;
        *) echo "prebuild: need Node >=22 but got '$nodever'" >&2; exit 5 ;;
    esac
}

copy_to_overlay() {  # guest-path mode
    local src="$staging_root$1" dst="$overlay_dir$1"
    [[ -e "$src" ]] || { echo "prebuild: missing $1 after install" >&2; exit 6; }
    [[ -L "$src" ]] && src="$(readlink -f "$src")"
    install -Dm"$2" "$src" "$dst"
}

# recursively copy the shared-library closure of an ELF into the overlay
copy_so_closure() {
    local pending=("$@") seen=" " gp lib d
    while [[ ${#pending[@]} -gt 0 ]]; do
        gp="${pending[0]}"; pending=("${pending[@]:1}")
        [[ "$seen" == *" $gp "* ]] && continue
        seen+="$gp "
        while IFS= read -r lib; do
            for d in lib usr/lib usr/local/lib; do
                if [[ -e "$staging_root/$d/$lib" ]]; then
                    copy_to_overlay "/$d/$lib" 0644
                    pending+=("/$d/$lib")
                    break
                fi
            done
        done < <(readelf -d "$staging_root$gp" 2>/dev/null | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p')
    done
}

populate_overlay() {
    copy_to_overlay /usr/bin/node 0755
    copy_so_closure /usr/bin/node

    # Stage the ELF program interpreter (musl dynamic loader) under its REAL name.
    # DT_NEEDED only names libc.musl-<arch>.so.1 (a symlink to ld-musl-<arch>.so.1),
    # so copy_so_closure alone leaves the interpreter path the node ELF requests
    # (/lib/ld-musl-<arch>.so.1) missing. Copy the resolved loader to both names.
    local interp
    interp="$(readelf -l "$staging_root/usr/bin/node" 2>/dev/null \
        | sed -n 's/.*program interpreter: \(.*\)\]/\1/p' | tr -d ' ')"
    if [[ -n "$interp" && -e "$staging_root$interp" ]]; then
        local real="$interp"
        [[ -L "$staging_root$interp" ]] && real="$(readlink -f "$staging_root$interp")" \
            && real="${real#$staging_root}"
        install -Dm0755 "$staging_root$real" "$overlay_dir$interp"
    fi

    # ICU data table (icu-data-full): node was compiled with a hard-coded archive
    # path under /usr/share/icu/<ver>/icudt*.dat — copy the whole tree so Intl.*
    # and toLocaleString work (the stub in icu-libs is data-less).
    if [[ -d "$staging_root/usr/share/icu" ]]; then
        mkdir -p "$overlay_dir/usr/share/icu"
        cp -a "$staging_root/usr/share/icu/." "$overlay_dir/usr/share/icu/"
    fi
    # musl loader search path (so the .so closure resolves at runtime).
    if [[ -f "$staging_root/etc/ld-musl-${arch}.path" ]]; then
        install -Dm0644 "$staging_root/etc/ld-musl-${arch}.path" \
            "$overlay_dir/etc/ld-musl-${arch}.path"
    else
        mkdir -p "$overlay_dir/etc"
        printf '/lib\n/usr/lib\n/usr/local/lib\n' > "$overlay_dir/etc/ld-musl-${arch}.path"
    fi

    # stage the carpet suite + the on-target runner under /usr/bin
    install -Dm0644 "$app_dir/node/node-carpet.js"     "$overlay_dir/usr/bin/node-carpet.js"
    install -Dm0644 "$app_dir/node/node-cli-carpet.sh" "$overlay_dir/usr/bin/node-cli-carpet.sh"
    install -Dm0755 "$app_dir/node/run_node_carpet.sh" "$overlay_dir/usr/bin/run_node_carpet.sh"

    echo "prebuild: staged node + .so closure + ICU data + 2 carpets; overlay ready for $arch"
}

ensure_host_tools
extract_base_rootfs
install_node
verify_node
populate_overlay
