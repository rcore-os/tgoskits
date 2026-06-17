#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
rootfs="${STARRY_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
arch="${STARRY_ARCH:-}"
apk_cache="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}/target/net-bench-apk-cache"

require_env() { [[ -n "$2" ]] || { echo "error: $1 is required" >&2; exit 1; }; }

qemu_runner_candidates() {
    case "$arch" in
        aarch64)    printf '%s\n' qemu-aarch64-static qemu-aarch64 ;;
        riscv64)    printf '%s\n' qemu-riscv64-static qemu-riscv64 ;;
        x86_64)     printf '%s\n' qemu-x86_64-static qemu-x86_64 ;;
        loongarch64) printf '%s\n' qemu-loongarch64-static qemu-loongarch64 ;;
        *) echo "error: unsupported arch: $arch" >&2; exit 1 ;;
    esac
}

find_qemu_runner() {
    local c; while IFS= read -r c; do command -v "$c" >/dev/null 2>&1 && { command -v "$c"; return; }; done < <(qemu_runner_candidates)
    echo "error: no qemu-user runner for $arch" >&2; exit 1
}

run_guest_apk_once() {
    local qemu_runner; qemu_runner="$(find_qemu_runner)"
    [[ -x "$staging_root/sbin/apk" ]] || { echo "error: missing apk in $staging_root" >&2; exit 1; }
    mkdir -p "$apk_cache"
    [[ -f /etc/resolv.conf ]] && cp /etc/resolv.conf "$staging_root/etc/resolv.conf"
    QEMU_LD_PREFIX="$staging_root" LD_LIBRARY_PATH="$staging_root/lib:$staging_root/usr/lib" \
        "$qemu_runner" -L "$staging_root" "$staging_root/sbin/apk" \
            --root "$staging_root" \
            --repositories-file "$staging_root/etc/apk/repositories" \
            --keys-dir "$staging_root/etc/apk/keys" \
            --cache-dir "$apk_cache" \
            --update-cache --timeout 60 --no-interactive --force-no-chroot --scripts=no \
            "$@"
}

run_guest_apk() {
    local attempt; local max=4
    for attempt in $(seq 1 "$max"); do
        run_guest_apk_once "$@" && return
        [[ "$attempt" -eq "$max" ]] && return 1
        echo "apk retry ($attempt/$max)..." >&2; sleep $((attempt * 3))
    done
}

find_library_path() {
    local lib="$1"
    for dir in lib usr/lib usr/local/lib; do
        [[ -e "$staging_root/$dir/$lib" ]] && { printf '/%s/%s\n' "$dir" "$lib"; return; }
    done
    return 1
}

copy_file_to_overlay() {
    local src="$staging_root$1" dst="$overlay_dir$1"
    [[ -e "$src" ]] || { echo "error: missing $1 after apk install" >&2; exit 1; }
    [[ -L "$src" ]] && src="$(readlink -f "$src")"
    install -Dm"$2" "$src" "$dst"
}

copy_runtime_deps() {
    local pending=("$@") seen=" " guest_path library library_path
    while [[ ${#pending[@]} -gt 0 ]]; do
        guest_path="${pending[0]}"; pending=("${pending[@]:1}")
        [[ "$seen" == *" $guest_path "* ]] && continue; seen+="$guest_path "
        while IFS= read -r library; do
            library_path="$(find_library_path "$library")" || continue
            copy_file_to_overlay "$library_path" 0644
            pending+=("$library_path")
        done < <(readelf -d "$staging_root$guest_path" 2>/dev/null | sed -n 's/.*Shared library: \[\(.*\)\].*/\1/p')
    done
}

require_env STARRY_ROOTFS       "$rootfs"
require_env STARRY_STAGING_ROOT "$staging_root"
require_env STARRY_OVERLAY_DIR  "$overlay_dir"
require_env STARRY_ARCH         "$arch"
command -v debugfs  >/dev/null 2>&1 || { echo "error: install e2fsprogs (debugfs)" >&2; exit 1; }
command -v readelf  >/dev/null 2>&1 || { echo "error: install binutils (readelf)" >&2; exit 1; }

debugfs -R "rdump / $staging_root" "$rootfs"
run_guest_apk add iperf3

copy_file_to_overlay /usr/bin/iperf3 0755
copy_runtime_deps /usr/bin/iperf3
install -Dm0755 "$app_dir/net-bench-common.sh" "$overlay_dir/usr/bin/net-bench-common.sh"
install -Dm0755 "$app_dir/net-bench.sh" "$overlay_dir/usr/bin/net-bench.sh"
install -Dm0755 "$app_dir/net-bench-tap.sh" "$overlay_dir/usr/bin/net-bench-tap.sh"
