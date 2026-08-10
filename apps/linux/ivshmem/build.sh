#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
arch="${AXVISOR_IVSHMEM_ARCH:-aarch64}"
out_dir="${AXVISOR_IVSHMEM_OUT_DIR:-${script_dir}/build/out-${arch}}"

pick_cross_prefix() {
    local preferred="$1"
    local fallback="$2"
    local native="${3:-__none__}"

    if command -v "${preferred}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${preferred}"
    elif command -v "${fallback}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${fallback}"
    elif [[ "${native}" != "__none__" ]] && command -v "${native}gcc" >/dev/null 2>&1; then
        printf '%s\n' "${native}"
    else
        echo "no usable compiler found: tried ${preferred}gcc, ${fallback}gcc and ${native}gcc" >&2
        return 1
    fi
}

case "${arch}" in
    aarch64)
        cross="$(pick_cross_prefix "${AARCH64_MUSL_CROSS:-aarch64-linux-musl-}" "${AARCH64_CROSS_COMPILE:-aarch64-linux-gnu-}")"
        ;;
    x86_64)
        cross="$(pick_cross_prefix "${X86_64_MUSL_CROSS:-x86_64-linux-musl-}" "${X86_64_GNU_CROSS:-x86_64-linux-gnu-}" "")"
        ;;
    *)
        echo "unsupported AXVISOR_IVSHMEM_ARCH: ${arch}" >&2
        exit 2
        ;;
esac

mkdir -p "${out_dir}"
"${cross}gcc" \
    -Wall -Wextra -Os -s -Wl,--gc-sections -static -no-pie \
    -o "${out_dir}/ivshmem-bar2-smoke" \
    "${script_dir}/bar2_smoke/main.c"

echo "AXVISOR_IVSHMEM_OUT_DIR=${out_dir}"
