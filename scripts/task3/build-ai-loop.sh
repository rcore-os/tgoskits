#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-${ROOT}/tmp/task2/task3-ai-loop-client}"
case "${OUT}" in
  /*) OUT_ABS="${OUT}" ;;
  *) OUT_ABS="${PWD}/${OUT}" ;;
esac
mkdir -p "$(dirname "${OUT_ABS}")"

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

SRC="${ROOT}/scripts/task2"
echo "[task3] Building ai-loop client with ${CC}"
"${CC}" -static -fno-PIE -no-pie -O2 \
  -I "${SRC}" \
  -o "${OUT_ABS}" \
  "${SRC}/icpc-wire.c" \
  "${SRC}/task3-ai-loop-client.c" \
  -lm
chmod 0755 "${OUT_ABS}"
echo "[task3] Wrote ${OUT_ABS} ($(wc -c < "${OUT_ABS}") bytes)"
