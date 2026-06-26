#!/usr/bin/env bash
# setup-rv-jdk25.sh — reproducibly cross-compile a NATIVE riscv64-musl OpenJDK 25 (server VM)
# for the java-lang carpet's riscv64 JDK25 cell, and stage the result where prebuild.sh
# expects it ($JAVA_DL_ROOT/jdk-multi/jdk25/openjdk25-riscv64-musl-srcbuild.tar.gz).
#
# WHY source build: the prebuilt riscv64 musl OpenJDK 25 options both fail on StarryOS:
#   - Alpine's riscv64 openjdk25 is a Zero-interpreter VM (no server-compiler codegen path);
#   - BellSoft's riscv64 JDK25 (server VM) emits a reserved RVC instruction in its CodeCache,
#     C.LUI x5,imm=0 (0x6281), which traps as IllegalInstruction at run time.
# Our patch guards the lui->c.lui peephole with (imm & 0xfff)==0 (mirroring c_lui's own
# precondition; upstream's imm!=0 guard is insufficient because a lui with the low 12 bits set
# but imm[17:12]==0 still compresses to the reserved C.LUI rd,0). A server VM built from the
# patched source therefore never emits that instruction. We cross-compile
# it against the riscv64-linux-musl toolchain. The 3 musl-port source fixes are in
# rv-jdk25-musl-port.patch (alongside this script).
#
# Host prerequisites: a riscv64-linux-musl cross GCC (tested: 11.2.0 at
# /opt/riscv64-linux-musl-cross), curl, tar, git, autoconf, and the host's API headers for
# alsa/cups/fontconfig/X11. Run once; the tarball it produces is consumed by prebuild.sh.
set -euo pipefail

JDK_TAG="jdk-25.0.4+5"                                    # OpenJDK jdk25u update release
SRC_REPO="https://github.com/openjdk/jdk25u.git"
# Default cache mirrors prebuild.sh's JAVA_DL_ROOT default so a tarball built here lands
# exactly where prebuild.sh later reads it. A developer with an existing cache overrides
# JAVA_DL_ROOT (e.g. JAVA_DL_ROOT=/path/to/download).
DL="${JAVA_DL_ROOT:-${STARRY_STAGING_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}/.cache/java-dl}"
JM="$DL/jdk-multi"
SRC="$DL/rv-jdk25-src"
CROSS="${RV_MUSL_CROSS:-/opt/riscv64-linux-musl-cross}"
SYSROOT="$CROSS/riscv64-linux-musl"
BOOTJDK="$DL/boot-jdk25-x64-glibc"                        # glibc x64 JDK25 (build/boot JDK)
OUT="$JM/jdk25/openjdk25-riscv64-musl-srcbuild.tar.gz"
HERE="$(cd "$(dirname "$0")" && pwd)"
ALPINE_RV="https://dl-cdn.alpinelinux.org/alpine/edge/main/riscv64"

[ -x "$CROSS/bin/riscv64-linux-musl-gcc" ] || { echo "ERROR: cross GCC not at $CROSS/bin (set RV_MUSL_CROSS)"; exit 1; }

# 1. boot/build JDK (glibc x64 JDK25) — same feature version as the build target.
if [ ! -x "$BOOTJDK/bin/javac" ]; then
    echo "== fetching glibc x64 JDK25 boot-JDK =="
    mkdir -p "$BOOTJDK"
    curl -fsSL "https://download.bell-sw.com/java/25+37/bellsoft-jdk25+37-linux-amd64.tar.gz" -o /tmp/bjdk25.tgz
    tar -xzf /tmp/bjdk25.tgz -C "$BOOTJDK" --strip-components=1 && rm -f /tmp/bjdk25.tgz
fi

# 2. source: clone the jdk25u update tag + apply the musl-port patch.
if [ ! -f "$SRC/configure" ]; then
    echo "== cloning $SRC_REPO @ $JDK_TAG =="
    git clone --depth 1 --branch "$JDK_TAG" "$SRC_REPO" "$SRC"
fi
echo "== applying rv-jdk25-musl-port.patch =="
( cd "$SRC" && git apply --check "$HERE/rv-jdk25-musl-port.patch" 2>/dev/null && git apply "$HERE/rv-jdk25-musl-port.patch" ) \
    || echo "  (patch already applied or partially present — continuing)"

# 3. cross sysroot: the JDK build needs target API headers + a libX11/libasound to link
#    against. The API headers are arch-independent (copy from host); the .so must be real
#    riscv64-musl (fetch from Alpine, the same source that builds openjdk25-riscv64).
echo "== staging target headers + libs into cross sysroot =="
for d in alsa cups fontconfig X11; do
    [ -d "/usr/include/$d" ] && cp -rn "/usr/include/$d" "$SYSROOT/include/" 2>/dev/null || true
done
mkdir -p "$DL/.alpine-rv-libs"; cd "$DL/.alpine-rv-libs"
[ -f APKINDEX ] || { curl -fsSL "$ALPINE_RV/APKINDEX.tar.gz" -o APKINDEX.tar.gz && tar -xzf APKINDEX.tar.gz; }
apkver() { awk -v p="$1" 'BEGIN{RS="";FS="\n"}{n="";v="";for(i=1;i<=NF;i++){if($i~/^P:/)n=substr($i,3);if($i~/^V:/)v=substr($i,3)} if(n==p)print v}' APKINDEX | head -1; }
for pkg in libx11 libxcb libxau libxdmcp libxext libxrender libxrandr libxtst libxi libxt alsa-lib; do
    v="$(apkver "$pkg")"; [ -n "$v" ] || { echo "  WARN no $pkg in Alpine index"; continue; }
    f="${pkg}-${v}.apk"; [ -f "$f" ] || curl -fsSL "$ALPINE_RV/$f" -o "$f"
    mkdir -p "ext/$pkg"; tar -xzf "$f" -C "ext/$pkg" 2>/dev/null || true
done
find ext -name "*.so*" -exec cp -P {} "$SYSROOT/lib/" \; 2>/dev/null || true
# unversioned dev symlinks for the link step
( cd "$SYSROOT/lib" && for b in libX11 libXext libXrender libXrandr libXtst libXi libXt libXau libXdmcp libxcb libasound; do
    r="$(ls ${b}.so.* 2>/dev/null | grep -E '\.so\.[0-9]+$' | head -1)"; [ -n "$r" ] && [ ! -e "${b}.so" ] && ln -s "$r" "${b}.so" || true
  done )

# 4. configure: cross x86_64-linux -> riscv64-linux-musl, headless, bundled media libs.
#    --build forces linux (avoids WSL mis-detecting the build host as Windows via /mnt/c).
echo "== configure =="
cd "$SRC"; rm -rf build/.configure-support
PATH="$(echo "$PATH" | tr ':' '\n' | grep -v '^/mnt/' | paste -sd: -)" \
bash configure \
    --build=x86_64-unknown-linux-gnu \
    --openjdk-target=riscv64-linux-musl \
    --with-toolchain-path="$CROSS/bin" --with-sysroot="$SYSROOT" \
    --x-includes="$SYSROOT/include" --x-libraries="$SYSROOT/lib" \
    --with-boot-jdk="$BOOTJDK" --with-build-jdk="$BOOTJDK" \
    --with-jvm-variants=server --enable-headless-only \
    --with-freetype=bundled --with-libjpeg=bundled --with-giflib=bundled \
    --with-libpng=bundled --with-zlib=bundled --with-lcms=bundled \
    --disable-warnings-as-errors

# 5. build the JDK image.
echo "== make images =="
unset CLASSPATH
PATH="$(echo "$PATH" | tr ':' '\n' | grep -v '^/mnt/' | paste -sd: -)" make images

IMG="$SRC/build/linux-riscv64-server-release/images/jdk"
[ -x "$IMG/bin/java" ] || { echo "ERROR: image not produced at $IMG"; exit 2; }
readelf -h "$IMG/bin/java" | grep -q RISC-V || { echo "ERROR: produced java is not RISC-V"; exit 2; }

# 6. tar where prebuild.sh expects it (top dir 'jdk' so untar_strip1 lands the contents).
echo "== packaging -> $OUT =="
mkdir -p "$(dirname "$OUT")"
tar -czf "$OUT" -C "$(dirname "$IMG")" jdk
echo "DONE: $OUT ($(du -h "$OUT" | cut -f1)) — native riscv64-musl OpenJDK 25 (server VM)"
