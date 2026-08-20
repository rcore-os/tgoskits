#!/usr/bin/env bash
set -euo pipefail

case_dir="$(cd "$(dirname "$0")" && pwd)"
src_dir="${case_dir}/rknn-yolov8-image"
build_dir="${src_dir}/build-rk3588-aarch64"
host_test_build_dir="${src_dir}/build-host-selftest"
install_dir="${src_dir}/install/rk3588_linux_aarch64/rknn_yolov8_image"
cross_prefix="${CROSS_COMPILE:-aarch64-linux-gnu-}"
cc="${CC:-${cross_prefix}gcc}"
cxx="${CXX:-${cross_prefix}g++}"
host_cc="${HOST_CC:-cc}"
host_cxx="${HOST_CXX:-c++}"

command -v "${cc}" >/dev/null
command -v "${cxx}" >/dev/null
command -v "${host_cc}" >/dev/null
command -v "${host_cxx}" >/dev/null
command -v ctest >/dev/null

rm -rf "${build_dir}" "${host_test_build_dir}" "${install_dir}"
mkdir -p "${build_dir}" "${host_test_build_dir}" "${install_dir}"

# The deployed runner is cross-compiled for aarch64 and cannot be executed on
# the build host. Run the mock-backed host CTests first so a frame-decoding
# regression blocks this deployment entry point.
cmake -S "${src_dir}" -B "${host_test_build_dir}" \
  -DCMAKE_C_COMPILER="${host_cc}" \
  -DCMAKE_CXX_COMPILER="${host_cxx}" \
  -DCMAKE_BUILD_TYPE=Release \
  -DTARGET_SOC=rk3588
cmake --build "${host_test_build_dir}" \
  --target uvc_capture_layout_selftest uvc_capture_mjpeg_selftest -j"$(nproc)"
ctest --test-dir "${host_test_build_dir}" --output-on-failure

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
