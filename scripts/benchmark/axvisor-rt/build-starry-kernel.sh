#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
host_toolchain_preparer=$script_dir/prepare-freestanding-c-toolchain.sh
toolchain=${STARRY_RT_TOOLCHAIN:-nightly-2026-07-15}
config=${STARRY_RT_CONFIG:-$script_dir/config/starry-aarch64-rt.toml}
output=${STARRY_RT_KERNEL_OUTPUT:-$workspace/tmp/axvisor-rt/starryos-rt.bin}

for input in "$config" "$host_toolchain_preparer"; do
    if [[ ! -r "$input" ]]; then
        echo "required StarryOS RT build input is not readable: $input" >&2
        exit 1
    fi
done
for command_name in bash cargo install mkdir rustup sed sha256sum; do
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "required StarryOS RT build command not found: $command_name" >&2
        exit 1
    }
done

bash "$host_toolchain_preparer"
host_toolchain_environment=${STARRY_RT_HOST_TOOLCHAIN_ENV:-$workspace/tmp/axvisor-rt/host-toolchain.env}
[[ -r "$host_toolchain_environment" ]] || {
    echo "StarryOS RT host toolchain environment is missing: $host_toolchain_environment" >&2
    exit 1
}
# shellcheck disable=SC1090
source "$host_toolchain_environment"

target=$(sed -n \
    's/^[[:space:]]*target[[:space:]]*=[[:space:]]*"\([A-Za-z0-9_.-]*\)"[[:space:]]*$/\1/p' \
    "$config")
if [[ -z "$target" || "$target" == *$'\n'* ]]; then
    echo "StarryOS RT build config must declare exactly one target: $config" >&2
    exit 1
fi
built_elf=$workspace/target/$target/release/starryos
built_kernel=$workspace/target/$target/release/starryos.bin

cd "$workspace"
cargo "+$toolchain" xtask starry build -c "$config" --smp 2
if [[ ! -s "$built_elf" ]]; then
    echo "StarryOS RT build did not produce $built_elf" >&2
    exit 1
fi
rustup run "$toolchain" rust-objcopy --strip-all -O binary "$built_elf" "$built_kernel"
if [[ ! -s "$built_kernel" ]]; then
    echo "StarryOS RT build did not produce $built_kernel" >&2
    exit 1
fi
mkdir -p "$(dirname -- "$output")"
install -m 0644 "$built_kernel" "$output"
sha256sum "$config" "$output"
echo "AXVISOR_RT_STARRY_KERNEL_READY path=$output config=$(basename -- "$config") smp=2"
