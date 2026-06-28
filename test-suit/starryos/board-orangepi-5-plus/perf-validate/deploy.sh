#!/usr/bin/env bash
# Cross-compile perf-validate to a static aarch64 binary and (optionally) deploy
# it to the Orange Pi 5 Plus's shared ext4 rootfs, so the board run is immediate.
#
# WHY static: the binary runs under StarryOS (which loads static ELF) AND can be
# sanity-checked under the board's OrangePi Linux on the SAME ext4 — a static
# musl build depends on neither rootfs's libc (same approach as the perf 6.6
# binary). It is pure syscalls + libc, no external deps.
#
# Usage:
#   ./deploy.sh build              # cross-compile only -> ./perf-validate
#   ./deploy.sh deploy             # build + scp to the board (board in Linux)
#   BOARD_USER=orangepi BOARD_IP=192.168.50.2 BOARD_DEST=/root/perf-validate \
#     ./deploy.sh deploy
#
# After deploy, power-cycle the board into StarryOS and run the board test:
#   (server) cargo xtask starry board -t perf-validate \
#       --board-config .../perf-validate/board-orangepi-5-plus.toml \
#       -b OrangePi-5-Plus --server localhost --port 2999
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/src/perf_validate.c"
OUT="$HERE/perf-validate"
IMAGE="${TGOS_IMAGE:-ghcr.io/rcore-os/tgoskits-container:latest}"
REPO_ROOT="$(cd "$HERE/../../../.." && pwd)"

# Board defaults (override via env). The DEST must be the path StarryOS sees as
# /root/perf-validate on the shared ext4 (board-orangepi-5-plus.toml runs
# `cd /root && ./perf-validate`). On first run, confirm the StarryOS-visible
# mount point and adjust BOARD_DEST if /root differs from the Linux path.
BOARD_USER="${BOARD_USER:-orangepi}"
BOARD_IP="${BOARD_IP:-192.168.50.2}"
BOARD_DEST="${BOARD_DEST:-/root/perf-validate}"

build() {
  echo "[perf-validate] cross-compiling static aarch64 musl binary..."
  docker run --rm --platform linux/amd64 \
    -v "$REPO_ROOT:$REPO_ROOT" -w "$HERE" \
    "$IMAGE" bash -lc '
      set -e
      CC=aarch64-linux-musl-gcc
      command -v "$CC" >/dev/null 2>&1 || CC=/opt/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc
      "$CC" -static -O2 -std=c11 -D_GNU_SOURCE \
        -Wall -Wextra -Werror \
        -o '"$OUT"' '"$SRC"'
    '
  echo "[perf-validate] built: $OUT"
  file "$OUT" 2>/dev/null || true
}

deploy() {
  build
  echo "[perf-validate] scp -> $BOARD_USER@$BOARD_IP:$BOARD_DEST"
  echo "  (board must be in OrangePi Linux with the cabled NIC up; pw 'orangepi')"
  scp "$OUT" "$BOARD_USER@$BOARD_IP:$BOARD_DEST" || {
    echo "scp failed — try: sshpass -p orangepi scp $OUT $BOARD_USER@$BOARD_IP:$BOARD_DEST" >&2
    exit 1
  }
  # shellcheck disable=SC2029
  ssh "$BOARD_USER@$BOARD_IP" "chmod +x $BOARD_DEST && ls -l $BOARD_DEST" || true
  echo "[perf-validate] deployed. Power-cycle into StarryOS, then run the board test."
}

case "${1:-build}" in
  build) build ;;
  deploy) deploy ;;
  *) echo "usage: $0 {build|deploy}" >&2; exit 2 ;;
esac
