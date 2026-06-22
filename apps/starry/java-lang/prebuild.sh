#!/usr/bin/env bash
# prebuild.sh — provision the multi-JDK Java LANGUAGE carpet for StarryOS.
#
# This is the LANGUAGE case for #764 `jdk17+ (openjdk 17 21 23 25 update-alternatives):
# javac · java`. Unlike the kotlin-lang case (which only RUNS a jar and can use a ~60MB
# JRE), the java-lang case must COMPILE on-target (`javac`), so it stages the FULL JDK
# (bin/javac + lib + jmods) for EVERY version. The per-arch JDK cells, staging paths,
# the per-JDK musl-loader-path discipline, and the rootfs sizing are byte-for-byte the
# PROVEN, 4-arch-green `openjdk-multi` recipe (download/jdk-multi/prep-jdk-multi-rootfs.sh,
# delivered hw4os-s5d1t2/java/jdk-multi).
#
# ── ROOTFS SIZE (the crucial lesson, notes/kotlin-lang) ──────────────────────────────
#   The apps/starry harness copies the 1 GiB base alpine rootfs to a per-app image, runs
#   THIS prebuild (handing us $STARRY_ROOTFS = that image), then injects $STARRY_OVERLAY_DIR
#   into the image via debugfs WITHOUT resizing it. Four FULL JDKs (~1.5 GiB on-disk) do not
#   fit in 1 GiB -> debugfs silently truncates large files (libjvm.so) -> dlopen ENOEXEC
#   ("Exec format error"). The kotlin case worked around this by dropping to a 156 MB JRE;
#   that is NOT an option here because javac needs the full JDK.
#   FIX: grow $STARRY_ROOTFS to 6 GiB (truncate + e2fsck + resize2fs) BEFORE the harness
#   injects — exactly as prep-jdk-multi-rootfs.sh grows its image to 6 GiB. Disk on the host
#   is cheap; the running QEMU only ever maps a -Xmx512m JVM, so the larger image is free.
#   We resize the image in place; the qemu-<arch>.toml points the drive at this same
#   per-app image (rootfs-<arch>-java-lang.img), so the grown image is what boots.
#
# ── PER-ARCH JDK SET (matches what openjdk-multi PROVED green) ────────────────────────
#                JDK17                 JDK21               JDK23                 JDK25
#   x86_64       apk (openjdk17)       BellSoft musl tar   BellSoft musl tar     BellSoft musl tar   (4 JDKs)
#   aarch64      apk (openjdk17)       BellSoft musl tar   BellSoft musl tar     BellSoft musl tar   (4 JDKs)
#   riscv64      native-musl cross tar Alpine-musl apk     BellSoft GLIBC+gcompat Alpine-musl apk     (4 attempted)
#   loongarch64  apk (openjdk17-loong) Alpine-musl apk     Loongson GLIBC+gcompat Alpine-musl apk     (4 attempted)
#   JDK23 ships only as glibc on riscv64 (BellSoft generic-glibc) and loongarch64 (Loongson),
#   so — exactly like prep-jdk-multi-rootfs.sh — we stage the gcompat shim (+ its libucontext /
#   musl-obstack deps) so the Alpine-musl loader can satisfy the glibc JDK's libc.so.6 / ld-linux
#   references, then ATTEMPT JDK23 on all 4 arches. The decision is made at run time by
#   run-java.sh's timeout-guarded liveness probe: a glibc JDK that still segfaults/hangs under
#   gcompat is a DOCUMENTED SKIP (partial-arch-deliver rule), never a fake pass or a failure.
#   All musl cells (x86/aa fully, plus 17+21+25 on rv/loong) are the clean native path; javac
#   WORKS on every staged musl cell.
#
# Guest layout (== openjdk-multi): /opt/jdk{17,21,23,25} (update-alternatives candidate
# roots), /opt/jdk-current symlink, /root/.sdkman/candidates/java/* candidate symlinks,
# /root/jdkm/*.java test sources. The per-JDK musl loader path is set by the qemu toml at
# run time (NOT here), one JDK at a time, to avoid the cross-JDK launcher mis-resolution.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"

# ── Asset cache root (PORTABLE) ───────────────────────────────────────────────────────
# Holds the PROVEN JDK distributions (same tree layout openjdk-multi prep uses). On a
# maintainer's clean machine this dir does not pre-exist; each asset is then fetched from
# its OFFICIAL URL into here (see ensure_asset + the ensure_* wrappers below) and re-used on
# subsequent runs. A developer who already has the assets locally points JAVA_DL_ROOT at
# their cache (e.g. JAVA_DL_ROOT=/path/to/download) and every fetch short-circuits to disk.
# Default is repo/staging-relative so a fresh checkout has a writable place to populate.
DL="${JAVA_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/java-dl}"
JM="$DL/jdk-multi"
ALP="$JM/jdk21-musl-alpine"
PROG="$app_dir/programs"

# Alpine apk CDN (override to a mirror, e.g. https://mirrors.ustc.edu.cn/alpine, if dl-cdn
# is slow/unreachable). edge/community + edge/main hold the riscv64/loongarch64 + libffi
# packages; openjdk17 x86_64/aarch64 live in v3.22/community (rolling — see ensure_openjdk17).
ALPINE_CDN="${ALPINE_CDN:-https://dl-cdn.alpinelinux.org/alpine}"
# BellSoft Liberica release CDN (immutable per-version tarballs; '+' is a literal path char).
BELLSOFT_CDN="${BELLSOFT_CDN:-https://download.bell-sw.com/java}"

# ── Portable fetch-ensure layer ───────────────────────────────────────────────────────
# ensure_asset <abs-local-path> <official-url> [sha256]
#   If the local file exists (and its sha256 matches when one is given) it is used as-is —
#   so a populated cache (JAVA_DL_ROOT) is hit instantly with zero network. Otherwise the
#   parent dir is created, the URL is curl'd to a temp file, the sha256 is verified when
#   given, and the file is atomically moved into place. A sha mismatch is a hard error.
#   An empty/omitted sha skips verification (used for Alpine apks that float on the rolling
#   edge CDN, where the cached copy is the pinned golden and the URL is a best-effort refill).
ensure_asset() {
    local dest="$1" url="$2" want="${3:-}"
    if [[ -f "$dest" ]]; then
        if [[ -n "$want" ]] && command -v sha256sum >/dev/null 2>&1; then
            local have; have="$(sha256sum "$dest" | cut -d' ' -f1)"
            if [[ "$have" == "$want" ]]; then
                echo "prebuild: cache hit $dest (sha256 ok)"; return 0
            fi
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

# ensure_alpine_apk <cache-abs-path> <branch/repo> <arch> <filename> [sha256]
#   Thin ensure_asset wrapper building the canonical Alpine pool URL
#   $ALPINE_CDN/<branch>/<repo>/<arch>/<filename>. The cache path keeps the prep tree's
#   layout (which may prefix the file with the arch, e.g. riscv64-openjdk21-jdk-...apk) while
#   the URL uses the real Alpine basename.
ensure_alpine_apk() {
    local dest="$1" path="$2" cdnarch="$3" fname="$4" sha="${5:-}"
    ensure_asset "$dest" "$ALPINE_CDN/$path/$cdnarch/$fname" "$sha"
}

# Target rootfs size — large enough for 4 full JDKs (~1.5 GiB on-disk) + headroom; mirrors
# prep-jdk-multi-rootfs.sh (which uses 6 GiB and is 4-arch-green).
ROOTFS_SIZE="${JAVA_ROOTFS_SIZE:-6G}"

ensure_host_tools() {
    local missing=()
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            echo "prebuild: installing host tools: ${missing[*]}"
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2
            exit 1
        fi
    fi
}

# Grow the per-app rootfs image so the injected JDKs fit without truncation. Idempotent:
# truncate only grows, e2fsck/resize2fs are safe to re-run. The harness has already copied
# the 1 GiB base into $rootfs_img by the time prebuild runs.
grow_rootfs() {
    [[ -f "$rootfs_img" ]] || { echo "prebuild: rootfs image missing: $rootfs_img" >&2; exit 2; }
    local before after
    before=$(stat -c %s "$rootfs_img")
    echo "prebuild: rootfs $rootfs_img is $((before/1024/1024)) MiB; growing to $ROOTFS_SIZE"
    truncate -s "$ROOTFS_SIZE" "$rootfs_img"
    e2fsck -f -y "$rootfs_img" >/dev/null 2>&1 || true
    resize2fs "$rootfs_img" >/dev/null 2>&1
    after=$(stat -c %s "$rootfs_img")
    echo "prebuild: rootfs grown to $((after/1024/1024)) MiB (fs resized)"
}

# untar a .tar.gz, stripping the single top-level dir, into <dest>  (jdk-multi untar_into ... 1)
untar_strip1() {
    local arc="$1" dest="$2"
    [[ -f "$arc" ]] || { echo "prebuild: missing archive $arc" >&2; exit 2; }
    mkdir -p "$dest"
    tar xzf "$arc" -C "$dest" --strip-components=1
}

# extract an Alpine apk (gzip tar) into <dest>, dropping apk metadata  (jdk-multi apk_into)
apk_into() {
    local apk="$1" dest="$2"
    [[ -f "$apk" ]] || { echo "prebuild: missing apk $apk" >&2; exit 2; }
    mkdir -p "$dest"
    tar xzf "$apk" -C "$dest" \
        --exclude='.PKGINFO' --exclude='.SIGN.*' --exclude='.pre-install' \
        --exclude='.post-install' --exclude='.pre-upgrade' --exclude='.post-upgrade' \
        --exclude='.trigger' 2>/dev/null || true
}

# Stage JDK17 into $overlay_dir/opt/jdk17 (full JDK with javac), per-arch source.
stage_jdk17() {
    local jdst="$overlay_dir/opt/jdk17"
    rm -rf "$jdst"; mkdir -p "$jdst"
    case "$arch" in
        x86_64|aarch64)
            local T; T="$(mktemp -d)"
            local a apk
            for a in openjdk17-jdk openjdk17-jmods openjdk17-jre-headless openjdk17-jre; do
                apk="$(ls "$DL/openjdk17-apks/$arch/${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && tar xzf "$apk" -C "$T" 2>/dev/null || true
            done
            cp -a "$T/usr/lib/jvm/java-17-openjdk/." "$jdst/"
            rm -rf "$T" ;;
        loongarch64)
            local T; T="$(mktemp -d)"
            local a apk
            for a in openjdk17-loongarch-jdk openjdk17-loongarch-jmods openjdk17-loongarch-jre-headless openjdk17-loongarch-jre; do
                apk="$(ls "$DL/openjdk17-apks/$arch/${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && tar xzf "$apk" -C "$T" 2>/dev/null || true
            done
            cp -a "$T"/usr/lib/jvm/*/. "$jdst/"
            rm -rf "$T" ;;
        riscv64)
            untar_strip1 "$DL/openjdk17-apks/riscv64/openjdk17-riscv64-musl-NATIVE-cross.tar.gz" "$jdst" ;;
    esac
    [[ -x "$jdst/bin/java" && -x "$jdst/bin/javac" ]] \
        || { echo "prebuild: jdk17 staged without java+javac for $arch" >&2; exit 3; }
    echo "prebuild: jdk17 staged ($(du -sh "$jdst" | cut -f1), javac present)"
}

# Stage JDK21 (full JDK with javac).
stage_jdk21() {
    local jdst="$overlay_dir/opt/jdk21"
    rm -rf "$jdst"; mkdir -p "$jdst"
    case "$arch" in
        x86_64)  untar_strip1 "$JM/jdk21/bellsoft-jdk21.0.11+11-linux-x64-musl.tar.gz"     "$jdst" ;;
        aarch64) untar_strip1 "$JM/jdk21/bellsoft-jdk21.0.11+11-linux-aarch64-musl.tar.gz" "$jdst" ;;
        riscv64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk21-jre-headless openjdk21-jdk openjdk21-jmods; do
                apk="$(ls "$ALP/riscv64-${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && apk_into "$apk" "$T"
            done
            cp -a "$T/usr/lib/jvm/java-21-openjdk/." "$jdst/"; rm -rf "$T" ;;
        loongarch64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk21-jre-headless openjdk21-jdk openjdk21-jmods; do
                apk="$(ls "$ALP/loongarch64-${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && apk_into "$apk" "$T"
            done
            cp -a "$T/usr/lib/jvm/java-21-openjdk/." "$jdst/"; rm -rf "$T" ;;
    esac
    [[ -x "$jdst/bin/java" && -x "$jdst/bin/javac" ]] \
        || { echo "prebuild: jdk21 staged without java+javac for $arch" >&2; exit 3; }
    echo "prebuild: jdk21 staged ($(du -sh "$jdst" | cut -f1), javac present)"
}

# Stage JDK23 (full JDK with javac), all 4 arches — exactly the prep-jdk-multi-rootfs.sh
# per-arch JDK23 cells. x86_64/aarch64 = BellSoft native-musl. riscv64 = BellSoft generic
# GLIBC build; loongarch64 = Loongson GLIBC build (the only JDK23 that exists for those two
# arches — no upstream Alpine-musl JDK23 for rv/loong). The glibc rv/loong cells are bridged
# by the staged gcompat shim (see stage_gcompat); they are staged and TRIED, then run-java.sh's
# timeout-guarded liveness probe includes them only if they actually run under gcompat, else
# documents a SKIP (never a fake pass).
stage_jdk23() {
    case "$arch" in
        x86_64|aarch64|riscv64|loongarch64) : ;;
        *) echo "prebuild: jdk23 unsupported arch $arch — skipping"; return 0 ;;
    esac
    local jdst="$overlay_dir/opt/jdk23"
    rm -rf "$jdst"; mkdir -p "$jdst"
    case "$arch" in
        x86_64)  untar_strip1 "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-x64-musl.tar.gz"     "$jdst" ;;
        aarch64) untar_strip1 "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-aarch64-musl.tar.gz" "$jdst" ;;
        riscv64) untar_strip1 "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-riscv64.tar.gz"      "$jdst" ;;  # glibc -> gcompat
        loongarch64) untar_strip1 "$JM/jdk23/loongson23.1.17-fx-jdk23_37-linux-loongarch64.tar.gz" "$jdst" ;;  # glibc -> gcompat
    esac
    [[ -x "$jdst/bin/java" && -x "$jdst/bin/javac" ]] \
        || { echo "prebuild: jdk23 staged without java+javac for $arch" >&2; exit 3; }
    echo "prebuild: jdk23 staged ($(du -sh "$jdst" | cut -f1), javac present)"
}

# Stage JDK25 (full JDK with javac).
stage_jdk25() {
    local jdst="$overlay_dir/opt/jdk25"
    rm -rf "$jdst"; mkdir -p "$jdst"
    case "$arch" in
        x86_64)  untar_strip1 "$JM/jdk25/bellsoft-jdk25+37-linux-x64-musl.tar.gz"     "$jdst" ;;
        aarch64) untar_strip1 "$JM/jdk25/bellsoft-jdk25+37-linux-aarch64-musl.tar.gz" "$jdst" ;;
        riscv64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk25-jre-headless openjdk25-jdk openjdk25-jmods; do
                apk="$(ls "$ALP/riscv64-${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && apk_into "$apk" "$T"
            done
            cp -a "$T/usr/lib/jvm/java-25-openjdk/." "$jdst/"; rm -rf "$T" ;;
        loongarch64)
            local T; T="$(mktemp -d)"; local a apk
            for a in openjdk25-loongarch-jre-headless openjdk25-loongarch-jre openjdk25-loongarch-jdk openjdk25-loongarch-jmods; do
                apk="$(ls "$JM/jdk25/loongarch64-alpine-musl/${a}-"*.apk 2>/dev/null | head -1)"
                [[ -n "$apk" ]] && apk_into "$apk" "$T"
            done
            cp -a "$T/usr/lib/jvm/java-25-openjdk/." "$jdst/"; rm -rf "$T" ;;
    esac
    if [[ -x "$jdst/bin/java" && -x "$jdst/bin/javac" ]]; then
        echo "prebuild: jdk25 staged ($(du -sh "$jdst" | cut -f1), javac present)"
    elif [[ "$arch" == riscv64 ]]; then
        # jdk25 on riscv64 is the Alpine Zero-VM build, which always SKIPs at the run-time
        # liveness probe (IllegalInstruction on the RV64GC baseline). Staging it is therefore
        # best-effort: a transient apk-extraction miss must not abort the whole suite, which
        # carries the real 17/21/23 cells. Drop the partial dir; the probe records the SKIP.
        echo "prebuild: WARNING jdk25 not fully staged for riscv64 (Zero-VM cell — SKIPs at probe regardless)" >&2
        rm -rf "$jdst"
    else
        echo "prebuild: jdk25 staged without java+javac for $arch" >&2; exit 3
    fi
}

stage_test_sources() {
    # Version feature tests (Jdk{17,21,23,25}Features.java), the LANGUAGE carpet
    # (JavaLangCarpet.java) + its golden, the full-JLS grammar carpet (JavaGrammar.java),
    # the cross-version backward-compat test (BackCompat.java), and the kernel-relevant
    # javac/java CLI carpet (java-cli-core.sh) all under /root/jdkm.
    local d="$overlay_dir/root/jdkm"
    mkdir -p "$d"
    install -m0644 "$PROG/Jdk17Features.java"  "$d/"
    install -m0644 "$PROG/Jdk21Features.java"  "$d/"
    install -m0644 "$PROG/Jdk23Features.java"  "$d/"
    install -m0644 "$PROG/Jdk25Features.java"  "$d/"
    install -m0644 "$PROG/JavaGrammar.java"    "$d/"
    install -m0644 "$PROG/JavaLangCarpet.java" "$d/"
    install -m0644 "$PROG/BackCompat.java"     "$d/"
    install -m0644 "$PROG/java-cli-core.sh"    "$d/"
    install -m0644 "$PROG/java-toolchain-carpet.sh" "$d/"
    # Stage the on-target gate script (invoked as the ENTIRE shell_init_cmd). Keeping the gate
    # in a staged script — not inline in the toml — avoids the harness false-positive where the
    # echoed shell_init_cmd text containing `echo "TEST PASSED"` self-matches success_regex
    # (FIX1), and carries the per-arch honest SKIP logic for JDKs that can't run on rv/loong
    # (FIX2). One script serves all 4 arches (it detects the arch + JDK set at run time).
    install -Dm0755 "$PROG/run-java.sh" "$overlay_dir/usr/bin/run-java.sh"
    echo "prebuild: staged $(ls "$d"/*.java | wc -l) .java + java-cli-core.sh + java-toolchain-carpet.sh into /root/jdkm + run-java.sh gate into /usr/bin"
}

# Stage a single .so (and its symlinks) from an apk into the overlay /usr/lib.
# $1 = apk path, $2 = base name glob (e.g. 'libffi.so')  (kotlin-lang stage_overlay_lib)
stage_overlay_lib() {
    local apk="$1" glob="$2"
    [[ -f "$apk" ]] || { echo "prebuild: WARNING missing apk $apk (relying on base rootfs)" >&2; return 0; }
    local T; T="$(mktemp -d)"
    apk_into "$apk" "$T"
    install -d "$overlay_dir/usr/lib"
    local f any=0
    for f in "$T"/usr/lib/${glob}* "$T"/lib/${glob}*; do
        [[ -e "$f" ]] || continue
        any=1
        if [[ -L "$f" ]]; then cp -P "$f" "$overlay_dir/usr/lib/$(basename "$f")"
        else install -Dm0644 "$f" "$overlay_dir/usr/lib/$(basename "$f")"; fi
    done
    rm -rf "$T"
    [[ "$any" = 1 ]] || echo "prebuild: WARNING no ${glob} matched in $apk" >&2
}

stage_deps() {
    # loongarch64 JDK21 ships the Zero VM (lib/zero/libjvm.so) which is DT_NEEDED
    # [libffi.so.8]; that lib is absent from the base loong rootfs. Stage it from the apk
    # bundled with the alpine-musl JDK set (== kotlin-lang loong cell). libz.so.1 (libjli's
    # zlib dep) is already in every base alpine rootfs (/usr/lib/libz.so.1), so it is NOT
    # staged here; jdk25 server VM + the rv server VMs need only libc.
    if [[ "$arch" == "loongarch64" ]]; then
        local fapk; fapk="$(ls "$ALP/loongarch64-libffi-"*.apk 2>/dev/null | head -1)"
        stage_overlay_lib "$fapk" "libffi.so"
        [[ -e "$overlay_dir/usr/lib/libffi.so.8" ]] \
            && echo "prebuild: staged libffi.so.8 (loong JDK21 Zero VM dep) into /usr/lib" \
            || echo "prebuild: WARNING libffi.so.8 not staged for loong; JDK21 Zero VM may fail" >&2
    fi
}

# Stage the gcompat glibc-on-musl shim (+ its libucontext / musl-obstack runtime deps) into
# the overlay for riscv64 / loongarch64, so the Alpine-musl loader can satisfy the JDK23 glibc
# build's libc.so.6 / ld-linux-<arch>.so.1 references. This MIRRORS prep-jdk-multi-rootfs.sh's
# gcompat staging (its `apk_into "$GC" ""` drops gcompat's lib/ -> ld-linux + libc.so.6 etc.
# at the rootfs root). gcompat's apk DT_NEEDED closure is `so:libucontext.so.1` +
# `so:libobstack.so.1` (musl-obstack), which are NOT in the base Alpine rootfs, so we extract
# those two apks too — otherwise libgcompat.so.0 itself would fail to load and the JDK23 cell
# would SKIP for the wrong reason. The probe (run-java.sh) still decides per cell: if JDK23
# segfaults/hangs even WITH a fully-resolvable gcompat, that is a documented SKIP, not a fake
# pass. (musl JDKs 17/21/25 do not touch gcompat.) No-op on x86_64/aarch64 (all-musl cells).
# Bridge the glibc JDK23 build's libc.so.6 / ld-linux-<arch>.so.1 references.
#   riscv64: stage the REAL Debian glibc runtime. The BellSoft generic-glibc JDK23 runs
#     cleanly under real glibc (verified rc=0 on qemu-user AND on StarryOS, printing the
#     carpet output with `-Xint -Xmx512m`). The musl gcompat shim is INSUFFICIENT for the
#     JVM, so real glibc is required: extract libc6 (ld-linux + libc/libm/libpthread/librt/
#     libdl) into the default multiarch search path; the JDK launcher's own interp
#     (/lib/ld-linux-riscv64-lp64d.so.1) then loads it. The musl JDKs (17/21/25) keep using
#     the musl loader and are unaffected.
#   loongarch64: the only JDK23 is the old Loongson abi1.0 build (needs GLIBC_2.27, the
#     legacy Loongson "world"); no compatible runtime exists for the upstreamed abi
#     (glibc 2.36+), so it stays on the gcompat path and run-java.sh records a documented SKIP.
stage_real_glibc_rv() {
    # Fetch (or cache-hit) the official Debian trixie riscv64 libc6 — the real glibc
    # runtime closure the BellSoft generic-glibc JDK23 needs. ensure_asset verifies the
    # pinned sha256; a populated JAVA_DL_ROOT cache is used with zero network.
    local deb="$DL/glibc-debian/riscv64/libc6_2.41-12+deb13u3_riscv64.deb"
    ensure_asset "$deb" \
        "http://deb.debian.org/debian/pool/main/g/glibc/libc6_2.41-12+deb13u3_riscv64.deb" \
        fee42ebb2a148cc0dbc46ba938d8d69495b6dd5250cecafed9d585c567550b7a \
        || { echo "prebuild: WARNING libc6 deb unavailable for riscv64; JDK23 cell will SKIP" >&2; return 0; }
    local t; t="$(mktemp -d)"
    ( cd "$t" && ar x "$deb" && tar xf data.tar.* )
    mkdir -p "$overlay_dir/lib/riscv64-linux-gnu" "$overlay_dir/usr/lib/riscv64-linux-gnu"
    cp -a "$t"/usr/lib/riscv64-linux-gnu/. "$overlay_dir/usr/lib/riscv64-linux-gnu/" 2>/dev/null || true
    cp -a "$t"/lib/riscv64-linux-gnu/.     "$overlay_dir/lib/riscv64-linux-gnu/"     2>/dev/null || true
    local ldso; ldso="$(find "$t" -name 'ld-linux-riscv64-lp64d.so.1' -type f 2>/dev/null | head -1)"
    [[ -n "$ldso" ]] && install -Dm0755 "$ldso" "$overlay_dir/lib/ld-linux-riscv64-lp64d.so.1"
    rm -rf "$t"
    echo "prebuild: staged REAL Debian glibc runtime for riscv64 (JDK23 glibc cell)"
}
stage_gcompat() {
    case "$arch" in
        riscv64) stage_real_glibc_rv; return 0 ;;
        loongarch64) : ;;
        *) return 0 ;;
    esac
    local d="$DL/openjdk17-apks/$arch" apk
    # gcompat: provides /lib/ld-linux-<arch>.so.1, /lib/libc.so.6, /lib/libm.so.6, etc. into root.
    apk="$(ls "$d/gcompat-"*.apk 2>/dev/null | head -1)"
    [[ -n "$apk" ]] && { apk_into "$apk" "$overlay_dir"; echo "prebuild: staged gcompat shim (glibc JDK23 on $arch)"; } \
        || echo "prebuild: WARNING no gcompat apk for $arch; JDK23 glibc cell will SKIP" >&2
    # libucontext.so.1 + libucontext_posix.so.1 (gcompat DT_NEEDED) -> /usr/lib.
    apk="$(ls "$d/libucontext-"*.apk 2>/dev/null | head -1)"
    [[ -n "$apk" ]] && { apk_into "$apk" "$overlay_dir"; echo "prebuild: staged libucontext (gcompat dep)"; } \
        || echo "prebuild: WARNING no libucontext apk for $arch (gcompat may not load)" >&2
    # libobstack.so.1 (musl-obstack; gcompat DT_NEEDED) -> /usr/lib.
    apk="$(ls "$d/musl-obstack-"*.apk 2>/dev/null | head -1)"
    [[ -n "$apk" ]] && { apk_into "$apk" "$overlay_dir"; echo "prebuild: staged musl-obstack (gcompat dep)"; } \
        || echo "prebuild: WARNING no musl-obstack apk for $arch (gcompat may not load)" >&2
}

stage_alternatives_and_sdkman() {
    # update-alternatives candidate "current" symlink (the qemu toml retargets it per switch).
    mkdir -p "$overlay_dir/opt"
    ln -sfn /opt/jdk17 "$overlay_dir/opt/jdk-current"
    # sdkman-style candidate layout pointing at the staged JDKs (offline `sdk use` switch).
    local cand="$overlay_dir/root/.sdkman/candidates/java"
    mkdir -p "$cand"
    local V
    for V in 17 21 23 25; do
        [[ -d "$overlay_dir/opt/jdk$V" ]] && ln -sfn "/opt/jdk$V" "$cand/$V-open"
    done
    ln -sfn /opt/jdk17 "$cand/current"
}

# ── BackCompatReal (real-world Java-8 forward-compat suite) ────────────────────────────
# The 12 third-party dependency jars: "<filename> <maven-path-under-repo1> <sha256>".
# <maven-path> is appended to https://repo1.maven.org/maven2/ to form the official URL.
# These are arch-independent (pure JVM bytecode), so the SAME set is staged for every arch.
# The cache lives at $DL/java-backcompat-libs/ (reused across arches/runs). On a clean
# machine each jar is fetched from Maven Central by sha256; a developer who already has
# them can populate the cache (or point BCREAL_LOCAL at a prebuilt libs/
# fallback below). sha256 are the host-verified, 299-test-green delivered copies.
BCREAL_LIBS=(
    "commons-io-2.11.0.jar            commons-io/commons-io/2.11.0/commons-io-2.11.0.jar                                       961b2f6d87dbacc5d54abf45ab7a6e2495f89b75598962d8c723cea9bc210908"
    "commons-math3-3.6.1.jar          org/apache/commons/commons-math3/3.6.1/commons-math3-3.6.1.jar                          1e56d7b058d28b65abd256b8458e3885b674c1d588fa43cd7d1cbb9c7ef2b308"
    "commons-lang3-3.12.0.jar         org/apache/commons/commons-lang3/3.12.0/commons-lang3-3.12.0.jar                        d919d904486c037f8d193412da0c92e22a9fa24230b9d67a57855c5c31c7e94e"
    "commons-collections4-4.4.jar     org/apache/commons/commons-collections4/4.4/commons-collections4-4.4.jar                1df8b9430b5c8ed143d7815e403e33ef5371b2400aadbe9bda0883762e0846d1"
    "log4j-api-2.17.1.jar             org/apache/logging/log4j/log4j-api/2.17.1/log4j-api-2.17.1.jar                          b0d8a4c8ab4fb8b1888d0095822703b0e6d4793c419550203da9e69196161de4"
    "log4j-core-2.17.1.jar            org/apache/logging/log4j/log4j-core/2.17.1/log4j-core-2.17.1.jar                        c967f223487980b9364e94a7c7f9a8a01fd3ee7c19bdbf0b0f9f8cb8511f3d41"
    "h2-2.1.214.jar                   com/h2database/h2/2.1.214/h2-2.1.214.jar                                                 d623cdc0f61d218cf549a8d09f1c391ff91096116b22e2475475fce4fbe72bd0"
    "hsqldb-2.5.2.jar                 org/hsqldb/hsqldb/2.5.2/hsqldb-2.5.2.jar                                                 e4aa39c5afb318e8effdec80a0e6de7c9dacc453c1cf7666c515f29a16658dac"
    "gson-2.10.1.jar                  com/google/code/gson/gson/2.10.1/gson-2.10.1.jar                                        4241c14a7727c34feea6507ec801318a3d4a90f070e4525681079fb94ee4c593"
    "bsh-2.0b6.jar                    org/apache-extras/beanshell/bsh/2.0b6/bsh-2.0b6.jar                                     a17955976070c0573235ee662f2794a78082758b61accffce8d3f8aedcd91047"
    "junit-4.13.2.jar                 junit/junit/4.13.2/junit-4.13.2.jar                                                     8e495b634469d64fb8acfa3495a065cbacc8a0fff55ce1e31007be4c16dc57d3"
    "hamcrest-core-1.3.jar            org/hamcrest/hamcrest-core/1.3/hamcrest-core-1.3.jar                                     66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9"
)
MAVEN_CENTRAL="${MAVEN_CENTRAL:-https://repo1.maven.org/maven2}"
# Local prebuilt fallback root (the host-built, 299-test-green artifacts).
BCREAL_LOCAL="${BCREAL_LOCAL:-}"   # optional prebuilt fast-path; empty => fetch jars from Maven Central + compile the jar on host
BCREAL_SRC="$app_dir/programs/backcompat/src"

# Stage the BackCompatReal suite: (a) ensure the 12 dependency jars (Maven Central by sha256,
# cached under $DL/java-backcompat-libs/, with a copy-from-local fallback), (b) ensure the
# Java-8 (--release 8 = bytecode 52) backcompat-real.jar — prefer COMPILING it on the host in
# this prebuild from the staged src/ for reproducibility; fall back to copying the prebuilt jar
# — and (c) stage libs + jar into the overlay at /root/bcreal/{libs,backcompat-real.jar}.
stage_backcompat() {
    local libcache="$DL/java-backcompat-libs"
    local ovl="$overlay_dir/root/bcreal"
    local ovllibs="$ovl/libs"
    mkdir -p "$libcache" "$ovllibs"

    # (a) ensure + stage the 12 dependency jars.
    local entry fname path sha cached
    for entry in "${BCREAL_LIBS[@]}"; do
        # shellcheck disable=SC2086  # word-split the 3 whitespace-separated fields on purpose
        set -- $entry
        fname="$1"; path="$2"; sha="$3"
        cached="$libcache/$fname"
        if [[ ! -f "$cached" && -f "$BCREAL_LOCAL/libs/$fname" ]]; then
            # local prebuilt available -> seed the cache so ensure_asset's sha check confirms it.
            cp -f "$BCREAL_LOCAL/libs/$fname" "$cached"
        fi
        ensure_asset "$cached" "$MAVEN_CENTRAL/$path" "$sha"
        install -m0644 "$cached" "$ovllibs/$fname"
    done

    # (b) ensure backcompat-real.jar: prefer reproducible host compile (--release 8), else copy.
    local jar="$libcache/backcompat-real.jar"
    if command -v javac >/dev/null 2>&1; then
        echo "prebuild: host javac present — compiling backcompat-real.jar (--release 8, bytecode 52)"
        local B; B="$(mktemp -d)"
        local cp_arg="$libcache/*"
        # compile all staged src/*.java with the cached dependency jars on the classpath.
        if javac --release 8 -cp "$cp_arg" -d "$B/classes" "$BCREAL_SRC"/*.java 2>"$B/javac.log"; then
            ( cd "$B/classes" && jar cf "$B/backcompat-real.jar" . )
            mv -f "$B/backcompat-real.jar" "$jar"
            echo "prebuild: compiled backcompat-real.jar in-prebuild ($(du -h "$jar" | cut -f1))"
        else
            echo "prebuild: WARNING host javac compile failed — falling back to prebuilt jar" >&2
            cat "$B/javac.log" >&2 || true
            [[ -f "$BCREAL_LOCAL/backcompat-real.jar" ]] \
                && cp -f "$BCREAL_LOCAL/backcompat-real.jar" "$jar" \
                || { echo "prebuild: no host javac success AND no prebuilt backcompat-real.jar" >&2; rm -rf "$B"; exit 5; }
        fi
        rm -rf "$B"
    else
        echo "prebuild: no host javac — copying prebuilt backcompat-real.jar"
        [[ -f "$BCREAL_LOCAL/backcompat-real.jar" ]] \
            && cp -f "$BCREAL_LOCAL/backcompat-real.jar" "$jar" \
            || { echo "prebuild: no host javac AND no prebuilt backcompat-real.jar at $BCREAL_LOCAL" >&2; exit 5; }
    fi

    # (c) stage the jar into the overlay.
    install -m0644 "$jar" "$ovl/backcompat-real.jar"
    echo "prebuild: staged BackCompatReal ($(ls "$ovllibs" | wc -l) libs + backcompat-real.jar) into /root/bcreal"
}

# ── ensure every asset the stage_* functions consume for THIS arch is present ──────────
# Maps each staged file -> its OFFICIAL download URL + recorded sha256. Runs BEFORE the
# stage_jdkNN functions so staging logic is UNCHANGED — assets are merely guaranteed on disk
# (from cache when JAVA_DL_ROOT is populated, else fetched). Provenance for every URL/sha:
# download/jdk-multi/SOURCES.md + download/openjdk17-apks/SOURCES.md (sha256 computed from the
# delivered, 4-arch-green copies where SOURCES.md did not already record it).
#
# Rolling-CDN caveat: Alpine edge/community (riscv64 + loongarch64 openjdk21/25, libffi) and
# v3.22/community (openjdk17 x86_64/aarch64) bump patch levels over time. The pinned filenames
# below are the delivered golden; if Alpine has rolled past them on a clean machine, the
# cached copy is authoritative — populate JAVA_DL_ROOT. The openjdk17 x86_64/aarch64 fetch
# targets the CURRENT rolling version (17.0.19_p10-r0; 17.0.18_p8-r0 has aged off the live
# CDN) so a clean machine still resolves a real file; the stage_jdk17 prefix-glob
# (openjdk17-jdk-*.apk) matches either patch level. apk sha is left unpinned for these rolling
# entries (cache copy is the verified golden; URL is a best-effort refill).
JDK17_X86AA_VER="${JDK17_X86AA_VER:-17.0.19_p10-r0}"  # current Alpine v3.22 community openjdk17
ensure_assets() {
    case "$arch" in
        x86_64|aarch64)
            local d="$DL/openjdk17-apks/$arch" a
            # openjdk17 full JDK apks (jdk + jmods + jre-headless + jre); rolling, unpinned sha.
            # If ANY patch level of the component apk is already cached, the stage_jdk17
            # prefix-glob (openjdk17-${a}-*.apk) will consume it, so skip the fetch entirely —
            # this keeps a populated JAVA_DL_ROOT (which holds the pinned 17.0.18_p8-r0 golden)
            # network-free even though that exact version has aged off the live Alpine CDN.
            for a in jdk jmods jre-headless jre; do
                if compgen -G "$d/openjdk17-${a}-"'*.apk' >/dev/null 2>&1; then
                    echo "prebuild: cache has openjdk17-${a} apk (stage-glob match) — skip fetch"
                    continue
                fi
                ensure_alpine_apk "$d/openjdk17-${a}-${JDK17_X86AA_VER}.apk" \
                    v3.22/community "$arch" "openjdk17-${a}-${JDK17_X86AA_VER}.apk"
            done ;;
        loongarch64)
            local d="$DL/openjdk17-apks/loongarch64"
            # openjdk17-loongarch native variant apks (edge/community; sha256 from local golden).
            ensure_alpine_apk "$d/openjdk17-loongarch-jdk-17.0.17_p10-r0.apk"          edge/community loongarch64 openjdk17-loongarch-jdk-17.0.17_p10-r0.apk          e55611f2280854e9bc4e76785b51decf840015d26888f3c4eb15df9d603cc49c
            ensure_alpine_apk "$d/openjdk17-loongarch-jmods-17.0.17_p10-r0.apk"        edge/community loongarch64 openjdk17-loongarch-jmods-17.0.17_p10-r0.apk        d9ad8763f8d7a13b5ce2618444bc5fcc43081b9c20fed50ee50cedb9f1eedbc1
            ensure_alpine_apk "$d/openjdk17-loongarch-jre-headless-17.0.17_p10-r0.apk" edge/community loongarch64 openjdk17-loongarch-jre-headless-17.0.17_p10-r0.apk 42ae887f2099d44bbaa7531dad11d29da47796ba06637e1259427d5e2a55d80d
            ensure_alpine_apk "$d/openjdk17-loongarch-jre-17.0.17_p10-r0.apk"          edge/community loongarch64 openjdk17-loongarch-jre-17.0.17_p10-r0.apk          9f867f80ce79cbffe51623e38b3085cb62e6d0d98e459425d8452a24e275f26f ;;
        riscv64)
            # openjdk17 riscv64: NATIVE-musl build self-compiled from openjdk/riscv-port-jdk17u
            # (no upstream vendor ships musl+riscv64 JDK17 — see openjdk17-apks/SOURCES.md §riscv64
            # + source-build/BUILD_FROM_SOURCE.md). No official prebuilt URL exists; this asset
            # MUST be supplied via cache (JAVA_DL_ROOT) or rebuilt from source.
            ensure_asset "$DL/openjdk17-apks/riscv64/openjdk17-riscv64-musl-NATIVE-cross.tar.gz" \
                "${JDK17_RISCV_TAR_URL:-}" \
                e321cfef413a133e4b11680f9166565f57a576e613803b23a79159b22703336b ;;
    esac

    # JDK21 — BellSoft musl tars (x86_64/aarch64); Alpine edge/community apks (riscv64/loongarch64).
    case "$arch" in
        x86_64)  ensure_asset "$JM/jdk21/bellsoft-jdk21.0.11+11-linux-x64-musl.tar.gz" \
                    "$BELLSOFT_CDN/21.0.11+11/bellsoft-jdk21.0.11+11-linux-x64-musl.tar.gz" \
                    5326af096fb1b943b4819ae2a51cbe3bfd4de45e4d1803d3fe3db2a6c8c8b125 ;;
        aarch64) ensure_asset "$JM/jdk21/bellsoft-jdk21.0.11+11-linux-aarch64-musl.tar.gz" \
                    "$BELLSOFT_CDN/21.0.11+11/bellsoft-jdk21.0.11+11-linux-aarch64-musl.tar.gz" \
                    6118fce93eb0f595b3ed48252a43e6610fd550b42b7740701212369cd934ce5a ;;
        riscv64)
            ensure_alpine_apk "$ALP/riscv64-openjdk21-jre-headless-21.0.11_p10-r0.apk" edge/community riscv64 openjdk21-jre-headless-21.0.11_p10-r0.apk
            ensure_alpine_apk "$ALP/riscv64-openjdk21-jdk-21.0.11_p10-r0.apk"          edge/community riscv64 openjdk21-jdk-21.0.11_p10-r0.apk
            ensure_alpine_apk "$ALP/riscv64-openjdk21-jmods-21.0.11_p10-r0.apk"        edge/community riscv64 openjdk21-jmods-21.0.11_p10-r0.apk ;;
        loongarch64)
            ensure_alpine_apk "$ALP/loongarch64-openjdk21-jre-headless-21.0.11_p10-r0.apk" edge/community loongarch64 openjdk21-jre-headless-21.0.11_p10-r0.apk
            ensure_alpine_apk "$ALP/loongarch64-openjdk21-jdk-21.0.11_p10-r0.apk"          edge/community loongarch64 openjdk21-jdk-21.0.11_p10-r0.apk
            ensure_alpine_apk "$ALP/loongarch64-openjdk21-jmods-21.0.11_p10-r0.apk"        edge/community loongarch64 openjdk21-jmods-21.0.11_p10-r0.apk ;;
    esac

    # JDK23 — BellSoft musl tars (x86_64/aarch64); BellSoft generic-glibc tar (riscv64);
    # Loongson glibc full tar (loongarch64). The rv/loong glibc cells are bridged by the
    # gcompat shim staged below; stage_jdk23 + run-java.sh's probe attempt them on all 4 arches.
    case "$arch" in
        x86_64)  ensure_asset "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-x64-musl.tar.gz" \
                    "$BELLSOFT_CDN/23.0.2+9/bellsoft-jdk23.0.2+9-linux-x64-musl.tar.gz" \
                    16f67aed6d6564f3bec7b5904ccc35f48b1b9dddf32540b47975eb1c155603ce ;;
        aarch64) ensure_asset "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-aarch64-musl.tar.gz" \
                    "$BELLSOFT_CDN/23.0.2+9/bellsoft-jdk23.0.2+9-linux-aarch64-musl.tar.gz" \
                    40b39d58eb66598f46245e85a34bd1271cd1ddf077fa2d4357a5377cda7c8b59 ;;
        riscv64) ensure_asset "$JM/jdk23/bellsoft-jdk23.0.2+9-linux-riscv64.tar.gz" \
                    "$BELLSOFT_CDN/23.0.2+9/bellsoft-jdk23.0.2+9-linux-riscv64.tar.gz" \
                    912dbba0e3dca9b0981891dda8746d221bd78f0346a46cb5027c70503952add4 ;;  # glibc -> gcompat
        loongarch64)
            # Loongson 'loongsonNN-fx-jdk' glibc full tar — Loongson ships no stable github
            # release for this asset (see jdk-multi/SOURCES.md §0); supply via cache or set
            # JDK23_LOONG_TAR_URL to a maintainer-hosted copy.
            ensure_asset "$JM/jdk23/loongson23.1.17-fx-jdk23_37-linux-loongarch64.tar.gz" \
                "${JDK23_LOONG_TAR_URL:-}" \
                55e4ab6a285962a24f2a916720899bcac0ce2838a63d44e75359aa49300fe8c9 ;;
    esac

    # JDK25 — BellSoft musl tars (x86_64/aarch64); Alpine edge/community apks (riscv64);
    # Alpine edge/community LoongArch native C2-JIT variant (loongarch64).
    case "$arch" in
        x86_64)  ensure_asset "$JM/jdk25/bellsoft-jdk25+37-linux-x64-musl.tar.gz" \
                    "$BELLSOFT_CDN/25+37/bellsoft-jdk25+37-linux-x64-musl.tar.gz" \
                    c39d961788095b7facc97d88cbf5c26e2b88b2df30438c0b0e5e637a44e708ef ;;
        aarch64) ensure_asset "$JM/jdk25/bellsoft-jdk25+37-linux-aarch64-musl.tar.gz" \
                    "$BELLSOFT_CDN/25+37/bellsoft-jdk25+37-linux-aarch64-musl.tar.gz" \
                    0d0aae364e44768059358434fe8bfe2aaa209866586c9f38842fec6f03f363c3 ;;
        riscv64)
            ensure_alpine_apk "$ALP/riscv64-openjdk25-jre-headless-25.0.3_p9-r1.apk" edge/community riscv64 openjdk25-jre-headless-25.0.3_p9-r1.apk
            ensure_alpine_apk "$ALP/riscv64-openjdk25-jdk-25.0.3_p9-r1.apk"          edge/community riscv64 openjdk25-jdk-25.0.3_p9-r1.apk
            ensure_alpine_apk "$ALP/riscv64-openjdk25-jmods-25.0.3_p9-r1.apk"        edge/community riscv64 openjdk25-jmods-25.0.3_p9-r1.apk ;;
        loongarch64)
            local g="$JM/jdk25/loongarch64-alpine-musl"
            ensure_alpine_apk "$g/openjdk25-loongarch-jre-headless-25.0.1_p8-r1.apk" edge/community loongarch64 openjdk25-loongarch-jre-headless-25.0.1_p8-r1.apk 28e19f2c14d8137d9e767347ef61953af99fec263a1374e6221d4d22b8ef3796
            ensure_alpine_apk "$g/openjdk25-loongarch-jre-25.0.1_p8-r1.apk"          edge/community loongarch64 openjdk25-loongarch-jre-25.0.1_p8-r1.apk          a8ab4d738501c5ff3a8257d2c9e85684200dd5275b097efe0cadaa80693dddb6
            ensure_alpine_apk "$g/openjdk25-loongarch-jdk-25.0.1_p8-r1.apk"          edge/community loongarch64 openjdk25-loongarch-jdk-25.0.1_p8-r1.apk          4e1c4d1aada0a4f524ec5200ec705cb0c7769fe77e0f5d7ccf998becafce61df
            ensure_alpine_apk "$g/openjdk25-loongarch-jmods-25.0.1_p8-r1.apk"        edge/community loongarch64 openjdk25-loongarch-jmods-25.0.1_p8-r1.apk        cb6886b8f3f5306f2f509c542d94a1adba523e08881a1fc73a762da7ce3b5fc0 ;;
    esac

    # loongarch64 JDK21 Zero VM dep: libffi.so.8 (edge/main; sha256 from local golden).
    if [[ "$arch" == "loongarch64" ]]; then
        ensure_alpine_apk "$ALP/loongarch64-libffi-3.5.2-r1.apk" edge/main loongarch64 libffi-3.5.2-r1.apk \
            f81152fb8a31cc7e3a3e32602b4278d1c1d2dcfeecf4a58ef47afa5237db924f
    fi

    # gcompat glibc-on-musl shim + its runtime deps (libucontext, musl-obstack), for the
    # riscv64 / loongarch64 glibc JDK23 cell — same gcompat staging prep-jdk-multi-rootfs.sh
    # uses (it stages gcompat alone; we additionally fetch its DT_NEEDED libucontext/obstack so
    # libgcompat.so.0 actually loads). All three live in Alpine edge/main; sha256 from the local
    # golden in download/openjdk17-apks/<arch>/. Consumed by stage_gcompat() for rv/loong only.
    case "$arch" in
        riscv64)
            local g="$DL/openjdk17-apks/riscv64"
            ensure_alpine_apk "$g/gcompat-1.1.0-r4.apk"       edge/main riscv64 gcompat-1.1.0-r4.apk \
                caab16a14d67186db08a40970e3ff00925cc0267ea5546591f9298126b3637eb
            ensure_alpine_apk "$g/libucontext-1.5.1-r0.apk"   edge/main riscv64 libucontext-1.5.1-r0.apk \
                5cea6f66a7e23ebdd0d2d31f7958fca4cd75812dd4f4c92e2c94bedd95e6aadc
            ensure_alpine_apk "$g/musl-obstack-1.2.3-r2.apk"  edge/main riscv64 musl-obstack-1.2.3-r2.apk \
                98081e0015a726db622fbc9da8bc8af744ab75eb31b6ba1590d4cdf36bdf5cff ;;
        loongarch64)
            local g="$DL/openjdk17-apks/loongarch64"
            ensure_alpine_apk "$g/gcompat-1.1.0-r4.apk"       edge/main loongarch64 gcompat-1.1.0-r4.apk \
                76719a89dddba800681cd8f3192d3298f5af6873cd72506f14d22bcc34cc9d8c
            ensure_alpine_apk "$g/libucontext-1.5.1-r0.apk"   edge/main loongarch64 libucontext-1.5.1-r0.apk \
                593431039c826f706359071f44362dc8ca50fcbddcaab0171ade1aaacd90491e
            ensure_alpine_apk "$g/musl-obstack-1.2.3-r2.apk"  edge/main loongarch64 musl-obstack-1.2.3-r2.apk \
                1db9aa304eebdce0d7d18c9e850e2dc4ecfb927c436ad6ea881ae5f4a35112ff ;;
    esac
}

main() {
    case "$arch" in x86_64|aarch64|riscv64|loongarch64) ;; *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;; esac
    ensure_host_tools
    ensure_assets
    grow_rootfs

    stage_jdk17
    stage_jdk21
    stage_jdk23   # all 4 arches; rv/loong are glibc -> bridged by stage_gcompat
    stage_jdk25
    stage_gcompat # rv/loong gcompat shim (+ libucontext/musl-obstack) for the glibc JDK23
    stage_deps    # loong JDK21 Zero VM libffi.so.8
    stage_test_sources
    stage_alternatives_and_sdkman
    stage_backcompat  # real-world Java-8 (--release 8) forward-compat suite into /root/bcreal

    echo "prebuild: java-lang overlay ready for $arch — staged JDKs:"
    local V
    for V in 17 21 23 25; do
        if [[ -x "$overlay_dir/opt/jdk$V/bin/javac" ]]; then
            echo "  /opt/jdk$V  javac+java OK ($(du -sh "$overlay_dir/opt/jdk$V" | cut -f1))"
        fi
    done
    echo "prebuild: total overlay $(du -sh "$overlay_dir" | cut -f1)"
}

main "$@"
