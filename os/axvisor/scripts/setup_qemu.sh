#!/usr/bin/env bash
set -euo pipefail

# Unified QEMU guest setup script for AxVisor testing.
# Usage:
#   ./scripts/setup_qemu.sh [--guest] <guest>
#   ./scripts/setup_qemu.sh arceos
#   ./scripts/setup_qemu.sh --guest linux
#   ./scripts/setup_qemu.sh nimbos
#
# Supported guests: arceos, arceos-riscv64, linux, nimbos

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE_STORAGE_ROOT="/tmp/.axvisor-images"
DEFAULT_REGISTRY_URL="https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/default.toml"
# Keep this version aligned with the guest release used in this branch.
BUILTIN_FALLBACK_REGISTRY_URL="https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/v0.0.25.toml"
IMAGE_DOWNLOAD_MAX_ATTEMPTS=2

bootstrap_image_registry() {
  local storage_dir="${IMAGE_STORAGE_ROOT}"
  local default_registry_url="${DEFAULT_REGISTRY_URL}"
  # Built-in fallback keeps setup resilient when default/include URLs are flaky.
  local fallback_registry_url="${AXVISOR_REGISTRY_FALLBACK_URL:-${BUILTIN_FALLBACK_REGISTRY_URL}}"
  local default_registry_file
  local include_url
  local source_kind
  local source_url

  mkdir -p "${storage_dir}"
  if [ -f "${storage_dir}/images.toml" ]; then
    return 0
  fi

  default_registry_file="$(mktemp)"
  if ! curl -4 --retry 5 --retry-delay 2 -fL "${default_registry_url}" -o "${default_registry_file}"; then
    rm -f "${default_registry_file}"
    echo "  -> Warning: failed to fetch default registry: ${default_registry_url}" >&2
    source_kind="fallback registry"
    source_url="${fallback_registry_url}"
  else
    include_url="$(sed -n 's/^[[:space:]]*url[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "${default_registry_file}" | sed -n '1p')"
    rm -f "${default_registry_file}"
    if [ -n "${include_url}" ]; then
      source_kind="included registry from default.toml"
      source_url="${include_url}"
    else
      source_kind="default registry"
      source_url="${default_registry_url}"
    fi
  fi

  echo "  -> Bootstrapping local image registry from ${source_kind}: ${source_url}"
  if ! curl -4 --retry 5 --retry-delay 2 -fL "${source_url}" -o "${storage_dir}/images.toml"; then
    if [ "${source_url}" != "${fallback_registry_url}" ]; then
      echo "  -> Warning: failed to fetch ${source_kind}, retrying fallback registry: ${fallback_registry_url}" >&2
      curl -4 --retry 5 --retry-delay 2 -fL "${fallback_registry_url}" -o "${storage_dir}/images.toml"
    else
      echo "  -> Error: failed to bootstrap local image registry from fallback registry." >&2
      return 1
    fi
  fi
  date +%s > "${storage_dir}/.last_sync" || true
}

usage() {
  echo "Usage: $0 [--guest] <arceos|arceos-riscv64|linux|nimbos>"
  echo ""
  echo "  arceos          - aarch64 ArceOS guest"
  echo "  arceos-riscv64  - riscv64 ArceOS guest"
  echo "  linux           - aarch64 Linux guest"
  echo "  nimbos          - x86_64 NimbOS guest (requires VT-x/KVM)"
  echo ""
  echo "Examples:"
  echo "  $0 arceos"
  echo "  $0 --guest arceos-riscv64"
  echo "  $0 --guest linux"
  exit 1
}

GUEST=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --guest)
      shift
      [[ $# -gt 0 ]] || usage
      GUEST="$1"
      shift
      break
      ;;
    arceos|arceos-riscv64|linux|nimbos)
      GUEST="$1"
      shift
      break
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      ;;
  esac
done

[[ -n "${GUEST}" ]] || usage

# Guest configuration: image_name|vmconfig|build_config|qemu_config|kernel_file|success_msg
case "$GUEST" in
  arceos)         CFG="qemu_aarch64_arceos|arceos-aarch64-qemu-smp1.toml|qemu-aarch64.toml|qemu-aarch64.toml|qemu-aarch64|Hello, world!" ;;
  arceos-riscv64) CFG="qemu_riscv64_arceos|arceos-riscv64-qemu-smp1.toml|qemu-riscv64.toml|qemu-riscv64.toml|qemu-riscv64|Hello, world!" ;;
  linux)          CFG="qemu_aarch64_linux|linux-aarch64-qemu-smp1.toml|qemu-aarch64.toml|qemu-aarch64.toml|qemu-aarch64|test pass!" ;;
  nimbos)         CFG="qemu_x86_64_nimbos|nimbos-x86_64-qemu-smp1.toml|qemu-x86_64.toml|qemu-x86_64-kvm.toml|qemu-x86_64|usertests passed!" ;;
  *)       echo "Unknown guest: $GUEST" >&2; usage ;;
esac

IFS='|' read -r IMAGE_NAME VMCONFIG BUILD_CONFIG QEMU_CONFIG KERNEL_FILE SUCCESS_MSG <<< "$CFG"
# NOTE:
#  - `cargo axvisor image pull` 默认把镜像解压到 `/tmp/.axvisor-images/<IMAGE_NAME>`
#  - 这里直接使用该目录作为镜像来源，避免路径不一致
IMAGE_DIR="${IMAGE_STORAGE_ROOT}/${IMAGE_NAME}"
VMCONFIG_TEMPLATE_PATH="${REPO_ROOT}/configs/vms/${VMCONFIG}"
VMCONFIG_TMP_DIR="${REPO_ROOT}/tmp/vmconfigs"
GENERATED_VMCONFIG_PATH="${VMCONFIG_TMP_DIR}/${VMCONFIG%.toml}.generated.toml"
ROOTFS_TARGET="${REPO_ROOT}/tmp/rootfs.img"
KERNEL_IMAGE="${IMAGE_DIR}/${KERNEL_FILE}"
ROOTFS_IMAGE="${IMAGE_DIR}/rootfs.img"
ABS_KERNEL_PATH="${IMAGE_DIR}/${KERNEL_FILE}"

echo "[setup_qemu] Guest: ${GUEST} | Repo: ${REPO_ROOT}"

echo "[setup_qemu] Step 1: ensure guest image is downloaded..."
if [ ! -d "${IMAGE_DIR}" ]; then
  echo "  -> Image directory ${IMAGE_DIR} not found, downloading via cargo axvisor image..."
  echo "  -> Download attempt 1/${IMAGE_DOWNLOAD_MAX_ATTEMPTS}"
  if ! (cd "${REPO_ROOT}" && cargo axvisor image pull "${IMAGE_NAME}"); then
    echo "  -> Attempt 1/${IMAGE_DOWNLOAD_MAX_ATTEMPTS} failed. Trying to bootstrap registry..."
    bootstrap_image_registry
    echo "  -> Download attempt 2/${IMAGE_DOWNLOAD_MAX_ATTEMPTS}"
    (cd "${REPO_ROOT}" && cargo axvisor image pull "${IMAGE_NAME}")
  fi
else
  echo "  -> Found existing image directory: ${IMAGE_DIR}"
fi

if [ ! -f "${KERNEL_IMAGE}" ]; then
  echo "ERROR: kernel image not found at ${KERNEL_IMAGE}" >&2
  exit 1
fi

if [ ! -f "${ROOTFS_IMAGE}" ]; then
  echo "ERROR: rootfs image not found at ${ROOTFS_IMAGE}" >&2
  exit 1
fi

# NimbOS x86_64 requires axvm-bios for bootstrapping
if [[ "$GUEST" == "nimbos" ]]; then
  BIOS_IMAGE="${IMAGE_DIR}/axvm-bios.bin"
  if [ ! -f "${BIOS_IMAGE}" ]; then
    echo "ERROR: axvm-bios.bin not found at ${BIOS_IMAGE}" >&2
    echo "  -> Please re-download the NimbOS image via 'cargo axvisor image pull qemu_x86_64_nimbos'." >&2
    exit 1
  fi
fi

echo "[setup_qemu] Step 2: patch VM config kernel_path..."
if [ ! -f "${VMCONFIG_TEMPLATE_PATH}" ]; then
  echo "ERROR: VM config file not found at ${VMCONFIG_TEMPLATE_PATH}" >&2
  exit 1
fi

mkdir -p "${VMCONFIG_TMP_DIR}"
cp "${VMCONFIG_TEMPLATE_PATH}" "${GENERATED_VMCONFIG_PATH}"
sed -i 's|^kernel_path *=.*|kernel_path = "'"${ABS_KERNEL_PATH}"'"|' "${GENERATED_VMCONFIG_PATH}"
echo "  -> Generated VM config: ${GENERATED_VMCONFIG_PATH}"
echo "  -> Updated kernel_path to ${ABS_KERNEL_PATH}"

if [[ "$GUEST" == "nimbos" ]]; then
  ABS_BIOS_PATH="${IMAGE_DIR}/axvm-bios.bin"
  sed -i 's|^bios_path *=.*|bios_path = "'"${ABS_BIOS_PATH}"'"|' "${GENERATED_VMCONFIG_PATH}"
  echo "  -> Updated bios_path to ${ABS_BIOS_PATH}"
fi

echo "[setup_qemu] Step 3: prepare rootfs..."
mkdir -p "$(dirname "${ROOTFS_TARGET}")"
cp "${ROOTFS_IMAGE}" "${ROOTFS_TARGET}"

echo "  -> Copied ${ROOTFS_IMAGE} -> ${ROOTFS_TARGET}"

cat <<EOF

[setup_qemu] Done. Guest: ${GUEST}
You can now run the QEMU test with:

  cd ${REPO_ROOT}
  cargo xtask qemu \\
    --config configs/board/${BUILD_CONFIG} \\
    --qemu-config .github/workflows/${QEMU_CONFIG} \\
    --vmconfigs ${GENERATED_VMCONFIG_PATH}

Success indicator: '${SUCCESS_MSG}'

EOF

if [[ "$GUEST" == "nimbos" ]]; then
  echo "*** NimbOS requires VT-x/VMX and KVM."
  echo ""
fi
