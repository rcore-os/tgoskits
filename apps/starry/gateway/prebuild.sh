#!/usr/bin/env bash
# prebuild.sh -- provision the StarryOS `gateway` app overlay: angie 1.11.5 (an nginx-compatible
# HTTP server / reverse-proxy fork) plus the gateway carpet configs + on-target driver.
#
#   * x86_64 / aarch64: angie ships an official musl apk (download.angie.software, Alpine v3.23
#     branch -- the SAME branch as the base image, so the musl/openssl/pcre/zlib ABI matches
#     byte-for-byte). The apk is the FULL upstream module set (http_ssl/v2/gzip/gzip_static/gunzip/
#     grpc/stream/stream_ssl/stream_ssl_preread/realip/sub/... + proxy_cache/limit_req/map). angie
#     itself is pinned at 1.11.5-r0 and sha256-verified; its dependency closure (musl/openssl/
#     libssl3/libcrypto3/pcre2/zlib) is version-resolved LIVE from the Alpine v3.23/main APKINDEX,
#     so the patch releases track whatever the branch currently ships. Alpine prunes superseded
#     patch versions from main, so a hard-pinned patch version 404s in a clean checkout; resolving
#     live keeps the fetch reproducible. Same branch => same soname/ABI as the base rootfs.
#   * riscv64 / loongarch64: angie's official Alpine repo ships NO apk for these (APKINDEX 404), so
#     angie 1.11.5 is cross-compiled from the official source tarball (build-angie-full.sh in the
#     asset store) WITH the full gateway module set -- http_ssl / http_v2 / gzip(+static+gunzip) /
#     proxy(+cache) / grpc / stream / stream_ssl / stream_ssl_preread / realip / sub -- linked
#     against openssl 3.5.x + zlib + pcre2 taken from the SAME Alpine v3.23/main branch as the base
#     rootfs (byte-for-byte ABI match). Delivered as a `payload-full` tree (angie + libssl/libcrypto/
#     libz/libpcre2 .so closure). NEVER a SKIP. See the assets SOURCES notes.
#
# Binaries are provisioned from a reproducible asset store (GATEWAY_BINS_DIR); the apk arches
# additionally fall back to the official CDNs when the store is absent (angie sha256-verified,
# the dependency apks resolved from the live APKINDEX). Env from the app runner: STARRY_ARCH,
# STARRY_ROOTFS, STARRY_STAGING_ROOT, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
base_rootfs="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"

BINS_DIR="${GATEWAY_BINS_DIR:-$HOME/rcore/download/gateway-bins/angie}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
ANGIE_CDN="${ANGIE_CDN:-https://download.angie.software/angie/alpine}"
APK_BRANCH="${GATEWAY_APK_BRANCH:-v3.23}"
ROOTFS_SIZE_MIB="${GATEWAY_ROOTFS_MIB:-1536}"   # angie is tiny; grow-only headroom

case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# angie 1.11.5-r0 apk sha256 (x86_64 + aarch64), pinned + verified byte-for-byte against the
# angie official APKINDEX. The dependency closure is version-resolved live (see stage_apk_arch),
# so those drifting patch releases are not pinned here.
apk_sha() { case "$1" in
    x86_64/angie-1.11.5-r0.apk)  echo 4ad91b7932451695193783aae41ec5f61a0cbbf032bf4d588cfdc6769c7edc66 ;;
    aarch64/angie-1.11.5-r0.apk) echo 2fb159974dd7da4618b10fb71cf682d8f8cbbe6af4197883538e9e3d61c9cee9 ;;
    *) echo "" ;;
esac ; }

ANGIE_APK="angie-1.11.5-r0.apk"
# angie's runtime dependency closure, by Alpine main package name; patch versions resolved live.
DEP_PKGS="musl openssl libssl3 libcrypto3 pcre2 zlib"

ensure_host_tools() {
    local missing=()
    for t in debugfs resize2fs e2fsck truncate tar sha256sum curl openssl awk; do
        command -v "$t" >/dev/null 2>&1 || missing+=("$t")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "prebuild: missing host tools: ${missing[*]}" >&2; exit 1
    fi
}

grow_rootfs() {
    [[ -f "$base_rootfs" ]] || { echo "prebuild: rootfs image missing: $base_rootfs" >&2; exit 2; }
    local before target
    before=$(stat -c %s "$base_rootfs")
    target=$((ROOTFS_SIZE_MIB * 1024 * 1024))
    if [[ "$before" -lt "$target" ]]; then
        echo "prebuild: growing rootfs to ${ROOTFS_SIZE_MIB}MiB (grow-only)"
        truncate -s "${ROOTFS_SIZE_MIB}M" "$base_rootfs"
        e2fsck -f -y "$base_rootfs" >/dev/null 2>&1 || true
        resize2fs "$base_rootfs" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed" >&2; exit 2; }
    fi
}

# fetch one apk into $1 if not already a valid local asset; verify sha256.
resolve_apk() {  # $1=destdir $2=arch/name key $3=url
    local dest="$1/$2"; local key="$3" url="$4"
    local want; want="$(apk_sha "$key")"
    mkdir -p "$(dirname "$dest")"
    if [[ ! -f "$dest" ]]; then
        echo "prebuild: fetch $url"
        curl -fL --retry 3 -o "$dest" "$url"
    fi
    if [[ -n "$want" ]]; then
        echo "$want  $dest" | sha256sum -c - >/dev/null 2>&1 \
            || { echo "prebuild: sha256 MISMATCH for $2" >&2; exit 3; }
    fi
}

# Fetches the live Alpine main APKINDEX for the current arch into $1 (once per run).
ensure_alpine_apkindex() {  # $1=index dir
    if [[ ! -f "$1/APKINDEX" ]]; then
        mkdir -p "$1"
        curl -fsSL --retry 3 "$ALPINE_CDN/$APK_BRANCH/main/$arch/APKINDEX.tar.gz" -o "$1/APKINDEX.tar.gz"
        tar --warning=no-unknown-keyword -xzf "$1/APKINDEX.tar.gz" -C "$1" APKINDEX
    fi
}

# Prints the currently published version of an Alpine main package from the APKINDEX.
alpine_pkg_ver() {  # $1=index dir  $2=package
    awk -v RS='' -v p="$2" 'match($0, "(^|\n)P:"p"(\n|$)"){print}' "$1/APKINDEX" \
        | sed -n 's/^V://p' | head -1
}

stage_apk_arch() {  # x86_64 / aarch64: extract angie apk + dep closure into overlay
    local stage; stage="$(mktemp -d)"
    local localdir="$BINS_DIR/$arch"
    local dl; dl="$(mktemp -d)"
    # angie itself (official angie repo, pinned 1.11.5-r0). Prefer the reproducible local store.
    if [[ -f "$localdir/$ANGIE_APK" ]]; then cp -f "$localdir/$ANGIE_APK" "$dl/$ANGIE_APK"
    else resolve_apk "$dl" "$ANGIE_APK" "$arch/$ANGIE_APK" "$ANGIE_CDN/$APK_BRANCH/main/$arch/$ANGIE_APK"; fi
    echo "$(apk_sha "$arch/$ANGIE_APK")  $dl/$ANGIE_APK" | sha256sum -c - >/dev/null 2>&1 \
        || { echo "prebuild: angie apk sha256 MISMATCH" >&2; exit 3; }
    # Dependency closure: patch versions resolved live from the Alpine main APKINDEX so the fetch
    # never chases a pruned patch release. Same branch as the base rootfs => same soname/ABI.
    local idxdir="$dl/apkindex"
    ensure_alpine_apkindex "$idxdir"
    local pkg ver apkfile
    for pkg in $DEP_PKGS; do
        ver="$(alpine_pkg_ver "$idxdir" "$pkg")"
        [[ -n "$ver" ]] || { echo "prebuild: $pkg not in Alpine $APK_BRANCH main APKINDEX" >&2; exit 3; }
        apkfile="${pkg}-${ver}.apk"
        if [[ -f "$localdir/$apkfile" ]]; then cp -f "$localdir/$apkfile" "$dl/$apkfile"
        else
            echo "prebuild: fetch $ALPINE_CDN/$APK_BRANCH/main/$arch/$apkfile"
            curl -fL --retry 3 -o "$dl/$apkfile" "$ALPINE_CDN/$APK_BRANCH/main/$arch/$apkfile"
        fi
    done
    local apk
    for apk in "$dl/$ANGIE_APK" "$dl"/*.apk; do
        [[ -f "$apk" ]] || continue
        tar xzf "$apk" -C "$stage" \
            --exclude='.PKGINFO' --exclude='.SIGN.*' --exclude='.pre-install' \
            --exclude='.post-install' --exclude='.pre-upgrade' --exclude='.post-upgrade' \
            --exclude='.trigger' 2>/dev/null || true
    done
    # overlay: the angie binary + symlink + the shared-library closure.
    mkdir -p "$overlay_dir/usr/sbin" "$overlay_dir/usr/lib"
    cp -a "$stage/usr/sbin/angie-nodebug" "$overlay_dir/usr/sbin/angie-nodebug"
    ln -sf angie-nodebug "$overlay_dir/usr/sbin/angie"
    cp -a "$stage"/usr/lib/. "$overlay_dir/usr/lib/"
    rm -rf "$stage" "$dl"
    [[ -x "$overlay_dir/usr/sbin/angie-nodebug" ]] || { echo "prebuild: angie binary not staged" >&2; exit 3; }
}

stage_payload_arch() {  # riscv64 / loongarch64: source-cross-built full-module payload + .so closure
    local pay="$BINS_DIR/$arch/payload-full"
    [[ -x "$pay/usr/sbin/angie-nodebug" ]] \
        || { echo "prebuild: missing full-module payload $pay (run build-angie-full.sh $arch first)" >&2; exit 3; }
    mkdir -p "$overlay_dir/usr/sbin" "$overlay_dir/usr/lib"
    cp -a "$pay/usr/sbin/angie-nodebug" "$overlay_dir/usr/sbin/angie-nodebug"
    ln -sf angie-nodebug "$overlay_dir/usr/sbin/angie"
    cp -a "$pay"/usr/lib/. "$overlay_dir/usr/lib/"
    # the full-module binary needs libssl/libcrypto/libz/libpcre2 at runtime (staged above).
    for so in libssl.so.3 libcrypto.so.3 libz.so.1 libpcre2-8.so.0; do
        [[ -e "$overlay_dir/usr/lib/$so" ]] || { echo "prebuild: payload-full missing runtime lib $so" >&2; exit 3; }
    done
}

# self-signed TLS fixtures for the ssl/mTLS/ssl_preread carpets. Generated once into the asset
# store (cached, so all four arches share identical certs + reruns are reproducible) then copied
# into the overlay at /etc/angie/certs. The carpet only asserts SNI/CN routing + verify success/
# failure, which the fixed CN + trusted/rogue CA split provides.
stage_certs() {
    local cdir="$BINS_DIR/certs"
    if [[ ! -f "$cdir/ca.crt" ]]; then
        echo "prebuild: generating TLS cert fixtures into $cdir"
        mkdir -p "$cdir"; ( cd "$cdir"
            openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt \
                -subj '/CN=GatewayTestCA' -days 3650 >/dev/null 2>&1
            openssl req -x509 -newkey rsa:2048 -nodes -keyout rogueca.key -out rogueca.crt \
                -subj '/CN=RogueCA' -days 3650 >/dev/null 2>&1
            for spec in backend.local:backend:ca foo.test:foo:ca bar.test:bar:ca \
                        gwclient:client:ca backend.local:rogue:rogueca; do
                cn="${spec%%:*}"; rest="${spec#*:}"; name="${rest%%:*}"; cap="${rest#*:}"
                openssl req -newkey rsa:2048 -nodes -keyout "$name.key" -out "$name.csr" \
                    -subj "/CN=$cn" >/dev/null 2>&1
                openssl x509 -req -in "$name.csr" -CA "$cap.crt" -CAkey "$cap.key" \
                    -CAcreateserial -out "$name.crt" -days 3650 >/dev/null 2>&1
                rm -f "$name.csr"
            done )
    fi
    mkdir -p "$overlay_dir/etc/angie/certs"
    local f
    for f in ca backend foo bar rogue client; do
        install -Dm0644 "$cdir/$f.crt" "$overlay_dir/etc/angie/certs/$f.crt"
        install -Dm0640 "$cdir/$f.key" "$overlay_dir/etc/angie/certs/$f.key"
    done
}

stage_configs_and_driver() {
    mkdir -p "$overlay_dir/etc/angie" "$overlay_dir/usr/bin"
    install -Dm0644 "$app_dir/conf/gateway.conf"         "$overlay_dir/etc/angie/gateway.conf"
    install -Dm0644 "$app_dir/conf/gateway-workers.conf" "$overlay_dir/etc/angie/gateway-workers.conf"
    install -Dm0755 "$app_dir/programs/run-gateway.sh"   "$overlay_dir/usr/bin/run-gateway.sh"
    printf '/lib\n/usr/lib\n' > "$overlay_dir/etc/ld-musl-$arch.path"
}

ensure_host_tools
grow_rootfs
case "$arch" in
    x86_64|aarch64)          stage_apk_arch ;;
    riscv64|loongarch64)     stage_payload_arch ;;
esac
stage_certs
stage_configs_and_driver
echo "prebuild: gateway overlay ready for $arch ($(du -sm "$overlay_dir" 2>/dev/null | cut -f1)M)"
