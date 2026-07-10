#!/usr/bin/env bash
# prebuild.sh - provision the StarryOS dropbear SSH carpet.
#
# dropbear is a small musl-dynamic SSH suite distributed as Alpine apk packages. This script
# provisions it the reproducible, network-free-at-runtime way: it resolves the CURRENT
# package version from the live Alpine APKINDEX of the branch that MATCHES the rootfs
# (/etc/alpine-release read out of the image), downloads the exact apks (no pinned, drifting
# URL - the version comes from the index), and unpacks the binaries + the two shared libs the
# base rootfs lacks into the overlay. QEMU then needs no guest network.
#
#   dropbear            -> /usr/sbin/dropbear, /usr/bin/dropbearkey
#   dropbear-dbclient   -> /usr/bin/dbclient
#   dropbear-convert    -> /usr/bin/dropbearconvert
#   dropbear-scp        -> /usr/bin/scp
#   dropbear-ssh        -> /usr/bin/ssh (symlink to dbclient)
#   utmps-libs          -> /usr/lib/libutmps.so.*    (dropbear/dbclient NEEDED, absent in base)
#   skalibs-libs        -> /usr/lib/libskarnet.so.*  (libutmps NEEDED, absent in base)
#
# libc.musl and libz.so.1 are already in the Alpine base rootfs, so only libutmps + libskarnet
# are staged. The binaries stay musl-DYNAMIC on every arch (loongarch64 included).
#
# The gate (programs/run-dropbear.sh) is environment-agnostic: it finds the staged binaries
# present and runs the carpet directly; on a fresh rootfs with a reachable mirror it would
# instead `apk add` the same suite. Either way the identical carpet runs.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:-}"

DL="${DROPBEAR_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/dropbear-dl}"
ROOTFS_SIZE="${DROPBEAR_ROOTFS_SIZE:-1536M}"
# Alpine arch dir names match StarryOS arch names 1:1 (x86_64/aarch64/riscv64/loongarch64).
case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) ALPINE_ARCH="$arch" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

PKGS="dropbear dropbear-dbclient dropbear-convert dropbear-scp dropbear-ssh utmps-libs skalibs-libs"

ensure_host_tools() {
    local missing=()
    command -v curl      >/dev/null 2>&1 || missing+=(curl)
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v debugfs   >/dev/null 2>&1 || missing+=(e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

# Resolve the Alpine branch that matches the rootfs (e.g. v3.23), so the musl/ABI stays in
# sync with the image. Read straight out of the ext4 image; fall back to an override.
detect_branch() {
    if [[ -n "${DROPBEAR_APK_BRANCH:-}" ]]; then echo "$DROPBEAR_APK_BRANCH"; return; fi
    local rel="" maj min
    if [[ -n "$rootfs_img" && -f "$rootfs_img" ]]; then
        rel="$(debugfs -R 'cat /etc/alpine-release' "$rootfs_img" 2>/dev/null | tr -d '\r\n ')"
    fi
    maj="$(printf '%s' "$rel" | cut -d. -f1)"; min="$(printf '%s' "$rel" | cut -d. -f2)"
    if [[ -n "$maj" && -n "$min" ]]; then echo "v$maj.$min"; else
        echo "prebuild: cannot read /etc/alpine-release from rootfs; set DROPBEAR_APK_BRANCH" >&2; exit 2
    fi
}

BRANCH="$(detect_branch)"
MIRRORS="${DROPBEAR_APK_MIRROR:-https://dl-cdn.alpinelinux.org/alpine} https://mirrors.tuna.tsinghua.edu.cn/alpine"

# Fetch + cache the main/community APKINDEX for the arch/branch (one per mirror success).
fetch_indexes() {
    mkdir -p "$DL/idx"
    local repo m ok
    for repo in main community; do
        [[ -s "$DL/idx/APKINDEX-$repo" ]] && continue
        ok=0
        for m in $MIRRORS; do
            if curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo/$ALPINE_ARCH/APKINDEX.tar.gz" -o "$DL/idx/$repo.tar.gz" 2>/dev/null; then
                tar xzf "$DL/idx/$repo.tar.gz" -O APKINDEX > "$DL/idx/APKINDEX-$repo" 2>/dev/null && { ok=1; break; }
            fi
        done
        [[ "$ok" = 1 ]] || { echo "prebuild: cannot fetch $repo APKINDEX for $ALPINE_ARCH/$BRANCH" >&2; exit 3; }
    done
}

# resolve <pkg> -> echoes "<repo> <version>" using the APKINDEX (main preferred).
resolve() {
    local pkg="$1" repo ver
    for repo in main community; do
        ver="$(awk -v p="$pkg" 'BEGIN{RS="";FS="\n"} {n="";v="";for(i=1;i<=NF;i++){if($i~/^P:/)n=substr($i,3);if($i~/^V:/)v=substr($i,3)} if(n==p){print v; exit}}' "$DL/idx/APKINDEX-$repo")"
        [[ -n "$ver" ]] && { echo "$repo $ver"; return 0; }
    done
    return 1
}

# download <repo> <pkg> <ver> -> cached apk path (live version, no committed URL).
download_apk() {
    local repo="$1" pkg="$2" ver="$3" dest="$DL/apks/$ALPINE_ARCH/$pkg-$ver.apk" m
    [[ -s "$dest" ]] && { echo "$dest"; return 0; }
    mkdir -p "$(dirname "$dest")"
    for m in $MIRRORS; do
        if curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo/$ALPINE_ARCH/$pkg-$ver.apk" -o "$dest.tmp" 2>/dev/null; then
            mv -f "$dest.tmp" "$dest"; echo "$dest"; return 0
        fi
    done
    echo "prebuild: cannot download $pkg-$ver.apk ($repo/$ALPINE_ARCH/$BRANCH)" >&2; return 1
}

# Grow-only, idempotent - room for the staged binaries and the keys the carpet writes.
grow_rootfs() {
    [[ -n "$rootfs_img" && -f "$rootfs_img" ]] || { echo "prebuild: rootfs not staged, skipping grow"; return 0; }
    local cur target
    cur=$(stat -c %s "$rootfs_img"); target=$(( ${ROOTFS_SIZE%M} * 1024 * 1024 ))
    if [[ "$cur" -lt "$target" ]]; then
        truncate -s "$ROOTFS_SIZE" "$rootfs_img"
        e2fsck -f -y "$rootfs_img" >/dev/null 2>&1 || true
        resize2fs "$rootfs_img" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed" >&2; exit 2; }
    fi
    echo "prebuild: rootfs sized to $(( $(stat -c %s "$rootfs_img")/1024/1024 )) MiB"
}

stage_suite() {
    local ex="$DL/extract/$ALPINE_ARCH"
    rm -rf "$ex"; mkdir -p "$ex"
    local pkg rv repo ver apk
    for pkg in $PKGS; do
        rv="$(resolve "$pkg")" || { echo "prebuild: $pkg not in APKINDEX ($ALPINE_ARCH/$BRANCH)" >&2; exit 3; }
        repo="${rv%% *}"; ver="${rv##* }"
        apk="$(download_apk "$repo" "$pkg" "$ver")" || exit 3
        # apk = gzip tar; extract the useful paths, tolerate the leading metadata members.
        tar -xzf "$apk" -C "$ex" 2>/dev/null || tar -xf "$apk" -C "$ex" 2>/dev/null || true
        echo "prebuild: unpacked $pkg=$ver ($repo)"
    done
    # Copy binaries + libs preserving symlinks/perms (ssh is a symlink to dbclient).
    mkdir -p "$overlay_dir/usr/sbin" "$overlay_dir/usr/bin" "$overlay_dir/usr/lib"
    cp -a "$ex/usr/sbin/dropbear"            "$overlay_dir/usr/sbin/dropbear"
    local b
    for b in dropbearkey dbclient dropbearconvert scp ssh; do
        cp -a "$ex/usr/bin/$b" "$overlay_dir/usr/bin/$b"
    done
    cp -a "$ex"/usr/lib/libutmps.so.*   "$overlay_dir/usr/lib/"
    cp -a "$ex"/usr/lib/libskarnet.so.* "$overlay_dir/usr/lib/"
    chmod 0755 "$overlay_dir/usr/sbin/dropbear" "$overlay_dir/usr/bin/dropbearkey" \
               "$overlay_dir/usr/bin/dropbearconvert" 2>/dev/null || true
    # dbclient/scp may be setuid in the apk; keep them executable (no setuid needed on-target).
    chmod 0755 "$overlay_dir/usr/bin/dbclient" "$overlay_dir/usr/bin/scp" 2>/dev/null || true
    echo "prebuild: staged dropbear suite -> overlay (dropbear/dropbearkey/dbclient/dropbearconvert/scp/ssh + libutmps + libskarnet)"
}

main() {
    ensure_host_tools
    echo "prebuild: arch=$arch alpine-arch=$ALPINE_ARCH branch=$BRANCH"
    grow_rootfs
    fetch_indexes
    stage_suite
    install -Dm0755 "$app_dir/programs/run-dropbear.sh" "$overlay_dir/usr/bin/run-dropbear.sh"
    echo "prebuild: dropbear overlay ready for $arch"
}

main "$@"
