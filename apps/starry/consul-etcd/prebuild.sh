#!/usr/bin/env bash
# prebuild.sh - provision the Consul + etcd distributed-KV carpet for StarryOS.
#
# Two production Go services run single-node on-target, driven by an anchored gate
# (programs/run-consul-etcd.sh):
#   Consul 1.22.7 - HashiCorp service discovery / KV / health / serf gossip / raft.
#   etcd   3.6.11 - Raft consensus + bbolt MVCC KV over gRPC.
# Both are FULLY STATIC, CGO-disabled Go ELF binaries (no libc / no interp), so the
# rootfs needs no dependency closure - the binaries drop in and run. The Go runtime
# gets entropy via getrandom(2) and parks goroutines via futex (both provided by
# StarryOS); loopback AF_INET TCP/UDP carries serf/raft/HTTP (consul) and client/peer
# RPC (etcd).
#
# --- SOURCE-ONLY REPO, REPRODUCIBLE PROVISION ----------------------------------------
#   The tree keeps only source + manifests (this script, the gate, the service config).
#   NO binaries are committed. prebuild fetches / builds every binary by pinned identity
#   into a portable cache (CONSUL_ETCD_DL_ROOT), re-used network-free on later runs:
#     consul x86_64/aarch64 : official HashiCorp release zip, sha256-pinned download.
#     consul riscv64/loong64: HashiCorp ships NO release for these arches (two indirect
#                             deps - boltdb + gopsutil - lack the arch files). The binary
#                             is CROSS-COMPILED IN-PREBUILD from the pinned consul source
#                             tag (CGO_ENABLED=0, GOARCH=<goarch>) with the two missing
#                             arch files patched in - exactly the official recipe. A
#                             pre-populated cache short-circuits the build; the output
#                             binary's own sha256 is NOT pinned (it tracks the Go
#                             toolchain version), reproducibility is anchored on the
#                             pinned SOURCE tag. If go/git are absent and no cache exists,
#                             prebuild fails hard (the gate has no skip path).
#     etcd all four arches  : official etcd-io release tarball, sha256-pinned download
#                             (v3.6.11 ships amd64/arm64/riscv64/loong64).
#
# --- DATA DIRS ON EXT4, NOT tmpfs ----------------------------------------------------
#   The gate runs etcd (and consul snapshots) under /root (ext4, bounded LRU page cache),
#   never /tmp (tmpfs, unbounded): bbolt mmaps its db with a huge InitialMmapSize; on a
#   tmpfs backend that pins unbounded pages. This is the etcd-0 lesson baked into the gate.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR, STARRY_ROOTFS,
# STARRY_STAGING_ROOT.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
rootfs_img="${STARRY_ROOTFS:?prebuild: STARRY_ROOTFS required}"

PROG="$app_dir/programs"
DL="${CONSUL_ETCD_DL_ROOT:-${STARRY_STAGING_ROOT:-$app_dir}/.cache/consul-etcd-dl}"
ROOTFS_SIZE="${CONSUL_ETCD_ROOTFS_SIZE:-2560M}"

CONSUL_VER=1.22.7
CONSUL_REL="${CONSUL_REL:-https://releases.hashicorp.com/consul/${CONSUL_VER}}"
CONSUL_SRC_TAG=v1.22.7
CONSUL_SRC_COMMIT=c18bcb9d
CONSUL_SRC_URL="${CONSUL_SRC_URL:-https://github.com/hashicorp/consul}"
ETCD_VER=v3.6.11
ETCD_REL="${ETCD_REL:-https://github.com/etcd-io/etcd/releases/download/${ETCD_VER}}"

# arch -> (consul release goarch, etcd release goarch)
case "$arch" in
    x86_64)      CONSUL_GOARCH=amd64;   ETCD_GOARCH=amd64 ;;
    aarch64)     CONSUL_GOARCH=arm64;   ETCD_GOARCH=arm64 ;;
    riscv64)     CONSUL_GOARCH=riscv64; ETCD_GOARCH=riscv64 ;;
    loongarch64) CONSUL_GOARCH=loong64; ETCD_GOARCH=loong64 ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

# ensure_asset <abs-local-path> <url> [sha256]
#   Cache hit (sha256 matches when given) -> used as-is, zero network. Otherwise curl the
#   URL to a temp file, verify sha256 when given (mismatch = hard error), atomically move
#   into place. An empty sha skips verification; an empty URL with no cache is a hard error.
ensure_asset() {
    local dest="$1" url="$2" want="${3:-}"
    if [[ -f "$dest" ]]; then
        if [[ -n "$want" ]] && command -v sha256sum >/dev/null 2>&1; then
            local have; have="$(sha256sum "$dest" | cut -d' ' -f1)"
            if [[ "$have" == "$want" ]]; then echo "prebuild: cache hit $(basename "$dest") (sha256 ok)"; return 0; fi
            echo "prebuild: cache $(basename "$dest") sha256 mismatch (have $have want $want) - refetching" >&2
            rm -f "$dest"
        else
            echo "prebuild: cache hit $(basename "$dest")"; return 0
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

ensure_host_tools() {
    local missing=()
    command -v tar       >/dev/null 2>&1 || missing+=(tar)
    command -v curl      >/dev/null 2>&1 || missing+=(curl)
    command -v unzip     >/dev/null 2>&1 || missing+=(unzip)
    command -v resize2fs >/dev/null 2>&1 || missing+=(resize2fs/e2fsprogs)
    command -v e2fsck    >/dev/null 2>&1 || missing+=(e2fsck/e2fsprogs)
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "prebuild: missing required host tools: ${missing[*]}" >&2
        echo "prebuild: install them (e.g. apt-get install -y tar curl unzip e2fsprogs) and re-run" >&2
        exit 1
    fi
}

# Grow the per-app rootfs so the injected binaries fit without truncation. Grow-only
# (never shrink a shared/base image). Idempotent.
grow_rootfs() {
    [[ -f "$rootfs_img" ]] || { echo "prebuild: rootfs image missing: $rootfs_img" >&2; exit 2; }
    local cur target
    cur=$(stat -c %s "$rootfs_img"); target=$(( ${ROOTFS_SIZE%M} * 1024 * 1024 ))
    [[ "$cur" -lt "$target" ]] && truncate -s "$ROOTFS_SIZE" "$rootfs_img"
    e2fsck -f -y "$rootfs_img" >/dev/null 2>&1 || true
    resize2fs "$rootfs_img" >/dev/null 2>&1 || { echo "prebuild: resize2fs failed on $rootfs_img" >&2; exit 2; }
    echo "prebuild: rootfs sized to $(( $(stat -c %s "$rootfs_img")/1024/1024 )) MiB"
}

# --- consul provisioning -------------------------------------------------------------
CONSUL_ZIP_SHA_amd64=fe25cecd8dd3552a8e5b0941cde1d79bb6004eac384aa45679dd1398f947201d
CONSUL_ZIP_SHA_arm64=db54c5fb7c5ceaef97a38ca45dcc0f649ff592a48c73ab320e2d535c78e136cc

# Cross-compile consul for riscv64/loong64 from the pinned source tag, patching in the two
# indirect-dep arch files HashiCorp's release build lacks. Pure Go, CGO_ENABLED=0 -> no C
# cross-toolchain needed. Emits <out>. Returns non-zero if go/git are unavailable.
build_consul_cross() {
    local goarch="$1" out="$2"
    command -v go  >/dev/null 2>&1 || { echo "prebuild: ERROR 'go' not on PATH; Go >=1.22 is required to cross-build consul for $goarch (no official release binary exists for this arch)" >&2; return 1; }
    command -v git >/dev/null 2>&1 || { echo "prebuild: ERROR 'git' not on PATH; git is required to clone the consul source for cross-building" >&2; return 1; }
    local src="$DL/consul-src"
    if [[ ! -d "$src/.git" ]]; then
        echo "prebuild: cloning consul $CONSUL_SRC_TAG (cross-build for $goarch)"
        git clone --depth 1 --branch "$CONSUL_SRC_TAG" "$CONSUL_SRC_URL" "$src" || { rm -rf "$src"; return 1; }
    fi
    ( cd "$src" && go mod download github.com/boltdb/bolt github.com/shirou/gopsutil/v3 ) || return 1
    local gomod; gomod="$(go env GOMODCACHE)"
    # boltdb@v1.3.1 (via raft-boltdb) lacks bolt_riscv64.go / bolt_loong64.go.
    local boltp="$DL/bolt-patched"
    rm -rf "$boltp"; cp -r "$gomod/github.com/boltdb/bolt@v1.3.1" "$boltp"; chmod -R u+w "$boltp"
    printf 'module github.com/boltdb/bolt\n\ngo 1.12\n' > "$boltp/go.mod"
    local a
    for a in riscv64 loong64; do
        cat > "$boltp/bolt_$a.go" <<GO
//go:build $a

package bolt

const maxMapSize = 0xFFFFFFFFFFFF // 256TB
const maxAllocSize = 0x7FFFFFFF
var brokenUnaligned = false
GO
    done
    # gopsutil/v3@v3.22.9 lacks host_linux_loong64.go (same LP64 utmp layout as riscv64).
    local gpp="$DL/gopsutil-patched"
    rm -rf "$gpp"; cp -r "$gomod/github.com/shirou/gopsutil/v3@v3.22.9" "$gpp"; chmod -R u+w "$gpp"
    cp "$gpp/host/host_linux_riscv64.go" "$gpp/host/host_linux_loong64.go"
    (
        cd "$src"
        go mod edit -replace github.com/boltdb/bolt="$boltp"
        go mod edit -replace github.com/shirou/gopsutil/v3="$gpp"
        local ld="-s -w -X github.com/hashicorp/consul/version.GitVersion=${CONSUL_VER} -X github.com/hashicorp/consul/version.GitCommit=${CONSUL_SRC_COMMIT} -X github.com/hashicorp/consul/version.GitDescribe=${CONSUL_SRC_TAG}"
        CGO_ENABLED=0 GOOS=linux GOARCH="$goarch" go build -trimpath -ldflags "$ld" -o "$out.tmp" .
    ) || return 1
    # sanity: output is an ELF binary.
    [[ "$(head -c4 "$out.tmp" 2>/dev/null | od -An -tx1 | tr -d ' \n')" == "7f454c46" ]] \
        || { echo "prebuild: NOTE cross-built consul $goarch is not an ELF" >&2; rm -f "$out.tmp"; return 1; }
    mv -f "$out.tmp" "$out"
    return 0
}

stage_consul() {
    local dst="$overlay_dir/usr/local/bin/consul"
    case "$arch" in
        x86_64|aarch64)
            local shavar="CONSUL_ZIP_SHA_${CONSUL_GOARCH}"
            local zip="$DL/consul/$arch/consul_${CONSUL_VER}_linux_${CONSUL_GOARCH}.zip"
            ensure_asset "$zip" "$CONSUL_REL/consul_${CONSUL_VER}_linux_${CONSUL_GOARCH}.zip" "${!shavar}"
            local t; t="$(mktemp -d)"
            unzip -oq "$zip" consul -d "$t" || { echo "prebuild: failed to extract consul from $zip" >&2; exit 3; }
            install -Dm0755 "$t/consul" "$dst"; rm -rf "$t" ;;
        riscv64|loongarch64)
            local bin="$DL/consul-cross/$arch/consul"
            if [[ ! -f "$bin" ]]; then
                mkdir -p "$(dirname "$bin")"
                build_consul_cross "$CONSUL_GOARCH" "$bin" \
                    || { echo "prebuild: ERROR could not provision consul for $arch (no cache and cross-build unavailable); the gate has no skip path" >&2; exit 3; }
                echo "prebuild: cross-compiled consul $arch from source tag $CONSUL_SRC_TAG"
            else
                echo "prebuild: consul $arch = cached cross-built binary"
            fi
            install -Dm0755 "$bin" "$dst" ;;
    esac
    echo "prebuild: staged consul ($(du -h "$dst" | cut -f1)) -> /usr/local/bin/consul"
}

# --- etcd provisioning (official release tarball, all four arches) -------------------
ETCD_SHA_amd64=8756f7a4eaf921668a83de0bf13c0f65cae9186a165696e3ae8396afe6f557ed
ETCD_SHA_arm64=5302f1a6157c34eb0568c75fba9d06da98353576df04399f08645bef634acd2d
ETCD_SHA_riscv64=78ab006f4045c98a91cc8f435f80f7c4893f91b784d2be7adf9b623ac6e5b721
ETCD_SHA_loong64=cf2d3f51b63f1884805163a1d11dd3f6179fb10d39d447d8a5ce5dcc5a6f2a70

stage_etcd() {
    local shavar="ETCD_SHA_${ETCD_GOARCH}"
    local tarball="$DL/etcd/$arch/etcd-${ETCD_VER}-linux-${ETCD_GOARCH}.tar.gz"
    ensure_asset "$tarball" "$ETCD_REL/etcd-${ETCD_VER}-linux-${ETCD_GOARCH}.tar.gz" "${!shavar}"
    local top="etcd-${ETCD_VER}-linux-${ETCD_GOARCH}"
    local t; t="$(mktemp -d)"
    tar xzf "$tarball" -C "$t" "$top/etcd" "$top/etcdctl" "$top/etcdutl" \
        || { echo "prebuild: failed to extract etcd from $tarball" >&2; exit 3; }
    local b
    for b in etcd etcdctl etcdutl; do
        install -Dm0755 "$t/$top/$b" "$overlay_dir/usr/bin/$b"
    done
    rm -rf "$t"
    echo "prebuild: staged etcd/etcdctl/etcdutl -> /usr/bin"
}

stage_payload() {
    install -Dm0644 "$PROG/consul-service.json" "$overlay_dir/root/consul-etcd/consul-service.json"
    install -Dm0755 "$PROG/run-consul-etcd.sh"  "$overlay_dir/usr/bin/run-consul-etcd.sh"
    echo "prebuild: staged run-consul-etcd.sh + consul-service.json"
}

preflight_cross_check() {
    # For rv64/la64 consul is cross-compiled from source; check go/git availability
    # before growing the rootfs so the caller gets a clear error immediately.
    case "$arch" in
        riscv64|loongarch64)
            local bin="$DL/consul-cross/$arch/consul"
            [[ -f "$bin" ]] && return 0
            command -v go  >/dev/null 2>&1 || { echo "prebuild: ERROR 'go' not on PATH; Go >=1.22 is required to cross-build consul for $arch (no official release binary exists for this arch)" >&2; exit 1; }
            command -v git >/dev/null 2>&1 || { echo "prebuild: ERROR 'git' not on PATH; git is required to clone the consul source for cross-building" >&2; exit 1; }
            ;;
    esac
}

main() {
    ensure_host_tools
    preflight_cross_check
    grow_rootfs
    stage_consul
    stage_etcd
    stage_payload
    echo "prebuild: consul-etcd overlay ready for $arch - $(du -sh "$overlay_dir" | cut -f1)"
}

main "$@"
