#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
iterations="${RT_TIMER_LOCK_AB_ITERATIONS:-5000}"
expiry_samples="${RT_TIMER_LOCK_AB_EXPIRY_SAMPLES:-64}"
expiry_delay_us="${RT_TIMER_LOCK_AB_EXPIRY_DELAY_US:-100000}"
timeout_sec="${RT_TIMER_LOCK_AB_TIMEOUT_SEC:-600}"
output_root="${RT_TIMER_LOCK_AB_OUTPUT_ROOT:-${repo_root}/results/task1/percpu-timer-wheel/formal-host-lock-ab}"
work_root="${repo_root}/tmp/rt-timer-wheel-lock-ab"
console_driver="${repo_root}/scripts/test/net-dual-guest/serial_console.py"
summary_tool="${repo_root}/scripts/test/rt-partition/summarize-timer-wheel-ab.py"
global_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt-global-timer.toml"
percpu_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
storm_command="rt timer-storm --cpus 0xe --iterations ${iterations} --expiry-samples ${expiry_samples} --expiry-delay-us ${expiry_delay_us}"
run_pid=""

for value in "$iterations" "$expiry_samples" "$expiry_delay_us" "$timeout_sec"; do
    [[ "$value" =~ ^[0-9]+$ ]] || {
        printf 'error: timer-lock A/B option is not numeric: %s\n' "$value" >&2
        exit 2
    }
done
(( iterations > 0 && expiry_samples > 0 && expiry_delay_us > 0 && timeout_sec > 0 )) || {
    printf 'error: timer-lock A/B numeric options must be positive\n' >&2
    exit 2
}
[[ ! -e "$output_root" ]] || {
    printf 'error: RT_TIMER_LOCK_AB_OUTPUT_ROOT already exists: %s\n' "$output_root" >&2
    exit 2
}

cleanup() {
    if [[ -n "$run_pid" ]] && kill -0 "$run_pid" 2>/dev/null; then
        kill -TERM "$run_pid" 2>/dev/null || true
        wait "$run_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

mkdir -p "$output_root" "$work_root"

run_case() {
    local name="$1"
    local implementation="$2"
    local board="$3"
    local out_dir="${output_root}/${name}"
    local work_dir="${work_root}/${name}"
    local serial_sock="${work_dir}/serial.sock"
    local qmp_sock="${work_dir}/qmp.sock"
    local qemu_config="${work_dir}/qemu.toml"
    local steps="${work_dir}/steps.txt"
    local run_log="${out_dir}/run.log"
    local build_log="${out_dir}/build-qemu.log"
    local storm_log="${out_dir}/timer-storm.txt"
    local axvisor_bin="${repo_root}/target/aarch64-unknown-linux-musl/release/axvisor.bin"

    mkdir -p "$out_dir" "$work_dir"
    rm -f "$serial_sock" "$qmp_sock" "$run_log" "$build_log"

    cat > "$qemu_config" <<EOF
args = [
  "-display", "none",
  "-monitor", "none",
  "-serial", "unix:${serial_sock},server,nowait",
  "-cpu", "cortex-a72",
  "-machine", "virt,virtualization=on,gic-version=3",
  "-smp", "4",
  "-m", "8g",
  "-qmp", "unix:${qmp_sock},server,nowait",
]
fail_regex = ["RT_TIMER_STORM_ERROR", "(?i)panic"]
success_regex = ["RT_TIMER_STORM_COMPLETE implementation=${implementation}"]
to_bin = true
uefi = false
EOF

    cat > "$steps" <<EOF
expect 120 Welcome to AxVisor Shell!
cmd ${storm_command}
expect 120 RT_TIMER_STORM_COMPLETE implementation=${implementation}
cmd rt stat
sleep 1
qmp-quit ${qmp_sock}
EOF

    printf 'timer_lock_ab case=%s implementation=%s host_only=1 smp=4 cpu_mask=0xe\n' \
        "$name" "$implementation" | tee "$run_log"

    timeout "$timeout_sec" cargo xtask axvisor qemu \
        --config "$board" \
        --qemu-config "$qemu_config" \
        > "$build_log" 2>&1 &
    run_pid=$!

    local socket_deadline=$((SECONDS + 300))
    while [[ ! -S "$serial_sock" ]]; do
        if ! kill -0 "$run_pid" 2>/dev/null; then
            printf 'error: %s build/run exited before the serial socket appeared\n' "$name" >&2
            tail -80 "$build_log" >&2
            exit 1
        fi
        (( SECONDS < socket_deadline )) || {
            printf 'error: %s serial socket did not appear within 300 seconds\n' "$name" >&2
            tail -80 "$build_log" >&2
            exit 1
        }
        sleep 0.05
    done

    python3 "$console_driver" "$serial_sock" "$run_log" \
        --script "$steps" --verbose --timestamp-lines \
        --qmp-sock "$qmp_sock" --forensics-dir "$out_dir/post-stall" \
        2>> "$build_log"

    set +e
    wait "$run_pid"
    local run_status=$?
    set -e
    run_pid=""
    (( run_status == 0 )) || {
        printf 'error: %s cargo xtask/QEMU exited with status %s\n' "$name" "$run_status" >&2
        tail -80 "$build_log" >&2
        exit 1
    }

    grep 'RT_TIMER_STORM_' "$run_log" > "$storm_log"
    python3 - "$storm_log" "$implementation" "$iterations" "$expiry_samples" <<'PY'
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
implementation = sys.argv[2]
iterations = int(sys.argv[3])
expiry_samples_per_worker = int(sys.argv[4])
text = path.read_text(errors="replace")

required = [
    "RT_TIMER_STORM_START",
    f"RT_TIMER_STORM_RESULT implementation={implementation}",
    "RT_TIMER_STORM_LOCK",
    "RT_TIMER_STORM_EXPIRY",
    f"RT_TIMER_STORM_COMPLETE implementation={implementation}",
]
missing = [marker for marker in required if marker not in text]
if missing:
    raise SystemExit("missing timer-storm markers: " + ", ".join(missing))

result = re.search(
    r"RT_TIMER_STORM_RESULT .*workers=(\d+) iterations_per_worker=(\d+) "
    r"register_cancel_pairs=(\d+) elapsed_ns=(\d+) pairs_per_second=(\d+)",
    text,
)
expiry = re.search(r"RT_TIMER_STORM_EXPIRY samples=(\d+) completed=(\d+)", text)
if result is None or expiry is None:
    raise SystemExit("timer-storm result fields are incomplete")
workers, observed_iterations, pairs, elapsed_ns, pairs_per_second = map(
    int, result.groups()
)
samples, completed = map(int, expiry.groups())
if workers != 3 or observed_iterations != iterations:
    raise SystemExit("timer-storm topology or iteration count changed")
if pairs != workers * iterations or elapsed_ns <= 0 or pairs_per_second <= 0:
    raise SystemExit("timer-storm register/cancel accounting is invalid")
if samples != workers * expiry_samples_per_worker or completed != samples:
    raise SystemExit("timer-storm expiry samples are incomplete")
PY

    [[ -f "$axvisor_bin" ]] || {
        printf 'error: %s Axvisor binary is missing after build\n' "$name" >&2
        exit 1
    }
    cp "$axvisor_bin" "$out_dir/axvisor.bin"
    cp "$board" "$out_dir/board.toml"
    cp "$qemu_config" "$out_dir/qemu.toml"
    cp "$steps" "$out_dir/steps.txt"
    cat > "$out_dir/meta.txt" <<EOF
case=${name}
implementation=${implementation}
host_only=1
physical_cpus=4
worker_cpu_mask=0xe
workers=3
worker_priority=90
timer_worker_priority=89
iterations_per_worker=${iterations}
expiry_samples_per_worker=${expiry_samples}
expiry_delay_us=${expiry_delay_us}
board_config=${board}
storm_command=${storm_command}
EOF
    (
        cd "$out_dir"
        sha256sum axvisor.bin board.toml qemu.toml steps.txt timer-storm.txt > sha256sums
    )
    printf 'accepted: %s\n' "$out_dir"
}

run_case global-lock global-lock "$global_board"
run_case per-cpu-lock per-cpu-lock "$percpu_board"

python3 "$summary_tool" "$output_root"
