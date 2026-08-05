#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
output_dir=$workspace/tmp/competition/ivc/starry
output_image=${1:-$output_dir/starry-ort-rootfs-smoke.img}
sysroot=${IVC_ORT_SYSROOT:-/usr/aarch64-linux-gnu}
cxx=${IVC_ORT_CXX:-aarch64-linux-gnu-g++}
readelf=${IVC_ORT_READELF:-aarch64-linux-gnu-readelf}
corpus_python=${IVC_ORT_PYTHON:-python3}
runner_source=$workspace/competition/ivc/model/thermal_ort_starry_reference.cpp
corpus_tool=$workspace/competition/ivc/model/thermal_rknn_linux_reference.py
model=$workspace/competition/ivc/model/thermal-4x6x1-v1.ort
autorun=$script_dir/ort-offline-autorun.sh
runner=$output_dir/thermal_ort_starry_reference
corpus=$output_dir/thermal-ort-corpus.csv
profile=$output_dir/ort-offline-profile
archive_name=onnxruntime-linux-aarch64-1.25.0.tgz
archive_url=https://github.com/microsoft/onnxruntime/releases/download/v1.25.0/$archive_name
archive_sha256=849c04634e76446bbe0a92f67955a9641415c37f11930804066057bf9eadbd03
runtime_sha256=e03801f70263a028207491471f09a17ed6a62b146568edada797483f8f8ec8d3
provider_shared_sha256=3b6be288fbfb7dff8770d08a23defdde18e8f7e0f5a2b344a0e5e238c999ea88
cache_root=${IVC_ORT_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tgoskits/onnxruntime/1.25.0}
archive=${IVC_ORT_AARCH64_ARCHIVE:-$cache_root/$archive_name}
extraction=

if [[ "$output_image" != /* ]]; then
    output_image=$workspace/$output_image
fi

cleanup() {
    case "$extraction" in
        "$output_dir"/ort-extract.*)
            if [[ -d "$extraction" ]]; then
                rm -rf -- "$extraction"
            fi
            ;;
        '') ;;
        *)
            echo "Refusing to remove unexpected ORT extraction path: $extraction" >&2
            ;;
    esac
}
trap cleanup EXIT HUP INT TERM

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

for input_path in "$runner_source" "$corpus_tool" "$model" "$autorun"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required StarryOS ORT rootfs input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in \
    "$cxx" "$readelf" curl debugfs e2fsck resize2fs sha256sum tar; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required StarryOS ORT rootfs command not found: $command_name" >&2
        exit 1
    fi
done
if ! "$corpus_python" -c 'import numpy' >/dev/null 2>&1; then
    echo "IVC_ORT_PYTHON cannot import NumPy: $corpus_python" >&2
    exit 1
fi

mkdir -p "$cache_root"
if [[ ! -f "$archive" ]]; then
    archive_partial=$archive.partial.$$
    curl -fL --retry 5 --retry-all-errors --retry-delay 2 \
        -o "$archive_partial" "$archive_url"
    actual_archive_sha256=$(sha256sum "$archive_partial" | cut -d ' ' -f 1)
    if [[ "$actual_archive_sha256" != "$archive_sha256" ]]; then
        echo "Downloaded ONNX Runtime archive SHA-256 differs" >&2
        exit 1
    fi
    mv -- "$archive_partial" "$archive"
fi
actual_archive_sha256=$(sha256sum "$archive" | cut -d ' ' -f 1)
if [[ "$actual_archive_sha256" != "$archive_sha256" ]]; then
    echo "Cached ONNX Runtime archive SHA-256 differs: $archive" >&2
    exit 1
fi
mkdir -p "$output_dir"
extraction=$(mktemp -d "$output_dir/ort-extract.XXXXXX")
tar -xzf "$archive" -C "$extraction"
package_root=$extraction/onnxruntime-linux-aarch64-1.25.0
ort_include=$package_root/include
ort_lib=$package_root/lib
ort_runtime=$ort_lib/libonnxruntime.so.1.25.0
ort_provider_shared=$ort_lib/libonnxruntime_providers_shared.so
if [[ $(sha256sum "$ort_runtime" | cut -d ' ' -f 1) != "$runtime_sha256" ]]; then
    echo "Extracted libonnxruntime SHA-256 differs" >&2
    exit 1
fi
if [[ $(sha256sum "$ort_provider_shared" | cut -d ' ' -f 1) != "$provider_shared_sha256" ]]; then
    echo "Extracted provider shared library SHA-256 differs" >&2
    exit 1
fi

declare -A runtime_libraries=(
    [ld-linux-aarch64.so.1]="$sysroot/lib/ld-linux-aarch64.so.1"
    [libc.so.6]="$sysroot/lib/libc.so.6"
    [libpthread.so.0]="$sysroot/lib/libpthread.so.0"
    [libdl.so.2]="$sysroot/lib/libdl.so.2"
    [librt.so.1]="$sysroot/lib/librt.so.1"
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
"$corpus_python" "$corpus_tool" prepare --output "$corpus" >/dev/null
"$cxx" \
    -std=c++17 -O2 -Wall -Wextra -Werror \
    -I"$ort_include" \
    "$runner_source" \
    -L"$ort_lib" \
    '-Wl,-rpath,$ORIGIN/lib' \
    -lonnxruntime -ldl -lpthread \
    -o "$runner"

if ! "$readelf" -h "$runner" | grep -Fq 'Machine:                           AArch64'; then
    echo "ORT runner is not an AArch64 ELF: $runner" >&2
    exit 1
fi
if ! "$readelf" -l "$runner" | grep -Fq '/lib/ld-linux-aarch64.so.1'; then
    echo "ORT runner uses an unexpected ELF interpreter" >&2
    exit 1
fi
if ! "$readelf" -d "$runner" | grep -Eq 'RUNPATH.*\[\$ORIGIN/lib\]'; then
    echo "ORT runner does not resolve the colocated frozen runtime" >&2
    exit 1
fi

audit_dynamic_dependencies() {
    local elf=$1
    local dependency
    while IFS= read -r dependency; do
        if [[ "$dependency" == libonnxruntime.so.1 ]]; then
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
audit_dynamic_dependencies "$ort_runtime"
audit_dynamic_dependencies "$ort_provider_shared"
for soname in "${!runtime_libraries[@]}"; do
    audit_dynamic_dependencies "${runtime_libraries[$soname]}"
done

runner_sha256=$(sha256sum "$runner" | cut -d ' ' -f 1)
model_sha256=$(sha256sum "$model" | cut -d ' ' -f 1)
corpus_sha256=$(sha256sum "$corpus" | cut -d ' ' -f 1)
printf '%s\n' \
    'schema=1' \
    'vectors=10000' \
    'warmup=32' \
    'lifecycle_cycles=5' \
    'runtime_version=1.25.0' \
    'maximum_post_destroy_growth_kib=16384' \
    'maximum_peak_rss_kib=131072' \
    'minimum_rootfs_available_percent_x100=2000' \
    "runner_sha256=$runner_sha256" \
    "model_sha256=$model_sha256" \
    "corpus_sha256=$corpus_sha256" \
    "runtime_sha256=$runtime_sha256" \
    "provider_shared_sha256=$provider_shared_sha256" \
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
truncate -s "${IVC_STARRY_ORT_ROOTFS_SIZE:-160M}" "$output_image"
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
    /opt/thermal-ort \
    /opt/thermal-ort/lib \
    /var \
    /var/lib \
    /var/lib/ort; do
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
    librt.so.1 \
    libm.so.6 \
    libstdc++.so.6 \
    libgcc_s.so.1; do
    install_rootfs_file \
        "${runtime_libraries[$soname]}" \
        "/lib/aarch64-linux-gnu/$soname" \
        0100755
done
install_rootfs_file "$ort_runtime" /opt/thermal-ort/lib/libonnxruntime.so.1 0100755
install_rootfs_file \
    "$ort_provider_shared" \
    /opt/thermal-ort/lib/libonnxruntime_providers_shared.so \
    0100755
install_rootfs_file "$runner" /opt/thermal-ort/thermal_ort_reference 0100755
install_rootfs_file "$model" /opt/thermal-ort/thermal-4x6x1-v1.ort 0100644
install_rootfs_file "$corpus" /opt/thermal-ort/corpus.csv 0100644
install_rootfs_file "$profile" /etc/ort-offline-profile 0100644
install_rootfs_file "$autorun" /usr/bin/starry-run-case-tests 0100755

set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed after populating $output_image" >&2
    exit "$fsck_status"
fi

debugfs -R 'stat /opt/thermal-ort/thermal_ort_reference' "$output_image"
debugfs -R 'stat /opt/thermal-ort/lib/libonnxruntime.so.1' "$output_image"
debugfs -R 'stat /usr/bin/starry-run-case-tests' "$output_image"
debugfs -R 'cat /etc/ort-offline-profile' "$output_image"
sha256sum "$runner" "$corpus" "$output_image"
echo "STARRY_ORT_ROOTFS_PASS image=$output_image vectors=10000 backend=onnxruntime-cpu"
