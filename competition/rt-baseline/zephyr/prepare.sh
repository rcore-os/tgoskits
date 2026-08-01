#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
workspace=${WEST_WORKSPACE:-"$repo_root/tmp"}
zephyr_base=${ZEPHYR_BASE:-"$workspace/zephyr-v4.3.0"}
venv=${ZEPHYR_VENV:-"$workspace/zephyr-venv"}
toolchain_bin=${RT_BASELINE_TOOLCHAIN_BIN:-"$workspace/zephyr-toolchain/bin"}
host_cross_compile=${RT_BASELINE_HOST_CROSS_COMPILE:-aarch64-linux-gnu-}
tag_object=981205b3e7cdf9fdf2e9e71b8b6b64fcc71c12a0
source_commit=3568e1b6d5cdd51a6b964a2a1d6d29200fea2056

for command in git cmake ninja python3 qemu-system-aarch64 \
    "${host_cross_compile}ar" "${host_cross_compile}gcc" \
    "${host_cross_compile}ld" "${host_cross_compile}objcopy" \
    "${host_cross_compile}objdump"; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "missing native Zephyr prerequisite: $command" >&2
        exit 2
    fi
done

mkdir -p -- "$workspace"
if [[ ! -x "$venv/bin/python" ]]; then
    python3 -m venv "$venv"
fi
"$venv/bin/python" -m pip install --upgrade pip 'west==1.5.0'

if [[ ! -d "$zephyr_base/.git" ]]; then
    git clone --branch v4.3.0 --depth 1 \
        https://github.com/zephyrproject-rtos/zephyr "$zephyr_base"
fi
if [[ $(git -C "$zephyr_base" rev-parse v4.3.0) != "$tag_object" ]] ||
    [[ $(git -C "$zephyr_base" rev-parse 'v4.3.0^{}') != "$source_commit" ]] ||
    [[ $(git -C "$zephyr_base" describe --tags --exact-match) != v4.3.0 ]] ||
    [[ -n $(git -C "$zephyr_base" status --porcelain=v1 --untracked-files=all) ]]; then
    echo "Zephyr source is not the clean pinned upstream v4.3.0 release" >&2
    exit 2
fi

if [[ ! -f "$workspace/.west/config" ]]; then
    "$venv/bin/west" init -l "$zephyr_base"
fi
"$venv/bin/python" -m pip install -r "$zephyr_base/scripts/requirements.txt"

mkdir -p -- "$toolchain_bin"
install -m 0755 "$script_dir/toolchain/aarch64-zephyr-tool" \
    "$toolchain_bin/aarch64-zephyr-tool"
for tool in ar as gcc ld ld.bfd nm objcopy objdump ranlib readelf strip; do
    ln -sfn aarch64-zephyr-tool "$toolchain_bin/aarch64-zephyr-$tool"
done

echo "native Zephyr baseline environment ready"
echo "  source: $zephyr_base"
echo "  west: $venv/bin/west"
echo "  compiler prefix: $toolchain_bin/aarch64-zephyr-"
