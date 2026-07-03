#!/usr/bin/env bash
# prebuild.sh — provision the J2SE library + JSE standard-library carpet for StarryOS.
#
# This is the J2SE case for #764 "JSE classic tools / common libraries": a set of real
# third-party J2SE libraries (jackson / guava / commons-lang3 / h2 / slf4j / logback /
# sqlite-jdbc + native JNI / lombok) and a 15-module JSE standard-library suite, each run
# on-target by OpenJDK 17 with an anchored self-check marker.
#
# ── SOURCE-ONLY REPO, REPRODUCIBLE BUILD (the java-lang model) ─────────────────────────
#   The source tree keeps ONLY source + manifests: the carpet .java under programs/, this
#   script, and the pinned dependency coordinates below (Maven groupId:artifactId:version +
#   sha256). NO compiled .jar and NO native .so are committed. Exactly like the merged
#   java-lang case (stage_backcompat), prebuild:
#     (a) FETCHES every third-party dependency jar from Maven Central BY sha256 into a cache
#         (JAVA_DL_ROOT), re-used network-free on later runs;
#     (b) COMPILES the carpet classes IN-PREBUILD with `javac --release 17` from the committed
#         programs/{lib-carpets,jse-suite}/*.java, the fetched deps on the classpath and lombok
#         on the annotation --processor-path, producing carpets.jar (arch-independent bytecode);
#     (c) STAGES carpets.jar + the fetched dependency jars into the overlay at /root/jse{,/libs}.
#   The compile uses the HOST javac (not the staged target-arch JDK17, whose javac is a
#   riscv64/loongarch64/aarch64 binary that cannot exec on an x86_64 build host); the emitted
#   `--release 17` bytecode is identical for every target arch. carpets.jar is cached so the
#   four per-arch prebuild runs compile it at most once.
#
# ── Per-arch JDK17 source — a fresh checkout provisions EVERY arch with a download ──────
#   StarryOS is libc-agnostic (it runs BOTH musl and glibc binaries), so any prebuilt JDK17
#   with matching major version works, regardless of the libc it was built against:
#     x86_64 / aarch64 : Alpine v3.22/community openjdk17 apks   (musl native)
#     loongarch64      : Alpine edge/community openjdk17-loongarch apks (musl native)
#     riscv64          : Adoptium Temurin 17.0.19+10 prebuilt GLIBC tarball (downloadable),
#                        bridged by a staged real Debian glibc runtime closure so the JDK's own
#                        ld-linux interp resolves its libc.so.6 references (stage_glibc_runtime_rv).
#   Alpine ships NO riscv64 openjdk17 (only openjdk21/25 for riscv64), so the riscv64 cell uses a
#   downloadable prebuilt GLIBC JDK17 instead of a musl one. This is the SAME "download a glibc
#   JDK + stage a real Debian glibc runtime closure" mechanism the merged java-lang case uses for
#   its riscv64 JDK23 cell (BellSoft generic-glibc bridged by the same libc6 deb). BellSoft
#   Liberica generic-glibc and the Debian apt openjdk-17 riscv64 build ship the same JDK and
#   would work identically; Adoptium Temurin is the pinned source here.
#
# ── NATIVE sqlite-jdbc JNI (.so), per arch ────────────────────────────────────────────
#   x86_64 / aarch64 : the xerial sqlite-jdbc jar BUNDLES a musl JNI at
#                      org/sqlite/native/Linux-Musl/{x86_64,aarch64}/libsqlitejdbc.so; the
#                      driver self-extracts + dlopens it at run time (run-jse.sh sets no
#                      lib.path), so nothing is staged and nothing is committed.
#   riscv64          : the rv64 JDK17 is the prebuilt GLIBC build, so the matching JNI is the
#                      sqlite-jdbc jar's OWN bundled GLIBC riscv64 native
#                      (org/sqlite/native/Linux/riscv64/libsqlitejdbc.so). prebuild extracts it
#                      from the already-fetched, sha256-pinned jar (fully reproducible, no extra
#                      download, no cross-build) and stages it at /root/jse/native.
#   loongarch64      : the upstream jar ships NO loongarch64 native at all (neither glibc nor
#                      musl), and the loong JDK17 is Alpine-musl, so a musl loongarch64 JNI is
#                      CROSS-COMPILED IN-PREBUILD from xerial/sqlite-jdbc's OWN C source
#                      (NativeDB.c) + the official SQLite amalgamation — exactly as xerial's
#                      Makefile builds it (same feature flags) — with loongarch64-linux-musl-gcc.
#                      Both source inputs are sha256-pinned so a clean checkout reproduces it
#                      (the small C lib compiles in ~1 min). If the loong musl cross-toolchain is
#                      genuinely absent it degrades to a DOCUMENTED SKIP (never a silent fallback).
#
# ── ROOTFS SIZE ───────────────────────────────────────────────────────────────────────
#   The harness copies the ~1 GiB base alpine rootfs to a per-app image, runs THIS prebuild,
#   then injects the overlay via debugfs WITHOUT resizing — large files get silently
#   truncated if the fs is full. One JDK17 (~330 MiB) + the dependency jars (~24 MiB) +
#   carpets.jar fit after we grow the image to 2.5 GiB (truncate + e2fsck + resize2fs). The
#   running JVM only maps a -Xmx384m heap, so the larger image is free.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"

PROG="$app_dir/programs"

# ── Asset cache root (PORTABLE) ───────────────────────────────────────────────────────
# Holds the JDK17 distributions, the Maven dependency jars, the compiled carpets.jar, the
# riscv64 glibc runtime deb, and the per-arch sqlite JNI .so (same tree layout
# openjdk-multi/java-lang prep use). On a clean machine this dir does not pre-exist; each asset
# is fetched from its OFFICIAL URL and re-used. A developer who already has the assets points
# JAVA_DL_ROOT at their cache and every fetch short-circuits.
DL="${JAVA_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/java-dl}"
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
MAVEN_CENTRAL="${MAVEN_CENTRAL:-https://repo1.maven.org/maven2}"
ROOTFS_SIZE="${JSE_ROOTFS_SIZE:-2560M}"

# ── Portable fetch-ensure layer ───────────────────────────────────────────────────────
# ensure_asset <abs-local-path> <official-url> [sha256]
#   Cache hit (sha256 matches when given) -> used as-is, zero network. Otherwise curl the URL
#   to a temp file, verify sha256 when given (mismatch = hard error), atomically move into
#   place. An empty/omitted sha skips verification (rolling Alpine apks: cache copy is the
#   pinned golden, URL is a best-effort refill). An empty URL with no cache is a hard error.
ensure_asset() {
    local dest="$1" url="$2" want="${3:-}"
    if [[ -f "$dest" ]]; then
        if [[ -n "$want" ]] && command -v sha256sum >/dev/null 2>&1; then
            local have; have="$(sha256sum "$dest" | cut -d' ' -f1)"
            if [[ "$have" == "$want" ]]; then echo "prebuild: cache hit $dest (sha256 ok)"; return 0; fi
            echo "prebuild: cache file $dest sha256 mismatch (have $have want $want) — refetching" >&2
            rm -f "$dest"
        else
            echo "prebuild: cache hit $dest"; return 0
        fi
    fi
    command -v curl >/dev/null 2>&1 || { echo "prebuild: need curl to fetch $url (no cached $dest)" >&2; exit 4; }
    [[ -n "$url" ]] || { echo "prebuild: no cached $dest and no URL to fetch it from" >&2; exit 4; }
    echo "prebuild: fetching $(basename "$dest") <- $url"
    mkdir -p "$(dirname "$dest")"
    curl -fSL --retry 3 --connect-timeout 20 "$url" -o "$dest.tmp"
    if [[ -n "$want" ]] && command -v sha256sum >/dev/null 2>&1; then
        local got; got="$(sha256sum "$dest.tmp" | cut -d' ' -f1)"
        [[ "$got" == "$want" ]] || { echo "prebuild: sha256 mismatch for $url (got $got want $want)" >&2; rm -f "$dest.tmp"; exit 4; }
    fi
    mv -f "$dest.tmp" "$dest"
}

# extract_jar_entry <jar> <entry-path> <dest-file>
#   Extract ONE entry from a jar/zip without assuming a single unzip tool (unzip -> jar ->
#   python3), then install it at <dest-file>. Used to pull the jar-bundled glibc riscv64 sqlite
#   JNI out of the sha256-pinned sqlite-jdbc jar (no extra download / no cross-build).
extract_jar_entry() {
    local jar="$1" entry="$2" dest="$3"
    [[ -f "$jar" ]] || { echo "prebuild: extract_jar_entry: missing jar $jar" >&2; return 1; }
    local t; t="$(mktemp -d)"
    if command -v unzip >/dev/null 2>&1; then
        unzip -oq "$jar" "$entry" -d "$t" >/dev/null 2>&1 || true
    elif command -v jar >/dev/null 2>&1; then
        ( cd "$t" && jar xf "$jar" "$entry" ) >/dev/null 2>&1 || true
    elif command -v python3 >/dev/null 2>&1; then
        python3 - "$jar" "$entry" "$t" <<'PY' || true
import sys, zipfile
jar, entry, dest = sys.argv[1], sys.argv[2], sys.argv[3]
with zipfile.ZipFile(jar) as z:
    z.extract(entry, dest)
PY
    else
        echo "prebuild: need unzip, jar, or python3 to extract $entry from $(basename "$jar")" >&2
        rm -rf "$t"; return 1
    fi
    [[ -f "$t/$entry" ]] || { echo "prebuild: entry $entry not found in $(basename "$jar")" >&2; rm -rf "$t"; return 1; }
    install -Dm0644 "$t/$entry" "$dest"
    rm -rf "$t"
}

ensure_host_tools() {
    local missing=()
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v curl      >/dev/null 2>&1 || missing+=(curl)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsprogs)
    # riscv64 also needs 'ar' (binutils) to unpack the Debian libc6 .deb for the glibc runtime
    # bridge, and 'unzip' to pull the jar-bundled glibc riscv64 sqlite JNI (jar/python3 fallbacks
    # exist, so unzip is best-effort).
    if [[ "$arch" == riscv64 ]]; then
        command -v ar    >/dev/null 2>&1 || missing+=(binutils)
        command -v unzip >/dev/null 2>&1 || missing+=(unzip)
    fi
    if [[ "$arch" == loongarch64 ]]; then
        # cross-compiling the musl loongarch64 sqlite JNI needs unzip (SQLite amalgamation zip) +
        # perl (the two sqlite3.c source patches xerial applies). The loongarch64-linux-musl-gcc
        # cross-toolchain is provided out-of-band (StarryOS .starry-env.sh PATH), not apt.
        command -v unzip >/dev/null 2>&1 || missing+=(unzip)
        command -v perl  >/dev/null 2>&1 || missing+=(perl)
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

# The reproducible in-prebuild compile needs a HOST javac (any JDK >= 17; --release 17 targets
# bytecode 61). The staged target-arch JDK17 cannot be used because its javac is a
# riscv64/loongarch64/aarch64 binary. If the host has no javac, install one via apt (a
# build-time toolchain dependency — NOT a committed binary).
ensure_host_jdk() {
    command -v javac >/dev/null 2>&1 && return 0
    if command -v apt-get >/dev/null 2>&1; then
        echo "prebuild: no host javac — installing a JDK for the in-prebuild compile"
        apt-get update
        apt-get install -y --no-install-recommends default-jdk-headless \
            || apt-get install -y --no-install-recommends openjdk-17-jdk-headless
    fi
    command -v javac >/dev/null 2>&1 || {
        echo "prebuild: host javac is required to compile carpets.jar from source (install a JDK17+)" >&2
        exit 1; }
}

# Grow the per-app rootfs so the injected JDK + jars fit without truncation. Idempotent.
grow_rootfs() {
    [[ -f "$rootfs_img" ]] || { echo "prebuild: rootfs image missing: $rootfs_img" >&2; exit 2; }
    # Grow-only: the per-app base image is shared and may already be larger (another app grew it);
    # NEVER shrink it (truncate -s to a smaller size corrupts the ext4). Only truncate up.
    local cur target
    cur=$(stat -c %s "$rootfs_img"); target=$(( ${ROOTFS_SIZE%M} * 1024 * 1024 ))
    [[ "$cur" -lt "$target" ]] && truncate -s "$ROOTFS_SIZE" "$rootfs_img"
    e2fsck -f -y "$rootfs_img" >/dev/null 2>&1 || true
    resize2fs "$rootfs_img" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed on $rootfs_img" >&2; exit 2; }
    echo "prebuild: rootfs sized to $(( $(stat -c %s "$rootfs_img")/1024/1024 )) MiB"
}

untar_strip1() {
    local arc="$1" dest="$2"
    [[ -f "$arc" ]] || { echo "prebuild: missing archive $arc" >&2; exit 2; }
    mkdir -p "$dest"; tar xzf "$arc" -C "$dest" --strip-components=1
}

# ── JDK17 provisioning (per-arch; every arch is DOWNLOADABLE on a clean checkout) ───────
# loongarch64 apks are pinned by sha256. x86_64/aarch64 openjdk17 is a ROLLING Alpine v3.22
# patch level: the fetch targets the CURRENT version (17.0.19_p10-r0; older patch levels age off
# the CDN) with sha left unpinned, and stage_jdk17's prefix-glob (openjdk17-*-*.apk) consumes any
# patch level so a populated JAVA_DL_ROOT holding an older golden is used network-free. riscv64 is
# the Adoptium Temurin prebuilt GLIBC tarball, pinned by sha256 (see JDK17_RISCV_* below).
JDK17_X86AA_VER="${JDK17_X86AA_VER:-17.0.19_p10-r0}"
# riscv64: downloadable prebuilt GLIBC JDK17 (Adoptium Temurin 17.0.19+10). Overridable to a
# mirror / BellSoft / distro build of the SAME major version via the env vars.
JDK17_RISCV_TAR="OpenJDK17U-jdk_riscv64_linux_hotspot_17.0.19_10.tar.gz"
JDK17_RISCV_URL="${JDK17_RISCV_URL:-https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.19%2B10/OpenJDK17U-jdk_riscv64_linux_hotspot_17.0.19_10.tar.gz}"
JDK17_RISCV_SHA="${JDK17_RISCV_SHA:-191cdd904aef8b8a7a91c98d649c7e3dc75b7341f112061231c2094c418fd630}"
ensure_jdk17() {
    local d="$DL/openjdk17-apks/$arch"
    case "$arch" in
        x86_64|aarch64)
            local alp a; [[ "$arch" == x86_64 ]] && alp=x86_64 || alp=aarch64
            for a in openjdk17-jdk openjdk17-jmods openjdk17-jre-headless openjdk17-jre; do
                [[ -n "$(ls "$d/${a}-"*.apk 2>/dev/null | head -1)" ]] && continue
                ensure_asset "$d/${a}-${JDK17_X86AA_VER}.apk" "$ALPINE_CDN/v3.22/community/$alp/${a}-${JDK17_X86AA_VER}.apk"
            done ;;
        loongarch64)
            local v=17.0.17_p10-r0
            ensure_asset "$d/openjdk17-loongarch-jdk-${v}.apk"          "$ALPINE_CDN/edge/community/loongarch64/openjdk17-loongarch-jdk-${v}.apk"          e55611f2280854e9bc4e76785b51decf840015d26888f3c4eb15df9d603cc49c
            ensure_asset "$d/openjdk17-loongarch-jmods-${v}.apk"        "$ALPINE_CDN/edge/community/loongarch64/openjdk17-loongarch-jmods-${v}.apk"        d9ad8763f8d7a13b5ce2618444bc5fcc43081b9c20fed50ee50cedb9f1eedbc1
            ensure_asset "$d/openjdk17-loongarch-jre-headless-${v}.apk" "$ALPINE_CDN/edge/community/loongarch64/openjdk17-loongarch-jre-headless-${v}.apk" 42ae887f2099d44bbaa7531dad11d29da47796ba06637e1259427d5e2a55d80d
            ensure_asset "$d/openjdk17-loongarch-jre-${v}.apk"          "$ALPINE_CDN/edge/community/loongarch64/openjdk17-loongarch-jre-${v}.apk"          9f867f80ce79cbffe51623e38b3085cb62e6d0d98e459425d8452a24e275f26f ;;
        riscv64)
            # Alpine ships NO riscv64 openjdk17 (only 21/25), so use a DOWNLOADABLE prebuilt GLIBC
            # riscv64 JDK17 (Adoptium Temurin). StarryOS runs both musl and glibc; the JDK's own
            # ld-linux interp is satisfied by the Debian glibc closure staged by
            # stage_glibc_runtime_rv. The prebuilt JDK statically bundles zlib/libstdc++/libgcc,
            # so libc6 is its entire external closure.
            ensure_asset "$d/$JDK17_RISCV_TAR" "$JDK17_RISCV_URL" "$JDK17_RISCV_SHA" ;;
    esac
}

# Stage JDK17 into $overlay_dir/opt/jdk17 (full JDK with javac), per-arch source.
stage_jdk17() {
    local jdst="$overlay_dir/opt/jdk17" d="$DL/openjdk17-apks/$arch"
    rm -rf "$jdst"; mkdir -p "$jdst"
    case "$arch" in
        x86_64|aarch64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk17-jdk openjdk17-jmods openjdk17-jre-headless openjdk17-jre; do
                apk="$(ls "$d/${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && tar xzf "$apk" -C "$T" 2>/dev/null || true
            done
            cp -a "$T/usr/lib/jvm/java-17-openjdk/." "$jdst/"; rm -rf "$T" ;;
        loongarch64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk17-loongarch-jdk openjdk17-loongarch-jmods openjdk17-loongarch-jre-headless openjdk17-loongarch-jre; do
                apk="$(ls "$d/${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && tar xzf "$apk" -C "$T" 2>/dev/null || true
            done
            cp -a "$T"/usr/lib/jvm/*/. "$jdst/"; rm -rf "$T" ;;
        riscv64)
            # Adoptium tarball top-level dir is jdk-17.0.19+10/ -> strip it.
            untar_strip1 "$d/$JDK17_RISCV_TAR" "$jdst" ;;
    esac
    [[ -x "$jdst/bin/java" ]] || { echo "prebuild: jdk17 staged without java for $arch" >&2; exit 3; }
    echo "prebuild: jdk17 staged ($(du -sh "$jdst" | cut -f1))"
}

# Stage a real Debian glibc runtime closure for riscv64 so the downloadable prebuilt GLIBC JDK17
# (and the jar-bundled glibc riscv64 sqlite JNI) resolve their libc.so.6 / libm.so.6 /
# ld-linux-riscv64-lp64d.so.1 references. This MIRRORS the merged java-lang app's
# stage_real_glibc_rv byte-for-byte (the SAME Debian trixie libc6 deb + sha256): extract the
# libc6 deb and drop libc/libm/libpthread/librt/libdl into the multiarch search path plus the
# loader at its interp path (/lib/ld-linux-riscv64-lp64d.so.1). StarryOS runs BOTH musl and
# glibc, so the glibc JDK uses its own interp + this closure while the base rootfs stays musl.
# The prebuilt JDK statically bundles zlib/libstdc++/libgcc, so libc6 is the ENTIRE external
# closure it needs (verified via the JDK's readelf NEEDED: libc.so.6 libm.so.6 libpthread.so.0
# libdl.so.2 librt.so.1). No-op on the all-musl arches (x86_64/aarch64/loongarch64).
GLIBC_RV_DEB="libc6_2.41-12+deb13u3_riscv64.deb"
GLIBC_RV_DEB_URL="${GLIBC_RV_DEB_URL:-http://deb.debian.org/debian/pool/main/g/glibc/libc6_2.41-12+deb13u3_riscv64.deb}"
GLIBC_RV_DEB_SHA="${GLIBC_RV_DEB_SHA:-fee42ebb2a148cc0dbc46ba938d8d69495b6dd5250cecafed9d585c567550b7a}"
stage_glibc_runtime_rv() {
    [[ "$arch" == riscv64 ]] || return 0
    local deb="$DL/glibc-debian/riscv64/$GLIBC_RV_DEB"
    ensure_asset "$deb" "$GLIBC_RV_DEB_URL" "$GLIBC_RV_DEB_SHA"
    command -v ar >/dev/null 2>&1 || { echo "prebuild: need 'ar' (binutils) to unpack the libc6 deb" >&2; exit 1; }
    local t; t="$(mktemp -d)"
    ( cd "$t" && ar x "$deb" && tar xf data.tar.* )
    mkdir -p "$overlay_dir/lib/riscv64-linux-gnu" "$overlay_dir/usr/lib/riscv64-linux-gnu"
    cp -a "$t"/usr/lib/riscv64-linux-gnu/. "$overlay_dir/usr/lib/riscv64-linux-gnu/" 2>/dev/null || true
    cp -a "$t"/lib/riscv64-linux-gnu/.     "$overlay_dir/lib/riscv64-linux-gnu/"     2>/dev/null || true
    local ldso; ldso="$(find "$t" -name 'ld-linux-riscv64-lp64d.so.1' 2>/dev/null | head -1)"
    [[ -n "$ldso" ]] && install -Dm0755 "$ldso" "$overlay_dir/lib/ld-linux-riscv64-lp64d.so.1"
    rm -rf "$t"
    [[ -e "$overlay_dir/lib/ld-linux-riscv64-lp64d.so.1" ]] \
        || { echo "prebuild: riscv64 glibc loader not staged from $deb" >&2; exit 4; }
    echo "prebuild: staged REAL Debian glibc runtime for riscv64 (bridges the prebuilt glibc JDK17 + glibc sqlite JNI)"
}

# ── Third-party dependency jars (Maven Central by sha256) ──────────────────────────────
# "<filename> <maven-path-under-repo1> <sha256>". <maven-path> is appended to $MAVEN_CENTRAL.
# Arch-independent (pure JVM bytecode) so the SAME set is staged for every arch. sha256 are the
# host-verified copies (each also cross-checked against Maven Central's published .sha1). These
# are the RUNTIME classpath deps (staged into /root/jse/libs); lombok (below) is compile-only.
DEP_LIBS=(
    "jackson-databind-2.17.2.jar     com/fasterxml/jackson/core/jackson-databind/2.17.2/jackson-databind-2.17.2.jar         c04993f33c0f845342653784f14f38373d005280e6359db5f808701cfae73c0c"
    "jackson-core-2.17.2.jar         com/fasterxml/jackson/core/jackson-core/2.17.2/jackson-core-2.17.2.jar                 721a189241dab0525d9e858e5cb604d3ecc0ede081e2de77d6f34fa5779a5b46"
    "jackson-annotations-2.17.2.jar  com/fasterxml/jackson/core/jackson-annotations/2.17.2/jackson-annotations-2.17.2.jar   873a606e23507969f9bbbea939d5e19274a88775ea5a169ba7e2d795aa5156e1"
    "guava-33.2.1-jre.jar            com/google/guava/guava/33.2.1-jre/guava-33.2.1-jre.jar                                 452b2d9787b7d366fa8cf5ed9a1c40404542d05effa7a598da03bbbbb76d9f31"
    "failureaccess-1.0.2.jar         com/google/guava/failureaccess/1.0.2/failureaccess-1.0.2.jar                           8a8f81cf9b359e3f6dfa691a1e776985c061ef2f223c9b2c80753e1b458e8064"
    "listenablefuture-9999.0-empty-to-avoid-conflict-with-guava.jar com/google/guava/listenablefuture/9999.0-empty-to-avoid-conflict-with-guava/listenablefuture-9999.0-empty-to-avoid-conflict-with-guava.jar b372a037d4230aa57fbeffdef30fd6123f9c0c2db85d0aced00c91b974f33f99"
    "jsr305-3.0.2.jar                com/google/code/findbugs/jsr305/3.0.2/jsr305-3.0.2.jar                                 766ad2a0783f2687962c8ad74ceecc38a28b9f72a2d085ee438b7813e928d0c7"
    "error_prone_annotations-2.26.1.jar com/google/errorprone/error_prone_annotations/2.26.1/error_prone_annotations-2.26.1.jar de25f2d9a2156529bd765f51d8efdfc0dfa7301e04efb9cc75b7f10cf5d0e0fb"
    "j2objc-annotations-3.0.0.jar    com/google/j2objc/j2objc-annotations/3.0.0/j2objc-annotations-3.0.0.jar                88241573467ddca44ffd4d74aa04c2bbfd11bf7c17e0c342c94c9de7a70a7c64"
    "commons-lang3-3.14.0.jar        org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.jar                       7b96bf3ee68949abb5bc465559ac270e0551596fa34523fddf890ec418dde13c"
    "h2-2.2.224.jar                  com/h2database/h2/2.2.224/h2-2.2.224.jar                                               b9d8f19358ada82a4f6eb5b174c6cfe320a375b5a9cb5a4fe456d623e6e55497"
    "slf4j-api-2.0.13.jar            org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar                                        e7c2a48e8515ba1f49fa637d57b4e2f590b3f5bd97407ac699c3aa5efb1204a9"
    "slf4j-simple-2.0.13.jar         org/slf4j/slf4j-simple/2.0.13/slf4j-simple-2.0.13.jar                                  3153fe1d689cffb94f1530b58470c306685ba68844de8857116e3b6ebb81d9f7"
    "logback-classic-1.5.6.jar       ch/qos/logback/logback-classic/1.5.6/logback-classic-1.5.6.jar                        6115c6cac5ed1d9db810d14f2f7f4dd6a9f21f0acbba8016e4daaca2ba0f5eb8"
    "logback-core-1.5.6.jar          ch/qos/logback/logback-core/1.5.6/logback-core-1.5.6.jar                              898c7d120199f37e1acc8118d97ab15a4d02b0e72e27ba9f05843cb374e160c6"
    "sqlite-jdbc-3.46.1.3.jar        org/xerial/sqlite-jdbc/3.46.1.3/sqlite-jdbc-3.46.1.3.jar                              4a4832720a65eaf7f4d6fd7ede52087b994dc5633c076f9e994dc0c8b4b0b4fa"
)
# lombok: COMPILE-ONLY (@Data/@Builder/... are RetentionPolicy.SOURCE; nothing is needed at
# run time), so it is fetched to the cache and put on the compile classpath + --processor-path,
# but NOT staged into the overlay.
LOMBOK_JAR="lombok-1.18.34.jar"
LOMBOK_PATH="org/projectlombok/lombok/1.18.34/lombok-1.18.34.jar"
LOMBOK_SHA="c27d6b2aff56241d1b07fcbcc6b183709e6b432c80f7374eeb1d823e86d4b81a"

DEP_CACHE="$DL/java-jse-libs"
ensure_deps() {
    mkdir -p "$DEP_CACHE"
    local entry fname path sha
    for entry in "${DEP_LIBS[@]}"; do
        # shellcheck disable=SC2086
        set -- $entry; fname="$1"; path="$2"; sha="$3"
        ensure_asset "$DEP_CACHE/$fname" "$MAVEN_CENTRAL/$path" "$sha"
    done
    ensure_asset "$DEP_CACHE/$LOMBOK_JAR" "$MAVEN_CENTRAL/$LOMBOK_PATH" "$LOMBOK_SHA"
}

# ── sqlite-jdbc native JNI (.so), per arch ─────────────────────────────────────────────
#   riscv64: the rv64 JDK17 is the prebuilt GLIBC build, so the matching JNI is the sqlite-jdbc
#     jar's OWN bundled GLIBC riscv64 native (org/sqlite/native/Linux/riscv64/libsqlitejdbc.so).
#     Extract it from the already-fetched, sha256-pinned jar — fully reproducible, no separate
#     download, no cross-build.
#   loongarch64: the upstream jar ships NO loongarch64 native at all; the loong JDK17 is
#     Alpine-musl, so a musl loong JNI is CROSS-COMPILED IN-PREBUILD from xerial/sqlite-jdbc's own
#     C source (build_loong_sqlite_jni below), reproducibly (sha256-pinned sources). If the loong
#     musl cross-toolchain is genuinely unavailable it degrades to a documented SKIP.
#
# build_loong_sqlite_jni <dest.so> — cross-compile the musl loongarch64 sqlite-jdbc JNI exactly
# as xerial's own Makefile does, then install it at <dest.so>. Steps (all from official source):
#   (1) fetch xerial/sqlite-jdbc SOURCE tag 3.46.1.3 + the official SQLite 3.46.1 amalgamation
#       (both sha256-pinned via ensure_asset);
#   (2) generate NativeDB.h with the HOST javac -h from NativeDB.java (sqlite-jdbc jar + slf4j-api
#       on the classpath — both are already-fetched dependency jars);
#   (3) patch sqlite3.c with xerial's two perl edits (register extension functions + the
#       JDBC_EXTENSIONS compile-option) and append src/main/ext/*.c;
#   (4) compile sqlite3.o + NativeDB.o with loongarch64-linux-musl-gcc using xerial's exact
#       CCFLAGS + SQLITE feature flags, link `-shared -static-libgcc -pthread -lm`, strip.
# Returns non-zero (never a hard exit) if the cross-toolchain / perl / unzip is unavailable, so
# the caller can fall back to a documented SKIP. The built .so's own sha256 is NOT pinned (it
# varies with the cross-gcc version); reproducibility is anchored on the pinned SOURCE inputs.
SQLITE_JDBC_SRC_URL="${SQLITE_JDBC_SRC_URL:-https://github.com/xerial/sqlite-jdbc/archive/refs/tags/3.46.1.3.tar.gz}"
SQLITE_JDBC_SRC_SHA="${SQLITE_JDBC_SRC_SHA:-5d662eb23a0db84ef597ef1800811a6dc42727e0d5fc43b752efd3224dc2695c}"
SQLITE_AMAL_URL="${SQLITE_AMAL_URL:-https://www.sqlite.org/2024/sqlite-amalgamation-3460100.zip}"
SQLITE_AMAL_SHA="${SQLITE_AMAL_SHA:-77823cb110929c2bcb0f5d48e4833b5c59a8a6e40cdea3936b99e199dbbe5784}"
SQLITE_AMAL_DIR="sqlite-amalgamation-3460100"
LOONG_MUSL_CC="${LOONG_MUSL_CC:-loongarch64-linux-musl-gcc}"
LOONG_MUSL_STRIP="${LOONG_MUSL_STRIP:-loongarch64-linux-musl-strip}"
build_loong_sqlite_jni() {
    local out="$1"
    command -v "$LOONG_MUSL_CC" >/dev/null 2>&1 || { echo "prebuild: NOTE $LOONG_MUSL_CC not on PATH — cannot cross-build loong sqlite JNI" >&2; return 1; }
    command -v perl  >/dev/null 2>&1 || { echo "prebuild: NOTE perl missing — cannot patch sqlite3.c" >&2; return 1; }
    command -v unzip >/dev/null 2>&1 || { echo "prebuild: NOTE unzip missing — cannot unpack the SQLite amalgamation" >&2; return 1; }
    ensure_host_jdk   # host javac -h to emit NativeDB.h
    local srctar="$DL/sqlitejdbc-src/sqlite-jdbc-3.46.1.3.tar.gz"
    local amalzip="$DL/sqlitejdbc-src/${SQLITE_AMAL_DIR}.zip"
    ensure_asset "$srctar"  "$SQLITE_JDBC_SRC_URL" "$SQLITE_JDBC_SRC_SHA"
    ensure_asset "$amalzip" "$SQLITE_AMAL_URL"     "$SQLITE_AMAL_SHA"
    local jdbcjar="$DEP_CACHE/sqlite-jdbc-3.46.1.3.jar" slf4j="$DEP_CACHE/slf4j-api-2.0.13.jar"
    [[ -f "$jdbcjar" && -f "$slf4j" ]] || { echo "prebuild: NOTE sqlite-jdbc/slf4j dependency jars missing — cannot generate NativeDB.h" >&2; return 1; }
    local B; B="$(mktemp -d)"
    tar xzf "$srctar" -C "$B" --strip-components=1
    unzip -qo "$amalzip" -d "$B/amal"
    local SRC="$B/src/main/java" AMAL="$B/amal/$SQLITE_AMAL_DIR"
    mkdir -p "$B/inc" "$B/o"
    # (2) NativeDB.h via host javac -h (no -sourcepath: resolve org.sqlite.* from the compiled jar)
    if ! javac -cp "$jdbcjar:$slf4j" -d "$B/cls" -h "$B/inc" "$SRC/org/sqlite/core/NativeDB.java" 2>"$B/javac.log"; then
        echo "prebuild: NOTE host javac -h failed generating NativeDB.h:" >&2; tail -5 "$B/javac.log" >&2; rm -rf "$B"; return 1
    fi
    mv -f "$B/inc/org_sqlite_core_NativeDB.h" "$B/inc/NativeDB.h"
    # (3) patch sqlite3.c (xerial's two perl edits) + append the extension-functions source
    perl -p -e 's/^opendb_out:/  if(!db->mallocFailed \&\& rc==SQLITE_OK){ rc = RegisterExtensionFunctions(db); }\nopendb_out:/;' "$AMAL/sqlite3.c" > "$B/o/sqlite3.c.tmp"
    perl -p -e 's/^(static const char \* const sqlite3azCompileOpt.+)$/\1\n  "JDBC_EXTENSIONS",/;' "$B/o/sqlite3.c.tmp" > "$B/o/sqlite3.c"
    cat "$B"/src/main/ext/*.c >> "$B/o/sqlite3.c"
    cp "$AMAL/sqlite3.h" "$B/o/sqlite3.h"
    # (4) compile + link + strip, using xerial's exact CCFLAGS + SQLITE feature flags
    local CCF="-I$B/lib/inc_linux -I$B/o -Os -fPIC -fvisibility=hidden"
    local SQLF="-DSQLITE_ENABLE_LOAD_EXTENSION=1 -DSQLITE_HAVE_ISNAN -DHAVE_USLEEP=1 \
        -DSQLITE_ENABLE_COLUMN_METADATA -DSQLITE_CORE -DSQLITE_ENABLE_FTS3 -DSQLITE_ENABLE_FTS3_PARENTHESIS \
        -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_STAT4 -DSQLITE_ENABLE_DBSTAT_VTAB \
        -DSQLITE_ENABLE_MATH_FUNCTIONS -DSQLITE_THREADSAFE=1 -DSQLITE_DEFAULT_MEMSTATUS=0 \
        -DSQLITE_DEFAULT_FILE_PERMISSIONS=0666 -DSQLITE_MAX_VARIABLE_NUMBER=250000 \
        -DSQLITE_MAX_MMAP_SIZE=1099511627776 -DSQLITE_MAX_LENGTH=2147483647 -DSQLITE_MAX_COLUMN=32767 \
        -DSQLITE_MAX_SQL_LENGTH=1073741824 -DSQLITE_MAX_FUNCTION_ARG=127 -DSQLITE_MAX_ATTACHED=125 \
        -DSQLITE_MAX_PAGE_COUNT=4294967294 -DSQLITE_DISABLE_PAGECACHE_OVERFLOW_STATS"
    echo "prebuild: cross-compiling loongarch64 sqlite JNI with $($LOONG_MUSL_CC -dumpversion 2>/dev/null) (this takes ~1 min)"
    # shellcheck disable=SC2086
    if ! "$LOONG_MUSL_CC" -o "$B/o/sqlite3.o" -c $CCF $SQLF "$B/o/sqlite3.c" 2>"$B/cc.log" \
      || ! "$LOONG_MUSL_CC" $CCF -I "$B/inc" -c -o "$B/o/NativeDB.o" "$SRC/org/sqlite/core/NativeDB.c" 2>>"$B/cc.log" \
      || ! "$LOONG_MUSL_CC" $CCF -o "$B/o/libsqlitejdbc.so" "$B/o/NativeDB.o" "$B/o/sqlite3.o" -shared -static-libgcc -pthread -lm 2>>"$B/cc.log"; then
        echo "prebuild: NOTE loong sqlite JNI cross-compile failed:" >&2; tail -8 "$B/cc.log" >&2; rm -rf "$B"; return 1
    fi
    command -v "$LOONG_MUSL_STRIP" >/dev/null 2>&1 && "$LOONG_MUSL_STRIP" "$B/o/libsqlitejdbc.so" 2>/dev/null || true
    # sanity: the link succeeded and the output is an ELF shared object (magic 0x7f 'ELF').
    if [[ "$(head -c4 "$B/o/libsqlitejdbc.so" 2>/dev/null | od -An -tx1 | tr -d ' \n')" != "7f454c46" ]]; then
        echo "prebuild: NOTE loong sqlite JNI build produced a non-ELF object" >&2; rm -rf "$B"; return 1
    fi
    install -Dm0644 "$B/o/libsqlitejdbc.so" "$out"
    rm -rf "$B"
    return 0
}
ensure_sqlite_native() {
    case "$arch" in
        x86_64|aarch64) return 0 ;;   # driver self-extracts its bundled Linux-Musl JNI
    esac
    local so="$DL/sqlitejdbc-native/$arch/libsqlitejdbc.so"
    case "$arch" in
        riscv64)
            if [[ ! -f "$so" ]]; then
                extract_jar_entry "$DEP_CACHE/sqlite-jdbc-3.46.1.3.jar" \
                    "org/sqlite/native/Linux/riscv64/libsqlitejdbc.so" "$so" \
                    || { echo "prebuild: WARNING could not extract the glibc riscv64 sqlite JNI from the jar; sqlite carpet will run as a documented SKIP" >&2; return 0; }
            fi
            echo "prebuild: sqlite-jdbc riscv64 JNI = jar-bundled glibc build (extracted from the pinned jar, staged)" ;;
        loongarch64)
            if [[ -f "$so" ]]; then
                echo "prebuild: sqlite-jdbc loongarch64 JNI present in cache (musl loong build; will be staged)"
            elif build_loong_sqlite_jni "$so"; then
                echo "prebuild: sqlite-jdbc loongarch64 JNI cross-compiled in-prebuild from official source (musl loong; staged)"
            else
                echo "prebuild: WARNING could not cross-compile the loongarch64 sqlite JNI (loong musl toolchain / source unavailable);" >&2
                echo "prebuild:      the sqlite carpet will run as a DOCUMENTED SKIP on loongarch64 (partial-arch-deliver; see programs/SOURCES.md)." >&2
            fi ;;
    esac
}

# ── Compile the carpet classes IN-PREBUILD (host javac, --release 17) ──────────────────
# Compiles programs/{lib-carpets,jse-suite}/*.java with the fetched deps on the classpath and
# lombok on --processor-path, into carpets.jar (org.starry.dod.* + the default-package stdlib
# tests). Bytecode is arch-independent, so the result is cached and reused across arches.
CARPET_JAR="$DL/java-jse-build/carpets.jar"
compile_carpets() {
    ensure_host_jdk
    mkdir -p "$(dirname "$CARPET_JAR")"
    local cp; cp="$(printf '%s:' "$DEP_CACHE"/*.jar)"
    local B; B="$(mktemp -d)"
    echo "prebuild: compiling carpets.jar with host $(javac -version 2>&1) (--release 17)"
    if javac --release 17 -encoding UTF-8 \
            --processor-path "$DEP_CACHE/$LOMBOK_JAR" -cp "$cp" \
            -d "$B/classes" "$PROG"/lib-carpets/*.java "$PROG"/jse-suite/*.java 2>"$B/javac.log"; then
        ( cd "$B/classes" && jar cf "$B/carpets.jar" . )
        mv -f "$B/carpets.jar" "$CARPET_JAR"
        echo "prebuild: compiled carpets.jar in-prebuild ($(du -h "$CARPET_JAR" | cut -f1))"
        rm -rf "$B"
    else
        echo "prebuild: ERROR host javac failed to compile the carpets:" >&2
        cat "$B/javac.log" >&2 || true
        rm -rf "$B"; exit 5
    fi
    [[ -s "$CARPET_JAR" ]] || { echo "prebuild: carpets.jar not produced" >&2; exit 5; }
}

# Stage carpets.jar + the dependency jars into /root/jse, the per-arch sqlite JNI native into
# /root/jse/native (rv/loong, when available), and the run-jse.sh gate into /usr/bin.
stage_payload() {
    local jse="$overlay_dir/root/jse"
    install -d "$jse" "$jse/libs" "$jse/native"
    install -m0644 "$CARPET_JAR" "$jse/carpets.jar"
    local entry fname
    for entry in "${DEP_LIBS[@]}"; do
        # shellcheck disable=SC2086
        set -- $entry; fname="$1"
        install -m0644 "$DEP_CACHE/$fname" "$jse/libs/$fname"
    done
    case "$arch" in
        riscv64|loongarch64)
            local so="$DL/sqlitejdbc-native/$arch/libsqlitejdbc.so"
            if [[ -f "$so" ]]; then
                install -m0644 "$so" "$jse/native/libsqlitejdbc.so"
                echo "prebuild: staged sqlite-jdbc JNI for $arch into /root/jse/native"
            else
                echo "prebuild: no sqlite-jdbc JNI for $arch — sqlite carpet handled as a documented SKIP by run-jse.sh"
            fi ;;
    esac
    install -Dm0755 "$PROG/run-jse.sh" "$overlay_dir/usr/bin/run-jse.sh"
    echo "prebuild: staged carpets.jar + $(ls "$jse/libs" | wc -l) dependency jars into /root/jse, run-jse.sh into /usr/bin"
}

main() {
    case "$arch" in x86_64|aarch64|riscv64|loongarch64) ;; *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;; esac
    ensure_host_tools
    ensure_jdk17
    ensure_deps
    ensure_sqlite_native
    grow_rootfs
    stage_jdk17
    stage_glibc_runtime_rv   # riscv64 only: real Debian glibc closure for the prebuilt glibc JDK17
    compile_carpets
    stage_payload
    echo "prebuild: java-jse overlay ready for $arch — $(du -sh "$overlay_dir" | cut -f1)"
}

main "$@"
