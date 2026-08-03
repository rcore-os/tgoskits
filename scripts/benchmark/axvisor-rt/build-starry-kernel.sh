#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
toolchain=${STARRY_RT_TOOLCHAIN:-nightly-2026-07-15}
config=$script_dir/config/starry-aarch64-rt.toml
output=${STARRY_RT_KERNEL_OUTPUT:-$workspace/tmp/axvisor-rt/starryos-rt.bin}
built_kernel=$workspace/target/aarch64-unknown-linux-musl/release/starryos.bin

for input in "$config"; do
    if [[ ! -r "$input" ]]; then
        echo "required StarryOS RT build input is not readable: $input" >&2
        exit 1
    fi
done
for command_name in cargo install mkdir sha256sum; do
    command -v "$command_name" >/dev/null 2>&1 || {
        echo "required StarryOS RT build command not found: $command_name" >&2
        exit 1
    }
done

cd "$workspace"
cargo "+$toolchain" xtask starry build -c "$config" --smp 2
if [[ ! -s "$built_kernel" ]]; then
    echo "StarryOS RT build did not produce $built_kernel" >&2
    exit 1
fi
mkdir -p "$(dirname -- "$output")"
install -m 0644 "$built_kernel" "$output"
sha256sum "$config" "$output"
echo "AXVISOR_RT_STARRY_KERNEL_READY path=$output feature=rt-irq-trace smp=2"
