#!/usr/bin/env bash
set -euo pipefail

case_dir="$(cd "$(dirname "$0")" && pwd)"
src_dir="${case_dir}/rknn-yolov8-image"
build_dir="${src_dir}/build-rk3588-aarch64"
install_dir="${src_dir}/install/rk3588_linux_aarch64/rknn_yolov8_image"
cross_prefix="${CROSS_COMPILE:-aarch64-linux-gnu-}"
cc="${CC:-${cross_prefix}gcc}"
cxx="${CXX:-${cross_prefix}g++}"

command -v "${cc}" >/dev/null
command -v "${cxx}" >/dev/null

rm -rf "${build_dir}" "${install_dir}"
mkdir -p "${build_dir}" "${install_dir}"

cmake -S "${src_dir}" -B "${build_dir}" \
  -DCMAKE_C_COMPILER="${cc}" \
  -DCMAKE_CXX_COMPILER="${cxx}" \
  -DCMAKE_SYSTEM_NAME=Linux \
  -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="${install_dir}" \
  -DTARGET_SOC=rk3588

cmake --build "${build_dir}" -j"$(nproc)"
cmake --install "${build_dir}"

echo "installed: ${install_dir}"
