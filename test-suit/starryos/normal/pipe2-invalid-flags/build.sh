#!/bin/bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "$0")" && pwd)"
OUT_DIR="${ROOT}/rootfs-files/root"
OUT_BIN="${OUT_DIR}/pipe2-invalid-flags"

mkdir -p "${OUT_DIR}"

riscv64-unknown-elf-gcc \
  -march=rv64gc \
  -mabi=lp64d \
  -static \
  -nostdlib \
  -ffreestanding \
  -fno-stack-protector \
  -Wl,-e,_start \
  -Wl,--build-id=none \
  -O2 \
  -Wall \
  -Wextra \
  -o "${OUT_BIN}" \
  "${ROOT}/src/pipe2_invalid_flags.c"

file "${OUT_BIN}"
