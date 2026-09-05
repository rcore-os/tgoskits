#!/usr/bin/env bash
# Build a fully static aarch64 sysbench — no dynamic loader, no shared libs — so
# the SAME binary runs under the board's glibc Linux AND under StarryOS, needs no
# board internet, and sidesteps the "error while loading shared libraries" risk
# that an apt-installed (glibc-dynamic, libluajit/…-linked) sysbench carries under
# StarryOS's loader.
#
# Builds natively on an arm64 host through an Alpine/musl container (no emulation,
# ~2-3 min when the apk mirror is responsive). Output: ./sysbench-static-aarch64.
# Verify the printed `file` line says "statically linked" before deploying.
#
# Requires Docker/OrbStack on PATH (e.g. export PATH="$HOME/.orbstack/bin:$PATH").
set -euo pipefail
cd "$(dirname "$0")"
OUT="$PWD"

docker run --rm --platform linux/arm64 -v "$OUT":/out alpine:3.23 sh -c '
  set -e
  # apk with timeout+retry — a busy OrbStack network can stall the mirror.
  n=0; until timeout 120 apk add --no-cache build-base bash autoconf automake \
      libtool pkgconf linux-headers git >/dev/null 2>&1; do
    n=$((n+1)); [ "$n" -ge 3 ] && { echo "apk add failed after $n tries"; exit 2; }
    echo "apk retry $n"; sleep 3
  done
  git config --global advice.detachedHead false
  cd /tmp && rm -rf sysbench
  git clone --depth 1 --branch 1.0.20 https://github.com/akopytov/sysbench.git 2>/dev/null
  cd sysbench
  ./autogen.sh >/tmp/autogen.log 2>&1
  # -static -no-pie: force a true static, non-PIE link (Alpine gcc defaults to
  # -fPIE/-pie, which otherwise silently yields a dynamic binary).
  ./configure --without-mysql --without-pgsql \
    CFLAGS="-O2 -fno-pie" LDFLAGS="-static -no-pie" >/tmp/configure.log 2>&1 \
    || { tail -30 /tmp/configure.log; exit 1; }
  make -j"$(nproc)" >/tmp/make.log 2>&1 || { tail -50 /tmp/make.log; exit 1; }
  cp src/sysbench /out/sysbench-static-aarch64
  echo "=== file ==="; file /out/sysbench-static-aarch64
'
echo "built: $OUT/sysbench-static-aarch64"
