#!/usr/bin/env bash
set -euo pipefail

# Unified QEMU guest setup script for AxVisor testing.
# Usage:
#   ./scripts/setup_qemu.sh [--guest] <guest>
#   ./scripts/setup_qemu.sh arceos
#   ./scripts/setup_qemu.sh --guest linux
#   ./scripts/setup_qemu.sh nimbos
#   ./scripts/setup_qemu.sh nimbos-uefi
#   ./scripts/setup_qemu.sh linux-x86_64
#   ./scripts/setup_qemu.sh linux-x86_64-uefi
#
# Supported guests: arceos, arceos-riscv64, linux, linux-x86_64, nimbos, nimbos-uefi, linux-x86_64-uefi
# LoongArch64 AxVisor shell smoke uses quick-start.sh instead of this script.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE_ROOT="$(cd "${REPO_ROOT}/../.." && pwd)"
# Use the same env var and default path as cargo xtask image pull
# (TGOS_IMAGE_LOCAL_STORAGE, defaults to $WORKSPACE_ROOT/tmp/axbuild/rootfs).
IMAGE_STORAGE_ROOT="${TGOS_IMAGE_LOCAL_STORAGE:-${WORKSPACE_ROOT}/tmp/axbuild/rootfs}"
export TGOS_IMAGE_LOCAL_STORAGE="${IMAGE_STORAGE_ROOT}"

# Sync xtask's persistent config so that `cargo xtask axvisor qemu` finds
# images at the same path even after this script exits and the env var is gone.
_image_config="${WORKSPACE_ROOT}/tmp/axbuild/.image.toml"
if [ -f "${_image_config}" ]; then
  sed -i 's|^local_storage = .*|local_storage = "'"${IMAGE_STORAGE_ROOT}"'"|' "${_image_config}"
fi

DEFAULT_REGISTRY_URL="https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/default.toml"
IMAGE_DOWNLOAD_MAX_ATTEMPTS=2

# Resolve the actual versioned registry URL from the default registry's
# [[includes]] directive.  Echoes the URL on stdout, or an empty line on
# failure (never returns non-zero, so set -e won't cut off the caller's
# fallback path).
resolve_registry_url() {
  local default_url="$1"
  local tmpfile include_url

  tmpfile="$(mktemp)"
  if curl -4 --retry 5 --retry-delay 2 -fsSL "${default_url}" -o "${tmpfile}"; then
    include_url="$(sed -n 's/^[[:space:]]*url[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "${tmpfile}" | sed -n '1p')"
    rm -f "${tmpfile}"
    if [ -n "${include_url}" ]; then
      echo "${include_url}"
    else
      echo "${default_url}"
    fi
    return 0
  fi
  rm -f "${tmpfile}"
  echo ""
}

bootstrap_image_registry() {
  local storage_dir="${IMAGE_STORAGE_ROOT}"
  local registry_url

  mkdir -p "${storage_dir}"
  if [ -f "${storage_dir}/images.toml" ]; then
    return 0
  fi

  registry_url="$(resolve_registry_url "${DEFAULT_REGISTRY_URL}")"
  if [ -z "${registry_url}" ] && [ -n "${AXVISOR_REGISTRY_FALLBACK_URL:-}" ]; then
    echo "  -> Default registry unreachable, trying AXVISOR_REGISTRY_FALLBACK_URL." >&2
    registry_url="${AXVISOR_REGISTRY_FALLBACK_URL}"
  fi

  if [ -z "${registry_url}" ]; then
    echo "  -> Could not resolve registry URL; letting cargo xtask handle image sync." >&2
    return 0
  fi

  echo "  -> Bootstrapping local image registry from: ${registry_url}"
  if ! curl -4 --retry 5 --retry-delay 2 -fsSL "${registry_url}" -o "${storage_dir}/images.toml"; then
    echo "  -> Error: failed to bootstrap local image registry." >&2
    return 0
  fi
  date +%s > "${storage_dir}/.last_sync" || true
}

TGOSIMAGES_RELEASE="${AXVISOR_TGOSIMAGES_RELEASE:-v0.0.5}"
TGOSIMAGES_QEMU_X86_64_ARCHIVE="qemu-x86_64.tar.xz"
TGOSIMAGES_QEMU_X86_64_URL="${AXVISOR_TGOSIMAGES_QEMU_X86_64_URL:-https://github.com/rcore-os/tgosimages/releases/download/${TGOSIMAGES_RELEASE}/${TGOSIMAGES_QEMU_X86_64_ARCHIVE}}"
TGOSIMAGES_QEMU_X86_64_SHA256="${AXVISOR_TGOSIMAGES_QEMU_X86_64_SHA256:-64434d91166bf70ebfab42481d935c68640301fd031d0836d2bdec3f82bb2e20}"

verify_sha256() {
  local file="$1"
  local expected="$2"
  local actual

  if [ -z "${expected}" ]; then
    return 0
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${file}" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${file}" | awk '{print $1}')"
  else
    echo "  -> Warning: neither sha256sum nor shasum is available; skipping checksum verification." >&2
    return 0
  fi

  if [ "${actual}" != "${expected}" ]; then
    echo "ERROR: checksum mismatch for ${file}" >&2
    echo "  expected: ${expected}" >&2
    echo "  actual:   ${actual}" >&2
    return 1
  fi
}

file_size_bytes() {
  local file="$1"

  if stat -c '%s' "${file}" >/dev/null 2>&1; then
    stat -c '%s' "${file}"
  else
    stat -f '%z' "${file}"
  fi
}

align_up_4k() {
  local value="$1"
  echo $(( (value + 0xfff) & ~0xfff ))
}

nimbos_image_ready() {
  [ -f "${IMAGE_DIR}/qemu-x86_64" ] \
    && [ -f "${IMAGE_DIR}/rootfs.img" ] \
    && [ -f "${IMAGE_DIR}/axvm-bios.bin" ]
}

prepare_nimbos_from_tgosimages() {
  local cache_dir="${IMAGE_STORAGE_ROOT}/.downloads"
  local archive_path="${cache_dir}/${TGOSIMAGES_QEMU_X86_64_ARCHIVE}"
  local extract_dir
  local nimbos_dir

  if nimbos_image_ready; then
    echo "  -> Found existing tgosimages-compatible NimbOS image directory: ${IMAGE_DIR}"
    return 0
  fi

  mkdir -p "${cache_dir}" "${IMAGE_DIR}"
  if [ ! -f "${archive_path}" ] || ! verify_sha256 "${archive_path}" "${TGOSIMAGES_QEMU_X86_64_SHA256}"; then
    rm -f "${archive_path}"
    echo "  -> Downloading NimbOS guest from rcore-os/tgosimages ${TGOSIMAGES_RELEASE}..."
    curl -4 --retry 5 --retry-delay 2 -fL "${TGOSIMAGES_QEMU_X86_64_URL}" -o "${archive_path}"
    verify_sha256 "${archive_path}" "${TGOSIMAGES_QEMU_X86_64_SHA256}"
  else
    echo "  -> Found cached tgosimages archive: ${archive_path}"
  fi

  extract_dir="$(mktemp -d)"
  tar -xJf "${archive_path}" -C "${extract_dir}"
  nimbos_dir="${extract_dir}/nimbos"
  if [ ! -d "${nimbos_dir}" ]; then
    echo "ERROR: ${TGOSIMAGES_QEMU_X86_64_ARCHIVE} does not contain a nimbos/ directory." >&2
    rm -rf "${extract_dir}"
    return 1
  fi

  cp "${nimbos_dir}/nimbos-qemu" "${IMAGE_DIR}/qemu-x86_64"
  if [ -f "${nimbos_dir}/nimbos-qemu-usertests" ]; then
    cp "${nimbos_dir}/nimbos-qemu-usertests" "${IMAGE_DIR}/qemu-x86_64_usertests"
  fi
  cp "${nimbos_dir}/nimbos-qemu.img" "${IMAGE_DIR}/rootfs.img"
  cp "${nimbos_dir}/axvm-bios.bin" "${IMAGE_DIR}/axvm-bios.bin"
  chmod +x "${IMAGE_DIR}/qemu-x86_64" "${IMAGE_DIR}/qemu-x86_64_usertests" 2>/dev/null || true
  rm -rf "${extract_dir}"
  echo "  -> Prepared ${IMAGE_DIR} from rcore-os/tgosimages ${TGOSIMAGES_RELEASE}."
}

usage() {
  echo "Usage: $0 [--guest] <arceos|arceos-riscv64|linux|linux-x86_64|nimbos|nimbos-uefi|linux-x86_64-uefi>"
  echo ""
  echo "  arceos          - aarch64 ArceOS guest"
  echo "  arceos-riscv64  - riscv64 ArceOS guest"
  echo "  linux           - aarch64 Linux guest"
  echo "  linux-x86_64    - x86_64 Linux guest through direct boot"
  echo "  nimbos          - x86_64 NimbOS guest (requires VT-x/KVM)"
  echo "  nimbos-uefi     - x86_64 NimbOS guest through external UEFI firmware"
  echo "  linux-x86_64-uefi - x86_64 Linux guest through external UEFI firmware"
  echo ""
  echo "LoongArch64 AxVisor shell smoke is a separate quick-start flow:"
  echo "  ./scripts/quick-start.sh qemu-loongarch64 start"
  echo ""
  echo "Examples:"
  echo "  $0 arceos"
  echo "  $0 --guest arceos-riscv64"
  echo "  $0 --guest linux"
  exit 1
}

show_loongarch_quick_start_hint() {
  cat <<'EOF'
LoongArch64 AxVisor shell smoke does not use setup_qemu.sh.

Use:
  ./scripts/quick-start.sh qemu-loongarch64 start

This path launches AxVisor directly and requires a virtualization-capable
LoongArch QEMU build such as QEMU-LVZ. If needed, export
AXBUILD_QEMU_SYSTEM_LOONGARCH64=/path/to/qemu-system-loongarch64 before
running quick-start.sh.
EOF
  exit 1
}

# Only execute main logic when run directly (not sourced).
# When sourced (e.g. by test scripts), only function/variable definitions are loaded.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then

GUEST=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --guest)
      shift
      [[ $# -gt 0 ]] || usage
      case "$1" in
        loongarch64|axvisor-loongarch64|loongarch64-axvisor)
          show_loongarch_quick_start_hint
          ;;
      esac
      GUEST="$1"
      shift
      break
      ;;
    arceos|arceos-riscv64|linux|linux-x86_64|nimbos|nimbos-uefi|linux-x86_64-uefi)
      GUEST="$1"
      shift
      break
      ;;
    loongarch64|axvisor-loongarch64|loongarch64-axvisor)
      show_loongarch_quick_start_hint
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

# Guest configuration:
# image_name|vmconfig_template|generated_vmconfig_name|build_config|qemu_config_path|kernel_file|success_msg
case "$GUEST" in
  arceos)         CFG="qemu-aarch64|qemu/aarch64/arceos-smp1.toml|arceos-aarch64-qemu-smp1.toml|qemu-aarch64.toml|.github/workflows/qemu-aarch64.toml|arceos/arceos-qemu|Hello, world!|rootfs-aarch64-alpine.img" ;;
  arceos-riscv64) CFG="qemu-riscv64|qemu/riscv64/arceos-smp1.toml|arceos-riscv64-qemu-smp1.toml|qemu-riscv64.toml|.github/workflows/qemu-riscv64.toml|arceos/arceos-qemu|Hello, world!|rootfs-riscv64-alpine.img" ;;
  linux)          CFG="qemu-aarch64|qemu/aarch64/linux-smp1.toml|linux-aarch64-qemu-smp1.toml|qemu-aarch64.toml|.github/workflows/qemu-aarch64.toml|linux/linux-qemu|BusyBox shell (~ #)|rootfs-aarch64-alpine.img" ;;
  linux-x86_64)   CFG="qemu-x86_64|qemu/x86_64/linux-vmx-smp1.toml|linux-x86_64-qemu-smp1.toml|qemu-x86_64-linux.toml|configs/qemu/qemu-x86_64-linux.toml|linux/linux-qemu|BusyBox shell (~ #)|rootfs-x86_64-alpine.img" ;;
  nimbos)         CFG="qemu-x86_64|qemu/x86_64/nimbos-smp1.toml|nimbos-x86_64-qemu-smp1.toml|qemu-x86_64.toml|.github/workflows/qemu-x86_64-kvm.toml|qemu-x86_64|usertests passed!|" ;;
  nimbos-uefi)    CFG="qemu-x86_64|qemu/x86_64/nimbos-uefi-smp1.toml|nimbos-x86_64-qemu-uefi-smp1.toml|qemu-x86_64.toml|.github/workflows/qemu-x86_64-uefi.toml|qemu-x86_64|usertests passed!|" ;;
  linux-x86_64-uefi) CFG="qemu-x86_64|qemu/x86_64/linux-uefi-smp1.toml|linux-x86_64-qemu-uefi-smp1.toml|qemu-x86_64.toml|.github/workflows/qemu-x86_64-uefi.toml|linux/linux-qemu|BusyBox shell (~ #)|rootfs-x86_64-alpine.img" ;;
  *)       echo "Unknown guest: $GUEST" >&2; usage ;;
esac

IFS='|' read -r IMAGE_NAME VMCONFIG VMCONFIG_OUTPUT_NAME BUILD_CONFIG QEMU_CONFIG_PATH KERNEL_FILE SUCCESS_MSG ROOTFS_IMAGE_NAME <<< "$CFG"
# NOTE:
#  - `cargo xtask image pull` extracts images to
#    `${IMAGE_STORAGE_ROOT}/<IMAGE_NAME>` by default.
#  - NimbOS x86_64 is normalized into the same directory layout even when it is
#    sourced from the rcore-os/tgosimages qemu-x86_64 release archive.
IMAGE_DIR="${IMAGE_STORAGE_ROOT}/${IMAGE_NAME}"
VMCONFIG_TEMPLATE_PATH="${REPO_ROOT}/configs/vms/${VMCONFIG}"
VMCONFIG_TMP_DIR="${REPO_ROOT}/tmp/vmconfigs"
GENERATED_VMCONFIG_PATH="${VMCONFIG_TMP_DIR}/${VMCONFIG_OUTPUT_NAME%.toml}.generated.toml"
ROOTFS_TARGET="${REPO_ROOT}/tmp/rootfs.img"
KERNEL_IMAGE="${IMAGE_DIR}/${KERNEL_FILE}"
ROOTFS_IMAGE_DIR="${IMAGE_STORAGE_ROOT}/${ROOTFS_IMAGE_NAME}"
ROOTFS_IMAGE="${ROOTFS_IMAGE_DIR}/${ROOTFS_IMAGE_NAME}"
ABS_KERNEL_PATH="${IMAGE_DIR}/${KERNEL_FILE}"
QEMU_CONFIG_ABS_PATH="${REPO_ROOT}/${QEMU_CONFIG_PATH}"

if [[ "$GUEST" == "nimbos" || "$GUEST" == "nimbos-uefi" ]]; then
  ROOTFS_IMAGE="${IMAGE_DIR}/rootfs.img"
fi

if [[ "$GUEST" == "linux-x86_64" ]]; then
  ROOTFS_TARGET="${REPO_ROOT}/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
fi

echo "[setup_qemu] Guest: ${GUEST} | Repo: ${REPO_ROOT}"

echo "[setup_qemu] Step 1: ensure guest image is downloaded..."
if [[ "$GUEST" == "nimbos" || "$GUEST" == "nimbos-uefi" ]]; then
  if ! prepare_nimbos_from_tgosimages; then
    echo "  -> Warning: failed to prepare NimbOS from tgosimages; falling back to cargo xtask image." >&2
    rm -rf "${IMAGE_DIR}"
    mkdir -p "${IMAGE_DIR}"
    (cd "${REPO_ROOT}" && cargo xtask image pull "${IMAGE_NAME}")
  fi
elif [ ! -d "${IMAGE_DIR}" ]; then
  echo "  -> Image directory ${IMAGE_DIR} not found, downloading via cargo xtask image..."
  echo "  -> Download attempt 1/${IMAGE_DOWNLOAD_MAX_ATTEMPTS}"
  if ! (cd "${REPO_ROOT}" && cargo xtask image pull "${IMAGE_NAME}"); then
    echo "  -> Attempt 1/${IMAGE_DOWNLOAD_MAX_ATTEMPTS} failed. Trying to bootstrap registry..."
    bootstrap_image_registry
    echo "  -> Download attempt 2/${IMAGE_DOWNLOAD_MAX_ATTEMPTS}"
    (cd "${REPO_ROOT}" && cargo xtask image pull "${IMAGE_NAME}")
  fi
else
  echo "  -> Found existing image directory: ${IMAGE_DIR}"
fi

if [[ -n "${ROOTFS_IMAGE_NAME}" && ! -f "${ROOTFS_IMAGE}" ]]; then
  echo "  -> Rootfs image not found, downloading ${ROOTFS_IMAGE_NAME}..."
  (cd "${REPO_ROOT}" && cargo xtask image pull "${ROOTFS_IMAGE_NAME}")
fi

if [ ! -f "${KERNEL_IMAGE}" ]; then
  echo "ERROR: kernel image not found at ${KERNEL_IMAGE}" >&2
  exit 1
fi

if [ ! -f "${ROOTFS_IMAGE}" ]; then
  echo "ERROR: rootfs image not found at ${ROOTFS_IMAGE}" >&2
  exit 1
fi

if [ ! -f "${QEMU_CONFIG_ABS_PATH}" ]; then
  echo "ERROR: QEMU config file not found at ${QEMU_CONFIG_ABS_PATH}" >&2
  if [[ "$GUEST" == "linux-x86_64" ]]; then
    echo "  -> linux-x86_64 direct boot expects configs/qemu/qemu-x86_64-linux.toml." >&2
  fi
  exit 1
fi

# NimbOS x86_64 BIOS mode requires axvm-bios for bootstrapping.
if [[ "$GUEST" == "nimbos" ]]; then
  BIOS_IMAGE="${IMAGE_DIR}/axvm-bios.bin"
  if [ ! -f "${BIOS_IMAGE}" ]; then
    echo "ERROR: axvm-bios.bin not found at ${BIOS_IMAGE}" >&2
    echo "  -> Please re-run to download the NimbOS image from rcore-os/tgosimages." >&2
    exit 1
  fi
fi

# x86_64 UEFI mode requires an external OVMF image. Keep the path
# configurable because distributions install OVMF in different locations.
if [[ "$GUEST" == "nimbos-uefi" || "$GUEST" == "linux-x86_64-uefi" ]]; then
  UEFI_FIRMWARE="${AXVISOR_X86_64_UEFI_FIRMWARE:-}"
  if [ -z "${UEFI_FIRMWARE}" ]; then
    for candidate in \
      "${IMAGE_DIR}/OVMF_CODE.fd" \
      "/usr/share/OVMF/OVMF_CODE_4M.fd" \
      "/usr/share/OVMF/OVMF_CODE.fd" \
      "/usr/share/ovmf/OVMF.fd" \
      "/usr/share/qemu/OVMF.fd"; do
      if [ -f "${candidate}" ]; then
        UEFI_FIRMWARE="${candidate}"
        break
      fi
    done
  fi
  if [ -z "${UEFI_FIRMWARE}" ] || [ ! -f "${UEFI_FIRMWARE}" ]; then
    echo "ERROR: UEFI firmware image not found." >&2
    echo "  -> Install OVMF or set AXVISOR_X86_64_UEFI_FIRMWARE=/path/to/OVMF_CODE.fd." >&2
    exit 1
  fi

  UEFI_FIRMWARE_SIZE="$(file_size_bytes "${UEFI_FIRMWARE}")"
  UEFI_FIRMWARE_WINDOW_SIZE="$(align_up_4k "${UEFI_FIRMWARE_SIZE}")"
  UEFI_FIRMWARE_LOAD_ADDR=$((0x100000000 - UEFI_FIRMWARE_WINDOW_SIZE))
  UEFI_FIRMWARE_LOAD_ADDR_HEX="$(printf '0x%x' "${UEFI_FIRMWARE_LOAD_ADDR}")"
  UEFI_FIRMWARE_WINDOW_SIZE_HEX="$(printf '0x%x' "${UEFI_FIRMWARE_WINDOW_SIZE}")"
fi

echo "[setup_qemu] Step 2: patch VM config kernel_path..."
if [ ! -f "${VMCONFIG_TEMPLATE_PATH}" ]; then
  echo "ERROR: VM config file not found at ${VMCONFIG_TEMPLATE_PATH}" >&2
  exit 1
fi

mkdir -p "${VMCONFIG_TMP_DIR}"
cp "${VMCONFIG_TEMPLATE_PATH}" "${GENERATED_VMCONFIG_PATH}"
sed -i 's|^kernel_path *=.*|kernel_path = "'"${ABS_KERNEL_PATH}"'"|' "${GENERATED_VMCONFIG_PATH}"
sed -i 's|^image_location *=.*|image_location = "memory"|' "${GENERATED_VMCONFIG_PATH}"
echo "  -> Generated VM config: ${GENERATED_VMCONFIG_PATH}"
echo "  -> Updated kernel_path to ${ABS_KERNEL_PATH}"
echo "  -> Updated image_location to memory"

if [[ "$GUEST" == "nimbos" ]]; then
  ABS_BIOS_PATH="${IMAGE_DIR}/axvm-bios.bin"
  sed -i 's|^# *bios_path *=.*|bios_path = "'"${ABS_BIOS_PATH}"'"|; s|^bios_path *=.*|bios_path = "'"${ABS_BIOS_PATH}"'"|' "${GENERATED_VMCONFIG_PATH}"
  sed -i 's|^# *bios_load_addr *=.*|bios_load_addr = 0x8000|; s|^bios_load_addr *=.*|bios_load_addr = 0x8000|' "${GENERATED_VMCONFIG_PATH}"
  echo "  -> Updated bios_path to ${ABS_BIOS_PATH}"
fi

if [[ "$GUEST" == "nimbos-uefi" || "$GUEST" == "linux-x86_64-uefi" ]]; then
  sed -i 's|^uefi_firmware_path *=.*|uefi_firmware_path = "'"${UEFI_FIRMWARE}"'"|' "${GENERATED_VMCONFIG_PATH}"
  sed -i 's|^bios_load_addr *=.*|bios_load_addr = '"${UEFI_FIRMWARE_LOAD_ADDR_HEX}"'|' "${GENERATED_VMCONFIG_PATH}"
  sed -i 's|^\(  \[\)0xffc0_0000, 0x40_0000\(, 0x7, 0\].*UEFI firmware window.*\)|\1'"${UEFI_FIRMWARE_LOAD_ADDR_HEX}"', '"${UEFI_FIRMWARE_WINDOW_SIZE_HEX}"'\2|' "${GENERATED_VMCONFIG_PATH}"
  echo "  -> Updated UEFI firmware path to ${UEFI_FIRMWARE}"
  echo "  -> Updated UEFI firmware load address to ${UEFI_FIRMWARE_LOAD_ADDR_HEX} (size ${UEFI_FIRMWARE_SIZE} bytes)"
fi

if [[ "$GUEST" == "linux-x86_64-uefi" ]]; then
  ABS_RAMDISK_PATH="${IMAGE_DIR}/initramfs.cpio.gz"
  if [ ! -f "${ABS_RAMDISK_PATH}" ]; then
    echo "ERROR: initramfs image not found at ${ABS_RAMDISK_PATH}" >&2
    exit 1
  fi
  sed -i 's|^# *ramdisk_path *=.*|ramdisk_path = "'"${ABS_RAMDISK_PATH}"'"|; s|^ramdisk_path *=.*|ramdisk_path = "'"${ABS_RAMDISK_PATH}"'"|' "${GENERATED_VMCONFIG_PATH}"
  echo "  -> Updated ramdisk_path to ${ABS_RAMDISK_PATH}"
fi

echo "[setup_qemu] Step 3: prepare rootfs..."
mkdir -p "$(dirname "${ROOTFS_TARGET}")"
cp "${ROOTFS_IMAGE}" "${ROOTFS_TARGET}"

echo "  -> Copied ${ROOTFS_IMAGE} -> ${ROOTFS_TARGET}"

cat <<EOF

[setup_qemu] Done. Guest: ${GUEST}
You can now run the QEMU test with:

  cargo xtask axvisor qemu \\
    --config ${REPO_ROOT}/configs/board/${BUILD_CONFIG} \\
    --qemu-config ${REPO_ROOT}/${QEMU_CONFIG_PATH} \\
    --vmconfigs ${GENERATED_VMCONFIG_PATH}

Success indicator: '${SUCCESS_MSG}'

EOF

if [[ "$GUEST" == "nimbos" ]]; then
  echo "*** NimbOS requires VT-x/VMX and KVM."
  echo ""
fi

if [[ "$GUEST" == "nimbos-uefi" ]]; then
  echo "*** NimbOS UEFI mode requires VT-x/VMX, KVM, and an OVMF-compatible firmware image."
  echo ""
fi

if [[ "$GUEST" == "linux-x86_64-uefi" ]]; then
  echo "*** Linux x86_64 UEFI mode requires VT-x/VMX, KVM, and an OVMF-compatible firmware image."
  echo ""
fi

fi  # end of sourced-guard: [[ "${BASH_SOURCE[0]}" == "${0}" ]]
