#!/usr/bin/env bash
# prebuild.sh - provision the higress standalone gateway (Envoy data plane) for
# StarryOS.
#
# higress standalone = the official Envoy release driven by a static bootstrap
# (no xDS control plane). Envoy ships prebuilt ONLY for glibc x86_64 + aarch64
# (github.com/envoyproxy/envoy/releases); there is no riscv64 / loongarch64 port
# upstream, so this app is x86_64 + aarch64 only (see README.md).
#
# StarryOS runs the stock glibc-dynamic Envoy ELF directly: this script drops the
# arch's glibc loader + the exact shared objects Envoy's ELF header asks for
# (readelf-driven, same technique as apps/starry/glibc-dynamic-smoke) into the
# overlay, alongside the bootstrap configs, TLS fixtures, and the carpet runner.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
cache_root="${HIGRESS_CACHE:-${HOME:-/root}/.cache/starry-higress-carpet}"

ENVOY_VER=1.38.3
case "$arch" in
    x86_64)
        envoy_asset="x86_64"
        gcc_prefix="x86_64-linux-gnu"
        envoy_sha=affffb8d08a14fdc375b1f7dd8d0f3004eacdf51ce07f5636d7e168a01c6b373
        ;;
    aarch64)
        envoy_asset="aarch_64"
        gcc_prefix="aarch64-linux-gnu"
        envoy_sha=eff9766ce1a7af71c38a6d4587367621753049ae3df1bde5b6b9e695752f3167
        ;;
    *)
        echo "prebuild: higress ships x86_64 + aarch64 only; upstream Envoy has no $arch build" >&2
        exit 1
        ;;
esac

command -v "${gcc_prefix}-gcc" >/dev/null 2>&1 || { echo "prebuild: ${gcc_prefix}-gcc not found" >&2; exit 1; }
command -v readelf >/dev/null 2>&1 || { echo "prebuild: readelf not found" >&2; exit 1; }
command -v openssl >/dev/null 2>&1 || { echo "prebuild: openssl not found" >&2; exit 1; }

# --- 1. fetch the official Envoy binary (reproducible: pinned version + sha256) ---
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

# --- 2. stage the glibc runtime the Envoy ELF asks for ---
# Read the interpreter + NEEDED sonames straight from the Envoy binary so the
# overlay carries exactly what it loads (libc/libm/librt/libdl/libpthread + ld).
interp=$(readelf -l "$envoy_bin" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
[[ -n "$interp" ]] || { echo "prebuild: no PT_INTERP in Envoy binary" >&2; exit 1; }
echo "prebuild: Envoy interpreter: $interp"

ld_soname=$(basename "$interp")
sysroot=$("${gcc_prefix}-gcc" -print-sysroot)
resolve_lib() {
    local name="$1" path
    path=$("${gcc_prefix}-gcc" -print-file-name="$name")
    if [[ "$path" == "$name" || ! -f "$path" ]]; then
        path="$sysroot$interp"
        [[ "$(basename "$path")" == "$name" ]] || path=""
    fi
    printf '%s' "$path"
}

# The loader itself lands at its ELF-declared interpreter path.
ld_path=$(resolve_lib "$ld_soname")
[[ -f "$ld_path" ]] || ld_path="$sysroot$interp"
[[ -f "$ld_path" ]] || { echo "prebuild: loader $ld_soname not found" >&2; exit 1; }
install -Dm0755 "$ld_path" "$overlay_dir$interp"

# Every other NEEDED soname lands in /lib (the runner also exports LD_LIBRARY_PATH).
readelf -d "$envoy_bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | while read -r soname; do
    [[ "$soname" == "$ld_soname" ]] && continue
    src=$("${gcc_prefix}-gcc" -print-file-name="$soname")
    [[ -f "$src" ]] || { echo "prebuild: NEEDED lib $soname not found in $gcc_prefix sysroot" >&2; exit 1; }
    # -print-file-name may hand back a linker script or a versioned real file;
    # copy the resolved regular file under its soname.
    real=$(readlink -f "$src")
    install -Dm0755 "$real" "$overlay_dir/lib/$soname"
    echo "prebuild: staged $soname <- $real"
done

# --- 3. bootstrap configs (baseline reuse_port:false + the reuse_port:true twin) ---
install -Dm0644 "$app_dir/conf/bootstrap.yaml" "$overlay_dir/etc/higress/bootstrap.yaml"
sed 's/enable_reuse_port: false/enable_reuse_port: true/' "$app_dir/conf/bootstrap.yaml" \
    > "$overlay_dir/etc/higress/bootstrap-reuseport.yaml"

# --- 4. TLS fixtures for downstream + upstream TLS (self-signed, SAN localhost) ---
cert_dir="$overlay_dir/etc/higress/certs"
mkdir -p "$cert_dir"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$cert_dir/server.key" -out "$cert_dir/server.crt" \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
chmod 0644 "$cert_dir/server.key" "$cert_dir/server.crt"

# --- 5. carpet runner ---
install -Dm0755 "$app_dir/programs/run-higress.sh" "$overlay_dir/usr/bin/run-higress.sh"

echo "prebuild: higress ready for $arch (Envoy ${ENVOY_VER}, glibc from $gcc_prefix sysroot)"
