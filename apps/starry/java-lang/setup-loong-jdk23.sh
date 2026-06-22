#!/usr/bin/env bash
# setup-loong-jdk23.sh — reproducibly cross-compile a NATIVE loongarch64-musl OpenJDK 23
# for the java-lang carpet's loongarch64 JDK23 cell, and stage the result where prebuild.sh
# expects it ($JAVA_DL_ROOT/jdk-multi/jdk23/openjdk23-loongarch64-musl-srcbuild.tar.gz).
#
# WHY source build: there is no prebuilt musl OpenJDK 23 for loongarch64 — upstream/Alpine
# ship only 17/21/25, and the Loongson distribution is old-abi (abi1.0) glibc that does not
# run under the upstream abi used by the Alpine-musl rootfs. So we cross-compile the Loongson
# LoongArch port (a finalized release tag) against the loongarch64-linux-musl toolchain.
#
# The 4 musl-port source fixes are in loong-jdk23-musl-port.patch (alongside this script).
# Host prerequisites: a loongarch64-linux-musl cross GCC (tested: musl.cc 13.2.0 at
# /opt/loongarch64-linux-musl-cross), curl, tar, git, autoconf, and the host's API headers
# for alsa/cups/fontconfig/X11 (libasound2-dev libcups2-dev libfontconfig-dev libx11-dev).
# Run once; the tarball it produces is then consumed by prebuild.sh on every carpet run.
set -euo pipefail

JDK_TAG="jdk-23+25-ls-0"                                  # Loongson LoongArch port, JDK23 GA
SRC_REPO="https://github.com/loongson/jdk.git"
DL="${JAVA_DL_ROOT:-/home/heke/rcore/download}"
JM="$DL/jdk-multi"
SRC="$DL/loong-jdk23-src"
CROSS="${LOONG_MUSL_CROSS:-/opt/loongarch64-linux-musl-cross}"
SYSROOT="$CROSS/loongarch64-linux-musl"
BOOTJDK="$DL/boot-jdk23-x64-glibc"                        # glibc x64 JDK23 (build/boot JDK)
OUT="$JM/jdk23/openjdk23-loongarch64-musl-srcbuild.tar.gz"
HERE="$(cd "$(dirname "$0")" && pwd)"
ALPINE_LOONG="https://dl-cdn.alpinelinux.org/alpine/edge/main/loongarch64"

[ -x "$CROSS/bin/loongarch64-linux-musl-gcc" ] || { echo "ERROR: cross GCC not at $CROSS/bin (set LOONG_MUSL_CROSS)"; exit 1; }

# 1. boot/build JDK (glibc x64 JDK23) — same feature version as the build target.
if [ ! -x "$BOOTJDK/bin/javac" ]; then
    echo "== fetching glibc x64 JDK23 boot-JDK =="
    mkdir -p "$BOOTJDK"
    curl -fsSL "https://download.bell-sw.com/java/23.0.2+9/bellsoft-jdk23.0.2+9-linux-amd64.tar.gz" -o /tmp/bjdk23.tgz
    tar -xzf /tmp/bjdk23.tgz -C "$BOOTJDK" --strip-components=1 && rm -f /tmp/bjdk23.tgz
fi

# 2. source: clone the finalized JDK23 LoongArch release tag + apply the musl-port patch.
if [ ! -f "$SRC/configure" ]; then
    echo "== cloning $SRC_REPO @ $JDK_TAG =="
    git clone --depth 1 --branch "$JDK_TAG" "$SRC_REPO" "$SRC"
fi
echo "== applying loong-jdk23-musl-port.patch =="
( cd "$SRC" && git apply --check "$HERE/loong-jdk23-musl-port.patch" 2>/dev/null && git apply "$HERE/loong-jdk23-musl-port.patch" ) \
    || echo "  (patch already applied or partially present — continuing)"

# 3. cross sysroot: the JDK build needs target API headers + a libX11/libasound to link
#    against. The API headers are arch-independent (copy from host); the .so must be real
#    loongarch-musl (fetch from Alpine, the same source that builds openjdk21/25-loong).
echo "== staging target headers + libs into cross sysroot =="
for d in alsa cups fontconfig X11; do
    [ -d "/usr/include/$d" ] && cp -rn "/usr/include/$d" "$SYSROOT/include/" 2>/dev/null || true
done
mkdir -p "$DL/.alpine-loong-libs"; cd "$DL/.alpine-loong-libs"
[ -f APKINDEX ] || { curl -fsSL "$ALPINE_LOONG/APKINDEX.tar.gz" -o APKINDEX.tar.gz && tar -xzf APKINDEX.tar.gz; }
apkver() { awk -v p="$1" 'BEGIN{RS="";FS="\n"}{n="";v="";for(i=1;i<=NF;i++){if($i~/^P:/)n=substr($i,3);if($i~/^V:/)v=substr($i,3)} if(n==p)print v}' APKINDEX | head -1; }
for pkg in libx11 libxcb libxau libxdmcp libxext libxrender libxrandr libxtst libxi libxt alsa-lib; do
    v="$(apkver "$pkg")"; [ -n "$v" ] || { echo "  WARN no $pkg in Alpine index"; continue; }
    f="${pkg}-${v}.apk"; [ -f "$f" ] || curl -fsSL "$ALPINE_LOONG/$f" -o "$f"
    mkdir -p "ext/$pkg"; tar -xzf "$f" -C "ext/$pkg" 2>/dev/null || true
done
find ext -name "*.so*" -exec cp -P {} "$SYSROOT/lib/" \; 2>/dev/null || true
# unversioned dev symlinks for the link step
( cd "$SYSROOT/lib" && for b in libX11 libXext libXrender libXrandr libXtst libXi libXt libXau libXdmcp libxcb libasound; do
    r="$(ls ${b}.so.* 2>/dev/null | grep -E '\.so\.[0-9]+$' | head -1)"; [ -n "$r" ] && [ ! -e "${b}.so" ] && ln -s "$r" "${b}.so" || true
  done )

# 4. configure: cross x86_64-linux -> loongarch64-linux-musl, headless, bundled media libs.
#    --build forces linux (avoids WSL mis-detecting the build host as Windows via /mnt/c).
echo "== configure =="
cd "$SRC"; rm -rf build/.configure-support
PATH="$(echo "$PATH" | tr ':' '\n' | grep -v '^/mnt/' | paste -sd: -)" \
bash configure \
    --build=x86_64-unknown-linux-gnu \
    --openjdk-target=loongarch64-linux-musl \
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

IMG="$SRC/build/linux-loongarch64-server-release/images/jdk"
[ -x "$IMG/bin/java" ] || { echo "ERROR: image not produced at $IMG"; exit 2; }
readelf -h "$IMG/bin/java" | grep -q LoongArch || { echo "ERROR: produced java is not LoongArch"; exit 2; }

# 6. tar where prebuild.sh expects it (top dir 'jdk' so untar_strip1 lands the contents).
echo "== packaging -> $OUT =="
mkdir -p "$(dirname "$OUT")"
tar -czf "$OUT" -C "$(dirname "$IMG")" jdk
echo "DONE: $OUT ($(du -h "$OUT" | cut -f1)) — native loongarch64-musl OpenJDK 23"
