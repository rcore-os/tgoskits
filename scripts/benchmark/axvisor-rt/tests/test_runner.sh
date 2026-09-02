#!/usr/bin/env bash
set -euo pipefail

benchmark_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
runner="$benchmark_dir/run.sh"
probe_source="$benchmark_dir/guest/axvisor_rt_probe.c"
temporary_dir="$(mktemp -d)"
probe_pid=""

cleanup() {
    if [[ -n "$probe_pid" ]] && kill -0 "$probe_pid" 2>/dev/null; then
        kill -TERM "$probe_pid" 2>/dev/null || true
        wait "$probe_pid" 2>/dev/null || true
    fi
    rm -rf -- "$temporary_dir"
}
trap cleanup EXIT

fail() {
    echo "test_runner: $*" >&2
    exit 1
}

expect_option_error() {
    local expected="$1"
    shift
    local output
    local status
    set +e
    output=$("$runner" --rootfs "$probe_source" "$@" 2>&1)
    status=$?
    set -e
    [[ "$status" -eq 2 ]] || fail "expected status 2, got $status: $output"
    [[ "$output" == *"$expected"* ]] || fail "missing error '$expected': $output"
}

bash -n "$runner"
grep -q 'AXVISOR_RT_GUEST_CPUS schema=1 online=' "$runner" || \
    fail "runner must record the online guest CPU count for every workload"
grep -q 'AXVISOR_RT_CPUSTAT schema=1 phase=' "$runner" || \
    fail "runner must record per-CPU load snapshots around every workload"
grep -q 'emit_cpu_stats start' "$runner" || \
    fail "runner must record the initial per-CPU load snapshot"
grep -q 'emit_cpu_stats end' "$runner" || \
    fail "runner must record the final per-CPU load snapshot"
grep -q 'workload_log=' "$runner" || \
    fail "cpu-stress stdout must be captured for lifecycle verification"
grep -q 'AXVISOR_RT_WORKLOAD_READY schema=1 kind=cpu-stress' "$runner" || \
    fail "runner must verify and forward the probe READY marker"
grep -q 'AXVISOR_RT_WORKLOAD_STOPPED schema=1 kind=cpu-stress' "$runner" || \
    fail "runner must verify and forward the probe STOPPED marker"
grep -q 'require_workload_running "after measurements"' "$runner" || \
    fail "runner must prove cpu-stress remains alive through the measured window"
guest_cpu_check_line="$(grep -n 'AXVISOR_RT_GUEST_CPUS schema=1 online=' "$runner" | head -n 1 | cut -d: -f1)"
workload_case_line="$(grep -n '^case "\$workload_mode" in' "$runner" | tail -n 1 | cut -d: -f1)"
[[ -n "$guest_cpu_check_line" && -n "$workload_case_line" ]] || \
    fail "could not locate generated guest CPU/workload gates"
((guest_cpu_check_line < workload_case_line)) || \
    fail "online guest CPU validation must precede every workload branch"
metadata_start_line="$(grep -n 'record_metadata.py" start' "$runner" | head -n 1 | cut -d: -f1)"
qemu_line="$(grep -n 'cargo xtask axvisor qemu' "$runner" | head -n 1 | cut -d: -f1)"
metadata_finalize_line="$(grep -n 'record_metadata.py" finalize' "$runner" | head -n 1 | cut -d: -f1)"
[[ -n "$metadata_start_line" && -n "$qemu_line" && -n "$metadata_finalize_line" ]] || \
    fail "runner must have explicit metadata start/finalize phases"
((metadata_start_line < qemu_line && qemu_line < metadata_finalize_line)) || \
    fail "immutable provenance must be recorded before QEMU and finalized afterwards"
expect_option_error "--workload must be" --workload unknown
expect_option_error "external workload labels" --workload external:bad/label
expect_option_error "cpu-stress requires --cpu 0" --workload cpu-stress --cpu 1
expect_option_error "--profile must be" --profile unknown

cc -std=c11 -O2 -Wall -Wextra -Werror -pthread \
    "$probe_source" -o "$temporary_dir/axvisor-rt-probe"

allowed_list="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' /proc/self/status)"
first_range="${allowed_list%%,*}"
test_cpu="${first_range%%-*}"
[[ "$test_cpu" =~ ^[0-9]+$ ]] || fail "could not select a test CPU from $allowed_list"

probe_log="$temporary_dir/cpu-stress.log"
"$temporary_dir/axvisor-rt-probe" \
    --metric cpu_stress \
    --cpu "$test_cpu" \
    --fifo-priority 0 >"$probe_log" &
probe_pid=$!

for _ in {1..100}; do
    if grep -q '^AXVISOR_RT_WORKLOAD_READY ' "$probe_log"; then
        break
    fi
    kill -0 "$probe_pid" 2>/dev/null || fail "CPU stress probe exited before ready"
    sleep 0.01
done
grep -q '^AXVISOR_RT_WORKLOAD_READY ' "$probe_log" || fail "missing ready marker"

actual_affinity="$(sed -n 's/^Cpus_allowed_list:[[:space:]]*//p' "/proc/$probe_pid/status")"
[[ "$actual_affinity" == "$test_cpu" ]] || \
    fail "expected CPU affinity $test_cpu, got $actual_affinity"

kill -TERM "$probe_pid"
wait "$probe_pid"
probe_pid=""
grep -q '^AXVISOR_RT_WORKLOAD_STOPPED ' "$probe_log" || fail "missing stopped marker"

echo "test_runner: PASS"
