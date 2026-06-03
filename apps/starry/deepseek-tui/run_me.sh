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
  --rootfs              Prepare legacy standalone rootfs (builds assets if needed)
  --smoke               Run offline smoke test through starry app qemu
  --test                Run online C prime test through starry app qemu
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

run_app_qemu() {
    local config="$1"
    shift
    cd "$workspace" && "$@" cargo xtask starry app qemu \
        -t deepseek-tui \
        --arch x86_64 \
        --qemu-config "$config"
}

smoke() {
    echo "=== Run offline smoke test ==="
    run_app_qemu apps/starry/deepseek-tui/qemu-x86_64.toml env
}

test_online() {
    if [[ -z "$api_key" ]]; then
        echo "Error: --api-key is required for --test" >&2
        exit 1
    fi
    echo "=== Run online C prime test ==="
    local env_args=(env "DEEPSEEK_API_KEY=$api_key")
    if [[ -n "$proxy" ]]; then
        env_args+=("DEEPSEEK_ONLINE_PROXY=$proxy")
    fi
    run_app_qemu apps/starry/deepseek-tui/qemu-x86_64-deepseek-prime-test.toml "${env_args[@]}"
}

shell() {
    local env_args=(env)
    if [[ -n "$api_key" ]]; then
        env_args+=("DEEPSEEK_API_KEY=$api_key")
    fi
    if [[ -n "$proxy" ]]; then
        env_args+=("DEEPSEEK_ONLINE_PROXY=$proxy")
    fi
    echo "=== Interactive QEMU shell ==="
    run_app_qemu apps/starry/deepseek-tui/qemu-x86_64-shell.toml "${env_args[@]}"
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
