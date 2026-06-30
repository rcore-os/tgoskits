#!/usr/bin/env bash
# setup-hdl-toolchain.sh — reproducible host EDA toolchain for the hdl-lang app.
#
# WHY: apps/starry/hdl-lang/prebuild.sh generates the DUT golden + the on-target
# simulation binaries on the HOST using verilator / iverilog / yosys / bsc, then
# injects them into the StarryOS rootfs. A reviewer therefore needs these exact
# host tools to run `cargo xtask starry app qemu -t hdl-lang --arch <arch>` to
# `TEST PASSED`. This script installs the PINNED versions the committed goldens
# were generated with, so the run is reproducible from a clean machine.
#
# Pinned versions (must match prebuild.sh's documented toolchain):
#   verilator 5.008   (github.com/verilator/verilator  tag v5.008)
#   iverilog  12       (github.com/steveicarus/iverilog tag v12_0)
#   yosys     0.58     (github.com/YosysHQ/yosys        tag 0.58)
#   bsc       2026.01  (github.com/B-Lang-org/bsc       release 2026.01)
#   musl-cross         (musl.cc prebuilt cross toolchains, per arch)
#   qemu-<arch>-static (distro package, for prebuild's foreign-arch apk step)
#
# Idempotent: each tool is skipped if the pinned version is already present.
# Usage:  sudo bash apps/starry/hdl-lang/setup-hdl-toolchain.sh
# Override install roots with PREFIX=/usr/local BSC_DIR=/usr/local/bsc etc.
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
BSC_DIR="${BSC_DIR:-/usr/local/bsc}"
MUSL_ROOT="${MUSL_ROOT:-/opt}"
JOBS="${JOBS:-$(nproc)}"
WORK="${WORK:-$(mktemp -d)}"
VERILATOR_TAG="v5.008"
IVERILOG_TAG="v12_0"
YOSYS_TAG="0.58"
BSC_VER="2026.01"
MUSL_ARCHES="${MUSL_ARCHES:-aarch64 riscv64 loongarch64 x86_64}"

log() { printf '\n=== %s ===\n' "$*"; }

# ---------------------------------------------------------------- apt build deps
if command -v apt-get >/dev/null 2>&1; then
  log "apt build deps"
  apt-get update -qq
  apt-get install -y --no-install-recommends \
    git build-essential autoconf flex bison libfl-dev help2man \
    gperf libreadline-dev tcl-dev libffi-dev zlib1g-dev \
    ca-certificates curl xz-utils python3 \
    qemu-user-static || true
fi

# ------------------------------------------------------------------- verilator
if verilator --version 2>/dev/null | grep -q "5.008"; then
  echo "verilator 5.008 present — skip"
else
  log "build verilator $VERILATOR_TAG"
  git clone --depth 1 --branch "$VERILATOR_TAG" https://github.com/verilator/verilator "$WORK/verilator"
  ( cd "$WORK/verilator" && autoconf && ./configure --prefix="$PREFIX" && make -j"$JOBS" && make install )
fi

# -------------------------------------------------------------------- iverilog
if command -v iverilog >/dev/null 2>&1 && iverilog -V 2>&1 | grep -qE "version 12"; then
  echo "iverilog 12 present — skip"
else
  log "build iverilog $IVERILOG_TAG"
  git clone --depth 1 --branch "$IVERILOG_TAG" https://github.com/steveicarus/iverilog "$WORK/iverilog"
  ( cd "$WORK/iverilog" && sh autoconf.sh && ./configure --prefix="$PREFIX" && make -j"$JOBS" && make install )
fi

# ----------------------------------------------------------------------- yosys
if yosys -V 2>/dev/null | grep -q "0.58"; then
  echo "yosys 0.58 present — skip"
else
  log "build yosys $YOSYS_TAG (with bundled abc)"
  git clone --depth 1 --branch "$YOSYS_TAG" --recurse-submodules https://github.com/YosysHQ/yosys "$WORK/yosys"
  ( cd "$WORK/yosys" && make -j"$JOBS" PREFIX="$PREFIX" && make install PREFIX="$PREFIX" )
fi

# ------------------------------------------------------------------------- bsc
if [ -x "$BSC_DIR/bin/bsc" ] && "$BSC_DIR/bin/bsc" -v 2>&1 | grep -q "$BSC_VER"; then
  echo "bsc $BSC_VER present — skip"
else
  log "install bsc $BSC_VER (prebuilt release tarball)"
  # B-Lang-org publishes per-distro prebuilt tarballs on the release page; pick the
  # one matching your distro (e.g. *-ubuntu-22.04.tar.gz). Source build (Haskell/GHC)
  # is documented in download/bsc/INSTALL.md if no matching release asset exists.
  rel="https://github.com/B-Lang-org/bsc/releases/download/${BSC_VER}"
  asset="${BSC_ASSET:-bsc-${BSC_VER}-ubuntu-22.04.tar.gz}"
  curl -fL -C - --retry 12 -o "$WORK/bsc.tgz" "$rel/$asset"
  mkdir -p "$BSC_DIR"
  tar -xzf "$WORK/bsc.tgz" -C "$WORK"
  cp -a "$WORK"/bsc-*/. "$BSC_DIR"/
fi

# ------------------------------------------------------------ musl-cross (per arch)
for a in $MUSL_ARCHES; do
  d="$MUSL_ROOT/${a}-linux-musl-cross"
  if [ -d "$d" ]; then echo "musl-cross $a present — skip"; continue; fi
  log "fetch musl-cross $a"
  curl -fL -C - --retry 12 -o "$WORK/${a}.tgz" "https://musl.cc/${a}-linux-musl-cross.tgz"
  tar -xzf "$WORK/${a}.tgz" -C "$MUSL_ROOT"
done

# ------------------------------------------------------------------------ verify
log "verify pinned versions"
verilator --version | head -1
iverilog -V 2>&1 | sed -n '1p'
yosys -V | head -1
"$BSC_DIR/bin/bsc" -v 2>&1 | head -1
for a in $MUSL_ARCHES; do printf '%s g++: ' "$a"; "$MUSL_ROOT/${a}-linux-musl-cross/bin/${a}-linux-musl-g++" --version | head -1; done
for a in aarch64 riscv64 loongarch64; do printf 'qemu-%s-static: ' "$a"; command -v "qemu-${a}-static" || echo MISSING; done
echo
echo "HDL toolchain ready. Now:  cargo xtask starry app qemu -t hdl-lang --arch riscv64"
