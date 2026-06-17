#!/usr/bin/env bash
# prebuild.sh — provision the Go 1.26 language carpet for StarryOS.
#
# The carpet (go/*.go) is cross-compiled with the OFFICIAL go1.26.3 toolchain
# as a FULLY STATIC binary (CGO_ENABLED=0 → no libc, no interpreter), so it runs
# on StarryOS musl directly. Output is 100% deterministic, so the on-target run
# is asserted byte-for-byte against the host-generated golden (golden.txt).
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR,
# STARRY_STAGING_ROOT (scratch, used to cache the downloaded toolchain).
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
# Go toolchain + module cache live OUTSIDE the per-app staging-root: the Go module cache is
# written read-only, and if it sat under STARRY_STAGING_ROOT the harness could not remove the
# staging dir on the next run (Permission denied). A persistent cache also avoids re-fetching
# the toolchain + framework deps each run. Overridable via GO_CARPET_CACHE.
cache_root="${GO_CARPET_CACHE:-${HOME:-/root}/.cache/starry-go-carpet}"

case "$arch" in
    x86_64)      goarch=amd64   ;;
    aarch64)     goarch=arm64   ;;
    riscv64)     goarch=riscv64 ;;
    loongarch64) goarch=loong64 ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

GO_VER=1.26.3
goroot="$cache_root/go"
if [[ ! -x "$goroot/bin/go" ]]; then
    # Fetch the official go.dev toolchain for the BUILD host (to cross-compile from),
    # verified against the published SHA256 so the build is reproducible.
    case "$(uname -m)" in
        x86_64)  ha=amd64; sha=2b2cfc7148493da5e73981bffbf3353af381d5f93e789c82c79aff64962eb556 ;;
        aarch64) ha=arm64; sha=9d89a3ea57d141c2b22d70083f2c8459ba3890f2d9e818e7e933b75614936565 ;;
        *) echo "prebuild: unsupported build host $(uname -m)" >&2; exit 1 ;;
    esac
    mkdir -p "$cache_root"
    echo "prebuild: fetching go${GO_VER}.linux-${ha} toolchain..."
    curl -fsSL "https://go.dev/dl/go${GO_VER}.linux-${ha}.tar.gz" -o "$cache_root/go.tgz"
    echo "${sha}  $cache_root/go.tgz" | sha256sum -c - || { echo "prebuild: go toolchain SHA256 mismatch" >&2; exit 1; }
    tar -C "$cache_root" -xzf "$cache_root/go.tgz"
fi
export GOROOT="$goroot" PATH="$goroot/bin:$PATH" GOTOOLCHAIN=local
export GOPATH="$cache_root/gopath" GOCACHE="$cache_root/gocache"
go version

# Cross-compile the carpet → fully static binary, no libc/interp.
# The carpet imports framework deps (gin/grpc/go-zero/gorm + the pure-Go modernc SQLite
# driver). go build fetches them per the checked-in go.mod/go.sum (versions pinned, incl
# modernc.org/libc v1.73.4 which supports loongarch64). -mod=readonly makes the build
# reproducible: it fails loudly if go.sum is incomplete rather than silently editing it.
# GOPROXY is overridable for mirrors.
mkdir -p "$overlay_dir/usr/local/bin" "$overlay_dir/root"
( cd "$app_dir/go" && CGO_ENABLED=0 GOOS=linux GOARCH="$goarch" \
    GOPROXY="${GOPROXY:-https://proxy.golang.org,direct}" GOFLAGS=-mod=readonly \
    go build -trimpath -o "$overlay_dir/usr/local/bin/golang-lang" . )

# Stage the host golden for the byte-exact on-target compare.
install -Dm0644 "$app_dir/golden.txt" "$overlay_dir/root/golang-lang-golden.txt"

# Stage the on-target gate script (invoked as the ENTIRE shell_init_cmd). Keeping the gate
# in a staged script — not inline in the toml — avoids the harness false-positive where the
# echoed shell_init_cmd text containing `echo "TEST PASSED"` would self-match success_regex.
install -Dm0755 "$app_dir/go/run-go.sh" "$overlay_dir/usr/local/bin/run-go.sh"

echo "prebuild: built static golang-lang for $arch ($goarch) + staged golden ($(wc -l <"$app_dir/golden.txt") lines) + run-go.sh gate"
