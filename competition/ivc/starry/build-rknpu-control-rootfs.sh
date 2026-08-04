#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
profile=smoke
output_image=
sysroot=${IVC_RKNN_SYSROOT:-/usr/aarch64-linux-gnu}
cc=${IVC_RKNN_CC:-aarch64-linux-gnu-gcc}
ar=${IVC_RKNN_AR:-aarch64-linux-gnu-ar}
readelf=${IVC_RKNN_READELF:-aarch64-linux-gnu-readelf}
rknn_header=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/include/rknn_api.h
rknn_runtime=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/Linux/aarch64/librknnrt.so
rknn_model=$workspace/competition/ivc/model/thermal-4x6x1-v1-rk3588-fp16.rknn
bridge_source=$workspace/tools/ivcproto/csrc/rknn_bridge.c
bridge_dir=$output_dir/rknpu-control-bridge
bridge_object=$bridge_dir/rknn_bridge.o
bridge_archive=$bridge_dir/libivc_rknn_bridge.a
target_dir=$output_dir/rknpu-control-target
controller=$target_dir/aarch64-unknown-linux-gnu/release/ivcproto
profile_file=

usage() {
    cat <<EOF
Usage: $0 [smoke|full] [--profile smoke|full] [--output IMAGE]

Builds the StarryOS IVC controller with the frozen RKNN NPU backend and
populates a glibc-capable rootfs for the Orange Pi 5 Plus control loop.
EOF
}

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
            echo "Unknown RKNN control rootfs option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$profile" in
    smoke)
        command_count=20
        output_image=${output_image:-$output_dir/starry-ivc-rootfs-rknpu-smoke.img}
        ;;
    full)
        command_count=1800
        output_image=${output_image:-$output_dir/starry-ivc-rootfs-rknpu.img}
        ;;
    *)
        echo "RKNN control profile must be smoke or full: $profile" >&2
        exit 2
        ;;
esac
if [[ "$output_image" != /* ]]; then
    output_image=$workspace/$output_image
fi

for input_path in \
    "$rknn_header" \
    "$rknn_runtime" \
    "$rknn_model" \
    "$bridge_source" \
    "$script_dir/autorun.sh"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required RKNN control rootfs input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in \
    "$cc" "$ar" "$readelf" cargo cut debugfs e2fsck grep mktemp resize2fs rm \
    rustup sed sha256sum truncate; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required RKNN control rootfs command not found: $command_name" >&2
        exit 1
    fi
done

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

mkdir -p "$output_dir" "$bridge_dir" "$target_dir" "$(dirname -- "$output_image")"
"$script_dir/build-rootfs.sh" \
    --profile "$profile" \
    --policy neural \
    --backend native \
    --output "$output_image"

"$cc" -std=c11 -O2 -fPIC -Wall -Wextra -Werror \
    -D_POSIX_C_SOURCE=200809L \
    -I"$(dirname -- "$rknn_header")" \
    -c "$bridge_source" \
    -o "$bridge_object"
rm -f -- "$bridge_archive"
"$ar" rcs "$bridge_archive" "$bridge_object"

cd "$workspace"
rustup "+$toolchain" target add aarch64-unknown-linux-gnu
CARGO_TARGET_DIR="$target_dir" \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$cc" \
IVC_RKNN_BRIDGE_LIB_DIR="$bridge_dir" \
IVC_RKNN_RUNTIME_LIB_DIR="$(dirname -- "$rknn_runtime")" \
    cargo "+$toolchain" build \
        --release \
        --target aarch64-unknown-linux-gnu \
        --package ivcproto \
        --features rknn

if ! "$readelf" -h "$controller" \
    | grep -Fq 'Machine:                           AArch64'; then
    echo "RKNN control binary is not an AArch64 ELF: $controller" >&2
    exit 1
fi
if ! "$readelf" -l "$controller" \
    | grep -Fq '/lib/ld-linux-aarch64.so.1'; then
    echo "RKNN control binary uses an unexpected ELF interpreter" >&2
    exit 1
fi
if ! "$readelf" -d "$controller" \
    | grep -Eq 'RUNPATH.*\[\$ORIGIN/lib\]'; then
    echo "RKNN control binary does not resolve its colocated runtime" >&2
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

audit_dynamic_dependencies "$controller"
audit_dynamic_dependencies "$rknn_runtime"
for soname in "${!runtime_libraries[@]}"; do
    audit_dynamic_dependencies "${runtime_libraries[$soname]}"
done

truncate -s "${IVC_STARRY_RKNPU_CONTROL_ROOTFS_SIZE:-128M}" "$output_image"
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
    /opt/thermal-rknn \
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
    libc.so.6 libpthread.so.0 libdl.so.2 libm.so.6 libstdc++.so.6 libgcc_s.so.1; do
    install_rootfs_file \
        "${runtime_libraries[$soname]}" \
        "/lib/aarch64-linux-gnu/$soname" \
        0100755
done
install_rootfs_file "$controller" /usr/local/bin/ivcproto 0100755
install_rootfs_file "$rknn_runtime" /usr/local/bin/lib/librknnrt.so 0100755
install_rootfs_file \
    "$rknn_model" \
    /opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn \
    0100644
install_rootfs_file "$script_dir/autorun.sh" /usr/bin/starry-run-case-tests 0100755

profile_file=$(mktemp "$output_dir/rknpu-control-profile.XXXXXX")
cleanup() {
    rm -f -- "$profile_file"
}
trap cleanup EXIT HUP INT TERM
debugfs -R 'cat /etc/ivc-profile' "$output_image" >"$profile_file"
sed -i 's/^ivc_backend=native$/ivc_backend=rknn-npu/' "$profile_file"
controller_sha256=$(sha256sum "$controller" | cut -d ' ' -f 1)
model_sha256=$(sha256sum "$rknn_model" | cut -d ' ' -f 1)
runtime_sha256=$(sha256sum "$rknn_runtime" | cut -d ' ' -f 1)
printf '%s\n' \
    'ivc_rknn_model=/opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn' \
    'ivc_rknn_runtime=/usr/local/bin/lib/librknnrt.so' \
    'ivc_rknn_evidence=/var/lib/ivc/rknn.csv' \
    'ivc_rknn_core_mask=0' \
    "ivc_rknn_model_sha256=$model_sha256" \
    "ivc_rknn_runtime_sha256=$runtime_sha256" \
    "ivc_rknn_controller_sha256=$controller_sha256" \
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
debugfs -R 'stat /usr/local/bin/lib/librknnrt.so' "$output_image"
debugfs -R 'stat /opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn' \
    "$output_image"
debugfs -R 'cat /etc/ivc-profile' "$output_image"
sha256sum "$controller" "$rknn_model" "$rknn_runtime" "$output_image"
echo "STARRY_RKNN_CONTROL_ROOTFS_PASS image=$output_image profile=$profile count=$command_count backend=rknn-npu"
