#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(git -C "$app_dir" rev-parse --show-toplevel)"

# shellcheck source=source.env
source "$app_dir/source.env"

archive_name="aka-rk3588-$AKA_RK3588_COMMIT.tar.gz"
cache_dir="${AKA_RK3588_CACHE_DIR:-$workspace/target/aka-rk3588/cache}"
output_dir="${AKA_RK3588_OUTPUT_DIR:-$workspace/target/aka-rk3588}"
archive="${AKA_RK3588_SOURCE_ARCHIVE:-$cache_dir/$archive_name}"
source_url="$AKA_RK3588_REPOSITORY/archive/$AKA_RK3588_COMMIT.tar.gz"
work_dir="$(mktemp -d)"
source_dir="$work_dir/source"
package_dir="$work_dir/aka-rk3588"
package_archive="$output_dir/aka-rk3588.tar.gz"

cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT

for command in curl sha256sum tar; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "aka-rk3588 package: missing required command: $command" >&2
        exit 1
    }
done

mkdir -p "$cache_dir" "$output_dir" "$source_dir" \
    "$package_dir/build" "$package_dir/lib"
if [[ ! -f "$archive" ]]; then
    download="$cache_dir/$archive_name.download"
    curl --fail --location --retry 3 --output "$download" "$source_url"
    mv "$download" "$cache_dir/$archive_name"
    archive="$cache_dir/$archive_name"
fi
printf '%s  %s\n' "$AKA_RK3588_SOURCE_SHA256" "$archive" | sha256sum -c -

tar -xzf "$archive" --strip-components=1 -C "$source_dir"
cp "$app_dir/prebuilt/aarch64/build/tennis" "$package_dir/build/tennis"
cp "$source_dir/3rd/rknpu2/Linux/aarch64/librknnrt.so" \
    "$package_dir/lib/librknnrt.so"
cp -a "$source_dir/config" "$source_dir/models" "$package_dir/"
for script in run_bucket_place_demo.sh run_lekiwi_full.sh run_lekiwi_loop.sh \
    run_lekiwi_test.sh run_robot_ci_once.sh run_vision_once.sh; do
    cp "$source_dir/$script" "$package_dir/$script"
done
chmod +x "$package_dir/build/tennis" "$package_dir"/*.sh
printf '%s  %s\n' "$AKA_RK3588_BINARY_SHA256" \
    "$package_dir/build/tennis" | sha256sum -c -

cat >"$package_dir/SOURCE" <<EOF
repository=$AKA_RK3588_REPOSITORY
commit=$AKA_RK3588_COMMIT
source_sha256=$AKA_RK3588_SOURCE_SHA256
binary_sha256=$AKA_RK3588_BINARY_SHA256
EOF

tar -czf "$package_archive" -C "$work_dir" aka-rk3588
echo "aka-rk3588 package: $package_archive"
