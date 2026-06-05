#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
workspace="${STARRY_WORKSPACE:-$(cd "${app_dir}/../../.." && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

find_storage_root() {
    local dir=$1
    while [[ "${dir}" != "/" ]]; do
        if [[ -d "${dir}/target/official-k230/k230-sdk-src" ]]; then
            printf '%s\n' "${dir}"
            return 0
        fi
        dir=$(dirname "${dir}")
    done
    return 1
}

if [[ -z "${overlay_dir}" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

storage_root="${workspace}"
if [[ -z "${K230_SDK_ROOT:-}" ]]; then
    if storage_root_candidate=$(find_storage_root "${workspace}"); then
        storage_root="${storage_root_candidate}"
    fi
fi

sdk_root="${K230_SDK_ROOT:-${storage_root}/target/official-k230/k230-sdk-src}"
kmodel="${K230_KMODEL:-${sdk_root}/src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel}"
bus_jpg="${K230_BUS_JPG:-${sdk_root}/src/big/kmodel/ai_poc/images/bus.jpg}"
bin_dir="${K230_PREBUILT_DIR:-${app_dir}/c/assets/bin}"

require_file() {
    local path=$1
    local hint=$2
    if [[ ! -f "${path}" ]]; then
        echo "error: missing ${path}" >&2
        echo "hint: ${hint}" >&2
        exit 1
    fi
}

require_file "${bin_dir}/kpu-nncase-minimal" \
    "run apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh"
require_file "${bin_dir}/k230-yolov8n-demo" \
    "run apps/starry/k230-kpu-nncase/c/tools/build-nncase-runtime-binaries.sh"
require_file "${kmodel}" \
    "prepare the official K230 SDK assets, or set K230_KMODEL=/path/to/yolov8n_320.kmodel"
require_file "${bus_jpg}" \
    "prepare the official K230 SDK assets, or set K230_BUS_JPG=/path/to/bus.jpg"

mkdir -p \
    "${overlay_dir}/usr/bin" \
    "${overlay_dir}/usr/share/k230-nncase-runtime/models" \
    "${overlay_dir}/usr/share/k230-nncase-runtime/images"

install -m 0755 "${bin_dir}/kpu-nncase-minimal" "${overlay_dir}/usr/bin/kpu-nncase-minimal"
install -m 0755 "${bin_dir}/k230-yolov8n-demo" "${overlay_dir}/usr/bin/k230-yolov8n-demo"
install -m 0755 "${app_dir}/c/src/run-nncase-runtime-demo.sh" "${overlay_dir}/usr/bin/k230-nncase-runtime-demo"
install -m 0644 "${kmodel}" "${overlay_dir}/usr/share/k230-nncase-runtime/models/yolov8n_320.kmodel"
install -m 0644 "${bus_jpg}" "${overlay_dir}/usr/share/k230-nncase-runtime/images/bus.jpg"

echo "k230-kpu-nncase prebuild: installed demo binaries, model, and image into rootfs overlay"
