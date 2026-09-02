#!/bin/sh

set -u

BB=/bin/busybox
PROFILE=/etc/axvisor-rt-profile
PROBE=/usr/local/bin/axvisor-rt-probe

exec >/dev/console 2>&1

fatal() {
    phase=$1
    status=${2:-1}
    echo "AXVISOR_RT_STARRY_COMPAT_FAILED schema=1 phase=$phase status=$status"
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
    phase=$1
    metric=$2
    priority=$3

    echo "AXVISOR_RT_STARRY_PHASE_START schema=1 phase=$phase metric=$metric fifo_priority=$priority"
    "$PROBE" \
        --metric "$metric" \
        --iterations "$iterations" \
        --warmup "$warmup" \
        --period-us "$period_us" \
        --cpu "$measurement_cpu" \
        --fifo-priority "$priority"
    status=$?
    [ "$status" -eq 0 ] || fatal "$phase" "$status"
    echo "AXVISOR_RT_STARRY_PHASE_PASS schema=1 phase=$phase"
}

cleanup_stress() {
    if [ -n "${stress_pid:-}" ] && "$BB" kill -0 "$stress_pid" 2>/dev/null; then
        "$BB" kill -TERM "$stress_pid" 2>/dev/null || true
        wait "$stress_pid" 2>/dev/null || true
    fi
}

run_stress_smoke() {
    stress_log=/tmp/axvisor-rt-stress.log
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
        "$BB" kill -0 "$stress_pid" 2>/dev/null || {
            "$BB" cat "$stress_log"
            fatal stress-exited-before-ready 1
        }
        attempt=$((attempt + 1))
        "$BB" sleep 0.01
    done
    "$BB" grep -q '^AXVISOR_RT_WORKLOAD_READY ' "$stress_log" || {
        "$BB" cat "$stress_log"
        fatal stress-ready-timeout 1
    }

    "$BB" sleep 1
    "$BB" kill -TERM "$stress_pid" || fatal stress-sigterm 1
    wait "$stress_pid"
    status=$?
    stress_pid=
    "$BB" cat "$stress_log"
    [ "$status" -eq 0 ] || fatal stress-wait "$status"
    "$BB" grep -q '^AXVISOR_RT_WORKLOAD_STOPPED ' "$stress_log" || \
        fatal stress-stopped-marker 1

    trap - EXIT HUP INT TERM
    echo "AXVISOR_RT_STARRY_PHASE_PASS schema=1 phase=cpu-stress"
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

online=$(
    "$BB" grep -c '^processor' /proc/cpuinfo 2>/dev/null || true
)
[ "$online" -eq 2 ] || fatal unexpected-vcpu-count 1
[ "$measurement_cpu" -lt "$online" ] || fatal measurement-cpu-offline 2
[ "$stress_cpu" -lt "$online" ] || fatal stress-cpu-offline 2
[ "$measurement_cpu" -ne "$stress_cpu" ] || fatal cpu-roles-overlap 2

echo "AXVISOR_RT_RUN_START"
echo "AXVISOR_RT_GUEST_CPUS schema=1 os=starryos online=$online"
echo "AXVISOR_RT_STARRY_COMPAT schema=1 iterations=$iterations warmup=$warmup period_us=$period_us measurement_cpu=$measurement_cpu stress_cpu=$stress_cpu fifo_priority=$fifo_priority"

# This first phase isolates absolute monotonic sleep and affinity from the
# real-time scheduling permission/policy check in the following phase.
run_metric clock-affinity periodic_jitter 0
run_metric sched-fifo periodic_jitter "$fifo_priority"
run_metric pthread-eventfd dispatch_latency "$fifo_priority"
run_metric timerfd emulated_irq_response "$fifo_priority"
run_stress_smoke

echo "AXVISOR_RT_RUN_COMPLETE"
for _copy in 1 2 3; do
    echo "AXVISOR_RT_STARRY_COMPAT_COMPLETE schema=1"
    "$BB" sleep 0.1
done
"$BB" sync || fatal final-sync 1
"$BB" poweroff -f
"$BB" sleep 5
fatal poweroff-returned 1
