#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
mkdir -p "$repo_root/tmp"
test_root="$(mktemp -d -p "$repo_root/tmp" source-link.XXXXXX)"
source_sha=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

cleanup() {
    rm -rf "$test_root"
}
trap cleanup EXIT

mkdir -p "$test_root/opt/tgoskits-sources/$source_sha"
touch "$test_root/opt/tgoskits-sources/$source_sha/Cargo.toml"

"$app_dir/install_source_link.sh" "$test_root" "$source_sha"

[ "$(readlink "$test_root/opt/tgoskits")" = "tgoskits-sources/$source_sha" ]
[ -f "$test_root/opt/tgoskits/Cargo.toml" ]

echo "install_source_link_host_and_chroot_visibility=PASS"
