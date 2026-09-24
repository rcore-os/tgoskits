#!/usr/bin/env bash
set -euo pipefail

AKARS_TENNIS_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
AKARS_TENNIS_TARGET="riscv64gc-unknown-linux-musl"
AKARS_TENNIS_DYNAMIC_LINKER="/lib/ld-musl-riscv64.so.1"

AKARS_TENNIS_TOOLCHAIN_URL="https://occ-oss-prod.oss-cn-hangzhou.aliyuncs.com/resource//1777015046405/Xuantie-900-gcc-linux-6.6.36-musl64-x86_64-V3.4.0-20260323.tar.gz"
AKARS_TENNIS_TOOLCHAIN_SHA256="10306ce30f98c8168d47f59487da83ba869d5d191193654a27032835d9bb16f8"
AKARS_TENNIS_TOOLCHAIN_ARCHIVE="Xuantie-900-gcc-linux-6.6.36-musl64-x86_64-V3.4.0-20260323.tar.gz"
AKARS_TENNIS_TOOLCHAIN_EXTRACTED="Xuantie-900-gcc-linux-6.6.36-musl64-x86_64-V3.4.0"

AKARS_TPU_SDK_COMMIT="6fa0d80a635db13b6b9dc061d68b8da0593b79f3"
AKARS_TPU_SDK_URL="https://github.com/milkv-duo/tpu-sdk-sg200x/archive/$AKARS_TPU_SDK_COMMIT.tar.gz"
AKARS_TPU_SDK_SHA256="08fa6715fdd48db370b6b945c58410c608101292deee710200b85501085bde8b"
AKARS_TPU_SDK_ARCHIVE="tpu-sdk-sg200x-$AKARS_TPU_SDK_COMMIT.tar.gz"
AKARS_TPU_SDK_EXTRACTED="tpu-sdk-sg200x-$AKARS_TPU_SDK_COMMIT"

AKARS_TENNIS_TOOLCHAINS_DIR="${AKARS_TENNIS_TOOLCHAINS_DIR:-$AKARS_TENNIS_ROOT/toolchains}"
AKARS_TENNIS_TOOLCHAIN_DIR="${AKARS_TENNIS_TOOLCHAIN_DIR:-$AKARS_TENNIS_TOOLCHAINS_DIR/xuantie-v3.4.0}"
AKARS_TPU_SDK_DIR="${AKARS_TPU_SDK_DIR:-$AKARS_TENNIS_ROOT/thirdparty/tpu-sdk-sg200x}"

AKARS_TENNIS_CC="$AKARS_TENNIS_TOOLCHAIN_DIR/bin/riscv64-unknown-linux-musl-gcc"
AKARS_TENNIS_CXX="$AKARS_TENNIS_TOOLCHAIN_DIR/bin/riscv64-unknown-linux-musl-g++"
AKARS_TENNIS_AR="$AKARS_TENNIS_TOOLCHAIN_DIR/bin/riscv64-unknown-linux-musl-ar"

akars_tennis_toolchain_ready() {
  [[ -x "$AKARS_TENNIS_CC" && -x "$AKARS_TENNIS_CXX" && -x "$AKARS_TENNIS_AR" ]]
}

akars_tennis_tpu_sdk_ready() {
  [[ -f "$AKARS_TPU_SDK_DIR/include/cviruntime.h" \
    && -f "$AKARS_TPU_SDK_DIR/include/cviruntime_context.h" \
    && -f "$AKARS_TPU_SDK_DIR/lib/libcviruntime.so" \
    && -f "$AKARS_TPU_SDK_DIR/lib/libcvikernel.so" \
    && -f "$AKARS_TPU_SDK_DIR/lib/libcvimath.so" \
    && -f "$AKARS_TPU_SDK_DIR/lib/libcnpy.so" ]]
}
