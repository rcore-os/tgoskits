#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR is required}"
staging_root="${STARRY_STAGING_ROOT:?prebuild: STARRY_STAGING_ROOT is required}"

# shellcheck source=source.env
source "$app_dir/source.env"

prebuilt_dir="$app_dir/prebuilt/aarch64"
package_dir="$staging_root/package/aka-rk3588"
package_archive="$overlay_dir/aka-rk3588.tar.gz"

need_command() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "aka-rk3588 prebuild: missing required command: $1" >&2
        exit 1
    }
}

for command in sha256sum tar; do
    need_command "$command"
done

test -s "$prebuilt_dir/build/tennis"
(
    cd "$prebuilt_dir"
    sha256sum -c SHA256SUMS
)
printf '%s  %s\n' "$AKA_RK3588_BINARY_SHA256" \
    "$prebuilt_dir/build/tennis" | sha256sum -c -

mkdir -p "$overlay_dir" "$package_dir"
cp -a "$prebuilt_dir/." "$package_dir/"

tar -czf "$package_archive" -C "$staging_root/package" aka-rk3588
echo "aka-rk3588 prebuild: staged $package_archive"
