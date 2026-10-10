#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

readonly DEFAULT_ARCH="x86_64"
readonly ARCHES=("x86_64" "aarch64" "riscv64" "loongarch64")

usage() {
  cat <<'USAGE'
Usage:
  bash apps/backtrace/run_demo.sh demo1 [arch]
  bash apps/backtrace/run_demo.sh demo2 [arch]
  bash apps/backtrace/run_demo.sh demo3 [arch]
  bash apps/backtrace/run_demo.sh demo4 [arch]
  bash apps/backtrace/run_demo.sh demo1-all
  bash apps/backtrace/run_demo.sh demo2-all
  bash apps/backtrace/run_demo.sh demo3-all
  bash apps/backtrace/run_demo.sh demo4-all
  bash apps/backtrace/run_demo.sh starry-rootfs [arch]
  bash apps/backtrace/run_demo.sh starry-rootfs-all
  bash apps/backtrace/run_demo.sh all [arch]
  bash apps/backtrace/run_demo.sh all-arch

Supported arch values:
  x86_64, aarch64, riscv64, loongarch64

Demos:
  demo1  ArceOS target-side backtrace with an AXBT map.
  demo2  Compatibility alias for the ArceOS target-side map demo.
  demo3  Compatibility alias for the ArceOS target-side map demo.
  demo4  StarryOS /dev/memtrack target-side backtrace.
  starry-rootfs  Prepare the StarryOS rootfs used by demo4.
  all    Prepare StarryOS rootfs, then run demo1 through demo4 for one arch.
  all-arch  Run the full workflow for all supported arch values.
USAGE
}

require_supported_arch() {
  local arch="${1}"

  for supported in "${ARCHES[@]}"; do
    if [[ "${arch}" == "${supported}" ]]; then
      return 0
    fi
  done

  echo "unsupported arch: ${arch}" >&2
  echo "supported arch values: ${ARCHES[*]}" >&2
  exit 2
}

run_for_all_arches() {
  local fn="${1}"

  for arch in "${ARCHES[@]}"; do
    printf '\n==> %s (%s)\n' "${fn}" "${arch}"
    "${fn}" "${arch}"
  done
}

run_starry_rootfs() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask starry rootfs --arch "${arch}"
}

run_demo1() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask arceos test qemu \
    --arch "${arch}" \
    --test-group rust \
    --test-case debug-backtrace
}

run_demo2() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask arceos test qemu \
    --arch "${arch}" \
    --test-group rust \
    --test-case debug-backtrace
}

run_demo3() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask arceos test qemu \
    --arch "${arch}" \
    --test-group rust \
    --test-case debug-backtrace
}

run_demo4() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask starry app qemu \
    -t qemu/memtrack-backtrace \
    --arch "${arch}" \
    --qemu-config "qemu-${arch}.toml"
}

run_all_one_arch() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  run_starry_rootfs "${arch}"
  run_demo1 "${arch}"
  run_demo2 "${arch}"
  run_demo3 "${arch}"
  run_demo4 "${arch}"
}

case "${1:-}" in
  starry-rootfs)
    run_starry_rootfs "${2:-${DEFAULT_ARCH}}"
    ;;
  starry-rootfs-all)
    run_for_all_arches run_starry_rootfs
    ;;
  demo1)
    run_demo1 "${2:-${DEFAULT_ARCH}}"
    ;;
  demo1-all)
    run_for_all_arches run_demo1
    ;;
  demo2)
    run_demo2 "${2:-${DEFAULT_ARCH}}"
    ;;
  demo2-all)
    run_for_all_arches run_demo2
    ;;
  demo3)
    run_demo3 "${2:-${DEFAULT_ARCH}}"
    ;;
  demo3-all)
    run_for_all_arches run_demo3
    ;;
  demo4)
    run_demo4 "${2:-${DEFAULT_ARCH}}"
    ;;
  demo4-all)
    run_for_all_arches run_demo4
    ;;
  all)
    run_all_one_arch "${2:-${DEFAULT_ARCH}}"
    ;;
  all-arch)
    run_for_all_arches run_all_one_arch
    ;;
  -h|--help|help|"")
    usage
    ;;
  *)
    echo "unknown demo: $1" >&2
    usage >&2
    exit 2
    ;;
esac
