#!/bin/sh
set -eu

if [ -z "${STARRY_STAGING_ROOT:-}" ]; then
    apk add binutils gcc musl-dev
    exit 0
fi

case "$STARRY_STAGING_ROOT" in
    /*) root=$STARRY_STAGING_ROOT ;;
    *) root=$(pwd -P)/$STARRY_STAGING_ROOT ;;
esac
run_dir=${root%/staging-root}
case_dir=${run_dir%/runs/*}
apk_cache_dir=$case_dir/cache/apk-cache

find_qemu_runner() {
    for candidate in "$@"; do
        if [ -z "$candidate" ]; then
            continue
        fi
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return 0
        fi
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

case "$root" in
    *aarch64*)
        qemu_runner=$(find_qemu_runner \
            /usr/bin/qemu-aarch64-static \
            qemu-aarch64-static \
            qemu-aarch64)
        ;;
    *x86_64*)
        qemu_runner=$(find_qemu_runner \
            /usr/bin/qemu-x86_64-static \
            qemu-x86_64-static \
            qemu-x86_64)
        ;;
    *riscv64*)
        qemu_runner=$(find_qemu_runner \
            /usr/bin/qemu-riscv64-static \
            qemu-riscv64-static \
            qemu-riscv64)
        ;;
    *loongarch64*)
        qemu_runner=$(find_qemu_runner \
            /usr/bin/qemu-loongarch64-static \
            /usr/local/bin/qemu-loongarch64 \
            qemu-loongarch64-static \
            qemu-loongarch64)
        ;;
    *)
        echo "unsupported staging root target: $root" >&2
        exit 1
        ;;
esac

"$qemu_runner" -L "$root" "$root/sbin/apk" \
    --root="$root" \
    --repositories-file="$root/etc/apk/repositories" \
    --keys-dir="$root/etc/apk/keys" \
    --cache-dir="$apk_cache_dir" \
    --update-cache \
    --timeout=60 \
    --no-interactive \
    --force-no-chroot \
    add --scripts=no binutils gcc musl-dev
