#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

config="${CONFIG:-$script_dir/build-aarch64-unknown-none-softfloat.toml}"
source "$script_dir/prepare_host_tools.sh"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/build_kernel.sh [extra cargo xtask starry build args]

Stage 1: builds the host-side AArch64 StarryOS seed kernel used to boot the
macOS self-build guest for the first time.

This script wraps:

  cargo xtask starry build -c apps/starry/macos-selfbuild/build-aarch64-unknown-none-softfloat.toml

It does not prepare the rootfs and does not run QEMU. The default full flow
calls it from full_self_build.sh before preparing rootfs inputs.

On macOS, build scripts may expect aarch64-linux-musl-{cc,gcc,ar} while
compiling bare-metal C helpers. If those tools are missing and zig is available,
this script creates local wrappers under target/starry-macos-selfbuild/host-tools.

Environment:
  CONFIG          Build config path
  HOST_TOOLS_DIR  Directory for generated host tool wrappers
  ZIG_CACHE_DIR   Directory for zig local/global caches
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

quote_toml_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "$value"
}

config_with_extra_features() {
    local source_config="$1"
    local extra_csv="$2"
    local output="$HOST_TOOLS_DIR/$(basename "$source_config" .toml)-extra-features.toml"
    local -a extra_features=()
    local feature line in_features=0 inserted=0

    IFS=',' read -r -a extra_features <<<"$extra_csv"
    mkdir -p "$HOST_TOOLS_DIR"
    : >"$output"

    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$in_features" = "1" && "$line" =~ ^[[:space:]]*] ]]; then
            if [[ "$inserted" = "0" ]]; then
                for feature in "${extra_features[@]}"; do
                    feature="${feature#"${feature%%[![:space:]]*}"}"
                    feature="${feature%"${feature##*[![:space:]]}"}"
                    if [[ -n "$feature" ]]; then
                        printf '  %s,\n' "$(quote_toml_string "$feature")" >>"$output"
                    fi
                done
                inserted=1
            fi
            in_features=0
        fi

        printf '%s\n' "$line" >>"$output"

        if [[ "$line" =~ ^[[:space:]]*features[[:space:]]*=.*\[[[:space:]]*$ ]]; then
            in_features=1
        fi
    done <"$source_config"

    if [[ "$inserted" = "0" ]]; then
        printf '\nfeatures = [\n' >>"$output"
        for feature in "${extra_features[@]}"; do
            feature="${feature#"${feature%%[![:space:]]*}"}"
            feature="${feature%"${feature##*[![:space:]]}"}"
            if [[ -n "$feature" ]]; then
                printf '  %s,\n' "$(quote_toml_string "$feature")" >>"$output"
            fi
        done
        printf ']\n' >>"$output"
    fi

    printf '%s\n' "$output"
}

prepare_macos_selfbuild_host_tools

cd "$repo_root"
if [[ -n "${STARRY_KERNEL_EXTRA_FEATURES:-}" ]]; then
    config="$(config_with_extra_features "$config" "$STARRY_KERNEL_EXTRA_FEATURES")"
    echo "using extra Starry kernel features from config: $config"
fi
exec cargo xtask starry build -c "$config" "$@"
