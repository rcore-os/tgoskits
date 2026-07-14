#!/usr/bin/env bash
# prebuild.sh - assemble the StarryWRT distribution rootfs overlay.
#
# StarryWRT is an OpenWrt-userland distribution on the StarryOS kernel: apart from the kernel
# (StarryOS, single-core) it is meant to be indistinguishable from OpenWrt. This script builds
# that userland into the overlay:
#
#   uci, opkg         - CROSS-COMPILED from OpenWrt upstream source (pinned commits, static
#                       musl; the per-arch musl cross-toolchain is provided out-of-band). The
#                       config + package core.
#   dropbear suite    - the SSH stack, provisioned from the live Alpine APKINDEX that matches
#   dnsmasq           - the DNS/DHCP stack   the rootfs (current version, no drifting URL).
#   /etc layout       - the OpenWrt filesystem identity: banner, openwrt_release / os-release,
#                       /etc/config/{system,network,dhcp,firewall,dropbear}, the init framework
#                       (/etc/rc.common + /etc/init.d/*). Staged from files/ with the arch
#                       placeholder substituted.
#   carpets + gate    - uci-carpet.sh, opkg-carpet.sh, starrywrt-carpet.sh, run-starrywrt.sh.
#
# No committed binaries: uci/opkg are built from source, the service binaries come from apk.
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:-}"

CACHE="${STARRYWRT_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/starrywrt}"
ROOTFS_SIZE="${STARRYWRT_ROOTFS_SIZE:-1280M}"

case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) ALPINE_ARCH="$arch" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# ---- source pins for the cross-built config/package core (verified on all four arches) -------
JSONC_URL="https://github.com/json-c/json-c";             JSONC_SHA="324e5ca5937c459812973149f2c31ae25b6439bb"
LIBUBOX_URL="https://git.openwrt.org/project/libubox.git"; LIBUBOX_SHA="17f527fb6c30bf9073104f03337c2b7c03158bdb"
UCI_URL="https://git.openwrt.org/project/uci.git";         UCI_SHA="74f6277aabffc943d026f406df57c22595134c42"
OPKG_URL="https://git.openwrt.org/project/opkg-lede.git";  OPKG_SHA="80503d94e356476250adaf1f669ee955ec26de76"

# ---- apk-provisioned service stacks ----------------------------------------------------------
APK_PKGS="dropbear dropbear-dbclient dropbear-convert utmps-libs skalibs-libs dnsmasq dnsmasq-common"

ensure_host_tools() {
    local missing=()
    for t in git cmake curl tar e2fsck resize2fs debugfs; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends git cmake curl tar e2fsprogs
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

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
    esac
    command -v "$CC" >/dev/null 2>&1 || { echo "prebuild: musl cross-compiler '$CC' not found (provide via .starry-env.sh)" >&2; exit 2; }
    echo "prebuild: arch=$arch CC=$(command -v "$CC")"
}

# ===================== A. cross-build uci + opkg from pinned source ============================
fetch_pinned() {
    local name="$1" url="$2" sha="$3"
    local dst="$CACHE/src/$name"
    [[ -f "$dst/.pinned-$sha" ]] && return 0
    rm -rf "$dst"; mkdir -p "$dst"
    ( cd "$dst" && git init -q && git remote add origin "$url" \
        && ( git fetch -q --depth 1 origin "$sha" || git fetch -q origin ) \
        && git checkout -q "$sha" )
    touch "$dst/.pinned-$sha"; echo "prebuild: fetched $name @ ${sha:0:12}"
}

build_tools() {
    local pfx="$CACHE/prefix-$arch" bdir="$CACHE/build-$arch"
    UCI_BIN="$bdir/uci/uci"; OPKG_BIN="$bdir/opkg/src/opkg-cl"
    if [[ -x "$UCI_BIN" && -x "$OPKG_BIN" ]]; then echo "prebuild: reusing cached $arch uci/opkg"; return 0; fi
    mkdir -p "$pfx" "$bdir"; local s="$CACHE/src" CF="-I$pfx/include"
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
    [[ -x "$UCI_BIN" ]] || UCI_BIN="$(find "$bdir/uci" -type f -executable -name uci | head -1)"
    echo "prebuild: building opkg ($arch)"
    cmake -S "$s/opkg" -B "$bdir/opkg" -DCMAKE_C_COMPILER="$CC" -DCMAKE_INSTALL_PREFIX="$pfx" \
        -DSTATIC_UBOX=ON -DBUILD_TESTS=OFF -DENABLE_USIGN=OFF \
        -DCMAKE_C_FLAGS="$CF -Wno-error" -DCMAKE_EXE_LINKER_FLAGS="-static -L$pfx/lib" \
        -DCMAKE_PREFIX_PATH="$pfx" >/dev/null
    cmake --build "$bdir/opkg" -j"$(nproc)" >/dev/null
    [[ -x "$OPKG_BIN" ]] || OPKG_BIN="$(find "$bdir/opkg" -type f -executable -name 'opkg*' | head -1)"
    [[ -x "$UCI_BIN" && -x "$OPKG_BIN" ]] || { echo "prebuild: uci/opkg build failed" >&2; exit 3; }
}

# ===================== B. apk-provision dropbear + dnsmasq stacks ==============================
detect_branch() {
    [[ -n "${STARRYWRT_APK_BRANCH:-}" ]] && { echo "$STARRYWRT_APK_BRANCH"; return; }
    local rel maj min
    [[ -n "$rootfs_img" && -f "$rootfs_img" ]] && rel="$(debugfs -R 'cat /etc/alpine-release' "$rootfs_img" 2>/dev/null | tr -d '\r\n ')"
    maj="$(printf '%s' "${rel:-}" | cut -d. -f1)"; min="$(printf '%s' "${rel:-}" | cut -d. -f2)"
    [[ -n "$maj" && -n "$min" ]] && echo "v$maj.$min" || { echo "prebuild: cannot read alpine-release; set STARRYWRT_APK_BRANCH" >&2; exit 2; }
}

apk_provision() {
    local BRANCH; BRANCH="$(detect_branch)"
    local MIRRORS="${STARRYWRT_APK_MIRROR:-https://dl-cdn.alpinelinux.org/alpine} https://mirrors.tuna.tsinghua.edu.cn/alpine"
    local idx="$CACHE/apk/idx" apks="$CACHE/apk/apks/$ALPINE_ARCH" ex="$CACHE/apk/extract/$ALPINE_ARCH"
    mkdir -p "$idx" "$apks"; rm -rf "$ex"; mkdir -p "$ex"
    local repo m ok
    for repo in main community; do
        [[ -s "$idx/APKINDEX-$repo-$ALPINE_ARCH" ]] && continue
        ok=0
        for m in $MIRRORS; do
            if curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo/$ALPINE_ARCH/APKINDEX.tar.gz" -o "$idx/$repo.tgz" 2>/dev/null; then
                tar xzf "$idx/$repo.tgz" -O APKINDEX > "$idx/APKINDEX-$repo-$ALPINE_ARCH" 2>/dev/null && { ok=1; break; }
            fi
        done
        [[ "$ok" = 1 ]] || { echo "prebuild: cannot fetch $repo APKINDEX ($ALPINE_ARCH/$BRANCH)" >&2; exit 3; }
    done
    local pkg rv repo2 ver apk
    for pkg in $APK_PKGS; do
        rv=""; for repo2 in main community; do
            ver="$(awk -v p="$pkg" 'BEGIN{RS="";FS="\n"}{n="";v="";for(i=1;i<=NF;i++){if($i~/^P:/)n=substr($i,3);if($i~/^V:/)v=substr($i,3)} if(n==p){print v; exit}}' "$idx/APKINDEX-$repo2-$ALPINE_ARCH")"
            [[ -n "$ver" ]] && { rv="$repo2 $ver"; break; }
        done
        [[ -n "$rv" ]] || { echo "prebuild: $pkg not in APKINDEX ($ALPINE_ARCH/$BRANCH)" >&2; exit 3; }
        repo2="${rv%% *}"; ver="${rv##* }"; apk="$apks/$pkg-$ver.apk"
        if [[ ! -s "$apk" ]]; then
            for m in $MIRRORS; do curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo2/$ALPINE_ARCH/$pkg-$ver.apk" -o "$apk.tmp" 2>/dev/null && { mv -f "$apk.tmp" "$apk"; break; }; done
        fi
        [[ -s "$apk" ]] || { echo "prebuild: cannot download $pkg-$ver.apk" >&2; exit 3; }
        tar -xzf "$apk" -C "$ex" 2>/dev/null || tar -xf "$apk" -C "$ex" 2>/dev/null || true
        echo "prebuild: unpacked $pkg=$ver ($repo2)"
    done
    APK_EXTRACT="$ex"
}

# ===================== staging ================================================================
stage_all() {
    local bdir="$CACHE/build-$arch" ex="$APK_EXTRACT"
    mkdir -p "$overlay_dir/usr/bin" "$overlay_dir/usr/sbin" "$overlay_dir/usr/lib" \
             "$overlay_dir/etc/config" "$overlay_dir/etc/init.d"
    # config + package core (source-built)
    install -Dm0755 "$UCI_BIN"  "$overlay_dir/usr/bin/uci"
    install -Dm0755 "$OPKG_BIN" "$overlay_dir/usr/bin/opkg"
    # SSH stack
    cp -a "$ex/usr/sbin/dropbear"      "$overlay_dir/usr/sbin/dropbear"
    for b in dropbearkey dbclient dropbearconvert; do [[ -e "$ex/usr/bin/$b" ]] && cp -a "$ex/usr/bin/$b" "$overlay_dir/usr/bin/$b"; done
    cp -a "$ex"/usr/lib/libutmps.so.*   "$overlay_dir/usr/lib/" 2>/dev/null || true
    cp -a "$ex"/usr/lib/libskarnet.so.* "$overlay_dir/usr/lib/" 2>/dev/null || true
    # DNS/DHCP stack
    cp -a "$ex/usr/sbin/dnsmasq" "$overlay_dir/usr/sbin/dnsmasq"
    chmod 0755 "$overlay_dir/usr/sbin/dropbear" "$overlay_dir/usr/sbin/dnsmasq" 2>/dev/null || true
    # OpenWrt /etc identity + config + init framework (arch placeholder substituted)
    cp -a "$app_dir/files/etc/." "$overlay_dir/etc/"
    for f in openwrt_release os-release; do
        [[ -f "$overlay_dir/etc/$f" ]] && sed -i "s/STARRY_ARCH_PLACEHOLDER/$arch/g" "$overlay_dir/etc/$f"
    done
    chmod 0755 "$overlay_dir/etc/rc.common" "$overlay_dir/etc/init.d/"* 2>/dev/null || true
    # carpets + gate
    for s in uci-carpet.sh opkg-carpet.sh starrywrt-carpet.sh run-starrywrt.sh; do
        install -Dm0755 "$app_dir/programs/$s" "$overlay_dir/usr/bin/$s"
    done
    echo "prebuild: staged StarryWRT userland (uci/opkg + dropbear + dnsmasq + /etc + carpets)"
}

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
    build_tools
    apk_provision
    stage_all
    echo "prebuild: StarryWRT overlay ready for $arch"
}

main "$@"
