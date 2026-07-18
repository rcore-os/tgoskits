#!/usr/bin/env bash
# prebuild.sh - provision the higress standalone gateway (Envoy data plane) for
# StarryOS.
#
# higress standalone = the official Envoy release driven by a static bootstrap
# (no xDS control plane). Envoy ships prebuilt ELFs only for glibc x86_64 +
# aarch64 (github.com/envoyproxy/envoy/releases), so those two run the stock
# release binary directly. There is no upstream riscv64 / loongarch64 build, so
# for those two this script source-builds Envoy 1.38.3 from pinned sources with
# clang-18 against a musl cross sysroot (assets/build-envoy-rvloong.sh, linked
# with lld), producing a musl-dynamic ELF whose interpreter /lib/ld-musl-<arch>.so.1
# the Alpine base rootfs already provides. The source build is cached under
# HIGRESS_CACHE so it runs once.
#
# The script is fully self-contained: it never relies on in-guest networking (apk)
# at runtime. It stages, into the overlay:
#   - the Envoy binary + the exact runtime libraries its ELF header asks for
#     (readelf-driven): the glibc loader + libs from the arch's <arch>-linux-gnu
#     cross sysroot for x86_64/aarch64, or libstdc++.so.6 + libgcc_s.so.1 from the
#     musl cross sysroot for riscv64/loongarch64 (musl libc + loader come from the
#     Alpine base);
#   - `echod`, a tiny static-musl HTTP echo backend cross-compiled from source
#     (the Alpine base busybox has no `httpd` applet);
#   - the `openssl` CLI (+ libssl/libcrypto), pulled from the matching Alpine
#     branch, for the downstream/upstream TLS backends and clients;
#   - the bootstrap configs, TLS fixtures, and the carpet runner.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
cache_root="${HIGRESS_CACHE:-${HOME:-/root}/.cache/starry-higress-carpet}"

ENVOY_VER=1.38.3
ALPINE_BRANCH=v3.23
ALPINE_MIRROR=https://dl-cdn.alpinelinux.org/alpine

envoy_source_build=0
case "$arch" in
    x86_64)
        envoy_asset="x86_64"
        gcc_prefix="x86_64-linux-gnu"
        musl_cc="x86_64-linux-musl-gcc"
        alpine_arch="x86_64"
        envoy_sha=affffb8d08a14fdc375b1f7dd8d0f3004eacdf51ce07f5636d7e168a01c6b373
        ;;
    aarch64)
        envoy_asset="aarch_64"
        gcc_prefix="aarch64-linux-gnu"
        musl_cc="aarch64-linux-musl-gcc"
        alpine_arch="aarch64"
        envoy_sha=eff9766ce1a7af71c38a6d4587367621753049ae3df1bde5b6b9e695752f3167
        ;;
    riscv64)
        envoy_source_build=1
        musl_triple="riscv64-linux-musl"
        musl_cc="riscv64-linux-musl-gcc"
        alpine_arch="riscv64"
        ;;
    loongarch64)
        envoy_source_build=1
        musl_triple="loongarch64-linux-musl"
        musl_cc="loongarch64-linux-musl-gcc"
        alpine_arch="loongarch64"
        ;;
    *)
        echo "prebuild: unsupported arch $arch" >&2
        exit 1
        ;;
esac

command -v "$musl_cc" >/dev/null 2>&1 || { echo "prebuild: $musl_cc not found (source .starry-env.sh)" >&2; exit 1; }
command -v readelf >/dev/null 2>&1 || { echo "prebuild: readelf not found" >&2; exit 1; }
command -v openssl >/dev/null 2>&1 || { echo "prebuild: openssl not found" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "prebuild: curl not found" >&2; exit 1; }

# --- 1. obtain the Envoy binary + install it into the overlay ---
if [[ "$envoy_source_build" == "1" ]]; then
    # riscv64 / loongarch64: no upstream build - source-build from pinned Envoy
    # sources (clang-18 + musl cross, lld), cached under $cache_root. See
    # assets/build-envoy-rvloong.sh and README.md.
    command -v "${musl_triple}-gcc" >/dev/null 2>&1 || { echo "prebuild: ${musl_triple}-gcc not found (source .starry-env.sh)" >&2; exit 1; }
    musl_root=$(dirname "$(dirname "$(command -v "${musl_triple}-gcc")")")
    envoy_bin="$cache_root/envoy-${ENVOY_VER}-linux-${arch}"
    if [[ ! -x "$envoy_bin" ]]; then
        echo "prebuild: no cached $arch Envoy; source-building (one-time, long)..."
        mkdir -p "$cache_root"
        MUSL_CROSS="$musl_root" bash "$app_dir/assets/build-envoy-rvloong.sh" "$arch" "$cache_root"
    fi
    [[ -x "$envoy_bin" ]] || { echo "prebuild: source build did not produce $envoy_bin" >&2; exit 1; }
    # The bazel output keeps debug info (~380MB); strip it for the rootfs image.
    "${musl_triple}-strip" -s "$envoy_bin" -o "$cache_root/envoy-stripped-$arch"
    install -Dm0755 "$cache_root/envoy-stripped-$arch" "$overlay_dir/usr/bin/envoy"
else
    envoy_bin="$cache_root/envoy-${ENVOY_VER}-linux-${envoy_asset}"
    verify_sha() { echo "${envoy_sha}  ${envoy_bin}" | sha256sum -c - >/dev/null 2>&1; }
    if [[ ! -f "$envoy_bin" ]] || ! verify_sha; then
        mkdir -p "$cache_root"
        url="https://github.com/envoyproxy/envoy/releases/download/v${ENVOY_VER}/envoy-${ENVOY_VER}-linux-${envoy_asset}"
        echo "prebuild: fetching $url ..."
        curl -fsSL --retry 3 "$url" -o "$envoy_bin"
        verify_sha || { echo "prebuild: Envoy SHA256 mismatch for $envoy_bin" >&2; exit 1; }
    fi
    install -Dm0755 "$envoy_bin" "$overlay_dir/usr/bin/envoy"
fi

# --- 2. stage the runtime libraries the Envoy ELF asks for ---
# Read the interpreter + NEEDED sonames straight from the installed binary so the
# overlay carries exactly what it loads.
interp=$(readelf -l "$overlay_dir/usr/bin/envoy" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
[[ -n "$interp" ]] || { echo "prebuild: no PT_INTERP in Envoy binary" >&2; exit 1; }
echo "prebuild: Envoy interpreter: $interp"
ld_soname=$(basename "$interp")

if [[ "$envoy_source_build" == "1" ]]; then
    # musl-dynamic: the interpreter /lib/ld-musl-<arch>.so.1 and libc.so are the
    # Alpine base musl itself; only the C++ runtime the binary pulls in
    # (libstdc++.so.6 and, transitively, libgcc_s.so.1) has to be staged, resolved
    # from the musl cross sysroot.
    stage_musl_lib() { # soname
        local soname="$1" real dreal dep
        case "$soname" in libc.so|"$ld_soname") return 0 ;; esac
        [[ -e "$overlay_dir/usr/lib/$soname" ]] && return 0
        real=$(readlink -f "$("${musl_triple}-gcc" -print-file-name="$soname")")
        [[ -f "$real" ]] || { echo "prebuild: NEEDED lib $soname not found in musl cross sysroot" >&2; exit 1; }
        install -Dm0755 "$real" "$overlay_dir/usr/lib/$soname"
        echo "prebuild: staged $soname <- $real"
        # follow the soname's own NEEDED (libstdc++ -> libgcc_s); one level suffices
        readelf -d "$real" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | while read -r dep; do
            case "$dep" in libc.so|"$ld_soname"|"$soname") continue ;; esac
            [[ -e "$overlay_dir/usr/lib/$dep" ]] && continue
            dreal=$(readlink -f "$("${musl_triple}-gcc" -print-file-name="$dep")")
            [[ -f "$dreal" ]] && { install -Dm0755 "$dreal" "$overlay_dir/usr/lib/$dep"; echo "prebuild: staged $dep <- $dreal"; }
        done
    }
    readelf -d "$overlay_dir/usr/bin/envoy" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | while read -r soname; do
        stage_musl_lib "$soname"
    done
else
    # glibc: stage the declared loader plus every NEEDED soname from the arch's
    # <arch>-linux-gnu cross sysroot.
    sysroot=$("${gcc_prefix}-gcc" -print-sysroot)
    ld_path=$("${gcc_prefix}-gcc" -print-file-name="$ld_soname")
    [[ -f "$ld_path" ]] || ld_path="$sysroot$interp"
    [[ -f "$ld_path" ]] || { echo "prebuild: loader $ld_soname not found" >&2; exit 1; }
    install -Dm0755 "$(readlink -f "$ld_path")" "$overlay_dir$interp"
    readelf -d "$overlay_dir/usr/bin/envoy" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | while read -r soname; do
        [[ "$soname" == "$ld_soname" ]] && continue
        src=$("${gcc_prefix}-gcc" -print-file-name="$soname")
        [[ -f "$src" ]] || { echo "prebuild: NEEDED lib $soname not found in $gcc_prefix sysroot" >&2; exit 1; }
        real=$(readlink -f "$src")
        install -Dm0755 "$real" "$overlay_dir/lib/$soname"
        echo "prebuild: staged $soname <- $real"
    done
fi

# --- 3. cross-compile the static-musl echo backend ---
"$musl_cc" -static -O2 -o "$cache_root/echod-$arch" "$app_dir/backend/echod.c"
install -Dm0755 "$cache_root/echod-$arch" "$overlay_dir/usr/bin/echod"
echo "prebuild: built echod ($arch, static musl)"

# --- 4. stage the openssl CLI (+ libssl/libcrypto) from the Alpine branch ---
# The Alpine base rootfs already carries libssl.so.3 / libcrypto.so.3 (for busybox
# ssl_client); the matching CLI + libs are resolved live from APKINDEX so the set
# stays self-consistent with whatever point release the branch currently ships.
apkindex_dir="$cache_root/apkindex-$alpine_arch"
if [[ ! -f "$apkindex_dir/APKINDEX" ]]; then
    mkdir -p "$apkindex_dir"
    curl -fsSL "$ALPINE_MIRROR/$ALPINE_BRANCH/main/$alpine_arch/APKINDEX.tar.gz" -o "$apkindex_dir/APKINDEX.tar.gz"
    tar --warning=no-unknown-keyword -xzf "$apkindex_dir/APKINDEX.tar.gz" -C "$apkindex_dir" APKINDEX
fi
stage_apk_file() { # pkg  path-in-apk  dest-in-overlay
    local pkg="$1" inpath="$2" dest="$3" ver apk
    ver=$(awk -v RS='' -v p="$pkg" 'match($0, "(^|\n)P:"p"(\n|$)"){print}' "$apkindex_dir/APKINDEX" \
          | sed -n 's/^V://p' | head -1)
    [[ -n "$ver" ]] || { echo "prebuild: $pkg not found in Alpine $ALPINE_BRANCH APKINDEX" >&2; exit 1; }
    apk="$cache_root/${pkg}-${ver}-${alpine_arch}.apk"
    [[ -f "$apk" ]] || curl -fsSL "$ALPINE_MIRROR/$ALPINE_BRANCH/main/$alpine_arch/${pkg}-${ver}.apk" -o "$apk"
    rm -rf "$cache_root/apk-x-$pkg"; mkdir -p "$cache_root/apk-x-$pkg"
    tar --warning=no-unknown-keyword -xzf "$apk" -C "$cache_root/apk-x-$pkg" "$inpath"
    install -Dm0755 "$cache_root/apk-x-$pkg/$inpath" "$overlay_dir$dest"
    echo "prebuild: staged $dest <- ${pkg}-${ver}"
}
stage_apk_file openssl     usr/bin/openssl           /usr/bin/openssl
stage_apk_file libssl3     usr/lib/libssl.so.3       /usr/lib/libssl.so.3
stage_apk_file libcrypto3  usr/lib/libcrypto.so.3    /usr/lib/libcrypto.so.3

# --- 5. bootstrap configs (baseline reuse_port:false + the reuse_port:true twin) ---
install -Dm0644 "$app_dir/conf/bootstrap.yaml" "$overlay_dir/etc/higress/bootstrap.yaml"
sed 's/enable_reuse_port: false/enable_reuse_port: true/' "$app_dir/conf/bootstrap.yaml" \
    > "$overlay_dir/etc/higress/bootstrap-reuseport.yaml"

# --- 6. TLS fixtures: the server cert plus an unrelated CA (upstream verify-fail) ---
cert_dir="$overlay_dir/etc/higress/certs"
mkdir -p "$cert_dir"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$cert_dir/server.key" -out "$cert_dir/server.crt" \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$cert_dir/otherca.key" -out "$cert_dir/otherca.crt" \
    -subj "/CN=higress-other-ca" >/dev/null 2>&1
chmod 0644 "$cert_dir"/*.key "$cert_dir"/*.crt

# --- 7. carpet runner ---
install -Dm0755 "$app_dir/programs/run-higress.sh" "$overlay_dir/usr/bin/run-higress.sh"

echo "prebuild: higress ready for $arch (Envoy ${ENVOY_VER}, echod + openssl staged)"
