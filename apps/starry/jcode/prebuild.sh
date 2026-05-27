#!/usr/bin/env bash
# prebuild.sh for the jcode app case.
# Delegates to prepare_jcode_rootfs.sh which handles asset download, glibc-to-musl
# patching, and rootfs injection in one step.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-$(cd "$script_dir/../../.." && pwd)}"
output_rootfs="$workspace/tmp/axbuild/rootfs/rootfs-x86_64-jcode.img"

if [[ -f "$output_rootfs" ]]; then
    echo "jcode rootfs already exists: $output_rootfs"
    echo "To rebuild, remove it first or run prepare_jcode_rootfs.sh directly."
    exit 0
fi

"$script_dir/prepare_jcode_rootfs.sh"
