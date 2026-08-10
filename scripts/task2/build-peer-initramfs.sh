#!/usr/bin/env bash
# Build a tiny aarch64 initramfs for Task 2 Guest B (peer = 10.0.9.3).
# icpc peer: CTRL_CMD -> STATE_REPORT, ERROR_NOTIFY -> ACK, plain-text echo compat.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-${ROOT}/tmp/task2/peer-initramfs.cpio}"
WORKDIR="$(mktemp -d /tmp/task2-initramfs-XXXXXX)"
trap 'rm -rf "${WORKDIR}"' EXIT

case "${OUT}" in
  /*) OUT_ABS="${OUT}" ;;
  *) OUT_ABS="${PWD}/${OUT}" ;;
esac
mkdir -p "$(dirname "${OUT_ABS}")" "${WORKDIR}/root/"{dev,proc,sys}

CC="${CC:-}"
if [[ -z "${CC}" ]]; then
  if [[ -x /home/allen/tools/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc ]]; then
    CC=/home/allen/tools/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc
  elif command -v aarch64-linux-musl-gcc >/dev/null 2>&1; then
    CC="$(command -v aarch64-linux-musl-gcc)"
  else
    CC=aarch64-linux-gnu-gcc
  fi
fi

command -v cpio >/dev/null 2>&1 || {
  echo "ERROR: cpio is required" >&2
  exit 1
}

SRC="${ROOT}/scripts/task2"
echo "[task2] Building icpc peer init with ${CC}"
"${CC}" -static -fno-PIE -no-pie -O2 \
  -I "${SRC}" \
  -o "${WORKDIR}/root/init" \
  "${SRC}/icpc-wire.c" \
  "${SRC}/icpc-pid-plant.c" \
  "${SRC}/icpc-peer-server.c"
chmod 0755 "${WORKDIR}/root/init"

(
  cd "${WORKDIR}/root"
  find . -print0 | cpio --null -o --format=newc > "${OUT_ABS}"
)

echo "[task2] Wrote ${OUT_ABS} ($(wc -c < "${OUT_ABS}") bytes)"
