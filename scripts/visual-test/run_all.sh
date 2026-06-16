#!/usr/bin/env bash
# Run every visual-test scenario for a given arch.
#
# Usage:
#   run_all.sh --arch <arch> [--update-golden]
#
# Discovers scenarios by enumerating `apps/starry/visual/scenarios/*/` and
# invokes `run_scenario.sh` on each. Mirrors the existing
# `cargo xtask starry test qemu --target <arch>` pattern: a single
# command runs every case, individual PASS/FAIL lines land in the
# final output, and the exit code is 0 iff every case passed.
#
# In CI (reusable-command.yml), this is what the matrix
# invokes per arch. On a dev machine, it's the quick "run everything
# visually" checker — same as `cargo xtask test` is for host tests.
set -euo pipefail

ARCH=""
UPDATE_GOLDEN_ARG=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2;;
        --update-golden) UPDATE_GOLDEN_ARG=(--update-golden); shift;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done
[[ -n "$ARCH" ]] || { echo "--arch required" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${CLAUDE_PROJECT_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
SCENARIO_ROOT="$REPO_ROOT/apps/starry/visual/scenarios"

# Ensure the kernel + rootfs the scenarios depend on actually exist. If
# a run is fresh from CI we may need to build them first. For now the
# expectation is that the host `test_qemu_matrix` step has already
# prepared these artifacts upstream of this job, same as how
# `cargo xtask` tests assume `cargo xtask starry build` has run.
case "$ARCH" in
    riscv64)
        KERNEL="$REPO_ROOT/target/riscv64gc-unknown-none-elf/release/starryos.bin"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img"
        ;;
    aarch64)
        KERNEL="$REPO_ROOT/target/aarch64-unknown-none-softfloat/release/starryos.bin"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
        ;;
    x86_64)
        KERNEL="$REPO_ROOT/target/x86_64-unknown-none/release/starryos"
        ROOTFS="$REPO_ROOT/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
        ;;
    loongarch64)
        # Visual tests on loongarch64 are gated on the toolchain being
        # installable. Emit a skip-line and exit 0 so the CI matrix can
        # still turn green when the arch isn't bootable.
        echo "SKIP loongarch64 (no Alpine weston rootfs available)"
        exit 0
        ;;
    *) echo "unsupported arch: $ARCH" >&2; exit 2;;
esac

if [[ ! -f "$KERNEL" ]]; then
    echo "kernel not built for $ARCH — run 'cargo xtask starry build --arch $ARCH' first" >&2
    exit 1
fi
if [[ ! -f "$ROOTFS" ]]; then
    echo "rootfs missing for $ARCH — run 'cargo xtask starry rootfs --arch $ARCH' first" >&2
    exit 1
fi

if [[ ! -d "$SCENARIO_ROOT" ]]; then
    echo "no scenarios registered under $SCENARIO_ROOT" >&2
    exit 1
fi

shopt -s nullglob
SCENARIOS=()
for d in "$SCENARIO_ROOT"/*/; do
    SCENARIOS+=("$(basename "${d%/}")")
done

if (( ${#SCENARIOS[@]} == 0 )); then
    echo "no scenarios found" >&2
    exit 1
fi

declare -i pass_count=0
declare -i fail_count=0
declare -i skip_count=0

for sc in "${SCENARIOS[@]}"; do
    # Per-scenario opt-in-by-arch: if the scenario has an `arches` file,
    # only run when the current arch is listed. Keeps Xwayland-class
    # scenarios (which may not yet be extracted for all arches) from
    # spuriously failing on arches that don't have the bundle yet.
    archfile="$SCENARIO_ROOT/$sc/arches"
    if [[ -f "$archfile" ]] && ! grep -qw "$ARCH" "$archfile"; then
        echo "SKIP $ARCH/$sc (not in $archfile)"
        skip_count+=1
        continue
    fi
    scenario_dir="$SCENARIO_ROOT/$sc"
    if [[ -f "$scenario_dir/rootfs_extras.packages" ]]; then
        if [[ ! -d "$scenario_dir/rootfs_extras" ]] \
            || ! find "$scenario_dir/rootfs_extras" -mindepth 1 -print -quit | grep -q .; then
            echo "FAIL $ARCH/$sc (declares rootfs_extras.packages but rootfs_extras is missing or empty)" >&2
            echo "run: python3 scripts/visual-test/prepare_rootfs_extras.py --arch $ARCH --scenario $sc" >&2
            fail_count+=1
            continue
        fi
    fi

    echo "=== $ARCH/$sc ==="
    if bash "$REPO_ROOT/scripts/visual-test/run_scenario.sh" \
            --arch "$ARCH" --scenario "$sc" "${UPDATE_GOLDEN_ARG[@]}"; then
        echo "PASS $ARCH/$sc"
        pass_count+=1
    else
        echo "FAIL $ARCH/$sc"
        fail_count+=1
    fi
done

echo "==========================================="
echo "  $ARCH: PASS=$pass_count FAIL=$fail_count SKIP=$skip_count"
echo "==========================================="
# A visual matrix entry must actually exercise the golden-diff / RFB /
# guest-runner pipeline at least once. Returning 0 when every scenario
# was gated out by `arches` would let the matrix go green without ever
# proving the new CI works end-to-end.
if (( pass_count + fail_count == 0 )); then
    echo "no scenarios actually ran for $ARCH (skip=$skip_count); visual CI must exercise the pipeline" >&2
    exit 1
fi
exit $(( fail_count > 0 ? 1 : 0 ))
