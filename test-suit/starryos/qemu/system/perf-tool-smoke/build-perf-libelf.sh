#!/bin/sh
# Native aarch64 Alpine (musl) build WITH libelf (user-symbol names) + zlib. Run:
#   PERF_BUILD_DIR=$HOME/perf-port-build docker run --rm --platform linux/arm64 \
#     -v $PERF_BUILD_DIR:/build alpine:3.21 sh /build/build-perf-libelf.sh
# (needs linux-6.1.tar.xz + musl-compat.h in /build; produces linux-6.1-src/tools/perf/perf)
#
# Reproducibility: the base image is PINNED to a concrete Alpine tag (alpine:3.21,
# not the floating alpine:latest) so the toolchain/libelf versions are stable
# across pulls. `set -e` plus explicit make-failure handling below mean a broken
# build fails loudly (non-zero) instead of silently emitting a bad binary. The
# recipe prints the artifact's SHA-256; that hash is committed as perf.sha256 and
# enforced by CMakeLists.txt, so the checked-in binary's provenance is verifiable.
set -e
echo "-- apk deps --"
apk add --no-cache build-base linux-headers flex bison \
  elfutils-dev zlib-dev zlib-static perl python3 xz \
  zstd-dev zstd-static xz-dev xz-static bzip2-dev bzip2-static >/dev/null
# Alpine's fortify wrappers (first in the -isystem path) pull fortify-headers.h
# into perf's .S test files, whose `#if _FORTIFY_SOURCE > 2 && __has_builtin(...)`
# mis-parses under assembler-cpp. Remove the wrapper dir (optional hardening;
# gcc tolerates the now-missing -isystem path) so libc headers are used directly.
rm -rf /usr/include/fortify
cd /build
# Always start from a clean tree: prior iterations left stale dep files (some
# referencing the now-removed fortify headers), which break make's dep tracking.
rm -rf linux-6.1-src
echo "-- extract fresh (native aarch64, fast) --"
mkdir linux-6.1-src && tar xf linux-6.1.tar.xz -C linux-6.1-src --strip-components=1
cd linux-6.1-src/tools/perf
# Alpine's static libelf.a references zstd/lzma/bz2 (compressed ELF sections), but
# perf only adds those to EXTLIBS in the DWARF case. Add them to the libelf group
# so the static link (LIBS wraps EXTLIBS in --start-group) resolves them.
sed -i 's/EXTLIBS += -lelf$/EXTLIBS += -lelf -lz -lzstd -llzma -lbz2/' Makefile.config
echo "-- build (static, +libelf +zlib) --"
# The make status must propagate: a `&& echo OK || echo FAIL` tail would make the
# line always succeed and let `set -e` sail past a broken build. Capture the real
# status, print diagnostics either way, and hard-fail (non-zero) on failure.
if make -j"$(nproc)" ARCH=arm64 LDFLAGS="-static" WERROR=0 \
    EXTRA_CFLAGS="-Wno-error -Wno-error=implicit-function-declaration -U_FORTIFY_SOURCE -include /build/musl-compat.h" \
    NO_LIBUNWIND=1 NO_LIBDW=1 NO_DWARF=1 NO_LIBTRACEEVENT=1 NO_LIBBPF=1 \
    NO_BPF_SKEL=1 NO_SLANG=1 NO_GTK2=1 NO_LIBPERL=1 NO_LIBPYTHON=1 \
    NO_LIBNUMA=1 NO_LIBCRYPTO=1 NO_JVMTI=1 NO_LIBBABELTRACE=1 \
    NO_LIBDEBUGINFOD=1 NO_LIBLLVM=1 NO_LIBZSTD=1 \
    > /build/alpine-build.out 2>&1; then
  echo "MAKE_OK"
else
  st=$?
  echo "MAKE_FAIL (status=$st)"
  echo "-- LIBELF feature line --"; grep -iE "libelf|gelf" /build/alpine-build.out | head -3
  echo "-- last 40 lines of build log --"; tail -40 /build/alpine-build.out
  exit "$st"
fi
echo "-- LIBELF feature line --"; grep -iE "libelf|gelf" /build/alpine-build.out | head -3
tail -20 /build/alpine-build.out
if [ ! -f perf ]; then
  echo "NO perf binary" >&2
  exit 1
fi
ls -la perf && file perf 2>/dev/null || true
# Record the artifact identity. Commit this value as perf.sha256 next to the
# binary; CMakeLists.txt enforces the match so the checked-in asset is verifiable.
echo "-- perf sha256 (commit as perf.sha256) --"
sha256sum perf 2>/dev/null || shasum -a 256 perf
