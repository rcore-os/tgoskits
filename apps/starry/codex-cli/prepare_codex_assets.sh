#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
asset_dir="$workspace/target/codex/assets"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/starry-codex-assets.XXXXXX")"
package="@openai/codex@0.115.0-linux-x64"

codex_sha256="440269f35afeb90d38115af844629d98705fb7266fdcd5fe7c040a78ebc75b85"
rg_sha256="ebeaf56f8a25e102e9419933423738b3a2a613a444fd749d695e15eba53f71f2"

cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

verify_sha256() {
    local expected="$1"
    local path="$2"
    local actual

    actual="$(sha256sum "$path" | awk '{print $1}')"
    if [[ "$actual" != "$expected" ]]; then
        echo "error: SHA-256 mismatch for $path" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

need_cmd npm
need_cmd tar
need_cmd sha256sum
need_cmd install

mkdir -p "$asset_dir"

echo "Preparing Codex CLI assets from npm package $package"
pack_output="$(npm pack "$package" --pack-destination "$tmp_dir" --silent)"
tarball="$(printf '%s\n' "$pack_output" | tail -n 1)"
tar -xzf "$tmp_dir/$tarball" -C "$tmp_dir"

codex_src="$tmp_dir/package/vendor/x86_64-unknown-linux-musl/codex/codex"
rg_src="$tmp_dir/package/vendor/x86_64-unknown-linux-musl/path/rg"

if [[ ! -f "$codex_src" || ! -f "$rg_src" ]]; then
    echo "error: expected Codex or ripgrep binary missing from $package" >&2
    exit 1
fi

install -m 0755 "$codex_src" "$asset_dir/codex"
install -m 0755 "$rg_src" "$asset_dir/rg"

verify_sha256 "$codex_sha256" "$asset_dir/codex"
verify_sha256 "$rg_sha256" "$asset_dir/rg"

"$asset_dir/codex" --version
"$asset_dir/rg" --version | head -n 1

echo "Codex CLI assets ready in $asset_dir"
