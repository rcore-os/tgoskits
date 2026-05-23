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

case "$root" in
    *aarch64*) qemu_runner=/usr/bin/qemu-aarch64-static ;;
    *x86_64*) qemu_runner=/usr/bin/qemu-x86_64-static ;;
    *riscv64*) qemu_runner=/usr/bin/qemu-riscv64-static ;;
    *loongarch64*) qemu_runner=/usr/bin/qemu-loongarch64-static ;;
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
