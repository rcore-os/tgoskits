#!/usr/bin/env bash
# Shared by the image build and the Go application's bare-host preparation.
set -euo pipefail

destination="${1:?Go installation directory is required}"
version=1.26.3
case "$(uname -m)" in
    x86_64) arch=amd64; checksum=2b2cfc7148493da5e73981bffbf3353af381d5f93e789c82c79aff64962eb556 ;;
    aarch64) arch=arm64; checksum=9d89a3ea57d141c2b22d70083f2c8459ba3890f2d9e818e7e933b75614936565 ;;
    *) echo "unsupported Go build host: $(uname -m)" >&2; exit 1 ;;
esac

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
curl -fsSL --retry 3 --connect-timeout 30 --max-time 600 \
    "https://go.dev/dl/go${version}.linux-${arch}.tar.gz" -o "$temporary/go.tar.gz"
echo "$checksum  $temporary/go.tar.gz" | sha256sum -c -
mkdir -p "$destination"
tar -xzf "$temporary/go.tar.gz" --strip-components=1 -C "$destination"
"$destination/bin/go" version
