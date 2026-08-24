#!/usr/bin/env bash
set -euo pipefail

# Measure Zephyr periodic wake-up latency while StarryOS runs real ncnn/YOLO.
# The two arms differ only in the AxVisor scheduler feature.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
output_root="${1:?usage: run-starry-task1-periodic-ab.sh OUTPUT_ROOT}"
repeats="${STARRY_TASK1_PERIODIC_REPEATS:-3}"
rr_host="$repo_root/scripts/test/net-dual-guest/axvisor-qemu-starry-task1-rr.toml"
fp_host="$repo_root/scripts/test/net-dual-guest/axvisor-qemu-starry-task1-fp-rr.toml"
qemu_config="$repo_root/scripts/test/net-dual-guest/qemu-aarch64-starry-zephyr-task1-capture.toml"
starry_vm="$repo_root/scripts/test/net-dual-guest/vm-aarch64-starry-task1-shared.toml"
zephyr_vm_template="$repo_root/scripts/test/net-dual-guest/vm-aarch64-zephyr-periodic-task1-shared.toml"
analyzer="$repo_root/scripts/test/net-dual-guest/analyze_starry_task1_periodic_ab.py"
runtime_dir="$repo_root/tmp/net-dual-guest"
socket_dir="/tmp/tgoskits-task123"
qemu_sock="$socket_dir/qmp-starry-zephyr-msix1-capture.sock"
serial_sock="$socket_dir/serial-starry-zephyr-msix1-capture.sock"
periodic_dir="$repo_root/tmp/starry-task1-periodic"
periodic_bin="$periodic_dir/zephyr-periodic.bin"
periodic_manifest="$periodic_dir/zephyr-periodic.manifest"
rootfs="${STARRY_TASK23_ROOTFS:-$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img}"
run_pid=""

[[ "$repeats" =~ ^[1-9][0-9]*$ ]] || {
    printf 'error: STARRY_TASK1_PERIODIC_REPEATS must be positive\n' >&2
    exit 2
}
[[ ! -e "$output_root" ]] || {
    printf 'error: output already exists: %s\n' "$output_root" >&2
    exit 2
}
if [[ "${ALLOW_DIRTY:-0}" != 1 ]] &&
    [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=no)" ]]; then
    printf 'error: tracked worktree changes exist; commit them or set ALLOW_DIRTY=1\n' >&2
    exit 1
fi
for artifact in "$periodic_bin" "$periodic_manifest" "$rootfs" \
    "$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin"; do
    [[ -s "$artifact" ]] || {
        printf 'error: missing experiment artifact: %s\n' "$artifact" >&2
        exit 1
    }
done
grep -qx 'sample_count=300' "$periodic_manifest"
grep -qx 'start_gated=1' "$periodic_manifest"

python3 - "$rr_host" "$fp_host" <<'PY'
import sys
import tomllib
from pathlib import Path

def load(path):
    with Path(path).open("rb") as stream:
        return tomllib.load(stream)

rr = load(sys.argv[1])
fp = load(sys.argv[2])
rr_features = set(rr.pop("features", []))
fp_features = set(fp.pop("features", []))
if rr != fp:
    raise SystemExit("scheduler A/B configs differ outside features")
if rr_features ^ fp_features != {"rr-scheduler", "fp-rr-scheduler"}:
    raise SystemExit("scheduler A/B feature difference is not RR versus FP-RR")
PY

stop_owned_run() {
    if [[ -S "$qemu_sock" ]]; then
        python3 "$repo_root/scripts/test/net-dual-guest/qmp_link.py" "$qemu_sock" quit \
            >/dev/null 2>&1 || true
    fi
    if [[ -n "$run_pid" ]] && kill -0 "$run_pid" 2>/dev/null; then
        kill -TERM "$run_pid" 2>/dev/null || true
        wait "$run_pid" 2>/dev/null || true
    fi
}
trap stop_owned_run EXIT

mkdir -p "$output_root"
zephyr_vm="$output_root/vm-zephyr.runtime.toml"
python3 "$repo_root/scripts/test/net-dual-guest/render_vm_entry.py" \
    "$periodic_manifest" "$zephyr_vm_template" "$zephyr_vm"
git -C "$repo_root" rev-parse HEAD > "$output_root/git-head.txt"
cp "$periodic_manifest" "$output_root/zephyr-periodic.manifest"
cat > "$output_root/protocol.txt" <<EOF
experiment=starryos-yolo-zephyr-periodic-scheduler-ab
single_variable=axvisor-scheduler-feature
arms=rr,fp-rr
repeats_per_arm=$repeats
run_order=rr,fp-rr,repeated
topology=starry-vcpu0->pcpu1,zephyr-vcpu0->pcpu1
starry_host_priority=89
zephyr_host_priority=90
zephyr_period_ms=10
zephyr_samples=300
interference=starry-in-guest-ncnn-yolo-startup-inference
EOF

run_arm() {
    local arm="$1" run_number="$2" host_config="$3"
    local run_id run_dir steps build_log run_log
    printf -v run_id '%s-%02d' "$arm" "$run_number"
    run_dir="$output_root/$run_id"
    steps="$run_dir/steps.txt"
    build_log="$run_dir/build.log"
    run_log="$run_dir/run.log"
    mkdir -p "$run_dir"
    cp "$host_config" "$run_dir/host-config.toml"
    cp "$qemu_config" "$run_dir/qemu.toml"
    cp "$starry_vm" "$run_dir/vm-starry.toml"
    cp "$zephyr_vm" "$run_dir/vm-zephyr.toml"

    for socket_path in "$qemu_sock" "$serial_sock"; do
        if lsof -t -- "$socket_path" >/dev/null 2>&1; then
            printf 'error: runtime socket is owned by another process: %s\n' "$socket_path" >&2
            exit 1
        fi
        rm -f -- "$socket_path"
    done
    cat > "$steps" <<EOF
expect 120 use (Round-robin|Fixed-priority round-robin) scheduler\\.
expect 120 \\[VM 1\\] Use .*apk
detach
expect 20 Welcome to AxVisor Shell!
attach 1
expect 120 root@starry:/root #
cmd (sh /usr/bin/t2n1-run.sh normal) &
expect 30 TASK3_MODEL_READY model=yolo11n.ncnn runtime=ncnn
expect 30 TASK3_INFER_STARTED model=yolo11n.ncnn request=1 phase=startup
attach 2
expect 120 PERIODIC LATENCY READY
raw g
expect 10 PERIODIC LATENCY START
expect 60 PERIODIC LATENCY COMPLETE samples=300
attach 1
expect 180 TASK3_INFER model=yolo11n.ncnn .*infer_us=.*request=1
detach
cmd rt stat
expect 30 RT vCPU wait counters:
qmp-quit $qemu_sock
EOF
    {
        printf 'arm=%s\nrun=%s\ngit_head=%s\n' "$arm" "$run_number" \
            "$(git -C "$repo_root" rev-parse HEAD)"
        printf 'command=cargo xtask axvisor qemu --config %s --qemu-config %s --vmconfigs %s --vmconfigs %s --rootfs %s\n' \
            "$host_config" "$qemu_config" "$starry_vm" "$zephyr_vm" "$rootfs"
    } > "$run_dir/command.txt"

    printf 'TASK1_PERIODIC_RUN_START arm=%s run=%s\n' "$arm" "$run_number"
    (
        cd "$repo_root"
        cargo xtask axvisor qemu \
            --config "$host_config" \
            --qemu-config "$qemu_config" \
            --vmconfigs "$starry_vm" \
            --vmconfigs "$zephyr_vm" \
            --rootfs "$rootfs"
    ) > "$build_log" 2>&1 &
    run_pid=$!
    for _ in $(seq 1 180); do
        [[ -S "$serial_sock" ]] && break
        if ! kill -0 "$run_pid" 2>/dev/null; then
            printf 'error: AxVisor exited before serial socket creation\n' >&2
            tail -40 "$build_log" >&2
            exit 1
        fi
        sleep 1
    done
    [[ -S "$serial_sock" ]] || {
        printf 'error: serial socket did not appear\n' >&2
        exit 1
    }
    (
        cd "$repo_root"
        python3 scripts/test/net-dual-guest/serial_console.py \
            "$serial_sock" "$run_log" --script "$steps" --verbose \
            --qmp-sock "$qemu_sock" --forensics-dir "$run_dir/forensics"
    ) 2>> "$build_log"
    wait "$run_pid" 2>/dev/null || true
    run_pid=""
    sha256sum "$periodic_bin" "$rootfs" \
        "$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin" \
        "$repo_root/target/aarch64-unknown-linux-musl/release/axvisor.bin" \
        > "$run_dir/artifact-hashes.txt"
    printf 'TASK1_PERIODIC_RUN_COMPLETE arm=%s run=%s\n' "$arm" "$run_number"
}

rr_runs=()
fp_runs=()
for run_number in $(seq 1 "$repeats"); do
    run_arm rr "$run_number" "$rr_host"
    rr_runs+=("$output_root/rr-$(printf '%02d' "$run_number")")
    run_arm fp-rr "$run_number" "$fp_host"
    fp_runs+=("$output_root/fp-rr-$(printf '%02d' "$run_number")")
done

python3 "$analyzer" --rr "${rr_runs[@]}" --fp-rr "${fp_runs[@]}" \
    --output "$output_root/comparison.md" | tee "$output_root/verify.log"
find "$output_root" -type f ! -name SHA256SUMS.txt -print0 \
    | sort -z | xargs -0 sha256sum > "$output_root/SHA256SUMS.txt"
printf 'PASS: StarryOS Task 1 periodic A/B retained in %s\n' "$output_root"
