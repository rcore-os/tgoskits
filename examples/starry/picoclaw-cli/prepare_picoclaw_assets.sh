#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

version="${PICOCLAW_VERSION:-v0.2.8}"
asset_name="${PICOCLAW_ASSET_NAME:-picoclaw_Linux_x86_64.tar.gz}"
asset_sha256="${PICOCLAW_ASSET_SHA256:-e35aea853711db829e0d1969d875f2efcca9cfeec92a43dedb84b46a56b890be}"
asset_url="${PICOCLAW_ASSET_URL:-https://github.com/sipeed/picoclaw/releases/download/${version}/${asset_name}}"
asset_dir="${PICOCLAW_ASSET_DIR:-${workspace}/target/picoclaw/assets}"

usage() {
    cat <<EOF
Usage: $0 [--asset-dir DIR] [--url URL] [--sha256 HEX]

Download or reuse the PicoClaw Linux x86_64 release asset and place the
static binaries under target/picoclaw/assets/.

Environment overrides:
  PICOCLAW_VERSION      Release tag, default: ${version}
  PICOCLAW_ASSET_URL    Full release asset URL
  PICOCLAW_ASSET_SHA256 Expected SHA-256 of the tarball
  PICOCLAW_ASSET_DIR    Output directory
EOF
}

while (($#)); do
    case "$1" in
        --asset-dir)
            asset_dir="$2"
            shift 2
            ;;
        --url)
            asset_url="$2"
            shift 2
            ;;
        --sha256)
            asset_sha256="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "missing required command: sha256sum or shasum" >&2
        exit 1
    fi
}

need_cmd curl
need_cmd tar
need_cmd install
need_cmd mktemp

mkdir -p "$asset_dir"

if [[ -x "${asset_dir}/picoclaw" && -x "${asset_dir}/picoclaw-launcher" ]]; then
    echo "PicoClaw assets already exist in ${asset_dir}"
    exit 0
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/picoclaw-assets.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT

tarball="${tmpdir}/${asset_name}"
echo "Downloading ${asset_url}"
curl -L --fail --retry 3 --output "$tarball" "$asset_url"

actual_sha256="$(sha256_of "$tarball")"
if [[ "$actual_sha256" != "$asset_sha256" ]]; then
    echo "SHA-256 mismatch for ${asset_name}" >&2
    echo "expected: ${asset_sha256}" >&2
    echo "actual:   ${actual_sha256}" >&2
    exit 1
fi

tar -xzf "$tarball" -C "$tmpdir"

if [[ ! -f "${tmpdir}/picoclaw" || ! -f "${tmpdir}/picoclaw-launcher" ]]; then
    echo "release asset does not contain expected PicoClaw binaries" >&2
    exit 1
fi

install -m 0755 "${tmpdir}/picoclaw" "${asset_dir}/picoclaw"
install -m 0755 "${tmpdir}/picoclaw-launcher" "${asset_dir}/picoclaw-launcher"

echo "PicoClaw assets ready in ${asset_dir}"
