#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
suite_dir="$overlay_dir/usr/share/starry-test-suit/syscall"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/sh/syscall.sh" "$overlay_dir/usr/bin/syscall-test"
install -Dm0755 "$app_dir/sh/syscall.sh" "$overlay_dir/usr/bin/starry-test-suit/syscall"
install -Dm0644 "$app_dir/TODO.txt" "$suite_dir/TODO.txt"

for syscall_file in "$app_dir"/syscalls/*.txt; do
    install -Dm0644 "$syscall_file" "$suite_dir/syscalls/$(basename "$syscall_file")"
done

if [[ -f "$app_dir/RUN.txt" ]]; then
    install -Dm0644 "$app_dir/RUN.txt" "$suite_dir/RUN.txt"
fi
