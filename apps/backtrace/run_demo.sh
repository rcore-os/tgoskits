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
  demo1  ArceOS raw backtrace without host symbolize.
  demo2  ArceOS raw backtrace with automatic host symbolize.
  demo3  ArceOS DWARF-enabled raw backtrace with automatic host symbolize.
  demo4  StarryOS /dev/memtrack allocation backtrace with host symbolize.
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

target_triple_for_arch() {
  case "${1}" in
    x86_64)
      echo "x86_64-unknown-none"
      ;;
    aarch64)
      echo "aarch64-unknown-none-softfloat"
      ;;
    riscv64)
      echo "riscv64gc-unknown-none-elf"
      ;;
    loongarch64)
      echo "loongarch64-unknown-none-softfloat"
      ;;
    *)
      echo "unsupported arch: ${1}" >&2
      exit 2
      ;;
  esac
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
    --test-case backtrace \
    --no-symbolize
}

run_demo2() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask arceos test qemu \
    --arch "${arch}" \
    --test-group rust \
    --test-case backtrace
}

run_demo3() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  cargo xtask arceos test qemu \
    --arch "${arch}" \
    --test-group rust \
    --test-case backtrace-raw-normal
}

run_demo4() {
  local arch="${1:-${DEFAULT_ARCH}}"
  require_supported_arch "${arch}"

  local target
  target="$(target_triple_for_arch "${arch}")"

  local log
  log="$(mktemp "${TMPDIR:-/tmp}/tgoskits-starry-memtrack-${arch}.XXXXXX.log")"

  set +e
  cargo xtask starry app qemu \
    -t qemu/memtrack-backtrace \
    --arch "${arch}" \
    --qemu-config "qemu-${arch}.toml" \
    2>&1 | tee "${log}"
  local status="${PIPESTATUS[0]}"
  set -e

  if [[ "${status}" -ne 0 ]]; then
    echo "demo4 qemu failed; raw log kept at ${log}" >&2
    return "${status}"
  fi

  if grep -q '^=== host backtrace symbolize ===$' "${log}"; then
    echo "demo4 host symbolize already emitted by runner; raw log kept at ${log}" >&2
    return 0
  fi

  if ! grep -Eq '^BACKTRACE_BEGIN([[:space:]]|$)' "${log}"; then
    echo "demo4 found no BACKTRACE_BEGIN blocks; raw log kept at ${log}" >&2
    return 1
  fi

  local symbolized
  symbolized="$(mktemp "${TMPDIR:-/tmp}/tgoskits-starry-memtrack-${arch}.symbolized.XXXXXX.log")"

  if ! cargo xtask backtrace symbolize \
    --elf "target/${target}/release/starryos" \
    --log "${log}" \
    --kind alloc \
    --adjust-ip false > "${symbolized}"; then
    echo "demo4 host symbolize failed; raw log kept at ${log}" >&2
    echo "demo4 partial symbolized output kept at ${symbolized}" >&2
    return 1
  fi

  if ! grep -Eq '^BACKTRACE_BLOCK[[:space:]].*kind=alloc([[:space:]]|$)' "${symbolized}"; then
    echo "demo4 host symbolize produced no alloc blocks; raw log kept at ${log}" >&2
    echo "demo4 symbolized output kept at ${symbolized}" >&2
    return 1
  fi

  printf '\n=== host backtrace symbolize ===\n'
  cat "${symbolized}"
  rm -f "${symbolized}"
  echo "demo4 raw log: ${log}" >&2
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
