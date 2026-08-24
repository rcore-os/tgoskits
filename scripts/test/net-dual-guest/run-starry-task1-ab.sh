#!/usr/bin/env bash
set -euo pipefail

# Run the same persistent StarryOS + RTOS ncnn/YOLO/T2N1 workload under
# AxVisor RR and bounded FP-RR, then retain and verify both evidence archives.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
output_root="${1:?usage: run-starry-task1-ab.sh OUTPUT_ROOT}"
scenario_runner="$repo_root/scripts/test/net-dual-guest/run-starry-task23-scenario.sh"
verifier="$repo_root/scripts/test/net-dual-guest/verify_starry_task1_ab.py"
rr_host="$repo_root/scripts/test/net-dual-guest/axvisor-qemu-starry-task1-rr.toml"
fp_rr_host="$repo_root/scripts/test/net-dual-guest/axvisor-qemu-starry-task1-fp-rr.toml"
starry_vm="$repo_root/scripts/test/net-dual-guest/vm-aarch64-starry-task1-shared.toml"
rtos_name="${STARRY_TASK1_RTOS_NAME:-zephyr}"
case "$rtos_name" in
    zephyr|rtthread) ;;
    *) printf 'error: STARRY_TASK1_RTOS_NAME must be zephyr or rtthread\n' >&2; exit 2 ;;
esac
qemu_config="${STARRY_TASK1_QEMU_CONFIG:-$repo_root/scripts/test/net-dual-guest/qemu-aarch64-starry-${rtos_name}-task1-capture.toml}"
rtos_vm="${STARRY_TASK1_RTOS_VM_CONFIG:-$repo_root/scripts/test/net-dual-guest/vm-aarch64-${rtos_name}-task1-shared.toml}"
runtime_tag="${STARRY_TASK1_RUNTIME_TAG:-}"
if [[ -z "$runtime_tag" ]]; then
    case "$rtos_name" in
        zephyr)   runtime_tag="starry-zephyr-msix1-capture" ;;
        rtthread) runtime_tag="starry-rtthread-task1-capture" ;;
    esac
fi
rtos_source_dir="${STARRY_TASK1_RTOS_SOURCE_DIR:-}"
if [[ -z "$rtos_source_dir" && "$rtos_name" == rtthread ]]; then
    rtos_source_dir="$repo_root/tmp/net-dual-guest/rtthread-task2-starry-task1-normal"
fi

if [[ -e "$output_root" ]]; then
    printf 'error: output already exists: %s\n' "$output_root" >&2
    exit 2
fi
if [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=no)" ]]; then
    printf 'error: tracked worktree changes exist; commit before collecting Task 1 evidence\n' >&2
    exit 1
fi

python3 - "$rr_host" "$fp_rr_host" <<'PY'
import sys
import tomllib
from pathlib import Path


def load(path: str) -> dict:
    with Path(path).open("rb") as stream:
        return tomllib.load(stream)


rr = load(sys.argv[1])
fp_rr = load(sys.argv[2])
rr_features = set(rr.pop("features", []))
fp_rr_features = set(fp_rr.pop("features", []))
schedulers = {"rr-scheduler", "fp-rr-scheduler"}
if rr != fp_rr:
    raise SystemExit("Task 1 host configs differ outside scheduler feature")
if rr_features & schedulers != {"rr-scheduler"}:
    raise SystemExit("Task 1 RR config does not select only rr-scheduler")
if fp_rr_features & schedulers != {"fp-rr-scheduler"}:
    raise SystemExit("Task 1 FP-RR config does not select only fp-rr-scheduler")
if rr_features - schedulers != fp_rr_features - schedulers:
    raise SystemExit("Task 1 host configs have different auxiliary features")
PY

mkdir -p "$output_root"
git -C "$repo_root" rev-parse HEAD > "$output_root/git-head.txt"
cat > "$output_root/protocol.txt" <<EOF
experiment=starryos-${rtos_name}-task1-scheduler-ab
variable=axvisor-scheduler-feature
arms=rr,fp-rr
topology=starry-vcpu0->pcpu1,${rtos_name}-vcpu0->pcpu1
starry_host_priority=89
${rtos_name}_host_priority=90
workload=starry-in-guest-ncnn-yolo,t2n1-control-status,persistent-normal-loop
qemu=software-in-the-loop
nvme_irq=legacy-intx-msix-qsize-1
run_order=rr,fp-rr
EOF

run_arm() {
    local label="$1" host_config="$2"
    printf 'TASK1_AB_RUN_START arm=%s\n' "$label"
    STARRY_TASK23_HOST_CONFIG="$host_config" \
        STARRY_TASK23_QEMU_CONFIG="$qemu_config" \
        STARRY_TASK23_STARRY_VM_CONFIG="$starry_vm" \
        STARRY_TASK23_RTOS_VM_CONFIG="$rtos_vm" \
        STARRY_TASK23_RTOS_NAME="$rtos_name" \
        STARRY_TASK23_RTOS_IMAGE="${rtos_name}-task2.bin" \
        STARRY_TASK23_RUNTIME_TAG="$runtime_tag" \
        STARRY_TASK23_RTOS_SOURCE_DIR="$rtos_source_dir" \
        STARRY_TASK23_COLLECT_RT_STAT=1 \
        "$scenario_runner" normal "$output_root/$label"
    printf 'TASK1_AB_RUN_COMPLETE arm=%s\n' "$label"
}

run_arm rr "$rr_host"
run_arm fp-rr "$fp_rr_host"

python3 "$verifier" \
    --rr-dir "$output_root/rr" \
    --fp-rr-dir "$output_root/fp-rr" \
    --rtos-name "$rtos_name" \
    --output "$output_root/comparison.md" | tee "$output_root/verify-ab.log"
find "$output_root" -type f ! -path "$output_root/SHA256SUMS.txt" -print0 \
    | sort -z | xargs -0 sha256sum > "$output_root/SHA256SUMS.txt"
printf 'PASS: Task 1 A/B evidence retained in %s\n' "$output_root"
