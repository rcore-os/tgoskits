#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
workspace="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"

require_env() {
    local name="$1" value="$2"
    if [[ -z "$value" ]]; then
        echo "error: $name is required" >&2
        exit 1
    fi
}

require_env STARRY_OVERLAY_DIR "$overlay_dir"

# Download, patch, and build jcode assets.
"$app_dir/prepare_jcode_assets.sh"

asset_dir="$workspace/target/jcode/assets"
for f in jcode.bin libglibc_stub.so jcode; do
    [[ -f "$asset_dir/$f" ]] || { echo "error: missing asset: $f" >&2; exit 1; }
done

# Populate overlay for the app runner to inject into rootfs.
install -Dm0755 "$asset_dir/jcode.bin"         "$overlay_dir/usr/lib/jcode/jcode.bin"
install -Dm0755 "$asset_dir/libglibc_stub.so"  "$overlay_dir/usr/lib/jcode/libglibc_stub.so"
install -Dm0755 "$asset_dir/jcode"             "$overlay_dir/usr/bin/jcode"

for f in "$asset_dir"/libssl.so* "$asset_dir"/libcrypto.so*; do
    [[ -f "$f" ]] && install -Dm0755 "$f" "$overlay_dir/usr/lib/jcode/"
done

for lib in libcom_err.so.2 libgssapi_krb5.so.2 libk5crypto.so.3 \
           libkeyutils.so.1 libkrb5.so.3 libkrb5support.so.0; do
    [[ -f "$asset_dir/$lib" ]] && install -Dm0755 "$asset_dir/$lib" "$overlay_dir/usr/lib/"
done

echo "jcode overlay ready in $overlay_dir"
