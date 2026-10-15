#!/usr/bin/env bash
set -euo pipefail
case_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
runtime="$case_dir/install/rknn-yolov8-image"

# Native build on an AArch64 board, cross build from an x86 workstation.
if [[ $(uname -m) == aarch64 ]]; then
    target_cc=${CC:-gcc}
    target_cxx=${CXX:-g++}
elif command -v aarch64-linux-gnu-g++-11 >/dev/null && [[ -z ${CROSS_COMPILE:-} ]]; then
    target_cc=${CC:-aarch64-linux-gnu-gcc-11}
    target_cxx=${CXX:-aarch64-linux-gnu-g++-11}
else
    target_cc=${CC:-${CROSS_COMPILE:-aarch64-linux-gnu-}gcc}
    target_cxx=${CXX:-${CROSS_COMPILE:-aarch64-linux-gnu-}g++}
fi
extra=()
[[ -z ${SYSROOT:-} ]] || extra+=("-DCMAKE_SYSROOT=$SYSROOT")
cmake -S "$case_dir" -B "$case_dir/build" -DCMAKE_SYSTEM_NAME=Linux \
    -DCMAKE_SYSTEM_PROCESSOR=aarch64 -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_COMPILER="$target_cc" -DCMAKE_CXX_COMPILER="$target_cxx" \
    -DCMAKE_INSTALL_PREFIX="$runtime" "${extra[@]}"
cmake --build "$case_dir/build" --parallel "${JOBS:-2}"
cmake --install "$case_dir/build"
printf '\nRuntime package: %s\nCopy the whole directory to the Linux guest, then run ./yolov8_demo.sh\n' "$runtime"
