#!/usr/bin/env bash
set -euo pipefail

# Run one real StarryOS + RTOS Task-2/Task-3 scenario and retain both
# Guest pcaps, console evidence, exact commands, manifests, and hashes.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scenario="${1:?usage: run-starry-task23-scenario.sh SCENARIO OUTPUT_DIR}"
output_dir="${2:?usage: run-starry-task23-scenario.sh SCENARIO OUTPUT_DIR}"
host_config="${STARRY_TASK23_HOST_CONFIG:-scripts/test/net-dual-guest/axvisor-qemu-debug.toml}"
qemu_config="${STARRY_TASK23_QEMU_CONFIG:-scripts/test/net-dual-guest/qemu-aarch64-starry-zephyr-switch-msix1-capture.toml}"
starry_vm_config="${STARRY_TASK23_STARRY_VM_CONFIG:-scripts/test/net-dual-guest/vm-aarch64-starry-switch.toml}"
rtos_vm_config="${STARRY_TASK23_RTOS_VM_CONFIG:-${STARRY_TASK23_ZEPHYR_VM_CONFIG:-scripts/test/net-dual-guest/vm-aarch64-p2-switch-rtos.toml}}"
rtos_name="${STARRY_TASK23_RTOS_NAME:-zephyr}"
rtos_image="${STARRY_TASK23_RTOS_IMAGE:-${rtos_name}-task2.bin}"
runtime_tag="${STARRY_TASK23_RUNTIME_TAG:-starry-zephyr-msix1-capture}"
rtos_source_dir="${STARRY_TASK23_RTOS_SOURCE_DIR:-}"
collect_rt_stat="${STARRY_TASK23_COLLECT_RT_STAT:-0}"
runtime_dir="$repo_root/tmp/net-dual-guest"
qemu_sock="$runtime_dir/qmp-${runtime_tag}.sock"
serial_sock="$runtime_dir/serial-${runtime_tag}.sock"
capture_prefix="$runtime_dir/starry-task23-current"
steps="$output_dir/steps.txt"
run_log="$output_dir/run.log"
build_log="$output_dir/build.log"
run_pid=""

case "$rtos_name" in
    zephyr|rtthread) ;;
    *) printf 'error: unsupported RTOS name: %s\n' "$rtos_name" >&2; exit 2 ;;
esac
if [[ ! "$runtime_tag" =~ ^[a-zA-Z0-9._-]+$ ]]; then
    printf 'error: invalid runtime tag: %s\n' "$runtime_tag" >&2
    exit 2
fi

case "$collect_rt_stat" in
    0|1) ;;
    *)
        printf 'error: STARRY_TASK23_COLLECT_RT_STAT must be 0 or 1\n' >&2
        exit 2
        ;;
esac

resolve_config_path() {
    if [[ "$1" == /* ]]; then
        printf '%s\n' "$1"
    else
        printf '%s/%s\n' "$repo_root" "$1"
    fi
}

host_config_path="$(resolve_config_path "$host_config")"
qemu_config_path="$(resolve_config_path "$qemu_config")"
starry_vm_config_path="$(resolve_config_path "$starry_vm_config")"
rtos_vm_config_path="$(resolve_config_path "$rtos_vm_config")"
for config_path in \
    "$host_config_path" "$qemu_config_path" \
    "$starry_vm_config_path" "$rtos_vm_config_path"; do
    if [[ ! -f "$config_path" ]]; then
        printf 'error: missing scenario config: %s\n' "$config_path" >&2
        exit 1
    fi
done

case "$scenario" in
    normal|blackout|model-rejected)
        rtos_variant="normal"
        ;;
    drop-ack)
        rtos_variant="drop-ack"
        ;;
    retry-exhausted)
        rtos_variant="retry-exhausted"
        ;;
    out-of-order|invalid-parameter)
        rtos_variant="normal"
        ;;
    *)
        printf 'error: unknown scenario %s\n' "$scenario" >&2
        exit 2
        ;;
esac

case "$scenario" in
    normal|drop-ack|retry-exhausted|blackout) run_mode="normal" ;;
    *) run_mode="$scenario" ;;
esac

if [[ -d "$output_dir" ]] && find "$output_dir" -mindepth 1 -print -quit | grep -q .; then
    printf 'error: output directory is not empty: %s\n' "$output_dir" >&2
    exit 1
fi
mkdir -p "$output_dir"

if [[ "${ALLOW_DIRTY:-0}" != 1 ]] &&
    [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=no)" ]]; then
    printf 'error: tracked worktree changes exist; commit them or set ALLOW_DIRTY=1 for a diagnostic run\n' >&2
    exit 1
fi

normal_dir="${rtos_source_dir:-$runtime_dir/${rtos_name}-task2-starry-normal}"
drop_dir="$runtime_dir/${rtos_name}-task2-starry-drop-ack"
retry_exhausted_dir="$runtime_dir/${rtos_name}-task2-starry-retry-exhausted"
case "$rtos_variant" in
    normal)
        selected_rtos_dir="$normal_dir"
        expected_fault_mode="none"
        ;;
    drop-ack)
        selected_rtos_dir="$drop_dir"
        expected_fault_mode="drop-ack-once"
        ;;
    retry-exhausted)
        selected_rtos_dir="$retry_exhausted_dir"
        expected_fault_mode="drop-ack-always"
        ;;
esac
for artifact in "$selected_rtos_dir/$rtos_image" "$selected_rtos_dir/manifest.toml"; do
    if [[ ! -s "$artifact" ]]; then
        printf 'error: missing %s artifact: %s\n' "$rtos_name" "$artifact" >&2
        exit 1
    fi
done
if ! grep -q "^fault_mode = \"$expected_fault_mode\"$" "$selected_rtos_dir/manifest.toml"; then
    printf 'error: %s manifest fault mode does not match %s\n' "$rtos_name" "$scenario" >&2
    exit 1
fi

rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img"
endpoint="$repo_root/target/starryos-task2-rust/aarch64-unknown-linux-musl/release/starryos-task2-endpoint"
endpoint_script="$repo_root/apps/starry/starryos-task2/t2n1-run.sh"
yolo_assets="$repo_root/tmp/task3-yolo/ncnn-model"
yolo_param="$yolo_assets/yolo11n.ncnn.param"
yolo_model="$yolo_assets/yolo11n.ncnn.bin"
yolo_input="$yolo_assets/input.ppm"
starry_image="$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin"
axvisor_image="$repo_root/target/aarch64-unknown-linux-musl/release/axvisor.bin"
for artifact in \
    "$rootfs" "$endpoint" "$endpoint_script" "$yolo_param" "$yolo_model" \
    "$yolo_input" "$starry_image"; do
    if [[ ! -s "$artifact" ]]; then
        printf 'error: missing StarryOS artifact: %s\n' "$artifact" >&2
        exit 1
    fi
done

host_endpoint_sha256="$(sha256sum "$endpoint" | awk '{print $1}')"
rootfs_endpoint_sha256="$(
    debugfs -R 'dump /usr/bin/starry-t2n1-endpoint /dev/stdout' "$rootfs" 2>/dev/null |
        sha256sum | awk '{print $1}'
)"
host_script_sha256="$(sha256sum "$endpoint_script" | awk '{print $1}')"
rootfs_script_sha256="$(
    debugfs -R 'dump /usr/bin/t2n1-run.sh /dev/stdout' "$rootfs" 2>/dev/null |
        sha256sum | awk '{print $1}'
)"
host_yolo_param_sha256="$(sha256sum "$yolo_param" | awk '{print $1}')"
host_yolo_model_sha256="$(sha256sum "$yolo_model" | awk '{print $1}')"
host_yolo_input_sha256="$(sha256sum "$yolo_input" | awk '{print $1}')"
rootfs_yolo_param_sha256="$(
    debugfs -R 'dump /usr/share/task3-yolo/yolo11n.ncnn.param /dev/stdout' "$rootfs" 2>/dev/null |
        sha256sum | awk '{print $1}'
)"
rootfs_yolo_model_sha256="$(
    debugfs -R 'dump /usr/share/task3-yolo/yolo11n.ncnn.bin /dev/stdout' "$rootfs" 2>/dev/null |
        sha256sum | awk '{print $1}'
)"
rootfs_yolo_input_sha256="$(
    debugfs -R 'dump /usr/share/task3-yolo/input.ppm /dev/stdout' "$rootfs" 2>/dev/null |
        sha256sum | awk '{print $1}'
)"
if [[ "$host_endpoint_sha256" != "$rootfs_endpoint_sha256" ]]; then
    printf 'error: rootfs endpoint does not match current release binary\n' >&2
    exit 1
fi
if [[ "$host_script_sha256" != "$rootfs_script_sha256" ]]; then
    printf 'error: rootfs runner does not match current source script\n' >&2
    exit 1
fi
for asset in param model input; do
    host_hash_variable="host_yolo_${asset}_sha256"
    rootfs_hash_variable="rootfs_yolo_${asset}_sha256"
    if [[ "${!host_hash_variable}" != "${!rootfs_hash_variable}" ]]; then
        printf 'error: rootfs YOLO %s does not match current asset\n' "$asset" >&2
        exit 1
    fi
done
{
    printf 'host_endpoint_sha256=%s\n' "$host_endpoint_sha256"
    printf 'rootfs_endpoint_sha256=%s\n' "$rootfs_endpoint_sha256"
    printf 'host_script_sha256=%s\n' "$host_script_sha256"
    printf 'rootfs_script_sha256=%s\n' "$rootfs_script_sha256"
    printf 'host_yolo_param_sha256=%s\n' "$host_yolo_param_sha256"
    printf 'rootfs_yolo_param_sha256=%s\n' "$rootfs_yolo_param_sha256"
    printf 'host_yolo_model_sha256=%s\n' "$host_yolo_model_sha256"
    printf 'rootfs_yolo_model_sha256=%s\n' "$rootfs_yolo_model_sha256"
    printf 'host_yolo_input_sha256=%s\n' "$host_yolo_input_sha256"
    printf 'rootfs_yolo_input_sha256=%s\n' "$rootfs_yolo_input_sha256"
} > "$output_dir/rootfs-content-hashes.txt"

stop_owned_run() {
    if [[ -S "$qemu_sock" ]]; then
        python3 "$repo_root/scripts/test/net-dual-guest/qmp_link.py" "$qemu_sock" quit \
            >/dev/null 2>&1 || true
    fi
    if [[ -n "$run_pid" ]] && kill -0 "$run_pid" 2>/dev/null; then
        kill -TERM "$run_pid" 2>/dev/null || true
        wait "$run_pid" 2>/dev/null || true
    fi
    for socket_path in "$qemu_sock" "$serial_sock"; do
        while read -r owner_pid; do
            [[ -n "$owner_pid" ]] && kill -TERM "$owner_pid" 2>/dev/null || true
        done < <(lsof -t -- "$socket_path" 2>/dev/null || true)
    done
}
trap stop_owned_run EXIT

for socket_path in "$qemu_sock" "$serial_sock"; do
    if lsof -t -- "$socket_path" >/dev/null 2>&1; then
        printf 'error: runtime socket is owned by another process: %s\n' "$socket_path" >&2
        exit 1
    fi
    rm -f -- "$socket_path"
done
rm -f -- "$capture_prefix.vm1.pcap" "$capture_prefix.vm2.pcap"

mkdir -p "$runtime_dir/${rtos_name}-task2"
cp "$selected_rtos_dir/$rtos_image" "$runtime_dir/${rtos_name}-task2/$rtos_image"
cp "$selected_rtos_dir/manifest.toml" "$runtime_dir/${rtos_name}-task2/manifest.toml"

{
    if [[ "$collect_rt_stat" == 1 ]]; then
        printf 'expect 120 use (Round-robin|Fixed-priority round-robin) scheduler\\.\n'
        printf 'expect 120 \\[VM 1\\] Use .*apk\n'
        printf 'send-until 30 1 \\x18h axvisor:/\\$\n'
    else
        printf 'send-until 30 1 \\x18h axvisor:/\\$\n'
    fi
    printf 'cmd virtnet capture on\n'
    printf 'expect 20 virtnet: capture ON\n'
    printf 'attach 1\n'
    printf 'expect 120 root@starry:/root #\n'
    printf 'cmd (sleep 2; sh /usr/bin/t2n1-run.sh %s) &\n' "$run_mode"
    printf 'expect 30 TASK3_MODEL_READY model=yolo11n.ncnn\n'
    case "$scenario" in
        normal)
            printf 'attach 2\n'
            printf 'expect 120 TASK2_CONTROL_RECEIVED seq=1 request=1\n'
            printf 'attach 1\n'
            printf 'expect 60 STARRY_T2N1_PASS\n'
            printf 'expect 180 STARRY_T2N1_STATUS_DELIVERED.*request=3\n'
            ;;
        drop-ack)
            printf 'attach 2\n'
            printf 'expect 120 TASK2_FAULT_DROP_ACK seq=1\n'
            printf 'attach 1\n'
            printf 'expect 30 STARRY_T2N1_RETRANSMIT seq=1 attempt=1\n'
            printf 'expect 30 STARRY_T2N1_ACK seq=1\n'
            printf 'expect 30 STARRY_T2N1_PASS\n'
            printf 'attach 2\n'
            printf 'expect 30 TASK2_FAULT_DROP_ACK_RECOVERED duplicate_seq=1\n'
            ;;
        retry-exhausted)
            printf 'attach 2\n'
            printf 'expect 120 TASK2_FAULT_DROP_ACK_ALWAYS seq=1\n'
            printf 'attach 1\n'
            printf 'expect 30 STARRY_T2N1_RETRANSMIT seq=1 attempt=5\n'
            printf 'expect 30 STARRY_T2N1_SAFE source=protocol reason=RetryExhausted\n'
            printf 'expect 30 STARRY_T2N1_RECOVERED state=Active\n'
            ;;
        out-of-order)
            printf 'attach 2\n'
            printf 'expect 30 TASK2_PROTOCOL_ERROR out_of_order=2 expected=1\n'
            printf 'attach 1\n'
            printf 'expect 120 STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=out-of-order\n'
            printf 'expect 30 STARRY_T2N1_PASS\n'
            ;;
        invalid-parameter)
            printf 'attach 2\n'
            printf 'expect 30 TASK2_PROTOCOL_ERROR invalid_parameter seq=1\n'
            printf 'attach 1\n'
            printf 'expect 120 STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=invalid-parameter\n'
            printf 'expect 120 STARRY_T2N1_PASS\n'
            ;;
        blackout)
            printf 'attach 1\n'
            printf 'expect 30 STARRY_T2N1_PASS\n'
            printf 'send-until 10 1 \\x18h axvisor:/\\$\n'
            printf 'cmd virtnet drop on\n'
            printf 'expect 20 virtnet: blackout ON\n'
            printf 'attach 1\n'
            printf 'expect 30 STARRY_T2N1_SAFE source=protocol\n'
            printf 'hold 3\n'
            printf 'clear-tail\n'
            printf 'attach 2\n'
            printf 'expect 30 TASK2_SAFE state=Safe event=HeartbeatTimeout\n'
            printf 'send-until 10 1 \\x18h axvisor:/\\$\n'
            printf 'cmd virtnet drop off\n'
            printf 'expect 20 virtnet: blackout OFF\n'
            printf 'attach 1\n'
            printf 'expect 30 STARRY_T2N1_RECOVERED state=Active\n'
            printf 'expect 180 STARRY_T2N1_FAULT_RECOVERY_COMPLETE mode=normal.*safe_observed=true recovered=true\n'
            printf 'attach 2\n'
            printf 'expect 60 TASK2_CONTROL_RECEIVED.*request=\n'
            ;;
        model-rejected)
            printf 'attach 1\n'
            printf 'expect 30 TASK3_MODEL_REJECTED.*reason=InjectedInvalidOutput\n'
            printf 'expect 30 STARRY_T2N1_SAFE source=model reason=InjectedInvalidOutput\n'
            printf 'hold 3\n'
            ;;
    esac
    printf 'send-until 10 1 \\x18h axvisor:/\\$\n'
    if [[ "$collect_rt_stat" == 1 ]]; then
        printf 'cmd rt stat\n'
        printf 'expect 30 RT vCPU wait counters:\n'
    fi
    printf 'dump-pcap %s\n' "$capture_prefix"
    printf 'qmp-quit %s\n' "$qemu_sock"
} > "$steps"

{
    printf 'scenario=%s\n' "$scenario"
    printf 'git_head=%s\n' "$(git -C "$repo_root" rev-parse HEAD)"
    printf 'rtos_name=%s\n' "$rtos_name"
    printf 'rtos_variant=%s\n' "$rtos_variant"
    printf 'host_config=%s\n' "$host_config"
    printf 'qemu_config=%s\n' "$qemu_config"
    printf 'starry_vm_config=%s\n' "$starry_vm_config"
    printf 'rtos_vm_config=%s\n' "$rtos_vm_config"
    printf 'collect_rt_stat=%s\n' "$collect_rt_stat"
    printf 'command=cargo xtask axvisor qemu --config %s --qemu-config %s --vmconfigs %s --vmconfigs %s --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img\n' \
        "$host_config" "$qemu_config" "$starry_vm_config" "$rtos_vm_config"
} > "$output_dir/command.txt"

(
    cd "$repo_root"
    cargo xtask axvisor qemu \
        --config "$host_config" \
        --qemu-config "$qemu_config" \
        --vmconfigs "$starry_vm_config" \
        --vmconfigs "$rtos_vm_config" \
        --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
) > "$build_log" 2>&1 &
run_pid=$!

for _ in $(seq 1 1200); do
    [[ -S "$serial_sock" ]] && break
    if ! kill -0 "$run_pid" 2>/dev/null; then
        printf 'error: AxVisor exited before serial socket creation\n' >&2
        tail -40 "$build_log" >&2
        exit 1
    fi
    sleep 0.05
done
if [[ ! -S "$serial_sock" ]]; then
    printf 'error: serial socket did not appear\n' >&2
    tail -40 "$build_log" >&2
    exit 1
fi

(
    cd "$repo_root"
    python3 scripts/test/net-dual-guest/serial_console.py \
        "$serial_sock" "$run_log" --script "$steps" --verbose \
        --qmp-sock "$qemu_sock" --forensics-dir "$output_dir/forensics"
) 2>> "$build_log"

sleep 2
if kill -0 "$run_pid" 2>/dev/null; then
    wait "$run_pid" 2>/dev/null || true
fi
run_pid=""

for pcap in "$capture_prefix.vm1.pcap" "$capture_prefix.vm2.pcap"; do
    if [[ ! -s "$pcap" ]]; then
        printf 'error: missing pcap: %s\n' "$pcap" >&2
        exit 1
    fi
done
cp "$capture_prefix.vm1.pcap" "$output_dir/starry.pcap"
cp "$capture_prefix.vm2.pcap" "$output_dir/${rtos_name}.pcap"
cp "$selected_rtos_dir/manifest.toml" "$output_dir/${rtos_name}-manifest.toml"
cp "$host_config_path" "$output_dir/host-config.toml"
cp "$qemu_config_path" "$output_dir/qemu.toml"
cp "$starry_vm_config_path" "$output_dir/vm-starry.toml"
cp "$rtos_vm_config_path" "$output_dir/vm-${rtos_name}.toml"

if [[ "$scenario" == model-rejected ]]; then
    pcap_requirements=(--tag '' --min-udp 2)
else
    pcap_requirements=(--tag '' --require-task2)
fi
python3 "$repo_root/scripts/test/net-dual-guest/verify_pcap.py" \
    "${pcap_requirements[@]}" "$output_dir/starry.pcap" "$output_dir/${rtos_name}.pcap" \
    | tee "$output_dir/verify-pcap.log"
python3 "$repo_root/scripts/test/net-dual-guest/verify_starry_task23.py" \
    --scenario "$scenario" \
    --starry-pcap "$output_dir/starry.pcap" \
    --zephyr-pcap "$output_dir/${rtos_name}.pcap" \
    --run-log "$run_log" | tee "$output_dir/verify-scenario.log"

{
    sha256sum "$rootfs"
    sha256sum "$endpoint"
    sha256sum "$yolo_param"
    sha256sum "$yolo_model"
    sha256sum "$yolo_input"
    sha256sum "$starry_image"
    sha256sum "$selected_rtos_dir/$rtos_image"
    sha256sum "$axvisor_image"
} > "$output_dir/artifact-hashes.txt"

find "$output_dir" -maxdepth 1 -type f ! -name SHA256SUMS.txt -print0 \
    | sort -z | xargs -0 sha256sum > "$output_dir/SHA256SUMS.txt"
printf 'PASS: evidence retained in %s\n' "$output_dir"
