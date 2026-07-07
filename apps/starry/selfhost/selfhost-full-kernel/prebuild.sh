#!/usr/bin/env bash
set -euo pipefail
#
# prebuild.sh — Generates ALL overlay files for the selfhost self-compilation app.
#
# This is the single source of truth for overlay content.  scripts/self-compile.sh
# calls this script, then injects the generated overlay directory into the rootfs.
#
# Called by the Starry app runner (cargo xtask starry app run) with:
#   STARRY_APP_DIR       — path to this app directory (apps/starry/selfhost)
#   STARRY_OVERLAY_DIR   — staging directory for rootfs injection
#
# When called from scripts/self-compile.sh, additional env vars:
#   SELF_COMPILE_COMMIT      — expected git commit in /opt/starryos
#   SELF_COMPILE_REF         — expected git ref in /opt/starryos
#   SEED_KERNEL_DIR          — directory containing the seed kernel (for linker.x)
#   REPO_ROOT                — repository root
#   CARGO_BUILD_JOBS         — cargo build parallelism
#   SELF_COMPILE_ARCH        — target architecture name (riscv64, x86_64, aarch64)
#   SELF_COMPILE_TARGET      — Rust target triple
#   SELF_COMPILE_SMP         — number of CPUs

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

# STARRY_WORKSPACE is set by the app runner (preferred); REPO_ROOT by self-compile.sh.
repo_root="${STARRY_WORKSPACE:-${REPO_ROOT:-$(cd "$app_dir/../../../.." && pwd)}}"
# STARRY_ARCH is passed by the app runner; SELF_COMPILE_ARCH by self-compile.sh
arch="${STARRY_ARCH:-${SELF_COMPILE_ARCH:-riscv64}}"
# Derive the bare-metal target from the architecture for the guest build.
# SELF_COMPILE_TARGET may also be set explicitly (by self-compile.sh wrapper).
_target_for_arch() {
    case "${1:-riscv64}" in
        riscv64)   echo "riscv64gc-unknown-none-elf" ;;
        x86_64)    echo "x86_64-unknown-none" ;;
        aarch64)   echo "aarch64-unknown-none-softfloat" ;;
        loongarch64) echo "loongarch64-unknown-none-softfloat" ;;
        *)         echo "[prebuild] ERROR: unsupported arch: ${1:-}" >&2; exit 1 ;;
    esac
}
target="${SELF_COMPILE_TARGET:-$(_target_for_arch "$arch")}"
seed_kernel_dir="${SEED_KERNEL_DIR:-}"
cargo_build_jobs="${CARGO_BUILD_JOBS:-4}"
smp="${SELF_COMPILE_SMP:-4}"

echo "[prebuild] Generating overlay files for $arch in $overlay_dir"

# ── .expected-commit — source identity verification ───────────────────────────
gen_commit_file() {
    local expect_commit="${1:-}"
    local expect_ref="${2:-}"

    mkdir -p "$overlay_dir/opt/starryos"
    if [[ -n "$expect_commit" ]]; then
        echo "TGOSKITS_COMMIT=$expect_commit" > "$overlay_dir/opt/starryos/.expected-commit"
        echo "[prebuild] .expected-commit: commit=$expect_commit"
    elif [[ -n "$expect_ref" ]]; then
        echo "TGOSKITS_REF=$expect_ref" > "$overlay_dir/opt/starryos/.expected-commit"
        echo "[prebuild] .expected-commit: ref=$expect_ref"
    fi
}

# ── self-compile-inner.sh — guest compilation script ──────────────────────────
gen_inner_script() {
    mkdir -p "$overlay_dir/usr/bin"
    local out="$overlay_dir/usr/bin/self-compile-inner.sh"

    # Dynamic platform features for x86_64 (ax-plat-x86-pc was removed).
    case "$arch" in
        x86_64) dyn_features=",plat-dyn,axplat-dyn/efi" ;;
        *)      dyn_features="" ;;
    esac

    cat > "$out" << INNER_EOF
#!/bin/sh
set -euo pipefail
# NOTE: shebang is /bin/sh (busybox) not /usr/bin/bash.  The rebased kernel
# cannot load dynamically-linked bash as a shebang interpreter (same root
# cause as the /bin/sh fix in bootstrap prebuild.sh:1af27f75b).  Busybox ash
# handles set -euo pipefail, POSIX $(...), and inline env vars correctly.

export CARGO_TARGET_DIR=/tmp/build
mkdir -p /tmp/build 2>/dev/null || true
export CARGO_BUILD_JOBS=${cargo_build_jobs}
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo
# Host-side build scripts (proc-macros etc.) target the gnu host triple and use
# gcc.  The musl build target's C deps use x86_64-linux-musl-cc, provided by the
# rootfs (musl-tools symlinks from prepare-selfhost-rootfs.sh) — do not shadow it.
export CC=gcc

cd /opt/starryos

	# Verify source commit/ref matches the expected value from the host.
	# We resolve the actual commit from the worktree.  When the source was
	# prepared via git-archive (no .git), we fall back to .source-commit
	# which is embedded by prepare-selfhost-rootfs.sh.
	RESOLVE_SHA=\$(cd /opt/starryos && git rev-parse HEAD 2>/dev/null) || true
	if [ -z "\$RESOLVE_SHA" ] && [ -f /opt/starryos/.source-commit ]; then
	    RESOLVE_SHA=\$(head -n1 /opt/starryos/.source-commit)
	    echo "[self-compile] Source commit resolved from .source-commit (git-archive worktree)"
	fi
	: "\${RESOLVE_SHA:=unknown}"
	if [ -f /opt/starryos/.expected-commit ]; then
	    EXPECTED_LINE=\$(head -n1 /opt/starryos/.expected-commit)
	    echo "[self-compile] Expected source identity: \$EXPECTED_LINE (actual: \$RESOLVE_SHA)"
	    if echo "\$EXPECTED_LINE" | grep -q '^TGOSKITS_COMMIT='; then
	        EXPECTED_SHA="\${EXPECTED_LINE#TGOSKITS_COMMIT=}"
	        if [ "\$RESOLVE_SHA" = "unknown" ]; then
	            echo "SELF_COMPILE_FAILED: cannot resolve source commit (no .git and no .source-commit)"
	            echo "Re-run scripts/prepare-selfhost-rootfs.sh with a current version that embeds .source-commit."
	            exit 1
	        fi
	        if [ "\$RESOLVE_SHA" != "\$EXPECTED_SHA" ]; then
	            echo "SELF_COMPILE_FAILED: source commit mismatch — expected \$EXPECTED_SHA, got \$RESOLVE_SHA"
	            echo "Re-run scripts/prepare-selfhost-rootfs.sh with TGOSKITS_COMMIT=\$EXPECTED_SHA to rebuild the rootfs with the correct source."
	            exit 1
	        fi
	        echo "[self-compile] Source commit verified: \$RESOLVE_SHA"
	    elif echo "\$EXPECTED_LINE" | grep -q '^TGOSKITS_REF='; then
	        EXPECTED_REF="\${EXPECTED_LINE#TGOSKITS_REF=}"
	        echo "[self-compile] Source ref requested: \$EXPECTED_REF (commit=\$RESOLVE_SHA)"
	    fi
	fi
echo "[self-compile] ARG ARCH=${arch} TARGET=${target} SMP=${smp} CARGO_BUILD_JOBS=${cargo_build_jobs}"

echo "[self-compile] Rustc version: \$(rustc --version 2>/dev/null || echo 'unknown')"
echo "[self-compile] Cargo version: \$(cargo --version 2>/dev/null || echo 'unknown')"
echo "[self-compile] Building (target=${target}, arch=${arch})..."
echo "BUILD_START"

export CARGO_TERM_PROGRESS_WHEN=always
export CARGO_TERM_PROGRESS_WIDTH=120
set +e
/usr/bin/bash -c 'while true; do sleep 30; echo "[self-compile] ... still compiling ..."; done' &
HEARTBEAT_PID=\$!

BINARY=""
BUILD_RC=1

if [ "${arch}" = "x86_64" ]; then
    # x86_64 is a dynamic-platform (EFI/PIE) build: the bootable kernel can ONLY be produced
    # by the canonical xtask flow (musl-PIE std target + -Zbuild-std + the rust-lld
    # linker wrapper that groups archives via --start-group/--end-group and forces
    # -pie).  A hand-rolled bare-metal cargo build cannot link someboot's _head/kernel_entry.
    export CARGO_NET_OFFLINE=true
    export AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL=0

    # ostool forces --target-dir=<workspace>/target and ignores CARGO_TARGET_DIR, so
    # symlink the multi-GB kernel target + std scaffolding onto the /tmp tmpfs to
    # avoid overflowing the near-full rootfs image.
    mkdir -p /tmp/build/target /tmp/build/ws-tmp
    [ -L target ] || { rm -rf target; ln -s /tmp/build/target target; }
    [ -L tmp ]    || { rm -rf tmp;    ln -s /tmp/build/ws-tmp tmp; }

    # Offline preconditions (reqwest ignores CARGO_NET_OFFLINE): AIC8800 firmware
    # blobs (hashed before every Starry build) and the kallsyms tools must be present,
    # else xtask would fetch/install them online and fail.
    if [ -z "\$(ls -A components/aic8800/firmware/*.bin 2>/dev/null)" ]; then
        echo "SELF_COMPILE_FAILED: AIC8800 firmware blobs missing (xtask would fetch online)"
        exit 1
    elif ! command -v gen_ksym >/dev/null 2>&1 || ! command -v rust-nm >/dev/null 2>&1 || ! command -v rust-objcopy >/dev/null 2>&1; then
        echo "SELF_COMPILE_FAILED: kallsyms tools (gen_ksym/rust-nm/rust-objcopy) missing offline"
        exit 1
    else
        # Build the host tool for the gnu host triple (prebuilt std), NOT the kernel's
        # bare x86_64-unknown-none default; else the std xtask binary is mis-built as
        # no_std and segfaults at startup.  Run the produced binary directly.
        unset CARGO_BUILD_TARGET
        # /root/.cargo/config.toml [build] rustflags carries --no-rosegment (for
        # the bare-metal kernel); that flag poisons the gnu host-tool link because
        # cc rejects it.  Scope RUSTFLAGS="" to this one cargo invocation so the
        # kernel build that follows (via xtask) keeps its own xtask-managed flags.
        RUSTFLAGS="" cargo build -p tg-xtask --target x86_64-unknown-linux-gnu
        XTASK=/tmp/build/x86_64-unknown-linux-gnu/debug/tg-xtask
        if [ -x "\$XTASK" ]; then
            CFLAGS=-fno-stack-protector CXXFLAGS=-fno-stack-protector \\
                "\$XTASK" starry build -c apps/starry/selfhost/build-${target}.toml --arch ${arch}
            BUILD_RC=\$?
            __elf=/tmp/build/target/x86_64-unknown-linux-musl/release/starryos
            if [ -f "\$__elf" ] && [ -s "\$__elf" ]; then
                cp "\$__elf" /opt/starryos-selfbuilt
                sync
                echo "BINARY=\$__elf"
                echo "BINARY_SIZE=\$(stat -c%s "\$__elf")"
                echo "SELF_COMPILE_SUCCESS"
                kill \$HEARTBEAT_PID 2>/dev/null || true
                exit 0
            fi
            kill \$HEARTBEAT_PID 2>/dev/null || true
            echo "SELF_COMPILE_FAILED: rc=\$BUILD_RC elf_not_found=\$__elf"
            exit 1
        else
            kill \$HEARTBEAT_PID 2>/dev/null || true
            echo "SELF_COMPILE_FAILED: host tg-xtask build failed"
            exit 1
        fi
    fi
else
    # riscv64/others: bare-metal build matches the seed (non-dynamic-platform arch).
    /usr/bin/filter-workspace.sh "${arch}" Cargo.toml
    export RUSTFLAGS="-Ccodegen-units=16 -Copt-level=0 -Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc"
    export AX_CONFIG_PATH=/opt/starryos/.axconfig.toml
    cargo build --ignore-rust-version -p starryos \\
                --target ${target} \\
                --features qemu,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket${dyn_features} \\
                --offline
    BUILD_RC=\$?
    [ -f Cargo.toml.bak ] && mv Cargo.toml.bak Cargo.toml
    BINARY=/tmp/build/${target}/debug/starryos
fi

kill \$HEARTBEAT_PID 2>/dev/null || true
wait \$HEARTBEAT_PID 2>/dev/null || true
echo "BUILD_END"

if [ "\$BUILD_RC" -eq 0 ] && [ -n "\$BINARY" ] && [ -f "\$BINARY" ] && [ -s "\$BINARY" ]; then
    cp "\$BINARY" /opt/starryos-selfbuilt
    sync
    echo "BINARY=\$BINARY"
    echo "BINARY_SIZE=\$(stat -c%s "\$BINARY")"
    echo "SELF_COMPILE_SUCCESS"
else
    echo "SELF_COMPILE_FAILED: rc=\$BUILD_RC binary=\$BINARY"
fi
INNER_EOF
    chmod +x "$out"
    echo "[prebuild] self-compile-inner.sh generated"
}

# ── filter-workspace.sh — arch-specific workspace filtering ──────────────────
gen_filter_workspace() {
    mkdir -p "$overlay_dir/usr/bin"
    local src="$repo_root/scripts/filter-workspace.sh"
    if [ -f "$src" ]; then
        cp "$src" "$overlay_dir/usr/bin/filter-workspace.sh"
        chmod +x "$overlay_dir/usr/bin/filter-workspace.sh"
        echo "[prebuild] filter-workspace.sh copied"
    else
        echo "[prebuild] WARNING: filter-workspace.sh not found at $src" >&2
    fi
}

# ── linker.x — link script from seed kernel build ─────────────────────────────
gen_linker_script() {
    mkdir -p "$overlay_dir/opt/starryos"
    if [ -n "$seed_kernel_dir" ] && [ -f "$seed_kernel_dir/linker.x" ]; then
        cp "$seed_kernel_dir/linker.x" "$overlay_dir/opt/starryos/linker.x"
        echo "[prebuild] linker.x copied from seed kernel dir"
    elif [ -f "$repo_root/target/$target/debug/linker.x" ]; then
        cp "$repo_root/target/$target/debug/linker.x" "$overlay_dir/opt/starryos/linker.x"
        echo "[prebuild] linker.x copied from target/debug"
    elif [ -f "$repo_root/target/$target/release/linker.x" ]; then
        cp "$repo_root/target/$target/release/linker.x" "$overlay_dir/opt/starryos/linker.x"
        echo "[prebuild] linker.x copied from target/release"
    fi
}

# ── .axconfig.toml — build configuration ─────────────────────────────────────
# The axconfig is generated by `cargo xtask starry build` during the seed kernel
# step. It lives under tmp/axbuild/axconfig/<package>/<target>/.axconfig.toml.
# For dynamic platforms (x86_64) the package name may differ from starryos.
gen_axconfig() {
    mkdir -p "$overlay_dir/opt/starryos"

    # Candidate packages — ordered by likelihood for Starry self-compile builds.
    local pkgs="starryos arceos-rust"
    local found=""

    for pkg in $pkgs; do
        local candidate="$repo_root/tmp/axbuild/axconfig/$pkg/${target}/.axconfig.toml"
        if [ -f "$candidate" ]; then
            found="$candidate"
            break
        fi
    done

    if [ -n "$found" ]; then
        cp "$found" "$overlay_dir/opt/starryos/.axconfig.toml"
        echo "[prebuild] .axconfig.toml copied from $found"
    else
        # For dynamic platforms (x86_64) the axbuild system may not generate a
        # static .axconfig.toml (the platform config is resolved at runtime via
        # FDT/ACPI).  Generate a minimal valid config from known-good defaults
        # so the guest cargo build has a working AX_CONFIG_PATH.
        echo "[prebuild] .axconfig.toml not found in build artifacts — generating minimal config for ${arch}"
        _generate_minimal_axconfig "$arch"
    fi
}

# Generate a minimal .axconfig.toml for architectures where the build system
# does not produce one (dynamic platforms).  The values are QEMU virt defaults
# and match what the inner bare-metal build expects.
_generate_minimal_axconfig() {
    local arch="$1"
    local out="$overlay_dir/opt/starryos/.axconfig.toml"

    case "$arch" in
        x86_64)
            cat > "$out" << 'AXEOF'
# Architecture identifier.
arch = "x86_64" # str
# Platform identifier.
platform = "x86_64-qemu" # str
# Stack size of each task.
task-stack-size = 0x40000 # uint
# Number of timer ticks per second (Hz).
ticks-per-sec = 100 # uint

#
# Device specifications
#
[devices]
# IPI interrupt num
ipi-irq = 0xf3 # uint
# MMIO ranges with format (`base_paddr`, `size`).
mmio-ranges = [
    [0xb000_0000, 0x1000_0000],
    [0xfe00_0000, 0xc0_0000],
    [0xfec0_0000, 0x1000],
    [0xfed0_0000, 0x1000],
    [0xfee0_0000, 0x1000]
] # [(uint, uint)]
# End PCI bus number.
pci-bus-end = 0xff # uint
# Base physical address of the PCIe ECAM space.
pci-ecam-base = 0xb000_0000 # uint
# PCI device memory ranges (not used on x86).
pci-ranges = [] # [(uint, uint)]
# Timer interrupt frequency in Hz.
timer-frequency = 4_000_000_000 # uint
# Timer interrupt num.
timer-irq = 0xf0 # uint
# VirtIO MMIO ranges.
virtio-mmio-ranges = [] # [(uint, uint)]

#
# Platform configs
#
[plat]
# Stack size on bootstrapping.
boot-stack-size = 0x40000 # uint
# Kernel address space base.
kernel-aspace-base = "0xffff_8000_0000_0000" # uint
# Kernel address space size.
kernel-aspace-size = "0x0000_7fff_ffff_f000" # uint
# Base physical address of the kernel image.
kernel-base-paddr = 0x20_0000 # uint
# Base virtual address of the kernel image.
kernel-base-vaddr = "0xffff_8000_0020_0000" # uint
# Maximum number of CPUs.
max-cpu-num = 4 # uint
# Offset of bus address and phys address.
phys-bus-offset = 0 # uint
# Base address of the whole physical memory.
phys-memory-base = 0 # uint
# Size of the whole physical memory.
phys-memory-size = 0x3_0000_0000 # uint
# Linear mapping offset.
phys-virt-offset = "0xffff_8000_0000_0000" # uint
AXEOF
            ;;
        *)
            echo "[prebuild] ERROR: no axconfig template for arch '${arch}' and no pre-generated axconfig found." >&2
            echo "[prebuild] The inner compile script requires AX_CONFIG_PATH to build." >&2
            exit 1
            ;;
    esac
    echo "[prebuild] minimal .axconfig.toml generated (${arch})"
}

# ── sh/ scripts — case-level shell scripts copied into guest ──────────────────
# When prebuild.sh is used, the app runner no longer auto-copies sh/* into the
# overlay.  We explicitly copy them here so existing qemu-*.toml files with
# shell_init_cmd = "/usr/bin/..." continue to work.
gen_sh_scripts() {
    local sh_dir="$app_dir/sh"
    if [ -d "$sh_dir" ]; then
        mkdir -p "$overlay_dir/usr/bin"
        for script in "$sh_dir"/*; do
            if [ -f "$script" ]; then
                cp "$script" "$overlay_dir/usr/bin/"
                chmod +x "$overlay_dir/usr/bin/$(basename "$script")"
                echo "[prebuild] sh/$(basename "$script") copied"
            fi
        done
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────────
gen_commit_file "${SELF_COMPILE_COMMIT:-}" "${SELF_COMPILE_REF:-}"
gen_inner_script
gen_filter_workspace
gen_linker_script
gen_axconfig
gen_sh_scripts

echo "[prebuild] All overlay files generated in $overlay_dir"
