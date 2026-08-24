#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/axvisor-rt-profile
PROBE=/usr/local/bin/axvisor-rt-probe
RESULT_DIR=/var/lib/axvisor-rt
RAW_LOG=$RESULT_DIR/raw.log
GUEST_IRQ_TRACE=/proc/axvisor_rt_timer_trace
GUEST_IRQ_TRACE_LOG=$RESULT_DIR/guest-timer-trace.log.gz

exec >/dev/console 2>&1

fatal() {
    phase=$1
    status=${2:-1}
    echo "AXVISOR_RT_STARRY_CAPTURE_FAILED schema=1 phase=$phase status=$status"
    "$BB" sync || true
    "$BB" poweroff -f || true
    while true; do
        "$BB" sleep 60
    done
}

require_nonnegative_integer() {
    name=$1
    value=$2

    case "$value" in
        ''|*[!0-9]*) fatal "invalid-$name" 2 ;;
    esac
}

run_metric() {
    metric=$1
    metric_log=$RESULT_DIR/$metric.log

    echo "AXVISOR_RT_STARRY_PHASE_START schema=1 phase=$metric"
    "$PROBE" \
        --metric "$metric" \
        --iterations "$iterations" \
        --warmup "$warmup" \
        --period-us "$period_us" \
        --cpu "$measurement_cpu" \
        --fifo-priority "$fifo_priority" >"$metric_log"
    status=$?
    [ "$status" -eq 0 ] || fatal "$metric" "$status"

    sample_count=$(
        "$BB" grep -c "^AXVISOR_RT_SAMPLE schema=1 metric=$metric " "$metric_log" || true
    )
    [ "$sample_count" -eq "$iterations" ] || fatal "$metric-sample-count" 1
    complete_count=$(
        "$BB" grep -c "^AXVISOR_RT_METRIC_COMPLETE schema=1 metric=$metric count=$iterations$" \
            "$metric_log" || true
    )
    [ "$complete_count" -eq 1 ] || fatal "$metric-complete-marker" 1

    "$BB" cat "$metric_log" >>"$RAW_LOG" || fatal "$metric-append" 1
    # The host snapshots this ext4 image while it is still mounted. Retaining
    # the small per-metric file avoids an orphan-list entry from a live unlink.
    echo "AXVISOR_RT_STARRY_PHASE_PASS schema=1 phase=$metric samples=$sample_count"
}

cleanup_stress() {
    if [ -n "${stress_pid:-}" ] && "$BB" kill -0 "$stress_pid" 2>/dev/null; then
        "$BB" kill -TERM "$stress_pid" 2>/dev/null || true
        wait "$stress_pid" 2>/dev/null || true
    fi
}

start_stress() {
    stress_log=$RESULT_DIR/cpu-stress.log
    stress_pid=
    trap cleanup_stress EXIT HUP INT TERM

    : >"$stress_log"
    "$PROBE" \
        --metric cpu_stress \
        --cpu "$stress_cpu" \
        --fifo-priority 0 >"$stress_log" 2>&1 &
    stress_pid=$!

    attempt=0
    while [ "$attempt" -lt 100 ]; do
        if "$BB" grep -q '^AXVISOR_RT_WORKLOAD_READY ' "$stress_log"; then
            break
        fi
        "$BB" kill -0 "$stress_pid" 2>/dev/null || fatal stress-exited-before-ready 1
        attempt=$((attempt + 1))
        "$BB" sleep 0.01
    done
    "$BB" grep -q '^AXVISOR_RT_WORKLOAD_READY ' "$stress_log" || \
        fatal stress-ready-timeout 1
    "$BB" grep '^AXVISOR_RT_WORKLOAD_READY ' "$stress_log" >>"$RAW_LOG" || \
        fatal stress-ready-append 1
    echo "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=cpu-stress pid=$stress_pid cpu=$stress_cpu affinity=$stress_cpu" \
        >>"$RAW_LOG"
    echo "AXVISOR_RT_STARRY_WORKLOAD_READY schema=1 kind=cpu-stress cpu=$stress_cpu"
}

stop_stress() {
    "$BB" kill -0 "$stress_pid" 2>/dev/null || fatal stress-exited-during-capture 1
    "$BB" kill -TERM "$stress_pid" || fatal stress-sigterm 1
    wait "$stress_pid"
    status=$?
    completed_pid=$stress_pid
    stress_pid=
    [ "$status" -eq 0 ] || fatal stress-wait "$status"
    "$BB" grep '^AXVISOR_RT_WORKLOAD_STOPPED ' "$stress_log" >>"$RAW_LOG" || \
        fatal stress-stopped-marker 1
    echo "AXVISOR_RT_WORKLOAD_CLEANED schema=1 kind=cpu-stress pid=$completed_pid status=0" \
        >>"$RAW_LOG"
    # Retain the stress log for the same live-snapshot consistency reason as
    # the metric logs. Every board run starts from the pristine staged image.
    trap - EXIT HUP INT TERM
    echo "AXVISOR_RT_STARRY_WORKLOAD_STOPPED schema=1 kind=cpu-stress cpu=$stress_cpu"
}

[ -x "$BB" ] || fatal busybox-not-found 1
[ -x "$PROBE" ] || fatal probe-not-found 1
[ -r "$PROFILE" ] || fatal profile-not-found 1
. "$PROFILE"

require_nonnegative_integer iterations "${iterations:-}"
require_nonnegative_integer warmup "${warmup:-}"
require_nonnegative_integer period-us "${period_us:-}"
require_nonnegative_integer measurement-cpu "${measurement_cpu:-}"
require_nonnegative_integer stress-cpu "${stress_cpu:-}"
require_nonnegative_integer fifo-priority "${fifo_priority:-}"
[ "$iterations" -gt 0 ] || fatal invalid-iterations 2
[ "$period_us" -gt 0 ] || fatal invalid-period-us 2
[ "$fifo_priority" -gt 0 ] && [ "$fifo_priority" -le 98 ] || \
    fatal invalid-fifo-priority 2
case "${workload:-}" in
    idle|cpu-stress) ;;
    *) fatal invalid-workload 2 ;;
esac

online=$(
    "$BB" grep -c '^processor' /proc/cpuinfo 2>/dev/null || true
)
[ "$online" -eq 2 ] || fatal unexpected-vcpu-count 1
[ "$measurement_cpu" -lt "$online" ] || fatal measurement-cpu-offline 2
[ "$stress_cpu" -lt "$online" ] || fatal stress-cpu-offline 2
[ "$measurement_cpu" -ne "$stress_cpu" ] || fatal cpu-roles-overlap 2

"$BB" mkdir -p "$RESULT_DIR" || fatal result-directory 1
: >"$RAW_LOG" || fatal raw-log-create 1
echo "AXVISOR_RT_RUN_START" >>"$RAW_LOG"
echo "AXVISOR_RT_GUEST_CPUS schema=1 os=starryos online=$online" >>"$RAW_LOG"
echo "AXVISOR_RT_STARRY_CAPTURE schema=1 iterations=$iterations warmup=$warmup period_us=$period_us measurement_cpu=$measurement_cpu stress_cpu=$stress_cpu fifo_priority=$fifo_priority workload=$workload" \
    >>"$RAW_LOG"

case "$workload" in
    idle)
        echo "AXVISOR_RT_WORKLOAD_ACTIVE schema=1 kind=idle" >>"$RAW_LOG"
        ;;
    cpu-stress)
        start_stress
        ;;
esac

run_metric periodic_jitter
run_metric dispatch_latency
run_metric emulated_irq_response

if [ "$workload" = cpu-stress ]; then
    stop_stress
fi

[ -r "$GUEST_IRQ_TRACE" ] || fatal guest-irq-trace-not-found 1
"$BB" gzip -c "$GUEST_IRQ_TRACE" >"$GUEST_IRQ_TRACE_LOG" || \
    fatal guest-irq-trace-compress 1
[ -s "$GUEST_IRQ_TRACE_LOG" ] || fatal guest-irq-trace-empty 1
guest_irq_checksum=$("$BB" sha256sum "$GUEST_IRQ_TRACE_LOG") || \
    fatal guest-irq-trace-sha256 1
guest_irq_sha256=${guest_irq_checksum%% *}
guest_irq_bytes=$("$BB" wc -c <"$GUEST_IRQ_TRACE_LOG") || \
    fatal guest-irq-trace-size 1
echo "AXVISOR_RT_GUEST_IRQ_TRACE_FILE schema=1 path=$GUEST_IRQ_TRACE_LOG compression=gzip bytes=$guest_irq_bytes sha256=$guest_irq_sha256" \
    >>"$RAW_LOG"
echo "AXVISOR_RT_RUN_COMPLETE" >>"$RAW_LOG"

"$BB" sync || fatal final-sync 1
raw_checksum=$("$BB" sha256sum "$RAW_LOG") || fatal raw-sha256 1
raw_sha256=${raw_checksum%% *}
raw_lines=$("$BB" wc -l <"$RAW_LOG") || fatal raw-line-count 1
echo "AXVISOR_RT_STARRY_RAW schema=1 path=$RAW_LOG lines=$raw_lines samples_per_metric=$iterations sha256=$raw_sha256"
for _copy in 1 2 3; do
    echo "AXVISOR_RT_STARRY_CAPTURE_COMPLETE schema=1 workload=$workload"
    "$BB" sleep 0.1
done
"$BB" poweroff -f
"$BB" sleep 5
fatal poweroff-returned 1
