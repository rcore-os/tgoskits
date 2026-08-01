#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(git -C "$script_dir" rev-parse --show-toplevel)"

rootfs=""
output_dir=""
probe=""
compiler="${CC:-aarch64-linux-musl-gcc}"
qemu_binary="qemu-system-aarch64"
iterations=10000
warmup=100
period_us=1000
guest_cpu=0
fifo_priority=80
workload="idle"
profile="partitioned"

usage() {
    cat <<'EOF'
usage: run.sh --rootfs PATH [options]

Options:
  --output DIR             New result directory (default: tmp/axvisor-rt/<UTC>)
  --probe PATH             Prebuilt static AArch64 probe; skips compilation
  --cc COMMAND             Cross C compiler (default: aarch64-linux-musl-gcc)
  --iterations N           Recorded samples per metric (default: 10000)
  --warmup N               Unrecorded warmup iterations (default: 100)
  --period-us N            Periodic/timerfd interval (default: 1000)
  --cpu N                  Guest logical CPU for probes (default: 0)
  --fifo-priority N        SCHED_FIFO priority, or 0 for SCHED_OTHER (default: 80)
  --workload MODE          idle, cpu-stress, or external:<safe-label> (default: idle)
  --profile PROFILE        partitioned or shared (default: partitioned)
EOF
}

while (($# > 0)); do
    case "$1" in
        --rootfs) rootfs="${2:-}"; shift 2 ;;
        --output) output_dir="${2:-}"; shift 2 ;;
        --probe) probe="${2:-}"; shift 2 ;;
        --cc) compiler="${2:-}"; shift 2 ;;
        --iterations) iterations="${2:-}"; shift 2 ;;
        --warmup) warmup="${2:-}"; shift 2 ;;
        --period-us) period_us="${2:-}"; shift 2 ;;
        --cpu) guest_cpu="${2:-}"; shift 2 ;;
        --fifo-priority) fifo_priority="${2:-}"; shift 2 ;;
        --workload) workload="${2:-}"; shift 2 ;;
        --profile) profile="${2:-}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ -n "$rootfs" && -d "$rootfs" ]]; then
    managed_image="$rootfs/$(basename -- "$rootfs")"
    if [[ -f "$managed_image" ]]; then
        rootfs="$managed_image"
    fi
fi
if [[ -z "$rootfs" || ! -f "$rootfs" ]]; then
    echo "--rootfs must name an existing image" >&2
    exit 2
fi
for value in "$iterations" "$warmup" "$period_us" "$guest_cpu" "$fifo_priority"; do
    if [[ ! "$value" =~ ^[0-9]+$ ]]; then
        echo "numeric benchmark options must be nonnegative integers" >&2
        exit 2
    fi
done
if ((iterations == 0 || period_us == 0 || fifo_priority > 98)); then
    echo "iterations/period-us must be positive and fifo-priority must be at most 98" >&2
    exit 2
fi

case "$workload" in
    idle)
        workload_mode="idle"
        ;;
    cpu-stress)
        workload_mode="cpu-stress"
        if ((guest_cpu != 0)); then
            echo "cpu-stress requires --cpu 0; its busy loop is pinned to guest CPU 1" >&2
            exit 2
        fi
        ;;
    external:[A-Za-z0-9]* )
        if [[ ! "$workload" =~ ^external:[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
            echo "external workload labels may contain only letters, digits, dot, underscore, and hyphen" >&2
            exit 2
        fi
        workload_mode="external"
        ;;
    *)
        echo "--workload must be idle, cpu-stress, or external:<safe-label>" >&2
        exit 2
        ;;
esac

case "$profile" in
    partitioned)
        axvisor_config="docs/realtime/axvisor-qemu-aarch64-partition.toml"
        ;;
    shared)
        axvisor_config="docs/realtime/axvisor-qemu-aarch64-shared.toml"
        ;;
    *)
        echo "--profile must be partitioned or shared" >&2
        exit 2
        ;;
esac

for command in cargo debugfs git python3 "$qemu_binary"; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command not found: $command" >&2
        exit 2
    fi
done

run_id="$(date -u +%Y%m%dT%H%M%SZ)"
if [[ -z "$output_dir" ]]; then
    output_dir="$workspace_root/tmp/axvisor-rt/$run_id"
fi
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
for artifact in metadata.json raw-console.log summary.json rootfs.img; do
    if [[ -e "$output_dir/$artifact" ]]; then
        echo "refusing to overwrite $output_dir/$artifact" >&2
        exit 2
    fi
done

probe_path="$output_dir/axvisor-rt-probe"
if [[ -n "$probe" ]]; then
    if [[ ! -f "$probe" ]]; then
        echo "--probe must name an existing file" >&2
        exit 2
    fi
    cp "$probe" "$probe_path"
else
    if ! command -v "$compiler" >/dev/null 2>&1; then
        echo "cross compiler not found: $compiler (use --cc or --probe)" >&2
        exit 2
    fi
    "$compiler" -std=c11 -O2 -Wall -Wextra -Werror -static -pthread \
        "$script_dir/guest/axvisor_rt_probe.c" -o "$probe_path"
fi

guest_runner="$output_dir/axvisor-rt-run"
cat >"$guest_runner" <<EOF
#!/bin/sh
set -eu
workload_pid=""
workload_cpu=1
workload_log="/tmp/axvisor-rt-cpu-stress.log"

show_workload_log() {
    if [ -r "\$workload_log" ]; then
        sed 's/^/cpu-stress: /' "\$workload_log" >&2
    fi
}

require_exact_workload_marker() {
    marker_name="\$1"
    expected="\$2"
    count="\$(grep -Fxc "\$expected" "\$workload_log" 2>/dev/null || true)"
    if [ "\$count" != 1 ]; then
        echo "cpu-stress must emit exactly one \$marker_name marker; found \${count:-0}" >&2
        show_workload_log
        return 1
    fi
    echo "\$expected"
}

require_workload_running() {
    phase="\$1"
    if [ -z "\$workload_pid" ] || ! kill -0 "\$workload_pid" 2>/dev/null; then
        echo "cpu-stress exited early \$phase" >&2
        show_workload_log
        return 1
    fi
    state="\$(sed -n 's/^State:[[:space:]]*\([A-Z]\).*/\1/p' "/proc/\$workload_pid/status")"
    case "\$state" in
        ''|Z|X)
            echo "cpu-stress process \$workload_pid is not live (state=\${state:-missing}) \$phase" >&2
            show_workload_log
            return 1
            ;;
    esac
    affinity="\$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "/proc/\$workload_pid/status")"
    if [ "\$affinity" != "\$workload_cpu" ]; then
        echo "cpu-stress process \$workload_pid has unexpected affinity \$affinity \$phase" >&2
        show_workload_log
        return 1
    fi
}

wait_for_workload_ready() {
    expected="AXVISOR_RT_WORKLOAD_READY schema=1 kind=cpu-stress pid=\$workload_pid cpu=\$workload_cpu"
    attempt=0
    while [ "\$attempt" -lt 100 ]; do
        require_workload_running "while waiting for READY" || return 1
        if grep -Fqx "\$expected" "\$workload_log" 2>/dev/null; then
            break
        fi
        attempt=\$((attempt + 1))
        sleep 0.01
    done
    require_exact_workload_marker READY "\$expected"
    if grep -q '^AXVISOR_RT_WORKLOAD_STOPPED ' "\$workload_log" 2>/dev/null; then
        echo "cpu-stress emitted STOPPED before measurement" >&2
        show_workload_log
        return 1
    fi
}

cleanup_workload() {
    if [ -z "\$workload_pid" ]; then
        return 0
    fi
    pid="\$workload_pid"
    if ! require_workload_running "before explicit termination"; then
        workload_pid=""
        echo "cpu-stress process \$pid exited before explicit termination" >&2
        show_workload_log
        wait "\$pid" 2>/dev/null || true
        return 1
    fi
    workload_pid=""
    if ! kill -TERM "\$pid" 2>/dev/null; then
        echo "failed to terminate cpu-stress process \$pid" >&2
        show_workload_log
        return 1
    fi
    if wait "\$pid"; then
        stopped="AXVISOR_RT_WORKLOAD_STOPPED schema=1 kind=cpu-stress pid=\$pid cpu=\$workload_cpu"
        if ! require_exact_workload_marker STOPPED "\$stopped"; then
            return 1
        fi
        echo "AXVISOR_RT_WORKLOAD_CLEANED schema=1 kind=cpu-stress pid=\$pid status=0"
        return 0
    fi
    cleanup_status=\$?
    echo "AXVISOR_RT_WORKLOAD_CLEANUP_FAILED schema=1 kind=cpu-stress pid=\$pid status=\$cleanup_status"
    return "\$cleanup_status"
}

emit_cpu_stats() {
    phase="\$1"
    while read -r cpu user nice system idle iowait irq softirq steal _; do
        case "\$cpu" in
            cpu[0-9]*)
                cpu_id="\${cpu#cpu}"
                case "\$cpu_id" in
                    ''|*[!0-9]*) continue ;;
                esac
                if [ "\$cpu_id" -lt "\$online" ]; then
                    echo "AXVISOR_RT_CPUSTAT schema=1 phase=\$phase cpu=\$cpu_id user=\${user:-0} nice=\${nice:-0} system=\${system:-0} idle=\${idle:-0} iowait=\${iowait:-0} irq=\${irq:-0} softirq=\${softirq:-0} steal=\${steal:-0}"
                fi
                ;;
        esac
    done < /proc/stat
}

on_exit() {
    status=\$?
    trap - EXIT
    set +e
    cleanup_workload
    cleanup_status=\$?
    if [ "\$status" -eq 0 ] && [ "\$cleanup_status" -ne 0 ]; then
        status=\$cleanup_status
    fi
    if [ "\$status" -ne 0 ]; then
        echo "AXVISOR_RT_RUN_FAILED status=\$status"
    fi
    exit "\$status"
}
trap on_exit EXIT

echo "AXVISOR_RT_RUN_START"
online="\$(getconf _NPROCESSORS_ONLN)"
if [ "\$online" -ne 2 ]; then
    echo "benchmark requires exactly two online guest CPUs; found \$online" >&2
    exit 1
fi
echo "AXVISOR_RT_GUEST_CPUS schema=1 online=\$online"
mount -t proc proc /proc 2>/dev/null || true
if [ ! -r /proc/stat ]; then
    echo "guest /proc/stat is unavailable" >&2
    exit 1
fi
case "$workload_mode" in
    cpu-stress)
        : >"\$workload_log"
        /axvisor-rt-probe --metric cpu_stress --cpu "\$workload_cpu" --fifo-priority 0 \
            >"\$workload_log" 2>&1 &
        workload_pid=\$!
        wait_for_workload_ready
        require_workload_running "before measurements"
        echo "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=cpu-stress pid=\$workload_pid cpu=\$workload_cpu affinity=\$workload_cpu"
        ;;
    external)
        echo "AXVISOR_RT_WORKLOAD_EXTERNAL schema=1 verification=caller label=$workload"
        ;;
    idle)
        echo "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=idle"
        ;;
esac
emit_cpu_stats start
for metric in periodic_jitter dispatch_latency emulated_irq_response; do
    /axvisor-rt-probe \
        --metric "\$metric" \
        --iterations "$iterations" \
        --warmup "$warmup" \
        --period-us "$period_us" \
        --cpu "$guest_cpu" \
        --fifo-priority "$fifo_priority"
done
emit_cpu_stats end
if [ "$workload_mode" = "cpu-stress" ]; then
    require_workload_running "after measurements"
fi
cleanup_workload
echo "AXVISOR_RT_RUN_COMPLETE"
EOF
chmod 0755 "$guest_runner" "$probe_path"

run_rootfs="$output_dir/rootfs.img"
cp --reflink=auto "$rootfs" "$run_rootfs"
debugfs -w -R "write \"$probe_path\" /axvisor-rt-probe" "$run_rootfs"
debugfs -w -R "set_inode_field /axvisor-rt-probe mode 0100755" "$run_rootfs"
debugfs -w -R "write \"$guest_runner\" /axvisor-rt-run" "$run_rootfs"
debugfs -w -R "set_inode_field /axvisor-rt-run mode 0100755" "$run_rootfs"
sync

raw_log="$output_dir/raw-console.log"
metadata="$output_dir/metadata.json"
summary="$output_dir/summary.json"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

python3 "$script_dir/record_metadata.py" start \
    --output "$metadata" \
    --workspace "$workspace_root" \
    --run-id "$run_id" \
    --started-at "$started_at" \
    --qemu-binary "$qemu_binary" \
    --rootfs "$rootfs" \
    --injected-rootfs "$run_rootfs" \
    --probe "$probe_path" \
    --guest-runner "$guest_runner" \
    --iterations "$iterations" \
    --warmup "$warmup" \
    --period-us "$period_us" \
    --cpu "$guest_cpu" \
    --fifo-priority "$fifo_priority" \
    --workload "$workload" \
    --profile "$profile"

if [[ -d /usr/lib/x86_64-linux-gnu/pkgconfig ]]; then
    export PKG_CONFIG_PATH="/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi

set +e
(
    cd "$workspace_root"
    cargo xtask axvisor qemu --arch aarch64 \
        --config "$axvisor_config" \
        --qemu-config scripts/benchmark/axvisor-rt/qemu-aarch64.toml \
        --rootfs "$run_rootfs"
) 2>&1 | tee "$raw_log"
qemu_status="${PIPESTATUS[0]}"
set -e

finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
python3 "$script_dir/record_metadata.py" finalize \
    --metadata "$metadata" \
    --finished-at "$finished_at" \
    --exit-code "$qemu_status" \
    --raw-log "$raw_log"

if ((qemu_status != 0)); then
    echo "QEMU capture failed; partial log and metadata are in $output_dir" >&2
    exit "$qemu_status"
fi

python3 "$script_dir/analyze.py" "$raw_log" --metadata "$metadata" --output "$summary"
echo "AxVisor RT capture complete: $output_dir"
