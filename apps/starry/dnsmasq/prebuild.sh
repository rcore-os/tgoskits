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
    command -v python3   >/dev/null 2>&1 || missing+=(python3)
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

# resolve <pkg> -> echoes "<repo> <version> <C:checksum>" using the APKINDEX.
# The C: field (Q1<base64-sha1>) is the SHA1 of the APK control section, used for
# post-download verification against the APKINDEX to detect transit corruption or substitution.
resolve() {
    local pkg="$1" repo fields
    for repo in main community; do
        fields="$(awk -v p="$pkg" '
            BEGIN{RS="";FS="\n"}
            {n="";v="";c=""
             for(i=1;i<=NF;i++){
               if($i~/^P:/) n=substr($i,3)
               if($i~/^V:/) v=substr($i,3)
               if($i~/^C:/) c=substr($i,3)
             }
             if(n==p){print v, c; exit}
            }' "$DL/idx/APKINDEX-$repo")"
        [[ -n "$fields" ]] && { echo "$repo $fields"; return 0; }
    done
    return 1
}

# verify_apk <apk-file> <C:checksum> - verify the APK control-section SHA1 against
# the APKINDEX C: field.  Alpine APK format: three concatenated gzip streams
# (signature | control | data); the C: field is Q1<base64(sha1(stream2))>.
verify_apk() {
    local apk="$1" want="$2"
    [[ -n "$want" ]] || { echo "prebuild: no C: checksum in APKINDEX for $(basename "$apk")" >&2; exit 4; }
    python3 - "$apk" "$want" <<'PYEOF'
import sys, gzip, io, hashlib, base64
p, want = sys.argv[1], sys.argv[2]
with open(p, 'rb') as f:
    raw = f.read()
# Skip gzip stream 1 (RSA signature section) to find where stream 2 starts.
buf = io.BytesIO(raw)
with gzip.GzipFile(fileobj=buf) as g:
    while g.read(65536): pass
ctrl_start = buf.tell()
# Stream 2 is the control section; capture its exact byte range.
buf2 = io.BytesIO(raw[ctrl_start:])
with gzip.GzipFile(fileobj=buf2) as g:
    while g.read(65536): pass
ctrl_bytes = raw[ctrl_start : ctrl_start + buf2.tell()]
# C: field format: Q1<base64-encoded-sha1>.
want_b64 = want[2:] if want.startswith('Q1') else want
want_sha1 = base64.b64decode(want_b64 + '==')
got_sha1 = hashlib.sha1(ctrl_bytes).digest()
if got_sha1 != want_sha1:
    print(f"prebuild: APK checksum mismatch for {p}", file=sys.stderr)
    print(f"  want {want_b64}", file=sys.stderr)
    print(f"  got  {base64.b64encode(got_sha1).decode()}", file=sys.stderr)
    sys.exit(4)
print(f"prebuild: verified {p} (C: {want})")
PYEOF
}

# download_apk <repo> <pkg> <ver> <chk> -> cached apk path.
# Downloads from the live mirror and verifies the APKINDEX C: checksum before caching.
download_apk() {
    local repo="$1" pkg="$2" ver="$3" chk="$4" dest="$DL/apks/$ALPINE_ARCH/$pkg-$ver.apk" m
    if [[ -s "$dest" ]]; then
        verify_apk "$dest" "$chk" && { echo "$dest"; return 0; }
        echo "prebuild: cached $pkg-$ver.apk failed checksum; re-downloading" >&2
        rm -f "$dest"
    fi
    mkdir -p "$(dirname "$dest")"
    for m in $MIRRORS; do
        if curl -fsSL --retry 3 --connect-timeout 20 "$m/$BRANCH/$repo/$ALPINE_ARCH/$pkg-$ver.apk" -o "$dest.tmp" 2>/dev/null; then
            verify_apk "$dest.tmp" "$chk" || { rm -f "$dest.tmp"; continue; }
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
    local pkg rv repo ver chk rest apk
    for pkg in $PKGS; do
        rv="$(resolve "$pkg")" || { echo "prebuild: $pkg not in APKINDEX ($ALPINE_ARCH/$BRANCH)" >&2; exit 3; }
        repo="${rv%% *}"; rest="${rv#* }"; ver="${rest%% *}"; chk="${rest##* }"
        apk="$(download_apk "$repo" "$pkg" "$ver" "$chk")" || exit 3
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
