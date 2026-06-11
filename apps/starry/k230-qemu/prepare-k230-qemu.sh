#!/usr/bin/env bash
set -euo pipefail

QEMU_REPO_URL="${QEMU_REPO_URL:-https://github.com/zevorn/qemu.git}"
QEMU_REF="${QEMU_REF:-chao-k230-dev}"
QEMU_COMMIT="${QEMU_COMMIT:-539bd413497ccac9d3cf878036210e64830e7fd6}"

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(git -C "$script_dir" rev-parse --show-toplevel)"

qemu_source="${QEMU_SOURCE_DIR:-$workspace/target/qemu-k230-source}"
qemu_build="${QEMU_BUILD_DIR:-$workspace/target/qemu-k230-docker-build}"
jobs="${QEMU_JOBS:-}"

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "prepare-k230-qemu: run this script inside the Docker/Linux test environment" >&2
    exit 1
fi

if [[ -z "$jobs" ]]; then
    jobs="$(nproc 2>/dev/null || echo 4)"
fi

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "prepare-k230-qemu: missing required command: $1" >&2
        exit 1
    fi
}

need_cmd git
need_cmd make
need_cmd python3
need_cmd pkg-config

if ! command -v ninja >/dev/null 2>&1 && ! command -v ninja-build >/dev/null 2>&1; then
    echo "prepare-k230-qemu: missing required command: ninja or ninja-build" >&2
    exit 1
fi

if ! pkg-config --exists glib-2.0 pixman-1; then
    echo "prepare-k230-qemu: missing glib-2.0 or pixman-1 development package" >&2
    exit 1
fi

mkdir -p "$(dirname "$qemu_source")"
if [[ ! -d "$qemu_source/.git" ]]; then
    echo "prepare-k230-qemu: cloning $QEMU_REPO_URL into $qemu_source"
    git clone "$QEMU_REPO_URL" "$qemu_source"
fi

if [[ -n "$QEMU_COMMIT" ]]; then
    if ! git -C "$qemu_source" cat-file -e "$QEMU_COMMIT^{commit}" 2>/dev/null; then
        echo "prepare-k230-qemu: fetching $QEMU_REF"
        git -C "$qemu_source" fetch origin "$QEMU_REF"
    fi
    echo "prepare-k230-qemu: checking out pinned commit $QEMU_COMMIT"
    git -C "$qemu_source" checkout --detach "$QEMU_COMMIT"
else
    echo "prepare-k230-qemu: fetching $QEMU_REF"
    git -C "$qemu_source" fetch origin "$QEMU_REF"
    echo "prepare-k230-qemu: checking out origin/$QEMU_REF"
    git -C "$qemu_source" checkout --detach "origin/$QEMU_REF"
fi

mkdir -p "$qemu_build"
if [[ ! -f "$qemu_build/build.ninja" ]]; then
    echo "prepare-k230-qemu: configuring build directory $qemu_build"
    (
        cd "$qemu_build"
        "$qemu_source/configure" \
            --target-list=riscv64-softmmu \
            --disable-werror \
            --disable-docs \
            --disable-gtk \
            --disable-sdl \
            --disable-vnc \
            --disable-curses
    )
fi

echo "prepare-k230-qemu: building qemu-system-riscv64 with $jobs job(s)"
make -C "$qemu_build" -j"$jobs" qemu-system-riscv64

qemu_bin="$qemu_build/qemu-system-riscv64"
if [[ ! -x "$qemu_bin" ]]; then
    echo "prepare-k230-qemu: expected QEMU binary not found: $qemu_bin" >&2
    exit 1
fi

if ! "$qemu_bin" -machine help | grep -Eq '(^|[[:space:]])k230([[:space:]]|$)'; then
    echo "prepare-k230-qemu: built QEMU does not list the k230 machine" >&2
    exit 1
fi

echo "prepare-k230-qemu: K230 QEMU is ready"
echo "prepare-k230-qemu: binary: $qemu_bin"
echo "prepare-k230-qemu: pc-bios: $qemu_build/pc-bios"
echo "prepare-k230-qemu: run tests with:"
echo "  PATH=\"$qemu_build:\$PATH\" cargo xtask starry app qemu -t k230-qemu/qemu-k230/kpu-smoke --arch riscv64"
