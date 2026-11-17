#!/usr/bin/env bash
# prebuild.sh - stage dnsmasq and the tftp-hpa client into the StarryOS overlay.
#
# The packages are the exact files recorded in apk.lock. Each one is fetched from an Alpine
# mirror and accepted only if its sha256 matches the lock, so a commit always tests the same
# bytes and a mirror cannot substitute anything. The live index is read only by
# update-apk-lock.sh, which checks Alpine's signature before it records a file.
#
#   dnsmasq  -> /usr/sbin/dnsmasq
#   tftp-hpa -> /usr/bin/tftp
#
# Both need only libc.musl, which the Alpine base rootfs ships. The DNS and DHCP clients
# (busybox nslookup and udhcpc) are base busybox applets, so nothing else is staged.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:-}"
lock="$app_dir/apk.lock"

DL="${DNSMASQ_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/dnsmasq-dl}"
ROOTFS_SIZE="${DNSMASQ_ROOTFS_SIZE:-1536M}"
MIRRORS="${DNSMASQ_APK_MIRROR:-https://dl-cdn.alpinelinux.org/alpine} https://mirrors.tuna.tsinghua.edu.cn/alpine"
# Alpine arch dir names match StarryOS arch names 1:1.
case "$arch" in
    x86_64|aarch64|riscv64|loongarch64) ALPINE_ARCH="$arch" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

ensure_host_tools() {
    local missing=()
    command -v curl      >/dev/null 2>&1 || missing+=(curl)
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v sha256sum >/dev/null 2>&1 || missing+=(coreutils)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v resize2fs >/dev/null 2>&1 || missing+=(e2fsprogs)
    command -v debugfs   >/dev/null 2>&1 || missing+=(e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        if [[ "${TGOSKITS_PREBUILT_CONTAINER:-}" = 1 ]]; then
            echo "error: missing host dependencies in prepared container: ${missing[*]}" >&2
            exit 1
        fi

        if command -v apt-get >/dev/null 2>&1; then
            apt-get update && apt-get install -y --no-install-recommends "${missing[@]}"
        else
            echo "prebuild: missing host tools and no apt-get: ${missing[*]}" >&2; exit 1
        fi
    fi
}

BRANCH="$(awk '$1 == "branch" { print $2; exit }' "$lock")"
[[ -n "$BRANCH" ]] || { echo "prebuild: $lock names no Alpine branch" >&2; exit 2; }

# The staged binaries link against the rootfs musl, so the rootfs must be the Alpine branch
# the lock was made for.
check_rootfs_branch() {
    [[ -n "$rootfs_img" && -f "$rootfs_img" ]] || return 0
    local rel have
    rel="$(debugfs -R 'cat /etc/alpine-release' "$rootfs_img" 2>/dev/null | tr -d '\r\n ')"
    have="v$(printf '%s' "$rel" | cut -d. -f1-2)"
    if [[ "$have" != "$BRANCH" ]]; then
        echo "prebuild: the rootfs is Alpine ${rel:-unknown} but apk.lock pins $BRANCH;" \
             "run update-apk-lock.sh $have and commit apk.lock" >&2
        exit 2
    fi
}

sha256_is() { [[ "$(sha256sum "$1" | cut -d' ' -f1)" == "$2" ]]; }

# fetch_locked <repo> <pkg> <version> <sha256> -> path of the cached, matching apk
fetch_locked() {
    local repo="$1" pkg="$2" ver="$3" sum="$4" m
    local dest="$DL/apks/$ALPINE_ARCH/$pkg-$ver.apk"
    if [[ -s "$dest" ]] && sha256_is "$dest" "$sum"; then echo "$dest"; return 0; fi
    mkdir -p "$(dirname "$dest")"
    for m in $MIRRORS; do
        rm -f "$dest.tmp"
        curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo/$ALPINE_ARCH/$pkg-$ver.apk" \
            -o "$dest.tmp" 2>/dev/null || continue
        if sha256_is "$dest.tmp" "$sum"; then mv -f "$dest.tmp" "$dest"; echo "$dest"; return 0; fi
        echo "prebuild: $m served $pkg-$ver.apk with a sha256 other than apk.lock's" >&2
    done
    rm -f "$dest.tmp"
    echo "prebuild: no mirror has $pkg-$ver.apk ($ALPINE_ARCH) with the locked sha256; if" \
         "Alpine has replaced that version, run update-apk-lock.sh and commit apk.lock" >&2
    return 1
}

# Grow-only, idempotent: room for the staged binaries and generated configs.
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
    local ex="$DL/extract/$ALPINE_ARCH" a repo pkg ver sum apk bin
    rm -rf "$ex"; mkdir -p "$ex"
    while read -r a repo pkg ver sum; do
        [[ "$a" == "$ALPINE_ARCH" ]] || continue
        apk="$(fetch_locked "$repo" "$pkg" "$ver" "$sum")" || exit 3
        # An apk is concatenated gzip tar streams; tar skips the metadata members.
        tar -xzf "$apk" -C "$ex" 2>/dev/null || true
        echo "prebuild: unpacked $pkg=$ver ($repo)"
    done < <(awk '!/^#/ && NF == 5' "$lock")
    for bin in usr/sbin/dnsmasq usr/bin/tftp; do
        [[ -x "$ex/$bin" ]] || { echo "prebuild: apk.lock staged no /$bin for $ALPINE_ARCH" >&2; exit 3; }
        install -Dm0755 "$ex/$bin" "$overlay_dir/$bin"
    done
    echo "prebuild: staged dnsmasq + tftp -> overlay"
}

main() {
    ensure_host_tools
    echo "prebuild: arch=$arch alpine-arch=$ALPINE_ARCH branch=$BRANCH"
    check_rootfs_branch
    grow_rootfs
    stage_suite
    install -Dm0755 "$app_dir/programs/run-dnsmasq.sh" "$overlay_dir/usr/bin/run-dnsmasq.sh"
    echo "prebuild: dnsmasq overlay ready for $arch"
}

main "$@"
