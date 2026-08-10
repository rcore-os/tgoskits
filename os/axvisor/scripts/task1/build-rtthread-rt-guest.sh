#!/usr/bin/env bash
# Build RT-Thread qemu-virt64-aarch64 guest image for AxVisor memory-loaded RT VM.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
CACHE_DIR="${AXVISOR_ROOT}/.cache/rtthread-build"
BUILD_DIR="${CACHE_DIR}/rt-thread"
BSP_DIR="${BUILD_DIR}/bsp/qemu-virt64-aarch64"
IMAGE_DIR="${AXVISOR_ROOT}/images/qemu_aarch64_rtthread"
OUTPUT_NAME="qemu-aarch64-rtthread-bench"

RTTHREAD_REPO_URL="${RTTHREAD_REPO_URL:-https://github.com/RT-Thread/rt-thread.git}"
RTTHREAD_REF="${RTTHREAD_REF:-b7eb6fb0e46f6674ea7711f1b5f2ff1a813ac678}"
TOOLCHAIN_URL="${RTTHREAD_TOOLCHAIN_URL:-https://developer.arm.com/-/media/Files/downloads/gnu/11.3.rel1/binrel/arm-gnu-toolchain-11.3.Rel1-x86_64-aarch64-none-elf.tar.xz}"
TOOLCHAIN_DIR="${CACHE_DIR}/arm-gnu-toolchain-aarch64-none-elf"

info() { echo "[task1] $*"; }

ensure_scons() {
  if command -v scons >/dev/null 2>&1; then
    return 0
  fi
  local venv="${CACHE_DIR}/venv"
  if [[ ! -x "${venv}/bin/scons" ]]; then
    info "Creating local Python venv for scons..."
    python3 -m venv "${venv}"
    "${venv}/bin/pip" install -q scons
  fi
  export PATH="${venv}/bin:${PATH}"
}

ensure_toolchain() {
  if [[ -x "${TOOLCHAIN_DIR}/bin/aarch64-none-elf-gcc" ]]; then
    return 0
  fi
  mkdir -p "${CACHE_DIR}"
  local archive="${CACHE_DIR}/arm-gnu-toolchain.tar.xz"
  info "Downloading aarch64-none-elf toolchain..."
  curl -fsSL -o "${archive}" "${TOOLCHAIN_URL}"
  tar -xf "${archive}" -C "${CACHE_DIR}"
  local extracted
  extracted="$(find "${CACHE_DIR}" -maxdepth 1 -type d -name 'arm-gnu-toolchain-*-aarch64-none-elf' | head -1)"
  [[ -n "${extracted}" ]] || { echo "toolchain extract failed" >&2; exit 1; }
  rm -rf "${TOOLCHAIN_DIR}"
  mv "${extracted}" "${TOOLCHAIN_DIR}"
}

clone_rtthread() {
  if [[ -d "${BUILD_DIR}/.git" ]]; then
    info "Updating RT-Thread source in ${BUILD_DIR}..."
    git -C "${BUILD_DIR}" fetch --depth 1 origin "${RTTHREAD_REF}" 2>/dev/null \
      || git -C "${BUILD_DIR}" fetch --depth 1 origin
  else
    info "Cloning RT-Thread ${RTTHREAD_REPO_URL}..."
    git clone --filter=blob:none --depth 1 "${RTTHREAD_REPO_URL}" "${BUILD_DIR}"
    git -C "${BUILD_DIR}" fetch --depth 1 origin "${RTTHREAD_REF}" 2>/dev/null \
      || git -C "${BUILD_DIR}" fetch --depth 1 origin
  fi
  info "Checking out RT-Thread ref ${RTTHREAD_REF}..."
  git -C "${BUILD_DIR}" checkout -q "${RTTHREAD_REF}"
}

info "Building RT-Thread qemu-virt64-aarch64 guest..."
ensure_scons
ensure_toolchain
clone_rtthread

export RTT_EXEC_PATH="${TOOLCHAIN_DIR}/bin"
export PATH="${RTT_EXEC_PATH}:${PATH}"

(
  cd "${BSP_DIR}"
  scons -j"$(nproc)"
)

BIN_SRC="${BSP_DIR}/rtthread.bin"
if [[ ! -f "${BIN_SRC}" ]]; then
  echo "RT-Thread guest binary not found at ${BIN_SRC}" >&2
  exit 1
fi

mkdir -p "${IMAGE_DIR}"
install -m 0644 "${BIN_SRC}" "${IMAGE_DIR}/${OUTPUT_NAME}"

TMP_IMAGE_DIR="${AXVISOR_ROOT}/tmp/images/qemu_aarch64_rtthread"
mkdir -p "${TMP_IMAGE_DIR}"
install -m 0644 "${BIN_SRC}" "${TMP_IMAGE_DIR}/${OUTPUT_NAME}"

info "Installed ${IMAGE_DIR}/${OUTPUT_NAME}"
info "Use with configs/vms/qemu/aarch64/rtthread-rt-baseline.toml"
