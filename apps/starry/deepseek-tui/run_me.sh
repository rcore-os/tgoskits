#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Orchestrate building, running, and testing DeepSeek TUI on StarryOS.

Options:
  --build               Build deepseek assets only
  --rootfs              Prepare offline rootfs (builds assets if needed)
  --smoke               Run offline smoke test (build + rootfs + qemu)
  --test                Run online C prime test (build + rootfs + qemu)
  --shell               Boot interactive QEMU shell
  --api-key KEY         DeepSeek API key (for online rootfs/test)
  --proxy URL           Proxy URL (for online rootfs/test)
  -h, --help            Show this help

Examples:
  $(basename "$0") --build
  $(basename "$0") --smoke
  $(basename "$0") --test --api-key sk-xxx --proxy http://10.0.2.2:7890
  $(basename "$0") --shell
EOF
}

build() {
    echo "=== Build deepseek assets ==="
    bash "$script_dir/prepare_deepseek_assets.sh"
}

rootfs_offline() {
    echo "=== Prepare offline rootfs ==="
    bash "$script_dir/prepare_deepseek_rootfs.sh"
}

rootfs_online() {
    echo "=== Prepare online rootfs ==="
    local args=()
    args+=(--output-rootfs "tmp/axbuild/rootfs/rootfs-x86_64-deepseek-online.img")
    if [[ -n "$api_key" ]]; then
        args+=(--api-key "$api_key")
    fi
    if [[ -n "$proxy" ]]; then
        args+=(--proxy "$proxy")
    fi
    bash "$script_dir/prepare_deepseek_rootfs.sh" "${args[@]}"
}

smoke() {
    build
    rootfs_offline
    echo "=== Run offline smoke test ==="
    cd "$workspace" && cargo xtask starry qemu \
        --arch x86_64 \
        --qemu-config apps/starry/deepseek-tui/qemu-x86_64.toml \
        --rootfs tmp/axbuild/rootfs/rootfs-x86_64-deepseek.img
}

test_online() {
    if [[ -z "$api_key" ]]; then
        echo "Error: --api-key is required for --test" >&2
        exit 1
    fi
    build
    rootfs_online
    echo "=== Run online C prime test ==="
    cd "$workspace" && cargo xtask starry qemu \
        --arch x86_64 \
        --qemu-config apps/starry/deepseek-tui/qemu-x86_64-deepseek-prime-test.toml \
        --rootfs tmp/axbuild/rootfs/rootfs-x86_64-deepseek-online.img
}

shell() {
    build
    if [[ -n "$api_key" ]]; then
        rootfs_online
        local rootfs="tmp/axbuild/rootfs/rootfs-x86_64-deepseek-online.img"
    else
        rootfs_offline
        local rootfs="tmp/axbuild/rootfs/rootfs-x86_64-deepseek.img"
    fi
    echo "=== Interactive QEMU shell ==="
    cd "$workspace" && cargo xtask starry qemu \
        --arch x86_64 \
        --rootfs "$rootfs"
}

api_key=""
proxy=""
mode=""

if [[ $# -eq 0 ]]; then
    usage
    exit 0
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --build)       mode="build" ;;
        --rootfs)      mode="rootfs" ;;
        --smoke)       mode="smoke" ;;
        --test)        mode="test" ;;
        --shell)       mode="shell" ;;
        --api-key)     api_key="$2"; shift ;;
        --proxy)       proxy="$2"; shift ;;
        -h|--help)     usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift
done

case "$mode" in
    build)   build ;;
    rootfs)
        if [[ -n "$api_key" ]]; then
            rootfs_online
        else
            rootfs_offline
        fi
        ;;
    smoke)   smoke ;;
    test)    test_online ;;
    shell)   shell ;;
    *)       usage; exit 1 ;;
esac
