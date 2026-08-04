#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
output_dir=$workspace/tmp/competition/ivc/starry
output_image=${1:-$output_dir/starry-rknpu-rootfs-smoke.img}
sysroot=${IVC_RKNN_SYSROOT:-/usr/aarch64-linux-gnu}
cxx=${IVC_RKNN_CXX:-aarch64-linux-gnu-g++}
readelf=${IVC_RKNN_READELF:-aarch64-linux-gnu-readelf}
rknn_python=${IVC_RKNN_PYTHON:-python3}
runner_source=$workspace/competition/ivc/model/thermal_rknn_linux_reference.cpp
corpus_tool=$workspace/competition/ivc/model/thermal_rknn_linux_reference.py
model=$workspace/competition/ivc/model/thermal-4x6x1-v1-rk3588-fp16.rknn
rknn_header=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/include/rknn_api.h
rknn_runtime=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/Linux/aarch64/librknnrt.so
autorun=$script_dir/rknpu-offline-autorun.sh
runner=$output_dir/thermal_rknn_starry_reference
corpus=$output_dir/thermal-rknn-corpus.csv
profile=$output_dir/rknpu-offline-profile

if [[ "$output_image" != /* ]]; then
    output_image=$workspace/$output_image
fi

find_base_image() {
    local candidate
    for candidate in \
        "$workspace/.tgos-images/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img" \
        "$workspace/tmp/axbuild/rootfs/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

for input_path in \
    "$runner_source" \
    "$corpus_tool" \
    "$model" \
    "$rknn_header" \
    "$rknn_runtime" \
    "$autorun"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required StarryOS RKNN rootfs input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in "$cxx" "$readelf" debugfs e2fsck resize2fs sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required StarryOS RKNN rootfs command not found: $command_name" >&2
        exit 1
    fi
done
if ! "$rknn_python" -c 'import numpy' >/dev/null 2>&1; then
    echo "IVC_RKNN_PYTHON cannot import NumPy: $rknn_python" >&2
    exit 1
fi

declare -A runtime_libraries=(
    [ld-linux-aarch64.so.1]="$sysroot/lib/ld-linux-aarch64.so.1"
    [libc.so.6]="$sysroot/lib/libc.so.6"
    [libpthread.so.0]="$sysroot/lib/libpthread.so.0"
    [libdl.so.2]="$sysroot/lib/libdl.so.2"
    [libm.so.6]="$sysroot/lib/libm.so.6"
    [libstdc++.so.6]="$sysroot/lib/libstdc++.so.6"
    [libgcc_s.so.1]="$sysroot/lib/libgcc_s.so.1"
)
for soname in "${!runtime_libraries[@]}"; do
    if [[ ! -r "${runtime_libraries[$soname]}" ]]; then
        echo "Required AArch64 dynamic runtime is missing: $soname" >&2
        exit 1
    fi
done

mkdir -p "$output_dir" "$(dirname -- "$output_image")"
"$rknn_python" "$corpus_tool" prepare --output "$corpus" >/dev/null

cxx_extra_args=()
if [[ -n "${IVC_RKNN_CXX_EXTRA_ARGS:-}" ]]; then
    read -r -a cxx_extra_args <<<"$IVC_RKNN_CXX_EXTRA_ARGS"
fi
"$cxx" "${cxx_extra_args[@]}" \
    -std=c++17 -O2 -Wall -Wextra -Werror \
    -I"$(dirname -- "$rknn_header")" \
    "$runner_source" \
    -L"$(dirname -- "$rknn_runtime")" \
    '-Wl,-rpath,$ORIGIN/lib' \
    -lrknnrt -ldl -lpthread \
    -o "$runner"

if ! "$readelf" -h "$runner" | grep -Fq 'Machine:                           AArch64'; then
    echo "RKNN runner is not an AArch64 ELF: $runner" >&2
    exit 1
fi
if ! "$readelf" -l "$runner" | grep -Fq '/lib/ld-linux-aarch64.so.1'; then
    echo "RKNN runner uses an unexpected ELF interpreter" >&2
    exit 1
fi
if ! "$readelf" -d "$runner" | grep -Eq 'RUNPATH.*\[\$ORIGIN/lib\]'; then
    echo "RKNN runner does not resolve the colocated frozen runtime" >&2
    exit 1
fi

audit_dynamic_dependencies() {
    local elf=$1
    local dependency
    while IFS= read -r dependency; do
        if [[ "$dependency" == librknnrt.so ]]; then
            continue
        fi
        if [[ ! -v "runtime_libraries[$dependency]" ]]; then
            echo "Unexpected AArch64 dynamic dependency in $elf: $dependency" >&2
            return 1
        fi
    done < <(
        "$readelf" -d "$elf" 2>/dev/null \
            | sed -n 's/^.*Shared library: \[\([^]]*\)\].*$/\1/p'
    )
}

audit_dynamic_dependencies "$runner"
audit_dynamic_dependencies "$rknn_runtime"
for soname in "${!runtime_libraries[@]}"; do
    audit_dynamic_dependencies "${runtime_libraries[$soname]}"
done

runner_sha256=$(sha256sum "$runner" | cut -d ' ' -f 1)
model_sha256=$(sha256sum "$model" | cut -d ' ' -f 1)
corpus_sha256=$(sha256sum "$corpus" | cut -d ' ' -f 1)
runtime_sha256=$(sha256sum "$rknn_runtime" | cut -d ' ' -f 1)
printf '%s\n' \
    'schema=1' \
    'vectors=10000' \
    'warmup=32' \
    'core_mask=0' \
    "runner_sha256=$runner_sha256" \
    "model_sha256=$model_sha256" \
    "corpus_sha256=$corpus_sha256" \
    "runtime_sha256=$runtime_sha256" \
    >"$profile"

cd "$workspace"
if ! base_image=$(find_base_image); then
    cargo +nightly-2026-07-15 xtask image pull rootfs-aarch64-busybox.img
    base_image=$(find_base_image) || {
        echo "Managed AArch64 BusyBox rootfs was not found after pull" >&2
        exit 1
    }
fi

cp --reflink=auto --sparse=always "$base_image" "$output_image"
truncate -s "${IVC_STARRY_RKNPU_ROOTFS_SIZE:-96M}" "$output_image"
set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed before resizing $output_image" >&2
    exit "$fsck_status"
fi
resize2fs "$output_image"

for directory in \
    /lib \
    /lib/aarch64-linux-gnu \
    /usr \
    /usr/bin \
    /opt \
    /opt/thermal-rknn \
    /opt/thermal-rknn/lib \
    /var \
    /var/lib \
    /var/lib/rknn; do
    debugfs -w -R "mkdir $directory" "$output_image" >/dev/null 2>&1 || true
done

install_rootfs_file() {
    local source=$1
    local destination=$2
    local mode=$3

    debugfs -w -R "rm $destination" "$output_image" >/dev/null 2>&1 || true
    debugfs -w -R "write $source $destination" "$output_image" >/dev/null
    debugfs -w -R "set_inode_field $destination mode $mode" "$output_image" >/dev/null
}

install_rootfs_file \
    "${runtime_libraries[ld-linux-aarch64.so.1]}" \
    /lib/ld-linux-aarch64.so.1 \
    0100755
for soname in \
    libc.so.6 \
    libpthread.so.0 \
    libdl.so.2 \
    libm.so.6 \
    libstdc++.so.6 \
    libgcc_s.so.1; do
    install_rootfs_file \
        "${runtime_libraries[$soname]}" \
        "/lib/aarch64-linux-gnu/$soname" \
        0100755
done
install_rootfs_file "$rknn_runtime" /opt/thermal-rknn/lib/librknnrt.so 0100755
install_rootfs_file "$runner" /opt/thermal-rknn/thermal_rknn_reference 0100755
install_rootfs_file "$model" \
    /opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn 0100644
install_rootfs_file "$corpus" /opt/thermal-rknn/corpus.csv 0100644
install_rootfs_file "$profile" /etc/rknpu-offline-profile 0100644
install_rootfs_file "$autorun" /usr/bin/starry-run-case-tests 0100755

set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed after populating $output_image" >&2
    exit "$fsck_status"
fi

debugfs -R 'stat /opt/thermal-rknn/thermal_rknn_reference' "$output_image"
debugfs -R 'stat /opt/thermal-rknn/lib/librknnrt.so' "$output_image"
debugfs -R 'stat /usr/bin/starry-run-case-tests' "$output_image"
debugfs -R 'cat /etc/rknpu-offline-profile' "$output_image"
sha256sum "$runner" "$corpus" "$output_image"
echo "STARRY_RKNN_ROOTFS_PASS image=$output_image vectors=10000 backend=rknn-npu"
