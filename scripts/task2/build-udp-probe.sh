#!/usr/bin/env bash
# Build a tiny aarch64 static UDP probe for Guest A (Task 2 dual-guest).
# Sends ICPC_PROBE to 10.0.9.3:9527 and prints any reply within TIMEOUT_SEC.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-${ROOT}/tmp/task2/icpc-udp-probe}"
WORKDIR="$(mktemp -d /tmp/task2-udp-probe-XXXXXX)"
trap 'rm -rf "${WORKDIR}"' EXIT

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

cat > "${WORKDIR}/probe.c" <<'EOF'
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

#define PEER_IP "10.0.9.3"
#define ICPC_PORT 9527
#define TIMEOUT_SEC 15
#define PAYLOAD "ICPC_PROBE"

int main(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        perror("socket");
        return 2;
    }

    struct sockaddr_in peer;
    memset(&peer, 0, sizeof(peer));
    peer.sin_family = AF_INET;
    peer.sin_port = htons(ICPC_PORT);
    if (inet_pton(AF_INET, PEER_IP, &peer.sin_addr) != 1) {
        fprintf(stderr, "inet_pton failed\n");
        return 2;
    }

    ssize_t sent = sendto(fd, PAYLOAD, sizeof(PAYLOAD) - 1, 0,
                          (struct sockaddr *)&peer, sizeof(peer));
    if (sent < 0) {
        perror("sendto");
        return 2;
    }

    for (;;) {
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(fd, &rfds);
        struct timeval tv;
        tv.tv_sec = TIMEOUT_SEC;
        tv.tv_usec = 0;
        int ready = select(fd + 1, &rfds, NULL, NULL, &tv);
        if (ready == 0) {
            fprintf(stderr, "udp-probe timeout\n");
            return 1;
        }
        if (ready < 0) {
            if (errno == EINTR)
                continue;
            perror("select");
            return 2;
        }

        char buf[1500];
        struct sockaddr_in from;
        socklen_t from_len = sizeof(from);
        ssize_t n = recvfrom(fd, buf, sizeof(buf) - 1, 0,
                             (struct sockaddr *)&from, &from_len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            perror("recvfrom");
            return 2;
        }
        buf[n] = '\0';
        fputs(buf, stdout);
        if (n == 0 || buf[n - 1] != '\n')
            fputc('\n', stdout);
        return 0;
    }
}
EOF

echo "[task2] Building udp probe with ${CC}"
"${CC}" -static -fno-PIE -no-pie -O2 -o "${OUT_ABS}" "${WORKDIR}/probe.c"
chmod 0755 "${OUT_ABS}"
echo "[task2] Wrote ${OUT_ABS} ($(wc -c < "${OUT_ABS}") bytes)"
