#!/usr/bin/env bash
set -euo pipefail

# Build and run one Task1 matrix scenario with scenario-specific VM configs.
# Evidence is accepted only when both guests complete, cyclictest emits a real
# histogram, and the requested duration is met. VM-exit/tick snapshots remain
# mandatory when the selected source tree supports those diagnostics.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source_root="${RT_SOURCE_ROOT:-$repo_root}"
scenario="${RT_SCENARIO:-idle}"
loops="${RT_LOOPS:-1800000}"
duration_sec="${RT_DURATION_SEC:-0}"
interval_us="${RT_INTERVAL_US:-1000}"
maxlat_us="${RT_MAXLAT_US:-20000}"
deadline_tolerance_ns="${RT_DEADLINE_TOLERANCE_NS:-1000000}"
priority="${RT_PRIORITY:-90}"
rt_cpu="${RT_CPU:-1}"
rt_cpu_override="${RT_CPU:-}"
start_delay_sec="${RT_START_DELAY_SEC:-25}"
result_drain_timeout="${RT_RESULT_DRAIN_TIMEOUT_SEC:-180}"
zephyr_timeout="${RT_ZEPHYR_TIMEOUT_SEC:-180}"
burner_config="${RT_BURNER:-}"
vmexit_diagnostics="${RT_VMEXIT_DIAGNOSTICS:-1}"
runtime_diagnostics="${RT_RUNTIME_DIAGNOSTICS:-0}"
timer_storm_command="${RT_TIMER_STORM_COMMAND:-}"
rootfs_override="${RT_ROOTFS:-}"
linux_image="${RT_LINUX_KERNEL_OVERRIDE:-${repo_root}/tmp/rt-partition/linux-qemu}"
linux_trace="${RT_LINUX_TRACE:-disabled}"
linux_trace_buffer_kb="${RT_LINUX_TRACE_BUFFER_KB:-8192}"
linux_virtual_timer_only="${RT_LINUX_VIRTUAL_TIMER_ONLY:-0}"
linux_wfi_policy="${RT_LINUX_WFI_POLICY:-auto}"
dedicated_cpus_override="${RT_DEDICATED_CPUS_OVERRIDE:-}"
linux_phys_cpu_ids="${RT_LINUX_PHYS_CPU_IDS:-2,3}"
zephyr_phys_cpu_ids="${RT_ZEPHYR_PHYS_CPU_IDS:-1}"
linux_template_override="${RT_LINUX_TEMPLATE:-}"
zephyr_template_override="${RT_ZEPHYR_TEMPLATE:-}"
require_init_done="${RT_REQUIRE_INIT_DONE:-1}"
hold_after_complete="${RT_HOLD_AFTER_COMPLETE:-0}"
qemu_exit_grace_sec="${RT_QEMU_EXIT_GRACE_SEC:-10}"
zephyr_sample_count_expected="${RT_ZEPHYR_SAMPLE_COUNT:-300}"
allow_dirty="${RT_ALLOW_DIRTY:-0}"

git -C "$source_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    printf 'error: RT_SOURCE_ROOT is not a git worktree: %s\n' "$source_root" >&2
    exit 2
}
case "$allow_dirty" in
    0|1) ;;
    *) printf 'error: RT_ALLOW_DIRTY must be 0 or 1\n' >&2; exit 2 ;;
esac
tracked_status="$(git -C "$source_root" status --porcelain --untracked-files=no)"
if [[ -n "$tracked_status" && "$allow_dirty" == "0" ]]; then
    printf 'error: source worktree has tracked changes; commit them or set RT_ALLOW_DIRTY=1\n' >&2
    printf '%s\n' "$tracked_status" >&2
    exit 2
fi
tracked_dirty=0
[[ -n "$tracked_status" ]] && tracked_dirty=1
untracked_count="$(git -C "$source_root" ls-files --others --exclude-standard | wc -l | tr -d ' ')"
case "$vmexit_diagnostics" in
    0|1) ;;
    *) printf 'error: RT_VMEXIT_DIAGNOSTICS must be 0 or 1\n' >&2; exit 2 ;;
esac
case "$runtime_diagnostics" in
    0|1) ;;
    *) printf 'error: RT_RUNTIME_DIAGNOSTICS must be 0 or 1\n' >&2; exit 2 ;;
esac
case "$require_init_done" in
    0|1) ;;
    *) printf 'error: RT_REQUIRE_INIT_DONE must be 0 or 1\n' >&2; exit 2 ;;
esac
case "$hold_after_complete" in
    0|1) ;;
    *) printf 'error: RT_HOLD_AFTER_COMPLETE must be 0 or 1\n' >&2; exit 2 ;;
esac
case "$linux_trace" in
    disabled|events|timerlat) ;;
    *) printf 'error: RT_LINUX_TRACE must be disabled, events, or timerlat\n' >&2; exit 2 ;;
esac
case "$linux_virtual_timer_only" in
    0|1) ;;
    *) printf 'error: RT_LINUX_VIRTUAL_TIMER_ONLY must be 0 or 1\n' >&2; exit 2 ;;
esac
case "$linux_wfi_policy" in
    auto|trap|passthrough) ;;
    *) printf 'error: RT_LINUX_WFI_POLICY must be auto, trap, or passthrough\n' >&2; exit 2 ;;
esac
if [[ "$linux_wfi_policy" == "passthrough" && "$linux_virtual_timer_only" != "1" ]]; then
    printf 'error: RT_LINUX_WFI_POLICY=passthrough requires RT_LINUX_VIRTUAL_TIMER_ONLY=1\n' >&2
    exit 2
fi
if [[ -n "$dedicated_cpus_override" && ! "$dedicated_cpus_override" =~ ^[0-9]+(,[0-9]+)*$ ]]; then
    printf 'error: RT_DEDICATED_CPUS_OVERRIDE must be a comma-separated CPU list\n' >&2
    exit 2
fi
if [[ -n "$burner_config" && ! "$burner_config" =~ ^[0-9]+:[0-9]+:[0-9]+(:[0-9]+)?$ ]]; then
    printf 'error: RT_BURNER must use <cpu>:<busy_ms>:<idle_ms>[:<start_delay_ms>]\n' >&2
    exit 2
fi
if [[ -n "$rootfs_override" && ! -f "$rootfs_override" ]]; then
    printf 'error: RT_ROOTFS does not exist: %s\n' "$rootfs_override" >&2
    exit 2
fi
if [[ ! -f "$linux_image" ]]; then
    printf 'error: Linux kernel image does not exist: %s\n' "$linux_image" >&2
    exit 2
fi

case "$scenario" in
    idle)
        dedicated_cpus=""
        zephyr_guest_type="virtualized"
        runtime_scale=3
        ;;
    stress-noiso)
        dedicated_cpus=""
        zephyr_guest_type="virtualized"
        runtime_scale=3
        ;;
    stress-guest-shared)
        # Guest-to-guest contention cell: Linux vCPU0 and Zephyr vCPU0 both
        # execute on pCPU1; Linux vCPU1 remains on pCPU2 for guest housekeeping.
        dedicated_cpus=""
        zephyr_guest_type="virtualized"
        linux_phys_cpu_ids="1,2"
        zephyr_phys_cpu_ids="1"
        [[ -n "$rt_cpu_override" ]] || rt_cpu=0
        runtime_scale=3
        ;;
    stress-rt)
        dedicated_cpus="1"
        zephyr_guest_type="passthrough"
        runtime_scale=3
        ;;
    stress-dedicated)
        dedicated_cpus="1"
        zephyr_guest_type="virtualized"
        runtime_scale=3
        ;;
    *)
        printf 'error: RT_SCENARIO must be idle, stress-noiso, stress-guest-shared, stress-dedicated, or stress-rt\n' >&2
        exit 2
        ;;
esac
if [[ -n "$dedicated_cpus_override" ]]; then
    dedicated_cpus="$dedicated_cpus_override"
fi
calibration_file="${RT_CALIBRATION_FILE:-${repo_root}/results/task1/calibration/runtime-scales.env}"
calibrated_scale=""
if [[ -f "$calibration_file" ]]; then
    calibrated_scale="$(sed -n "s/^${scenario}=//p" "$calibration_file" | tail -n 1)"
fi
if [[ -n "${RT_TCG_RUNTIME_SCALE:-}" ]]; then
    runtime_scale="$RT_TCG_RUNTIME_SCALE"
    runtime_scale_source="environment"
elif [[ -n "$calibrated_scale" ]]; then
    runtime_scale="$calibrated_scale"
    runtime_scale_source="$calibration_file"
else
    runtime_scale_source="default"
fi
if (( runtime_diagnostics == 1 )); then
    runtime_final_steps=$'cmd rt stat\nsleep 2'
else
    runtime_final_steps=""
fi
host_tick_args=()
if [[ -n "$dedicated_cpus" ]]; then
    IFS=',' read -r -a dedicated_cpu_list <<< "$dedicated_cpus"
    for dedicated_cpu in "${dedicated_cpu_list[@]}"; do
        host_tick_args+=(--require-zero-cpu "$dedicated_cpu")
    done
fi

for value in "$loops" "$duration_sec" "$interval_us" "$maxlat_us" "$deadline_tolerance_ns" "$priority" "$rt_cpu" "$start_delay_sec" "$runtime_scale" "$result_drain_timeout" "$zephyr_timeout" "$linux_trace_buffer_kb" "$qemu_exit_grace_sec"; do
    [[ "$value" =~ ^[0-9]+$ ]] || { printf 'error: numeric RT option is invalid: %s\n' "$value" >&2; exit 2; }
done
[[ "$zephyr_sample_count_expected" =~ ^[1-9][0-9]*$ ]] || {
    printf 'error: RT_ZEPHYR_SAMPLE_COUNT must be a positive integer\n' >&2
    exit 2
}
(( interval_us > 0 && runtime_scale > 0 && result_drain_timeout > 0 && zephyr_timeout > 0 && linux_trace_buffer_kb > 0 && qemu_exit_grace_sec > 0 )) || {
    printf 'error: interval, runtime scale, result drain timeout, Zephyr timeout, trace buffer, and QEMU exit grace must be positive\n' >&2
    exit 2
}
(( rt_cpu <= 1 )) || { printf 'error: RT_CPU must be 0 or 1 for the two-vCPU Linux guest\n' >&2; exit 2; }
for cpu_list in "$linux_phys_cpu_ids" "$zephyr_phys_cpu_ids"; do
    [[ "$cpu_list" =~ ^[0-9]+(,[0-9]+)*$ ]] || {
        printf 'error: physical CPU mapping must be a comma-separated CPU list: %s\n' "$cpu_list" >&2
        exit 2
    }
done
IFS=',' read -r -a linux_phys_cpu_list <<< "$linux_phys_cpu_ids"
IFS=',' read -r -a zephyr_phys_cpu_list <<< "$zephyr_phys_cpu_ids"
(( ${#linux_phys_cpu_list[@]} == 2 )) || {
    printf 'error: Linux VM requires exactly two physical CPU IDs: %s\n' "$linux_phys_cpu_ids" >&2
    exit 2
}
(( ${#zephyr_phys_cpu_list[@]} == 1 )) || {
    printf 'error: Zephyr VM requires exactly one physical CPU ID: %s\n' "$zephyr_phys_cpu_ids" >&2
    exit 2
}
load_cpu=$((1 - rt_cpu))

if (( duration_sec > 0 )); then
    run_mode=duration
    cyclictest_loops=0
    expected_runtime_sec=$duration_sec
else
    (( loops > 0 )) || { printf 'error: RT_LOOPS must be positive in loop mode\n' >&2; exit 2; }
    run_mode=loops
    cyclictest_loops=$loops
    expected_runtime_sec=$(( (loops * interval_us + 999999) / 1000000 ))
fi
expected_wall_runtime_sec=$((expected_runtime_sec * runtime_scale))
experiment_timeout=$(( expected_wall_runtime_sec + 300 ))
linux_start_timeout=300
serial_socket_timeout=600
progress_timeout="${RT_PROGRESS_TIMEOUT_SEC:-300}"
minimum_outer_timeout=$((
    serial_socket_timeout + 120 + linux_start_timeout + 60 + 10 + 2 + 10 + 10 +
    zephyr_timeout + 10 + 2 + 10 + 1 + experiment_timeout + result_drain_timeout +
    30 + 2 + 30 + qemu_exit_grace_sec + 60
))
timeout_sec="${RT_TIMEOUT_SEC:-$minimum_outer_timeout}"
(( timeout_sec >= minimum_outer_timeout )) || {
    printf 'error: RT_TIMEOUT_SEC=%s is shorter than the complete phase budget %s\n' "$timeout_sec" "$minimum_outer_timeout" >&2
    exit 2
}
[[ "$progress_timeout" =~ ^[0-9]+$ ]] && (( progress_timeout > 0 )) || {
    printf 'error: RT_PROGRESS_TIMEOUT_SEC must be a positive integer\n' >&2
    exit 2
}

work="${repo_root}/tmp/rt-partition"
out_root="${RT_OUTPUT_ROOT:-${repo_root}/results/task1/matrix}"
out_dir="${out_root}/${scenario}"
board_toml="${RT_BOARD_CONFIG:-${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml}"
linux_template="${repo_root}/scripts/test/rt-partition/vm-aarch64-rt-linux.toml"
if [[ -n "$linux_template_override" ]]; then
    linux_template="$linux_template_override"
fi
zephyr_template="${repo_root}/scripts/test/rt-partition/rt-partition-zephyr.toml"
if [[ -n "$zephyr_template_override" ]]; then
    zephyr_template="$zephyr_template_override"
fi
zephyr_image="${RT_ZEPHYR_IMAGE:-${work}/zephyr-periodic.bin}"
zephyr_manifest="${RT_ZEPHYR_MANIFEST:-${work}/zephyr-periodic.manifest}"
linux_config="${work}/generated-${scenario}-linux.toml"
zephyr_config="${work}/generated-${scenario}-zephyr.toml"
qemu_config="${work}/generated-${scenario}-qemu.toml"
serial_sock="${work}/serial-${scenario}.sock"
qmp_sock="${work}/qmp-${scenario}.sock"
steps="${work}/steps-${scenario}.txt"
run_log="${out_dir}/run.log"
build_log="${out_dir}/build-qemu.log"

for path in "$board_toml" "$linux_template" "$zephyr_template"; do
    [[ -f "$path" ]] || { printf 'error: missing %s\n' "$path" >&2; exit 1; }
done
for path in "$linux_image" "$work/rt-linux-initramfs.cpio.gz" \
    "$zephyr_image" "$zephyr_manifest"; do
    [[ -f "$path" ]] || {
        printf 'error: missing %s (stage the guest images and run build-rt-tools.sh)\n' "$path" >&2
        exit 1
    }
done

rg -a -F "PERIODIC LATENCY COMPLETE samples=%d" "$zephyr_image" >/dev/null || {
    printf 'error: Zephyr image is not the periodic latency sampler\n' >&2
    exit 1
}
if rg -a -F "TASK2_MAIN_START" "$zephyr_image" >/dev/null; then
    printf 'error: Zephyr image is the Task2 networking guest, not the periodic sampler\n' >&2
    exit 1
fi
zephyr_entry="$(sed -n 's/^entry_point=//p' "$zephyr_manifest")"
zephyr_samples="$(sed -n 's/^sample_count=//p' "$zephyr_manifest")"
zephyr_start_gated="$(sed -n 's/^start_gated=//p' "$zephyr_manifest")"
zephyr_start_delay_ms="$(sed -n 's/^start_delay_ms=//p' "$zephyr_manifest")"
[[ "$zephyr_entry" =~ ^0x[0-9a-fA-F]+$ ]] || {
    printf 'error: invalid Zephyr entry point in manifest: %s\n' "$zephyr_entry" >&2
    exit 1
}
[[ "$zephyr_samples" == "$zephyr_sample_count_expected" ]] || {
    printf 'error: Zephyr manifest sample count is not %s: %s\n' "$zephyr_sample_count_expected" "$zephyr_samples" >&2
    exit 1
}
[[ "$zephyr_start_gated" == "1" ]] || {
    printf 'error: matrix Zephyr image must be built with ZEPHYR_START_GATED=1\n' >&2
    exit 1
}
[[ "$zephyr_start_delay_ms" =~ ^[0-9]+$ ]] || {
    printf 'error: invalid Zephyr start delay in manifest: %s\n' "$zephyr_start_delay_ms" >&2
    exit 1
}
rg -a -F "PERIODIC LATENCY READY" "$zephyr_image" >/dev/null || {
    printf 'error: matrix Zephyr image does not contain the UART start gate\n' >&2
    exit 1
}

mkdir -p "$work" "$out_dir"
rm -f "$serial_sock" "$qmp_sock" "$run_log" "$build_log" \
    "$out_dir/cyclictest.csv" "$out_dir/linux-cpustat.csv" \
    "$out_dir/cyclictest-summary.txt" \
    "$out_dir/zephyr.csv" "$out_dir/zephyr-stats.txt" \
    "$out_dir/vmexit-before.txt" "$out_dir/vmexit-zephyr-after.txt" \
    "$out_dir/vmexit-after.txt" \
    "$out_dir/vmexit-stat.txt" "$out_dir/host-periodic-ticks.csv" \
    "$out_dir/linux-ftrace.txt" "$out_dir/linux-ftrace-latency.csv" \
    "$out_dir/linux-ftrace-latency-summary.txt" \
    "$out_dir/linux-timerlat.txt" "$out_dir/linux-timerlat-latency.csv" \
    "$out_dir/linux-timerlat-latency-summary.txt" \
    "$out_dir/meta.txt" "$out_dir/sha256sums" \
    "$out_dir/linux-qemu" "$out_dir/rt-linux-initramfs.cpio.gz" \
    "$out_dir/zephyr-periodic.bin" "$out_dir/zephyr-periodic.manifest" \
    "$out_dir/axvisor.bin" \
    "$out_dir/post-stall/query-status.json" \
    "$out_dir/post-stall/query-cpus-fast.json" \
    "$out_dir/post-stall/query-chardev.json" \
    "$out_dir/post-stall/info-registers-1.json" \
    "$out_dir/post-stall/info-registers-2.json" \
    "$out_dir/post-stall/qmp-error.txt" \
    "$out_dir/post-stall/serial-actions.txt" \
    "$out_dir/post-stall/serial-tail.bin"

cmdline="console=ttyAMA0 rdinit=/init devtmpfs.mount=1 loglevel=7 isolcpus=${rt_cpu} nohz_full=${rt_cpu} irqaffinity=${load_cpu} rt_scenario=${scenario} rt_cpu=${rt_cpu} rt_load_cpu=${load_cpu} rt_loops=${cyclictest_loops} rt_duration_sec=${duration_sec} rt_interval_us=${interval_us} rt_maxlat_us=${maxlat_us} rt_priority=${priority} rt_trace=${linux_trace} rt_trace_buffer_kb=${linux_trace_buffer_kb} rt_start_delay_sec=${start_delay_sec} rt_hold_after_complete=${hold_after_complete}"

python3 - "$linux_template" "$linux_config" "$cmdline" "$linux_image" \
    "$linux_virtual_timer_only" "$linux_wfi_policy" "$linux_phys_cpu_ids" <<'PY'
import sys
from pathlib import Path

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
cmdline = sys.argv[3]
linux_image = sys.argv[4]
virtual_timer_only = sys.argv[5] == "1"
wfi_policy = sys.argv[6]
phys_cpu_ids = sys.argv[7]
lines = source.read_text().splitlines()
cmdline_replaced = False
kernel_replaced = False
timer_contract_replaced = False
wfi_policy_replaced = False
for index, line in enumerate(lines):
    if line.startswith("cmdline = "):
        lines[index] = f'cmdline = "{cmdline}"'
        cmdline_replaced = True
    elif line.startswith("kernel_path = "):
        lines[index] = f'kernel_path = "{linux_image}"'
        kernel_replaced = True
    elif line.startswith("aarch64_virtual_timer_only = "):
        lines[index] = (
            "aarch64_virtual_timer_only = "
            + ("true" if virtual_timer_only else "false")
        )
        timer_contract_replaced = True
    elif line.startswith("aarch64_wfi_policy = "):
        lines[index] = f'aarch64_wfi_policy = "{wfi_policy}"'
        wfi_policy_replaced = True
    elif line.startswith("phys_cpu_ids = "):
        lines[index] = f"phys_cpu_ids = [{', '.join(phys_cpu_ids.split(','))}]"
if not cmdline_replaced:
    raise SystemExit("Linux VM template has no cmdline field")
if not kernel_replaced:
    raise SystemExit("Linux VM template has no kernel_path field")
# Older upstream AxVisor templates do not expose the realtime timer/WFI
# contract knobs. Keep those templates usable for an official baseline; the
# current tree still replaces both fields when present.
destination.write_text("\n".join(lines) + "\n")
PY

python3 - "$zephyr_template" "$zephyr_config" "$zephyr_guest_type" "$zephyr_entry" "$zephyr_image" "$zephyr_phys_cpu_ids" <<'PY'
import sys
from pathlib import Path

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
guest_type = sys.argv[3]
entry_point = sys.argv[4]
image_path = sys.argv[5]
phys_cpu_ids = sys.argv[6]
lines = source.read_text().splitlines()
found_guest_type = False
found_entry_point = False
found_kernel_path = False
for index, line in enumerate(lines):
    if line.startswith("guest_type = "):
        lines[index] = f'guest_type = "{guest_type}"'
        found_guest_type = True
    elif line.startswith("entry_point = "):
        lines[index] = f"entry_point = {entry_point}"
        found_entry_point = True
    elif line.startswith("kernel_path = "):
        lines[index] = f'kernel_path = "{image_path}"'
        found_kernel_path = True
    elif line.startswith("phys_cpu_ids = "):
        lines[index] = f"phys_cpu_ids = [{', '.join(phys_cpu_ids.split(','))}]"
if not found_guest_type:
    raise SystemExit("Zephyr VM template has no guest_type field")
if not found_entry_point:
    raise SystemExit("Zephyr VM template has no entry_point field")
if not found_kernel_path:
    raise SystemExit("Zephyr VM template has no kernel_path field")
destination.write_text("\n".join(lines) + "\n")
PY

host_bootargs=()
if [[ -n "$dedicated_cpus" ]]; then
    host_bootargs+=("dedicated_cpus=${dedicated_cpus}")
fi
if [[ -n "$burner_config" ]]; then
    host_bootargs+=("rt_burner=${burner_config}")
fi
host_append_line=""
if (( ${#host_bootargs[@]} > 0 )); then
    host_append_line="  \"-append\", \"${host_bootargs[*]}\","
fi
cat > "$qemu_config" <<EOF
args = [
  "-display", "none",
  "-monitor", "none",
  "-serial", "unix:${serial_sock},server,nowait",
  "-cpu", "cortex-a72",
  "-machine", "virt,virtualization=on,gic-version=3",
  "-smp", "4",
  "-m", "8g",
${host_append_line}
  "-qmp", "unix:${qmp_sock},server,nowait",
]
fail_regex = ["TASK2_ERROR=", "RT_CYCLICTEST_ERROR", "(?i)panic"]
success_regex = ["RT_CYCLICTEST_COMPLETE", "PERIODIC LATENCY COMPLETE"]
to_bin = true
uefi = false
EOF

if (( vmexit_diagnostics == 1 )); then
    vmexit_before_steps=$'cmd vmexit stat\nsleep 2'
    vmexit_after_zephyr_steps=$'cmd vmexit stat\nsleep 2'
    vmexit_final_steps=$'cmd vmexit stat\nsleep 2'
else
    vmexit_before_steps=""
    vmexit_after_zephyr_steps=""
    vmexit_final_steps=""
fi
if (( require_init_done == 1 )); then
    init_done_step="expect ${result_drain_timeout} RT_INIT_DONE scenario=${scenario}"
else
    init_done_step=""
fi
if [[ "$linux_trace" != "disabled" ]]; then
    trace_dump_steps="$(cat <<EOF
expect 30 RT_FTRACE_DUMP_READY encoding=gzip-base64
cmd dump
expect ${result_drain_timeout} RT_FTRACE_DUMP_END
EOF
)"
else
    trace_dump_steps=""
fi
if (( hold_after_complete == 1 )); then
    linux_hold_steps="$(cat <<EOF
expect ${result_drain_timeout} RT_CYCLICTEST_HOLD_READY
cmd release
expect ${result_drain_timeout} RT_CYCLICTEST_RELEASED
EOF
)"
else
    linux_hold_steps=""
fi

if [[ -n "$timer_storm_command" ]]; then
    zephyr_measurement_steps="$(cat <<EOF
send-until 60 0.5 g PERIODIC LATENCY START
detach
expect 10 \\[Axvisor\\] detached VM\\[2\\] console
cmd ${timer_storm_command}
expect 300 RT_TIMER_STORM_COMPLETE
cmd vm console 2
expect 10 Attached VM\\[2\\] console
expect ${zephyr_timeout} PERIODIC LATENCY COMPLETE samples=${zephyr_samples}
EOF
)"
else
    zephyr_measurement_steps="$(cat <<EOF
send-until 60 0.5 g PERIODIC LATENCY START
expect ${zephyr_timeout} PERIODIC LATENCY COMPLETE samples=${zephyr_samples}
EOF
)"
fi

cat > "$steps" <<EOF
expect 120 Default guest initialized
expect ${linux_start_timeout} RT_CYCLICTEST_START
expect 60 RT_PROGRESS uptime_s=
detach
expect 10 \[Axvisor\] detached VM\[1\] console
${vmexit_before_steps}
cmd vm console 2
expect 10 Attached VM\[2\] console
expect 10 PERIODIC LATENCY READY
${zephyr_measurement_steps}
detach
expect 10 \[Axvisor\] detached VM\[2\] console
${vmexit_after_zephyr_steps}
cmd vm console 1
expect 10 Attached VM\[1\] console
sleep 1
expect ${experiment_timeout} RT_CYCLICTEST_COMPLETE
${trace_dump_steps}
${init_done_step}
${linux_hold_steps}
detach-if-attached
expect 30 (\[Axvisor\] VM\[1\] stopped; returning to the management shell|VM\[1\] PSCI_SYSTEM_OFF)
${runtime_final_steps}
${vmexit_final_steps}
qmp-quit ${qmp_sock}
EOF

start_ns="$(date +%s%N)"
printf 'rt_experiment scenario=%s mode=%s start_ns=%s expected_runtime_sec=%s\n' \
    "$scenario" "$run_mode" "$start_ns" "$expected_runtime_sec" | tee "$run_log"

rootfs_args=()
if [[ -n "$rootfs_override" ]]; then
    rootfs_args=(--rootfs "$rootfs_override")
fi

cd "$source_root"
timeout "$timeout_sec" cargo xtask axvisor qemu \
    --config "$board_toml" \
    --qemu-config "$qemu_config" \
    --vmconfigs "$linux_config" \
    --vmconfigs "$zephyr_config" \
    "${rootfs_args[@]}" \
    > "$build_log" 2>&1 &
run_pid=$!

cleanup() {
    if [[ -n "${run_pid:-}" ]] && kill -0 "$run_pid" 2>/dev/null; then
        kill -TERM "$run_pid" 2>/dev/null || true
        wait "$run_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

wait_for_run_exit() {
    local deadline=$((SECONDS + $1))
    while kill -0 "$run_pid" 2>/dev/null; do
        (( SECONDS < deadline )) || return 1
        sleep 0.1
    done
}

socket_wait_deadline=$((SECONDS + 600))
while [[ ! -S "$serial_sock" ]]; do
    if ! kill -0 "$run_pid" 2>/dev/null; then
        printf 'error: Axvisor build/run exited before the serial socket appeared\n' >&2
        tail -80 "$build_log" >&2
        exit 1
    fi
    (( SECONDS < socket_wait_deadline )) || {
        printf 'error: serial socket did not appear within 600 seconds\n' >&2
        tail -80 "$build_log" >&2
        exit 1
    }
    sleep 0.05
done

python3 "$repo_root/scripts/test/net-dual-guest/serial_console.py" \
    "$serial_sock" "$run_log" --script "$steps" --verbose \
    --timestamp-lines --progress-regex 'RT_PROGRESS uptime_s=' \
    --progress-timeout "$progress_timeout" --qmp-sock "$qmp_sock" \
    --forensics-dir "$out_dir/post-stall" 2>> "$build_log"

forced_shutdown=0
qemu_shutdown="qmp"
if ! wait_for_run_exit "$qemu_exit_grace_sec"; then
    forced_shutdown=1
    qemu_shutdown="forced-term"
    printf 'warning: QEMU did not exit within %ss after completed sampling; terminating run PID %s\n' \
        "$qemu_exit_grace_sec" "$run_pid" | tee -a "$run_log" >&2
    kill -TERM "$run_pid" 2>/dev/null || true
    if ! wait_for_run_exit "$qemu_exit_grace_sec"; then
        qemu_shutdown="forced-kill"
        printf 'warning: run PID %s ignored TERM for %ss; sending KILL\n' \
            "$run_pid" "$qemu_exit_grace_sec" | tee -a "$run_log" >&2
        kill -KILL "$run_pid" 2>/dev/null || true
    fi
fi

set +e
wait "$run_pid"
run_status=$?
set -e
run_pid=""
if (( forced_shutdown == 1 )); then
    printf 'rt_experiment qemu_shutdown=%s run_status=%s\n' "$qemu_shutdown" "$run_status" | tee -a "$run_log"
elif (( run_status != 0 )); then
    printf 'error: cargo xtask/QEMU exited with status %s\n' "$run_status" >&2
    tail -80 "$build_log" >&2
    exit 1
else
    printf 'rt_experiment qemu_shutdown=qmp run_status=%s\n' "$run_status" | tee -a "$run_log"
fi

end_ns="$(date +%s%N)"
elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
printf 'rt_experiment end_ns=%s elapsed_ms=%s\n' "$end_ns" "$elapsed_ms" | tee -a "$run_log"

python3 - "$run_log" "$out_dir" "$vmexit_diagnostics" "$zephyr_samples" <<'PY'
import csv
import re
import sys
from pathlib import Path

log_path = Path(sys.argv[1])
out = Path(sys.argv[2])
vmexit_diagnostics = sys.argv[3] == "1"
expected_samples = int(sys.argv[4])
raw_log = log_path.read_text(errors="replace")
progress = re.findall(
    r"^\[host_monotonic_s=([0-9.]+)\].*RT_PROGRESS uptime_s=([0-9.]+)",
    raw_log,
    flags=re.MULTILINE,
)
if len(progress) >= 2:
    host_elapsed = float(progress[-1][0]) - float(progress[0][0])
    guest_elapsed = float(progress[-1][1]) - float(progress[0][1])
    ratio = guest_elapsed / host_elapsed if host_elapsed > 0 else 0.0
    (out / "progress.txt").write_text(
        f"markers={len(progress)}\n"
        f"host_elapsed_s={host_elapsed:.6f}\n"
        f"guest_elapsed_s={guest_elapsed:.6f}\n"
        f"guest_wall_ratio={ratio:.9f}\n"
    )
else:
    (out / "progress.txt").write_text(f"markers={len(progress)}\n")
log = re.sub(r"(?m)^\[host_monotonic_s=[0-9.]+\] ", "", raw_log)

if vmexit_diagnostics:
    starts = [match.start() for match in re.finditer(r"VM-exit counters per physical CPU", log)]
    if len(starts) < 3:
        raise SystemExit(f"expected at least three vmexit snapshots, found {len(starts)}")
    blocks = []
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(log)
        block = log[start:end]
        block = re.split(r"\n(?:\[Axvisor\]|\[driver\]|rt_experiment)", block, maxsplit=1)[0]
        blocks.append(block.rstrip() + "\n")
    (out / "vmexit-before.txt").write_text(blocks[0])
    (out / "vmexit-zephyr-after.txt").write_text(blocks[1])
    (out / "vmexit-after.txt").write_text(blocks[-1])
    (out / "vmexit-stat.txt").write_text(
        "\n--- before ---\n"
        + blocks[0]
        + "\n--- zephyr-after ---\n"
        + blocks[1]
        + "\n--- linux-final ---\n"
        + blocks[-1]
    )
else:
    for name in ("vmexit-before.txt", "vmexit-zephyr-after.txt", "vmexit-after.txt"):
        (out / name).write_text("diagnostics=disabled\n")
    (out / "vmexit-stat.txt").write_text("diagnostics=disabled\n")

header = "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns"
header_index = log.rfind(header)
complete_index = log.rfind(f"PERIODIC LATENCY COMPLETE samples={expected_samples}")
if header_index < 0 or complete_index < header_index:
    raise SystemExit("Zephyr periodic CSV block is missing")
rows = []
ansi_escape = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
for line in log[header_index + len(header):complete_index].splitlines():
    candidate = ansi_escape.sub("", line.strip())
    candidate = re.sub(r"^\[VM 2\] ", "", candidate)
    # serial_console.py prefixes every line when --timestamp-lines is enabled.
    # Strip that transport timestamp before validating the guest CSV record.
    candidate = re.sub(r"^\[host_monotonic_s=[0-9.]+\]\s*", "", candidate)
    if re.fullmatch(r"\d+,-?\d+,-?\d+,-?\d+,-?\d+", candidate):
        rows.append(candidate.split(","))
if len(rows) != expected_samples:
    raise SystemExit(f"expected {expected_samples} Zephyr samples, found {len(rows)}")
if [int(row[0]) for row in rows] != list(range(expected_samples)):
    raise SystemExit("Zephyr sample sequence is incomplete or out of order")
with (out / "zephyr.csv").open("w", newline="") as stream:
    writer = csv.writer(stream)
    writer.writerow(header.split(","))
    writer.writerows(rows)

cpu_rows = []
pattern = re.compile(
    r"RT_CPUSTAT sample=(\d+) cpu=cpu(\d+) user=(\d+) nice=(\d+) "
    r"system=(\d+) idle=(\d+) iowait=(\d+) irq=(\d+) softirq=(\d+) steal=(\d+)"
)
for match in pattern.finditer(log):
    cpu_rows.append(match.groups())
if not cpu_rows:
    raise SystemExit("Linux per-CPU load samples are missing")
with (out / "linux-cpustat.csv").open("w", newline="") as stream:
    writer = csv.writer(stream)
    writer.writerow(["sample", "cpu", "user", "nice", "system", "idle", "iowait", "irq", "softirq", "steal"])
    writer.writerows(cpu_rows)
PY

if [[ "$linux_trace" != "disabled" ]]; then
    if [[ "$linux_trace" == "events" ]]; then
        trace_output="$out_dir/linux-ftrace.txt"
    else
        trace_output="$out_dir/linux-timerlat.txt"
    fi
    python3 - "$run_log" "$trace_output" "$linux_trace" <<'PY'
import base64
import gzip
import re
import sys
from pathlib import Path

raw = Path(sys.argv[1]).read_text(errors="replace")
trace_mode = sys.argv[3]
log = re.sub(r"(?m)^\[host_monotonic_s=[0-9.]+\] ", "", raw)
log = re.sub(r"(?m)^\[VM 1\] ", "", log)
begin_marker = "RT_FTRACE_DUMP_BEGIN encoding=gzip-base64"
end_marker = "RT_FTRACE_DUMP_END"
begin = log.rfind(begin_marker)
end = log.find(end_marker, begin + len(begin_marker))
if begin < 0 or end < 0:
    raise SystemExit("Linux ftrace dump markers are missing")
payload = "".join(log[begin + len(begin_marker):end].split())
try:
    trace = gzip.decompress(base64.b64decode(payload, validate=True)).decode(
        errors="replace"
    )
except (ValueError, OSError) as error:
    raise SystemExit(f"Linux ftrace dump decoding failed: {error}") from error
if not trace.endswith("\n"):
    trace += "\n"
if trace_mode == "events":
    has_records = re.search(
        r"(?:irq_handler_entry|hrtimer_expire_entry|sched_wakeup|sched_switch):",
        trace,
    )
else:
    has_records = re.search(r"context\s+(?:irq|thread)\s+timer_latency", trace)
if not has_records:
    raise SystemExit(f"Linux {trace_mode} dump contains no requested records")
Path(sys.argv[2]).write_text(trace)
PY
fi
if [[ "$linux_trace" == "events" ]]; then
    python3 "$repo_root/scripts/test/rt-partition/linux-ftrace-latency.py" \
        "$out_dir/linux-ftrace.txt" \
        "$out_dir/linux-ftrace-latency-summary.txt" \
        --csv "$out_dir/linux-ftrace-latency.csv" \
        --kernel-prio "$((99 - priority))"
    printf 'disabled\n' > "$out_dir/linux-timerlat.txt"
    printf 'disabled\n' > "$out_dir/linux-timerlat-latency.csv"
    printf 'disabled\n' > "$out_dir/linux-timerlat-latency-summary.txt"
elif [[ "$linux_trace" == "timerlat" ]]; then
    python3 "$repo_root/scripts/test/rt-partition/linux-timerlat-latency.py" \
        "$out_dir/linux-timerlat.txt" \
        "$out_dir/linux-timerlat-latency-summary.txt" \
        --csv "$out_dir/linux-timerlat-latency.csv"
    printf 'disabled\n' > "$out_dir/linux-ftrace.txt"
    printf 'disabled\n' > "$out_dir/linux-ftrace-latency.csv"
    printf 'disabled\n' > "$out_dir/linux-ftrace-latency-summary.txt"
else
    printf 'disabled\n' > "$out_dir/linux-ftrace.txt"
    printf 'disabled\n' > "$out_dir/linux-ftrace-latency.csv"
    printf 'disabled\n' > "$out_dir/linux-ftrace-latency-summary.txt"
    printf 'disabled\n' > "$out_dir/linux-timerlat.txt"
    printf 'disabled\n' > "$out_dir/linux-timerlat-latency.csv"
    printf 'disabled\n' > "$out_dir/linux-timerlat-latency-summary.txt"
fi

python3 "$repo_root/scripts/test/rt-partition/cyclictest-hist-to-csv.py" \
    "$run_log" "$out_dir/cyclictest.csv" "$out_dir/cyclictest-summary.txt"
if (( vmexit_diagnostics == 1 )); then
    python3 "$repo_root/scripts/test/rt-partition/host-periodic-ticks-to-csv.py" \
        "$run_log" "$out_dir/host-periodic-ticks.csv" "${host_tick_args[@]}"
else
    printf 'diagnostics=disabled\n' > "$out_dir/host-periodic-ticks.csv"
fi
python3 "$repo_root/scripts/test/rt_latency_stats.py" \
    --tolerance-ns "$deadline_tolerance_ns" \
    "$out_dir/zephyr.csv" > "$out_dir/zephyr-stats.txt"

axvisor_bin="$(find "$source_root/target" -path '*/release/axvisor.bin' -type f -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
[[ -n "$axvisor_bin" && -f "$axvisor_bin" ]] || { printf 'error: built axvisor.bin not found\n' >&2; exit 1; }
strings "$axvisor_bin" | rg -F "$cmdline" >/dev/null || {
    printf 'error: generated guest cmdline is not embedded in axvisor.bin\n' >&2
    exit 1
}

python3 - "$run_log" "$out_dir/cyclictest.csv" "$out_dir/cyclictest-summary.txt" \
    "$scenario" "$run_mode" "$loops" "$duration_sec" "$elapsed_ms" \
    "$burner_config" "$require_init_done" "$linux_trace" "$hold_after_complete" \
    "$zephyr_samples" <<'PY'
import csv
import re
import sys
from decimal import Decimal
from pathlib import Path

log_path, csv_path, summary_path = map(Path, sys.argv[1:4])
scenario = sys.argv[4]
run_mode = sys.argv[5]
loops = int(sys.argv[6])
duration_sec = int(sys.argv[7])
elapsed_ms = int(sys.argv[8])
burner_config = sys.argv[9]
require_init_done = sys.argv[10] == "1"
linux_trace = sys.argv[11]
hold_after_complete = sys.argv[12] == "1"
expected_samples = int(sys.argv[13])
log = re.sub(
    r"(?m)^\[host_monotonic_s=[0-9.]+\] ",
    "",
    log_path.read_text(errors="replace"),
)
required = [
    f"RT_INIT scenario={scenario}",
    "RT_CYCLICTEST_START",
    "PERIODIC LATENCY READY",
    "PERIODIC LATENCY START",
    "RT_CYCLICTEST_TIMING_START",
    "RT_CYCLICTEST_TIMING_END",
    "# Min Latencies:",
    "# Histogram Overflows:",
    "RT_CYCLICTEST_COMPLETE",
    f"PERIODIC LATENCY COMPLETE samples={expected_samples}",
]
if require_init_done:
    required.append(f"RT_INIT_DONE scenario={scenario}")
if linux_trace != "disabled":
    required.extend(
        [
            f"RT_FTRACE_START mode={linux_trace}",
            "RT_FTRACE_DUMP_READY encoding=gzip-base64",
            "RT_FTRACE_DUMP_BEGIN encoding=gzip-base64",
            "RT_FTRACE_DUMP_END",
        ]
    )
if hold_after_complete:
    required.extend(["RT_CYCLICTEST_HOLD_READY", "RT_CYCLICTEST_RELEASED"])
if burner_config:
    required.append(f"RT_BURNER_READY cpu={burner_config.split(':', 1)[0]}")
missing = [marker for marker in required if marker not in log]
if missing:
    raise SystemExit("missing acceptance markers: " + ", ".join(missing))
if "RT_CYCLICTEST_ERROR" in log:
    raise SystemExit("cyclictest reported an execution error")
if "RT_FTRACE_ERROR" in log:
    raise SystemExit("Linux ftrace setup reported an execution error")
linux_start = log.index("RT_CYCLICTEST_START")
zephyr_start = log.index("PERIODIC LATENCY START")
zephyr_complete = log.index(f"PERIODIC LATENCY COMPLETE samples={expected_samples}")
linux_complete = log.index("RT_CYCLICTEST_COMPLETE")
if not linux_start < zephyr_start < zephyr_complete < linux_complete:
    raise SystemExit("Zephyr samples were not captured inside the Linux workload window")
if hold_after_complete:
    hold_ready = log.index("RT_CYCLICTEST_HOLD_READY")
    released = log.index("RT_CYCLICTEST_RELEASED")
    if not linux_complete < hold_ready < released:
        raise SystemExit("Linux hold/release markers are out of order")
start_matches = re.findall(r"RT_CYCLICTEST_TIMING_START uptime_s=([0-9]+(?:\.[0-9]+)?)", log)
end_matches = re.findall(r"RT_CYCLICTEST_TIMING_END uptime_s=([0-9]+(?:\.[0-9]+)?)", log)
if len(start_matches) != 1 or len(end_matches) != 1:
    raise SystemExit("expected exactly one cyclictest guest-uptime interval")
start_uptime_s = Decimal(start_matches[0])
end_uptime_s = Decimal(end_matches[0])
guest_elapsed_s = end_uptime_s - start_uptime_s
if guest_elapsed_s <= 0:
    raise SystemExit("cyclictest guest-uptime interval is not positive")
with csv_path.open(newline="") as stream:
    bucket_samples = sum(int(row["count"]) for row in csv.DictReader(stream))
summary = {}
for line in summary_path.read_text().splitlines():
    name, value = line.split("=", 1)
    summary[name] = int(value)
if summary["bucket_samples"] != bucket_samples:
    raise SystemExit("cyclictest summary does not match histogram buckets")
if summary["total_samples"] != bucket_samples + summary["overflow_samples"]:
    raise SystemExit("cyclictest total does not include all histogram overflows")
if run_mode == "loops" and summary["total_samples"] != loops:
    raise SystemExit(
        f"cyclictest sample count mismatch: {summary['total_samples']} != requested {loops}"
    )
if run_mode == "duration" and duration_sec <= 0:
    raise SystemExit("duration mode requires a positive duration")
if summary["total_samples"] <= 0:
    raise SystemExit("cyclictest histogram has no samples")
if run_mode == "duration":
    minimum_guest_elapsed_s = Decimal(duration_sec) * Decimal("0.9")
    if guest_elapsed_s < minimum_guest_elapsed_s:
        raise SystemExit(
            "cyclictest covered too little guest time: "
            f"{guest_elapsed_s} s < {minimum_guest_elapsed_s} s"
        )
print(
    f"accepted scenario={scenario} mode={run_mode} requested_loops={loops} "
    f"duration_sec={duration_sec} total_samples={summary['total_samples']} "
    f"bucket_samples={bucket_samples} overflow_samples={summary['overflow_samples']} "
    f"guest_elapsed_s={guest_elapsed_s} elapsed_ms={elapsed_ms}"
)
PY

{
    printf 'scenario=%s\n' "$scenario"
    printf 'git_commit=%s\n' "$(git -C "$source_root" rev-parse HEAD)"
    printf 'tracked_dirty=%s\n' "$tracked_dirty"
    printf 'untracked_count=%s\n' "$untracked_count"
    printf 'allow_dirty=%s\n' "$allow_dirty"
    printf 'source_root=%s\n' "$source_root"
    printf 'run_mode=%s\n' "$run_mode"
    printf 'requested_loops=%s\n' "$loops"
    printf 'duration_sec=%s\n' "$duration_sec"
    printf 'cyclictest_loops=%s\n' "$cyclictest_loops"
    printf 'interval_us=%s\n' "$interval_us"
    printf 'deadline_tolerance_ns=%s\n' "$deadline_tolerance_ns"
    printf 'expected_runtime_sec=%s\n' "$expected_runtime_sec"
    printf 'tcg_runtime_scale=%s\n' "$runtime_scale"
    printf 'runtime_scale_source=%s\n' "$runtime_scale_source"
    printf 'expected_wall_runtime_sec=%s\n' "$expected_wall_runtime_sec"
    printf 'elapsed_ms=%s\n' "$elapsed_ms"
    printf 'dedicated_cpus=%s\n' "${dedicated_cpus:-none}"
    printf 'rt_burner=%s\n' "${burner_config:-disabled}"
    printf 'vmexit_diagnostics=%s\n' "$vmexit_diagnostics"
    printf 'runtime_diagnostics=%s\n' "$runtime_diagnostics"
    printf 'require_init_done=%s\n' "$require_init_done"
    printf 'hold_after_complete=%s\n' "$hold_after_complete"
    printf 'rootfs_override=%s\n' "${rootfs_override:-none}"
    printf 'linux_kernel=%s\n' "$linux_image"
    printf 'board_config=%s\n' "$board_toml"
    printf 'zephyr_guest_type=%s\n' "$zephyr_guest_type"
    printf 'zephyr_start_delay_ms=%s\n' "$zephyr_start_delay_ms"
    printf 'zephyr_sample_count=%s\n' "$zephyr_samples"
    printf 'progress_timeout_sec=%s\n' "$progress_timeout"
    printf 'zephyr_timeout_sec=%s\n' "$zephyr_timeout"
    printf 'result_drain_timeout_sec=%s\n' "$result_drain_timeout"
    printf 'qemu_exit_grace_sec=%s\n' "$qemu_exit_grace_sec"
    printf 'qemu_shutdown=%s\n' "$qemu_shutdown"
    printf 'timestamp_format=host_monotonic_s=seconds\n'
    printf 'realtime_trace=%s\n' "$linux_trace"
    printf 'linux_trace_buffer_kb=%s\n' "$linux_trace_buffer_kb"
    printf 'linux_virtual_timer_only=%s\n' "$linux_virtual_timer_only"
    printf 'linux_wfi_policy=%s\n' "$linux_wfi_policy"
    printf 'host_periodic_tick_policy=%s\n' "${host_tick_args[*]:-record-only}"
    printf 'linux_rt_cpu=%s\n' "$rt_cpu"
    printf 'linux_load_cpu=%s\n' "$load_cpu"
    printf 'linux_phys_cpu_ids=%s\n' "$linux_phys_cpu_ids"
    printf 'zephyr_phys_cpu_ids=%s\n' "$zephyr_phys_cpu_ids"
    printf 'guest_cmdline=%s\n' "$cmdline"
    printf 'axvisor_bin=%s\n' "$axvisor_bin"
    printf 'build_command=cd %s && cargo xtask axvisor qemu --config %s --qemu-config %s --vmconfigs %s --vmconfigs %s\n' \
        "$source_root" \
        "$board_toml" "$qemu_config" "$linux_config" "$zephyr_config"
} > "$out_dir/meta.txt"

cp "$linux_config" "$out_dir/linux.toml"
cp "$zephyr_config" "$out_dir/zephyr.toml"
cp "$qemu_config" "$out_dir/qemu.toml"
cp "$linux_image" "$out_dir/linux-qemu"
cp "$work/rt-linux-initramfs.cpio.gz" "$out_dir/"
cp "$zephyr_image" "$out_dir/zephyr-periodic.bin"
cp "$zephyr_manifest" "$out_dir/zephyr-periodic.manifest"
cp "$axvisor_bin" "$out_dir/"
(
    cd "$out_dir"
    sha256sum \
        build-qemu.log run.log cyclictest.csv cyclictest-summary.txt \
        linux-cpustat.csv zephyr.csv zephyr-stats.txt progress.txt \
        linux-ftrace.txt linux-ftrace-latency.csv \
        linux-ftrace-latency-summary.txt \
        linux-timerlat.txt linux-timerlat-latency.csv \
        linux-timerlat-latency-summary.txt \
        vmexit-before.txt vmexit-zephyr-after.txt vmexit-after.txt vmexit-stat.txt \
        host-periodic-ticks.csv \
        linux.toml zephyr.toml qemu.toml meta.txt \
        linux-qemu rt-linux-initramfs.cpio.gz zephyr-periodic.bin \
        zephyr-periodic.manifest axvisor.bin > sha256sums
)

printf 'accepted: %s\n' "$out_dir"
