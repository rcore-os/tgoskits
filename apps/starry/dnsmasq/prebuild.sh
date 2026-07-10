#!/usr/bin/env bash
# prebuild.sh - provision the StarryOS dnsmasq DNS/DHCP carpet.
#
# dnsmasq is a single musl-dynamic binary distributed as an Alpine apk package. This script
# provisions it the reproducible, network-free-at-runtime way: it resolves the CURRENT
# package version from the live Alpine APKINDEX of the branch that MATCHES the rootfs
# (/etc/alpine-release read out of the image), downloads the exact apk (no pinned, drifting
# URL - the version comes from the index), and unpacks the one binary into the overlay.
# QEMU then needs no guest network.
#
#   dnsmasq -> /usr/sbin/dnsmasq
#
# dnsmasq NEEDs only libc.musl, which the Alpine base rootfs already ships, so no extra
# shared library is staged. tftp-hpa (a single musl-dynamic /usr/bin/tftp needing only libc)
# is staged alongside so the integrated TFTP server can be driven by a real client and byte-
# checked. The DNS clients (busybox nslookup, with -type= for every record class) and the
# DHCP client (busybox udhcpc) are base busybox applets, so nothing else is staged. Every
# staged binary stays musl-DYNAMIC on every arch (loongarch64 included).
#
# The gate (programs/run-dnsmasq.sh) is environment-agnostic: it finds the staged binary
# present and runs the carpet directly; on a fresh rootfs with a reachable mirror it would
# instead `apk add dnsmasq`. Either way the identical carpet runs.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:-}"

DL="${DNSMASQ_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/dnsmasq-dl}"
ROOTFS_SIZE="${DNSMASQ_ROOTFS_SIZE:-1536M}"
# Alpine arch dir names match StarryOS arch names 1:1 (x86_64/aarch64/riscv64/loongarch64).
case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) ALPINE_ARCH="$arch" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

PKGS="dnsmasq tftp-hpa"

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
    if [[ -n "${DNSMASQ_APK_BRANCH:-}" ]]; then echo "$DNSMASQ_APK_BRANCH"; return; fi
    local rel="" maj min
    if [[ -n "$rootfs_img" && -f "$rootfs_img" ]]; then
        rel="$(debugfs -R 'cat /etc/alpine-release' "$rootfs_img" 2>/dev/null | tr -d '\r\n ')"
    fi
    maj="$(printf '%s' "$rel" | cut -d. -f1)"; min="$(printf '%s' "$rel" | cut -d. -f2)"
    if [[ -n "$maj" && -n "$min" ]]; then echo "v$maj.$min"; else
        echo "prebuild: cannot read /etc/alpine-release from rootfs; set DNSMASQ_APK_BRANCH" >&2; exit 2
    fi
}

BRANCH="$(detect_branch)"
MIRRORS="${DNSMASQ_APK_MIRROR:-https://dl-cdn.alpinelinux.org/alpine} https://mirrors.tuna.tsinghua.edu.cn/alpine"

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

# Grow-only, idempotent - room for the staged binary, the apk cache and generated configs.
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
    mkdir -p "$overlay_dir/usr/sbin" "$overlay_dir/usr/bin"
    cp -a "$ex/usr/sbin/dnsmasq" "$overlay_dir/usr/sbin/dnsmasq"
    chmod 0755 "$overlay_dir/usr/sbin/dnsmasq"
    cp -a "$ex/usr/bin/tftp" "$overlay_dir/usr/bin/tftp"
    chmod 0755 "$overlay_dir/usr/bin/tftp"
    echo "prebuild: staged dnsmasq + tftp -> overlay (base musl / busybox nslookup + udhcpc suffice)"
}

main() {
    ensure_host_tools
    echo "prebuild: arch=$arch alpine-arch=$ALPINE_ARCH branch=$BRANCH"
    grow_rootfs
    fetch_indexes
    stage_suite
    install -Dm0755 "$app_dir/programs/run-dnsmasq.sh" "$overlay_dir/usr/bin/run-dnsmasq.sh"
    echo "prebuild: dnsmasq overlay ready for $arch"
}

main "$@"
