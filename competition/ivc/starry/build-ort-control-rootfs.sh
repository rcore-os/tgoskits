#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
profile=smoke
output_image=
sysroot=${IVC_ORT_SYSROOT:-/usr/aarch64-linux-gnu}
cxx=${IVC_ORT_CXX:-aarch64-linux-gnu-g++}
ar=${IVC_ORT_AR:-aarch64-linux-gnu-ar}
readelf=${IVC_ORT_READELF:-aarch64-linux-gnu-readelf}
model=$workspace/competition/ivc/model/thermal-4x6x1-v1.ort
model_sha256=3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887
bridge_source=$workspace/tools/ivcproto/csrc/ort_bridge.cpp
bridge_dir=$output_dir/ort-control-bridge
bridge_object=$bridge_dir/ort_bridge.o
bridge_archive=$bridge_dir/libivc_ort_bridge.a
target_dir=$output_dir/ort-control-target
controller=$target_dir/aarch64-unknown-linux-gnu/release/ivcproto
archive_name=onnxruntime-linux-aarch64-1.25.0.tgz
archive_url=https://github.com/microsoft/onnxruntime/releases/download/v1.25.0/$archive_name
archive_sha256=849c04634e76446bbe0a92f67955a9641415c37f11930804066057bf9eadbd03
runtime_sha256=e03801f70263a028207491471f09a17ed6a62b146568edada797483f8f8ec8d3
provider_shared_sha256=3b6be288fbfb7dff8770d08a23defdde18e8f7e0f5a2b344a0e5e238c999ea88
c_api_header_sha256=4763b298a1ab8b5df88c3949b560c1e00a3605e61995d2d3243fd8007679d585
cxx_api_header_sha256=e713be1d2b5a11a0750e6a581c11c1d0d589324084fe7bf8b09064eba3835b9d
cache_root=${IVC_ORT_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tgoskits/onnxruntime/1.25.0}
archive=${IVC_ORT_AARCH64_ARCHIVE:-$cache_root/$archive_name}
extraction=
profile_file=
archive_partial=

usage() {
    cat <<EOF
Usage: $0 [smoke|full] [--profile smoke|full] [--output IMAGE]

Builds the StarryOS IVC controller with the frozen ONNX Runtime 1.25.0 CPU
backend and populates a glibc-capable Orange Pi 5 Plus control rootfs.
EOF
}

cleanup() {
    if [[ -n "$profile_file" ]]; then
        rm -f -- "$profile_file"
    fi
    if [[ -n "$archive_partial" ]]; then
        case "$archive_partial" in
            "$cache_root"/$archive_name.partial.*) rm -f -- "$archive_partial" ;;
            *) echo "Refusing to remove unexpected ORT partial archive: $archive_partial" >&2 ;;
        esac
    fi
    case "$extraction" in
        "$output_dir"/ort-control-extract.*)
            if [[ -d "$extraction" ]]; then
                rm -rf -- "$extraction"
            fi
            ;;
        '') ;;
        *) echo "Refusing to remove unexpected ORT extraction path: $extraction" >&2 ;;
    esac
}
trap cleanup EXIT HUP INT TERM

if (($# > 0)) && [[ "$1" != -* ]]; then
    profile=$1
    shift
fi
while (($# > 0)); do
    case "$1" in
        --profile)
            profile=${2:?--profile requires a value}
            shift 2
            ;;
        --output)
            output_image=${2:?--output requires a value}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown ORT control rootfs option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$profile" in
    smoke)
        command_count=20
        output_image=${output_image:-$output_dir/starry-ivc-rootfs-ort-control-smoke.img}
        ;;
    full)
        command_count=1800
        output_image=${output_image:-$output_dir/starry-ivc-rootfs-ort-control.img}
        ;;
    *)
        echo "ORT control profile must be smoke or full: $profile" >&2
        exit 2
        ;;
esac
if [[ "$output_image" != /* ]]; then
    output_image=$workspace/$output_image
fi

for input_path in "$model" "$bridge_source" "$script_dir/autorun.sh"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required ORT control rootfs input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in \
    "$cxx" "$ar" "$readelf" cargo curl cut debugfs e2fsck grep mktemp \
    resize2fs rustup sed sha256sum tar truncate; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required ORT control rootfs command not found: $command_name" >&2
        exit 1
    fi
done

actual_model_sha256=$(sha256sum "$model" | cut -d ' ' -f 1)
if [[ "$actual_model_sha256" != "$model_sha256" ]]; then
    echo "Frozen ORT model SHA-256 differs: $model" >&2
    exit 1
fi

mkdir -p "$cache_root" "$output_dir" "$bridge_dir" "$target_dir" \
    "$(dirname -- "$output_image")"
if [[ ! -f "$archive" ]]; then
    archive_partial=$archive.partial.$$
    curl -fL --retry 5 --retry-all-errors --retry-delay 2 \
        -o "$archive_partial" "$archive_url"
    downloaded_sha256=$(sha256sum "$archive_partial" | cut -d ' ' -f 1)
    if [[ "$downloaded_sha256" != "$archive_sha256" ]]; then
        echo "Downloaded ONNX Runtime archive SHA-256 differs" >&2
        exit 1
    fi
    mv -- "$archive_partial" "$archive"
    archive_partial=
fi
actual_archive_sha256=$(sha256sum "$archive" | cut -d ' ' -f 1)
if [[ "$actual_archive_sha256" != "$archive_sha256" ]]; then
    echo "Cached ONNX Runtime archive SHA-256 differs: $archive" >&2
    exit 1
fi

extraction=$(mktemp -d "$output_dir/ort-control-extract.XXXXXX")
tar -xzf "$archive" -C "$extraction"
package_root=$extraction/onnxruntime-linux-aarch64-1.25.0
ort_include=$package_root/include
ort_lib=$package_root/lib
ort_runtime=$ort_lib/libonnxruntime.so.1.25.0
ort_provider_shared=$ort_lib/libonnxruntime_providers_shared.so
ort_c_api_header=$ort_include/onnxruntime_c_api.h
ort_cxx_api_header=$ort_include/onnxruntime_cxx_api.h
declare -A frozen_files=(
    ["$ort_runtime"]=$runtime_sha256
    ["$ort_provider_shared"]=$provider_shared_sha256
    ["$ort_c_api_header"]=$c_api_header_sha256
    ["$ort_cxx_api_header"]=$cxx_api_header_sha256
)
for frozen_file in "${!frozen_files[@]}"; do
    actual_sha256=$(sha256sum "$frozen_file" | cut -d ' ' -f 1)
    if [[ "$actual_sha256" != "${frozen_files[$frozen_file]}" ]]; then
        echo "Extracted ONNX Runtime file SHA-256 differs: $frozen_file" >&2
        exit 1
    fi
done

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

"$script_dir/build-rootfs.sh" \
    --profile "$profile" \
    --policy neural \
    --backend native \
    --output "$output_image"

"$cxx" -std=c++17 -O2 -fPIC -Wall -Wextra -Werror \
    -I"$ort_include" \
    -c "$bridge_source" \
    -o "$bridge_object"
rm -f -- "$bridge_archive"
"$ar" rcs "$bridge_archive" "$bridge_object"

cd "$workspace"
rustup "+$toolchain" target add aarch64-unknown-linux-gnu
CARGO_TARGET_DIR="$target_dir" \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$cxx" \
IVC_ORT_BRIDGE_LIB_DIR="$bridge_dir" \
IVC_ORT_RUNTIME_LIB_DIR="$ort_lib" \
    cargo "+$toolchain" build \
        --release \
        --target aarch64-unknown-linux-gnu \
        --package ivcproto \
        --features onnxruntime

if ! "$readelf" -h "$controller" \
    | grep -Fq 'Machine:                           AArch64'; then
    echo "ORT control binary is not an AArch64 ELF: $controller" >&2
    exit 1
fi
if ! "$readelf" -l "$controller" \
    | grep -Fq '/lib/ld-linux-aarch64.so.1'; then
    echo "ORT control binary uses an unexpected ELF interpreter" >&2
    exit 1
fi
if ! "$readelf" -d "$controller" \
    | grep -Eq 'RUNPATH.*\[\$ORIGIN/lib\]'; then
    echo "ORT control binary does not resolve its colocated runtime" >&2
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

audit_dynamic_dependencies "$controller"
audit_dynamic_dependencies "$ort_runtime"
audit_dynamic_dependencies "$ort_provider_shared"
for soname in "${!runtime_libraries[@]}"; do
    audit_dynamic_dependencies "${runtime_libraries[$soname]}"
done

truncate -s "${IVC_STARRY_ORT_CONTROL_ROOTFS_SIZE:-160M}" "$output_image"
set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed before growing $output_image" >&2
    exit "$fsck_status"
fi
resize2fs "$output_image"

for directory in \
    /lib \
    /lib/aarch64-linux-gnu \
    /usr \
    /usr/local \
    /usr/local/bin \
    /usr/local/bin/lib \
    /opt \
    /opt/thermal-ort \
    /var \
    /var/lib \
    /var/lib/ivc; do
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
    libc.so.6 libpthread.so.0 libdl.so.2 librt.so.1 libm.so.6 libstdc++.so.6 \
    libgcc_s.so.1; do
    install_rootfs_file \
        "${runtime_libraries[$soname]}" \
        "/lib/aarch64-linux-gnu/$soname" \
        0100755
done
install_rootfs_file "$controller" /usr/local/bin/ivcproto 0100755
install_rootfs_file "$ort_runtime" /usr/local/bin/lib/libonnxruntime.so.1 0100755
install_rootfs_file \
    "$ort_provider_shared" \
    /usr/local/bin/lib/libonnxruntime_providers_shared.so \
    0100755
install_rootfs_file "$model" /opt/thermal-ort/thermal-4x6x1-v1.ort 0100644
install_rootfs_file "$script_dir/autorun.sh" /usr/bin/starry-run-case-tests 0100755

profile_file=$(mktemp "$output_dir/ort-control-profile.XXXXXX")
debugfs -R 'cat /etc/ivc-profile' "$output_image" >"$profile_file"
sed -i 's/^ivc_backend=native$/ivc_backend=onnxruntime/' "$profile_file"
controller_sha256=$(sha256sum "$controller" | cut -d ' ' -f 1)
printf '%s\n' \
    'ivc_ort_model=/opt/thermal-ort/thermal-4x6x1-v1.ort' \
    'ivc_ort_runtime=/usr/local/bin/lib/libonnxruntime.so.1' \
    'ivc_ort_provider_shared=/usr/local/bin/lib/libonnxruntime_providers_shared.so' \
    'ivc_ort_evidence=/var/lib/ivc/ort.csv' \
    'ivc_ort_runtime_version=1.25.0' \
    "ivc_ort_model_sha256=$model_sha256" \
    "ivc_ort_runtime_sha256=$runtime_sha256" \
    "ivc_ort_provider_shared_sha256=$provider_shared_sha256" \
    "ivc_ort_controller_sha256=$controller_sha256" \
    >>"$profile_file"
install_rootfs_file "$profile_file" /etc/ivc-profile 0100644

set +e
e2fsck -fy "$output_image"
fsck_status=$?
set -e
if ((fsck_status > 1)); then
    echo "e2fsck failed after populating $output_image" >&2
    exit "$fsck_status"
fi

debugfs -R 'stat /usr/local/bin/ivcproto' "$output_image"
debugfs -R 'stat /usr/local/bin/lib/libonnxruntime.so.1' "$output_image"
debugfs -R 'stat /usr/local/bin/lib/libonnxruntime_providers_shared.so' "$output_image"
debugfs -R 'stat /opt/thermal-ort/thermal-4x6x1-v1.ort' "$output_image"
debugfs -R 'cat /etc/ivc-profile' "$output_image"
sha256sum "$controller" "$model" "$ort_runtime" "$ort_provider_shared" "$output_image"
echo "STARRY_ORT_CONTROL_ROOTFS_PASS image=$output_image profile=$profile count=$command_count backend=onnxruntime"
