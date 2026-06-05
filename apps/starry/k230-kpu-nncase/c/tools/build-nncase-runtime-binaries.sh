#!/bin/sh
set -eu

find_storage_root() {
    dir=$1
    while [ "$dir" != "/" ]; do
        if [ -d "$dir/target/official-k230/k230-sdk-src" ]; then
            printf '%s\n' "$dir"
            return 0
        fi
        dir=$(dirname "$dir")
    done
    return 1
}

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CASE_C_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
WORKTREE_ROOT=$(git -C "$CASE_C_DIR" rev-parse --show-toplevel)
if ! STORAGE_ROOT=$(find_storage_root "$WORKTREE_ROOT"); then
    echo "build-nncase-runtime-binaries: missing target/official-k230/k230-sdk-src" >&2
    echo "build-nncase-runtime-binaries: see docs/k230-kpu-nncase-runtime.md for asset preparation" >&2
    exit 1
fi
if [ "$WORKTREE_ROOT" = "$STORAGE_ROOT" ]; then
    REL_WORKTREE=
else
    REL_WORKTREE=${WORKTREE_ROOT#"$STORAGE_ROOT"/}
fi
IMAGE=${K230_SDK_DOCKER_IMAGE:-ghcr.io/kendryte/k230_sdk:latest}
STARRY_DEV_IMAGE=${STARRY_DEV_DOCKER_IMAGE:-starryos-dev:ubuntu-qemu10.2.1}
SYSROOT_VOLUME=${K230_LINUX_MUSL_SYSROOT_VOLUME:-tgoskits-riscv64-linux-musl-cross}
BUILD_DIR=/workspace/target/k230-nncase-runtime/build-sdk
HOST_BUILD_DIR="$STORAGE_ROOT/target/k230-nncase-runtime/build-sdk"
SDK_CXX=/workspace/target/official-k230/k230-sdk-src/toolchain/riscv64-linux-musleabi_for_x86_64-pc-linux-gnu/bin/riscv64-unknown-linux-musl-g++
HOST_ASSET_DIR="$CASE_C_DIR/assets/bin"

docker volume create "$SYSROOT_VOLUME" >/dev/null
docker run --rm \
    -v "$SYSROOT_VOLUME":/dst \
    "$STARRY_DEV_IMAGE" \
    bash -lc "set -eu; if [ ! -e /dst/riscv64-linux-musl/lib/crt1.o ]; then rm -rf /dst/*; cp -a /opt/riscv64-linux-musl-cross/. /dst/; fi; test -e /dst/riscv64-linux-musl/lib/crt1.o"

docker run --rm --platform linux/amd64 \
    -v "$STORAGE_ROOT":/workspace \
    -v "$SYSROOT_VOLUME":/linux-musl-cross:ro \
    -w "/workspace/$REL_WORKTREE" \
    "$IMAGE" \
    bash -lc "set -eu; rm -rf '$BUILD_DIR'; cmake -S apps/starry/k230-kpu-nncase/c -B '$BUILD_DIR' -DK230_CXX='$SDK_CXX' -DK230_LINUX_MUSL_PREFIX=/linux-musl-cross; cmake --build '$BUILD_DIR' -j2; test -x '$BUILD_DIR/kpu-nncase-minimal'; test -x '$BUILD_DIR/k230-yolov8n-demo'"

mkdir -p "$HOST_ASSET_DIR"
install -m 755 "$HOST_BUILD_DIR/kpu-nncase-minimal" "$HOST_ASSET_DIR/kpu-nncase-minimal"
install -m 755 "$HOST_BUILD_DIR/k230-yolov8n-demo" "$HOST_ASSET_DIR/k230-yolov8n-demo"

ls -lh "$CASE_C_DIR/assets/bin"
