#!/bin/bash
set -euo pipefail

rootfs=${1:-}
source_sha256=${2:-}

case "$rootfs" in
    /*) ;;
    *) echo "install-source-link: rootfs must be an absolute path" >&2; exit 2 ;;
esac
if [ "${#source_sha256}" -ne 64 ]; then
    echo "install-source-link: invalid source SHA-256" >&2
    exit 2
fi
case "$source_sha256" in
    *[!0-9a-f]*) echo "install-source-link: invalid source SHA-256" >&2; exit 2 ;;
esac

source_dir="$rootfs/opt/tgoskits-sources/$source_sha256"
[ -f "$source_dir/Cargo.toml" ] || {
    echo "install-source-link: source tree has no Cargo.toml: $source_dir" >&2
    exit 1
}

# Keep the link valid both before chroot and after `/opt` becomes the chroot's
# root-relative directory tree.
ln -sfn "tgoskits-sources/$source_sha256" "$rootfs/opt/tgoskits"
[ -f "$rootfs/opt/tgoskits/Cargo.toml" ] || {
    echo "install-source-link: installed source link is not host-visible" >&2
    exit 1
}
