#!/usr/bin/env bash
# Reproducible recipe for the on-target `perf` binary used by perf-tool-smoke
# (and the board perf validation). Builds upstream linux-6.1 `perf`, statically
# linked for aarch64-musl, so it runs under StarryOS (static-ELF loader) via the
# Linux-compatible perf_event_open(2) ABI the kernel implements.
#
# Usage:  ./build-perf.sh            # -> ./perf (stripped, ~2.8 MB)
#         PERF_VER=6.1 ./build-perf.sh
#
# WHY these choices:
#  - linux-6.1 matches the OrangePi board kernel (6.1.43) and still vendors
#    tools/lib/traceevent (removed ~6.7), simplifying a static build.
#  - GCC 11 musl cross toolchain (tgoskits container): lenient enough to build
#    vanilla perf without Alpine's musl patch set (newer GCCs error on perf-6.1's
#    calloc arg-order / implicit basename; see docs for the libelf follow-up).
#  - NO_LIBELF: user symbols resolve as offsets on-target; kernel symbols resolve
#    fully via /proc/kallsyms. Add aarch64-musl libelf for user-symbol *names*
#    (tracked follow-up).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PERF_VER="${PERF_VER:-6.1}"
IMAGE="${TGOS_IMAGE:-ghcr.io/rcore-os/tgoskits-container:latest}"
WORK="${PERF_BUILD_DIR:-$HOME/perf-port-build}"
mkdir -p "$WORK"

docker run --rm --platform linux/amd64 -v "$WORK:/build" -v "$HERE:/out" "$IMAGE" bash -lc '
  set -e
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq flex bison >/dev/null 2>&1
  cd /build
  V='"$PERF_VER"'
  [ -f linux-$V.tar.xz ] || wget -q https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$V.tar.xz
  [ -d linux-$V ] || tar xf linux-$V.tar.xz
  cd linux-$V/tools/perf
  make -j"$(nproc)" ARCH=arm64 CROSS_COMPILE=aarch64-linux-musl- \
    LDFLAGS="-static" EXTRA_CFLAGS="-Wno-error" \
    NO_LIBELF=1 NO_LIBDW=1 NO_DWARF=1 NO_LIBUNWIND=1 NO_LIBCAP=1 \
    NO_LIBBPF=1 NO_BPF_SKEL=1 NO_SLANG=1 NO_GTK2=1 NO_LIBPERL=1 \
    NO_LIBPYTHON=1 NO_LIBNUMA=1 NO_LIBCRYPTO=1 NO_LIBZSTD=1 \
    NO_LZMA=1 NO_ZLIB=1 NO_JVMTI=1 NO_LIBBABELTRACE=1 NO_AUXTRACE=1 \
    NO_LIBDEBUGINFOD=1 NO_LIBTRACEEVENT=1 NO_LIBLLVM=1
  aarch64-linux-musl-strip -o /out/perf perf
  echo "built: $(file /out/perf)"
'
