#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
base_initramfs="${BASE_INITRAMFS:-/home/huhu/tgoskits-realtime/tmp/initramfs-custom}"
task2_binary="${TASK2_BINARY:-$repo_root/tmp/net-dual-guest/linux-task2/controller/task2-net}"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest/linux-task2}"
out_file="$out_dir/task2-linux-initramfs.cpio.gz"
template="$repo_root/scripts/test/net-dual-guest/linux-init.sh"

for input in "$base_initramfs" "$task2_binary" "$template"; do
    if [[ ! -f "$input" ]]; then
        printf 'error: required input is missing: %s\n' "$input" >&2
        exit 1
    fi
done
mkdir -p "$out_dir"
root_dir="$(mktemp -d /tmp/task2-initramfs-root.XXXXXX)"
trap 'rm -rf "$root_dir"' EXIT

gzip -dc "$base_initramfs" | (cd "$root_dir" && cpio -idm --quiet)
install -m 0755 "$task2_binary" "$root_dir/bin/task2-net"
install -m 0755 "$template" "$root_dir/init"

(cd "$root_dir" && find . -print | cpio -o -H newc --quiet) | gzip -n -9 > "$out_file"
sha256sum "$out_file" | awk '{print $1}' > "$out_file.sha256"
printf 'initramfs=%s sha256=%s\n' "$out_file" "$(cat "$out_file.sha256")"
