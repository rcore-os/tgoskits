#!/usr/bin/env bash
# prebuild.sh - provision the StarryOS OpenWrt-userland carpet (uci + opkg).
#
# uci (Unified Configuration Interface) and opkg (the .ipk package manager) are the two
# signature OpenWrt userland tools. Neither is packaged by Alpine, so - unlike the apk-based
# apps - this app CROSS-COMPILES them from the OpenWrt upstream C source IN-PREBUILD, exactly
# the way the language apps cross-build their native bits: the per-arch musl cross-toolchain is
# supplied out-of-band (StarryOS .starry-env.sh PATH), the sources are pinned to immutable
# commits (reproducible - no drifting HEAD, no committed binaries), and only the resulting
# static-musl binaries are staged into the overlay. QEMU then needs no guest network.
#
# Dependency chain (all small cmake C, static musl):
#   json-c ->  libubox ->  uci   (the `cli` target, OUTPUT_NAME uci; BUILD_STATIC)
#              libubox ->  opkg  (opkg-cl; STATIC_UBOX - opkg ships its own libbb .ipk
#                                 extractor, so NO libarchive, and downloads via wget shell-out
#                                 so NO libcurl - libubox + pthread is the whole dep set)
#
# Staged:
#   uci      -> /usr/bin/uci
#   opkg     -> /usr/bin/opkg        (built as opkg-cl, installed under the canonical name)
#   the two carpets + the gate -> /usr/bin/{uci-carpet.sh,opkg-carpet.sh,run-openwrt.sh}
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:-}"

CACHE="${OPENWRT_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/openwrt-build}"
ROOTFS_SIZE="${OPENWRT_ROOTFS_SIZE:-1024M}"

# Immutable source pins (reproducible; verified to cross-build all four arches).
JSONC_URL="https://github.com/json-c/json-c";        JSONC_SHA="324e5ca5937c459812973149f2c31ae25b6439bb"
LIBUBOX_URL="https://git.openwrt.org/project/libubox.git"; LIBUBOX_SHA="17f527fb6c30bf9073104f03337c2b7c03158bdb"
UCI_URL="https://git.openwrt.org/project/uci.git";   UCI_SHA="74f6277aabffc943d026f406df57c22595134c42"
OPKG_URL="https://git.openwrt.org/project/opkg-lede.git"; OPKG_SHA="80503d94e356476250adaf1f669ee955ec26de76"

# Resolve the musl cross-compiler for the target arch. The toolchains are provided out-of-band
# (not apt): x86_64/aarch64/riscv64 land on PATH; the loongarch64 musl cross lives under
# /opt (add it to PATH if present) - matching how the language apps locate it.
resolve_cc() {
    case "$arch" in
        x86_64|aarch64|riscv64) CC="${arch}-linux-musl-gcc" ;;
        loongarch64)
            CC="loongarch64-linux-musl-gcc"
            # only fall back to the /opt toolchain when it is NOT already on PATH
            if ! command -v "$CC" >/dev/null 2>&1 && [[ -x /opt/loongarch64-linux-musl-cross/bin/$CC ]]; then
                export PATH="/opt/loongarch64-linux-musl-cross/bin:$PATH"
            fi
            ;;
        *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
    esac
    if ! command -v "$CC" >/dev/null 2>&1; then
        echo "prebuild: musl cross-compiler '$CC' not found on PATH (provide it via .starry-env.sh)" >&2
        exit 2
    fi
    echo "prebuild: arch=$arch CC=$(command -v "$CC")"
}

ensure_host_tools() {
    local missing=()
    command -v git   >/dev/null 2>&1 || missing+=(git)
    command -v cmake >/dev/null 2>&1 || missing+=(cmake)
    command -v e2fsck>/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

# shallow-fetch a pinned commit into $CACHE/src/<name> (idempotent).
fetch_pinned() {
    local name="$1" url="$2" sha="$3"
    local dst="$CACHE/src/$name"
    if [[ -f "$dst/.pinned-$sha" ]]; then return 0; fi
    rm -rf "$dst"; mkdir -p "$dst"
    ( cd "$dst" && git init -q && git remote add origin "$url" \
        && ( git fetch -q --depth 1 origin "$sha" || git fetch -q origin ) \
        && git checkout -q "$sha" )
    touch "$dst/.pinned-$sha"
    echo "prebuild: fetched $name @ ${sha:0:12}"
}

build_all() {
    local pfx="$CACHE/prefix-$arch" bdir="$CACHE/build-$arch"
    # UCI_BIN / OPKG_BIN are global so stage() sees them on BOTH the build and cache-reuse paths.
    UCI_BIN="$bdir/uci/uci"; OPKG_BIN="$bdir/opkg/src/opkg-cl"
    if [[ -x "$UCI_BIN" && -x "$OPKG_BIN" ]]; then
        echo "prebuild: reusing cached $arch binaries"; return 0
    fi
    mkdir -p "$pfx" "$bdir"
    local s="$CACHE/src" CF="-I$pfx/include"

    echo "prebuild: building json-c ($arch)"
    cmake -S "$s/json-c" -B "$bdir/json-c" -DCMAKE_C_COMPILER="$CC" -DCMAKE_INSTALL_PREFIX="$pfx" \
        -DBUILD_SHARED_LIBS=OFF -DBUILD_STATIC_LIBS=ON -DDISABLE_WERROR=ON -DBUILD_TESTING=OFF >/dev/null
    cmake --build "$bdir/json-c" -j"$(nproc)" --target install >/dev/null

    echo "prebuild: building libubox ($arch)"
    cmake -S "$s/libubox" -B "$bdir/libubox" -DCMAKE_C_COMPILER="$CC" -DCMAKE_INSTALL_PREFIX="$pfx" \
        -DBUILD_LUA=OFF -DBUILD_EXAMPLES=OFF -DBUILD_STATIC=ON -DUNIT_TESTING=OFF \
        -DCMAKE_C_FLAGS="$CF" -DCMAKE_PREFIX_PATH="$pfx" >/dev/null
    cmake --build "$bdir/libubox" -j"$(nproc)" --target install >/dev/null

    echo "prebuild: building uci ($arch)"
    cmake -S "$s/uci" -B "$bdir/uci" -DCMAKE_C_COMPILER="$CC" -DCMAKE_INSTALL_PREFIX="$pfx" \
        -DBUILD_LUA=OFF -DBUILD_STATIC=ON -DCMAKE_C_FLAGS="$CF" \
        -DCMAKE_EXE_LINKER_FLAGS="-static -L$pfx/lib" -DCMAKE_PREFIX_PATH="$pfx" >/dev/null
    cmake --build "$bdir/uci" -j"$(nproc)" --target cli >/dev/null
    [[ -x "$UCI_BIN" ]] || { UCI_BIN="$(find "$bdir/uci" -type f -executable -name uci | head -1)"; }

    echo "prebuild: building opkg ($arch)"
    cmake -S "$s/opkg" -B "$bdir/opkg" -DCMAKE_C_COMPILER="$CC" -DCMAKE_INSTALL_PREFIX="$pfx" \
        -DSTATIC_UBOX=ON -DBUILD_TESTS=OFF -DENABLE_USIGN=OFF \
        -DCMAKE_C_FLAGS="$CF -Wno-error" -DCMAKE_EXE_LINKER_FLAGS="-static -L$pfx/lib" \
        -DCMAKE_PREFIX_PATH="$pfx" >/dev/null
    cmake --build "$bdir/opkg" -j"$(nproc)" >/dev/null
    [[ -x "$OPKG_BIN" ]] || { OPKG_BIN="$(find "$bdir/opkg" -type f -executable -name 'opkg*' | head -1)"; }

    [[ -x "$UCI_BIN"  ]] || { echo "prebuild: uci build produced no binary"  >&2; exit 3; }
    [[ -x "$OPKG_BIN" ]] || { echo "prebuild: opkg build produced no binary" >&2; exit 3; }
}

stage() {
    [[ -x "$UCI_BIN" && -x "$OPKG_BIN" ]] || { echo "prebuild: uci/opkg binaries missing at stage" >&2; exit 3; }
    mkdir -p "$overlay_dir/usr/bin"
    install -Dm0755 "$UCI_BIN"  "$overlay_dir/usr/bin/uci"
    install -Dm0755 "$OPKG_BIN" "$overlay_dir/usr/bin/opkg"
    install -Dm0755 "$app_dir/programs/uci-carpet.sh"  "$overlay_dir/usr/bin/uci-carpet.sh"
    install -Dm0755 "$app_dir/programs/opkg-carpet.sh" "$overlay_dir/usr/bin/opkg-carpet.sh"
    install -Dm0755 "$app_dir/programs/run-openwrt.sh" "$overlay_dir/usr/bin/run-openwrt.sh"
    echo "prebuild: staged uci + opkg + carpets -> overlay/usr/bin"
}

# Grow-only, idempotent (never shrink an already-grown image - truncate '>' only extends).
grow_rootfs() {
    [[ -n "$rootfs_img" && -f "$rootfs_img" ]] || { echo "prebuild: rootfs not staged, skipping grow"; return 0; }
    local cur target
    cur=$(stat -c %s "$rootfs_img"); target=$(( ${ROOTFS_SIZE%M} * 1024 * 1024 ))
    if [[ "$cur" -lt "$target" ]]; then
        truncate -s ">$ROOTFS_SIZE" "$rootfs_img"
        e2fsck -f -y "$rootfs_img" >/dev/null 2>&1 || true
        resize2fs "$rootfs_img" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed" >&2; exit 2; }
    fi
    echo "prebuild: rootfs sized to $(( $(stat -c %s "$rootfs_img")/1024/1024 )) MiB"
}

main() {
    ensure_host_tools
    resolve_cc
    grow_rootfs
    fetch_pinned json-c  "$JSONC_URL"   "$JSONC_SHA"
    fetch_pinned libubox "$LIBUBOX_URL" "$LIBUBOX_SHA"
    fetch_pinned uci     "$UCI_URL"     "$UCI_SHA"
    fetch_pinned opkg    "$OPKG_URL"    "$OPKG_SHA"
    build_all
    stage
    echo "prebuild: openwrt (uci + opkg) overlay ready for $arch"
}

main "$@"
