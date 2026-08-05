#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
built_elf=$workspace/target/aarch64-unknown-linux-musl/release/starryos
built_kernel=$workspace/target/aarch64-unknown-linux-musl/release/starryos.bin
kernel=$output_dir/starryos-ort.bin
guest_dtb=$output_dir/starry-orangepi-5-plus-ort.dtb
rootfs=$output_dir/starry-ort-rootfs-smoke.img
ort_exporter=$workspace/competition/ivc/model/export_thermal_ort.py

configure_system_libclang() {
    local libclang

    if [[ -n "${LIBCLANG_PATH:-}" ]]; then
        echo "STARRY_ORT_HOST_LIBCLANG path=$LIBCLANG_PATH source=environment"
        return
    fi
    for libclang in /usr/lib/llvm-*/lib/libclang.so*; do
        if [[ -r "$libclang" ]]; then
            LIBCLANG_PATH=${libclang%/*}
            export LIBCLANG_PATH
            echo "STARRY_ORT_HOST_LIBCLANG path=$LIBCLANG_PATH source=system"
            return
        fi
    done
}

resolve_ort_python() {
    local candidate

    if [[ -n "${IVC_ORT_PYTHON:-}" ]]; then
        echo "STARRY_ORT_HOST_PYTHON path=$IVC_ORT_PYTHON source=environment"
        return
    fi
    candidate=${XDG_CACHE_HOME:-$HOME/.cache}/tgoskits/ivc-ort-py312/bin/python
    if [[ -x "$candidate" ]]; then
        IVC_ORT_PYTHON=$candidate
        export IVC_ORT_PYTHON
        echo "STARRY_ORT_HOST_PYTHON path=$IVC_ORT_PYTHON source=managed-cache"
        return
    fi
    echo "Set IVC_ORT_PYTHON to the locked CPython 3.12 ORT environment" >&2
    exit 1
}

for command_name in cargo dtc rustup sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required StarryOS ORT artifact command not found: $command_name" >&2
        exit 1
    fi
done

configure_system_libclang
resolve_ort_python
"$IVC_ORT_PYTHON" "$ort_exporter" --check

cd "$workspace"
cargo "+$toolchain" xtask starry build \
    -c competition/ivc/config/starry-aarch64.toml \
    --smp 2
if [[ ! -s "$built_elf" ]]; then
    echo "StarryOS ORT build did not produce $built_elf" >&2
    exit 1
fi
rustup run "$toolchain" llvm-objcopy --strip-all -O binary \
    "$built_elf" "$built_kernel"
if [[ ! -s "$built_kernel" ]]; then
    echo "StarryOS ORT objcopy did not produce $built_kernel" >&2
    exit 1
fi

mkdir -p "$output_dir"
install -m 0644 "$built_kernel" "$kernel"
dtc -I dts -O dtb -o "$guest_dtb" \
    "$script_dir/orangepi-5-plus.dts"
dtc -I dtb -O dts -o /dev/null "$guest_dtb"
bash "$script_dir/build-ort-rootfs.sh" "$rootfs"

sha256sum "$kernel" "$guest_dtb" "$rootfs"
echo "STARRY_ORT_ARTIFACTS_PASS output_dir=$output_dir"
