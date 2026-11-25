#!/bin/sh
set -eu

: "${AXBUILD_TARGET_DIR:?host_prepare did not provide AXBUILD_TARGET_DIR}"

case_dir=test-suit/axvisor/qemu/aarch64/ivc-starry
artifact_dir="$AXBUILD_TARGET_DIR/axbuild/axvisor/qemu/aarch64/ivc-starry"
guest_bin="$AXBUILD_TARGET_DIR/aarch64-unknown-none-softfloat/release/starryos.bin"
archive="$artifact_dir/initramfs-aarch64.cpio.gz"

mkdir -p "$artifact_dir"

# Build the guest from this checkout. The host_prepare sidecar is invoked before
# the outer Axvisor build, so this child never races the outer Cargo build.
CARGO_TARGET_DIR="$AXBUILD_TARGET_DIR" \
    cargo xtask starry build --config "$case_dir/guest-build.toml" --smp 1

if [ ! -s "$guest_bin" ]; then
    echo "missing Starry AArch64 flat binary: $guest_bin" >&2
    exit 1
fi

work_dir=$(mktemp -d "$artifact_dir/ivc-starry.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

init_object="$work_dir/ivc-starry.o"
init_binary="$work_dir/init"
stage="$work_dir/initramfs"
archive_tmp="$work_dir/initramfs-aarch64.cpio.gz"

mkdir -p "$stage/dev"

clang \
    --target=aarch64-unknown-linux-musl \
    -std=gnu11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    -ffreestanding \
    -fno-builtin \
    -fno-builtin-memcpy \
    -fno-builtin-memset \
    -fno-builtin-memcmp \
    -fno-stack-protector \
    -fno-pic \
    -fno-pie \
    -fno-asynchronous-unwind-tables \
    -fno-unwind-tables \
    -nostdlib \
    -c "$case_dir/client/ivc-starry.c" \
    -o "$init_object"

rust-lld \
    -flavor gnu \
    -static \
    -e _start \
    "$init_object" \
    -o "$init_binary"

cp "$init_binary" "$stage/init"
chmod 0755 "$stage/init"

cargo xtask image pack-initramfs "$stage" "$archive_tmp" --gzip
mv "$archive_tmp" "$archive"
