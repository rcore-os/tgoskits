#!/usr/bin/env bash
# Runs a single visual scenario end-to-end and reports PASS/FAIL.
#
# Usage:
#   run_scenario.sh --arch <arch> --scenario <name> [--update-golden]
#
# A scenario is a directory under `test-suit/starryos/visual/<name>/`
# that holds:
#   - `scenario.env`  : shell-sourced knobs (capture_after_secs, etc.)
#   - `runner.sh`     : the /test_runner.sh written into the rootfs
#   - (optional) `rootfs_extras/` : extra files copied into the guest
#
# Goldens live in `test-suit/starryos/golden/<arch>/<name>.ppm`. With
# `--update-golden`, the captured frame overwrites the golden instead
# of diffing. Intended workflow: make a change, run without the flag,
# if the diff is intended rerun with `--update-golden` and commit the
# new golden + code together.
#
# QEMU port assignment is VNC-slot 40 + first-free. Two scenarios can
# run in parallel without clashing because each picks its own slot.
set -euo pipefail

log()  { printf "[visual] %s\n" "$*" >&2; }
die()  { log "FATAL: $*"; exit 1; }

ARCH=""
SCENARIO=""
UPDATE_GOLDEN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)          ARCH="$2"; shift 2;;
        --scenario)      SCENARIO="$2"; shift 2;;
        --update-golden) UPDATE_GOLDEN=1; shift;;
        *) die "unknown arg: $1";;
    esac
done

[[ -n "$ARCH" && -n "$SCENARIO" ]] \
    || die "both --arch and --scenario are required"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${CLAUDE_PROJECT_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
SCENARIO_DIR="$REPO_ROOT/test-suit/starryos/visual/$SCENARIO"
GOLDEN_PATH="$REPO_ROOT/test-suit/starryos/golden/$ARCH/$SCENARIO.ppm"
[[ -d "$SCENARIO_DIR" ]] || die "no scenario dir at $SCENARIO_DIR"

source "$SCENARIO_DIR/scenario.env"
CAPTURE_AFTER_SECS="${CAPTURE_AFTER_SECS:-25}"
RUN_FOR_SECS="${RUN_FOR_SECS:-$((CAPTURE_AFTER_SECS + 15))}"

# Per-arch QEMU invocation bits. Keep in lockstep with starry-harness'
# pipeline.sh — if something there changes (e.g., ECAM layout), mirror
# it here or unify. Today we hand-roll because the pipeline runs kernel
# tests (no graphics) and we need the graphics-specific device list.
case "$ARCH" in
    riscv64)
        QEMU=qemu-system-riscv64
        MACHINE_ARGS=(-machine virt -bios default -m 1G)
        KERNEL="$REPO_ROOT/target/riscv64gc-unknown-none-elf/release/starryos.bin"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img"
        GRAPHICS_ARGS=(-device virtio-gpu-pci)
        ;;
    aarch64)
        QEMU=qemu-system-aarch64
        MACHINE_ARGS=(-M virt -cpu cortex-a72 -m 2G -smp 2)
        KERNEL="$REPO_ROOT/target/aarch64-unknown-none-softfloat/release/starryos.bin"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
        GRAPHICS_ARGS=(-device virtio-gpu-pci)
        ;;
    x86_64)
        QEMU=qemu-system-x86_64
        MACHINE_ARGS=(-machine q35 -m 2G -smp 2)
        KERNEL="$REPO_ROOT/target/x86_64-unknown-none/release/starryos"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
        # x86_64 exposes cirrus VGA by default alongside virtio-gpu; mask
        # it so QEMU's VNC captures our DRM scanout, not the VGA text.
        GRAPHICS_ARGS=(-device virtio-gpu-pci -vga none)
        ;;
    *) die "unsupported arch: $ARCH";;
esac

[[ -f "$KERNEL" ]] || die "kernel not built: $KERNEL"
[[ -f "$ROOTFS" ]] || die "rootfs missing: $ROOTFS"

# Pick an unused VNC slot (port = 5900 + slot).
for slot in 40 41 42 43 44 45; do
    if ! lsof -nP -iTCP:$((5900 + slot)) -sTCP:LISTEN >/dev/null 2>&1; then
        break
    fi
done
VNC_PORT=$((5900 + slot))

# Stage the rootfs: drop in the scenario runner + any extras.
#
# We support two injection backends so this script works in both the
# CI container (Linux, no Docker daemon, has e2tools) and the dev laptop
# (macOS, Docker Desktop available). Picking at runtime keeps a single
# code path for both.
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"; [[ -n "${QPID:-}" ]] && kill "$QPID" 2>/dev/null || true' EXIT
# Copy the rootfs we're about to mutate. APFS (macOS) and btrfs/xfs
# (most Linux CI containers) support `cp --reflink` / `cp -c` for
# near-instant copy-on-write clones — essential because the rootfs is
# often ~1 GB and a plain byte-for-byte copy bottlenecks a fast run.
# Fall back to plain `cp` on filesystems without reflink support; the
# scenario still works, just slower.
if cp -c "$ROOTFS" "$SCRATCH/disk.img" 2>/dev/null; then
    :  # macOS APFS reflink
elif cp --reflink=auto "$ROOTFS" "$SCRATCH/disk.img" 2>/dev/null; then
    :  # Linux reflink where supported
else
    cp "$ROOTFS" "$SCRATCH/disk.img"
fi

inject_via_e2tools() {
    # e2cp / e2mkdir from e2tools (or e2fsprogs on modern distros).
    # Works without any privilege — pure userspace ext4 driver.
    e2cp -G0 -O0 -P0755 "$SCENARIO_DIR/runner.sh" "$SCRATCH/disk.img:/test_runner.sh"
    if [[ -d "$SCENARIO_DIR/rootfs_extras" ]]; then
        (cd "$SCENARIO_DIR/rootfs_extras" && find . -type d | while read -r d; do
            [[ "$d" == "." ]] && continue
            e2mkdir -G0 -O0 "$SCRATCH/disk.img:$d" 2>/dev/null || true
        done)
        (cd "$SCENARIO_DIR/rootfs_extras" && find . \( -type f -o -type l \) | while read -r f; do
            src="$f"
            tmp=""
            if [[ -L "$f" ]]; then
                target="$(readlink "$f")"
                if [[ "$target" = /* ]]; then
                    src=".$target"
                else
                    src="$(dirname "$f")/$target"
                fi
                if [[ ! -f "$src" ]]; then
                    echo "rootfs_extras symlink target is not a file: $f -> $target" >&2
                    exit 1
                fi
                tmp="$(mktemp "$SCRATCH/rootfs-extra.XXXXXX")"
                cp "$src" "$tmp"
                src="$tmp"
            fi
            e2cp -G0 -O0 "$src" "$SCRATCH/disk.img:${f#.}"
            [[ -z "$tmp" ]] || rm -f "$tmp"
        done)
    fi
}

inject_via_docker() {
    docker run --rm --privileged \
        -v "$SCRATCH/disk.img:/tmp/disk.img" \
        -v "$SCENARIO_DIR:/tmp/scenario:ro" \
        alpine:edge sh -c '
set -e
mount -o loop /tmp/disk.img /mnt
cp /tmp/scenario/runner.sh /mnt/test_runner.sh
chmod +x /mnt/test_runner.sh
if [ -d /tmp/scenario/rootfs_extras ]; then
    cp -r /tmp/scenario/rootfs_extras/* /mnt/ 2>/dev/null || true
fi
sync && umount /mnt
' >/dev/null
}

if command -v e2cp >/dev/null 2>&1; then
    log "injecting via e2tools"
    inject_via_e2tools
elif command -v docker >/dev/null 2>&1; then
    log "injecting via Docker (slower; install e2tools to speed this up)"
    inject_via_docker
else
    die "neither e2tools nor docker available — can't inject scenario into rootfs"
fi

log "booting $QEMU for scenario=$SCENARIO arch=$ARCH on vnc=:$slot"
"$QEMU" "${MACHINE_ARGS[@]}" \
    -kernel "$KERNEL" \
    -device virtio-blk-pci,drive=disk0 \
    -drive id=disk0,if=none,format=raw,file="$SCRATCH/disk.img" \
    -device virtio-net-pci,netdev=net0 -netdev user,id=net0 \
    "${GRAPHICS_ARGS[@]}" \
    -device virtio-tablet-pci -device virtio-keyboard-pci \
    -serial file:"$SCRATCH/serial.log" \
    -vnc :"$slot" \
    </dev/null >"$SCRATCH/qemu.stdout" 2>&1 &
QPID=$!

# Wait for the scenario to reach the "ready-to-capture" point.
sleep "$CAPTURE_AFTER_SECS"
if ! kill -0 "$QPID" 2>/dev/null; then
    log "QEMU died during warmup; last 30 lines of serial:"
    tail -30 "$SCRATCH/serial.log" >&2
    exit 1
fi

# Assert that the in-guest runner actually launched. The init.sh hook
# emits `[init] /test_runner.sh started pid=<n>` to /dev/console
# immediately after spawning the scenario. Without this check a
# scenario that silently failed to start would still pass on a
# golden-match if the captured frame happened to resemble the
# pre-launch screen.
if ! grep -q '/test_runner.sh started pid=' "$SCRATCH/serial.log"; then
    log "in-guest /test_runner.sh never launched; serial tail:"
    tail -60 "$SCRATCH/serial.log" >&2
    exit 1
fi

# Capture.
OUT_PPM="$SCRATCH/actual.ppm"
if ! python3 "$REPO_ROOT/scripts/visual-test/rfb_capture.py" \
        localhost "$VNC_PORT" "$OUT_PPM"; then
    log "RFB capture failed"
    tail -30 "$SCRATCH/serial.log" >&2
    exit 1
fi

# Quit QEMU cleanly.
kill "$QPID" 2>/dev/null || true
wait "$QPID" 2>/dev/null || true
unset QPID

# Update-golden path: overwrite and exit. Committing is the user's job.
if (( UPDATE_GOLDEN )); then
    mkdir -p "$(dirname "$GOLDEN_PATH")"
    cp "$OUT_PPM" "$GOLDEN_PATH"
    log "golden updated: $GOLDEN_PATH"
    exit 0
fi

if [[ ! -f "$GOLDEN_PATH" ]]; then
    die "no golden at $GOLDEN_PATH — run with --update-golden to create"
fi

# Perceptual diff.
exec python3 "$REPO_ROOT/scripts/visual-test/perceptual_diff.py" \
    "$GOLDEN_PATH" "$OUT_PPM" \
    --delta "${DIFF_DELTA:-8}" \
    --max-changed-pct "${DIFF_MAX_CHANGED_PCT:-1.5}"
