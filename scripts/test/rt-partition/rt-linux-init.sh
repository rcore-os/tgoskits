#!/bin/sh
# RT-partition Linux measurement init.
#
# Runs inside the Linux guest initramfs. Controlled by kernel cmdline
# placeholders replaced by run-cyclictest.sh via the `cmdline` field:
#   rt_scenario=idle|stress-noiso|stress-dedicated|stress-rt
#   rt_cpu=N            isolated measurement vCPU (default 1)
#   rt_load_cpu=N       housekeeping/load vCPU (default 0)
#   rt_loops=N          cyclictest loop count (0 = endless when duration is set)
#   rt_duration_sec=N   stop after this guest duration; 0 uses exact loop mode
#   rt_interval_us=N    cyclictest interval (default 1000)
#   rt_maxlat_us=N      histogram max latency (default 400)
#   rt_priority=N       cyclictest SCHED_FIFO priority (default 90)
#   rt_trace=disabled|events|timerlat
#                       optional tracefs diagnostic capture
#   rt_trace_buffer_kb=N per-CPU trace buffer size (default 8192 KiB)
#   rt_start_delay_sec=N delay before workload start so the runner can sample
#                        the pre-test VM-exit counters
#   rt_hold_after_complete=0|1 keep Linux alive after measurement until the
#                           runner sends the `release` token
#
# The measurement task is pinned to the isolated guest CPU. Stress stays on the
# other housekeeping CPU, so the Linux workload is identical between the
# no-isolation and RT-partition scenarios while host placement policy changes.

set -u

/bin/busybox rm -f /dev/console /dev/null
/bin/busybox mknod -m 0600 /dev/console c 5 1
/bin/busybox mknod -m 0666 /dev/null c 1 3
exec </dev/console >/dev/console 2>&1

/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mkdir -p /tmp
/bin/busybox mkdir -p /dev/shm
/bin/busybox mount -t tmpfs tmpfs /dev/shm

scenario=idle
cpu=1
load_cpu=0
loops=1800000
duration_sec=0
interval_us=1000
maxlat_us=400
priority=90
trace_mode=disabled
trace_buffer_kb=8192
start_delay_sec=25
hold_after_complete=0

for arg in $(/bin/busybox cat /proc/cmdline); do
    case "$arg" in
        rt_scenario=*) scenario="${arg#rt_scenario=}" ;;
        rt_cpu=*) cpu="${arg#rt_cpu=}" ;;
        rt_load_cpu=*) load_cpu="${arg#rt_load_cpu=}" ;;
        rt_loops=*) loops="${arg#rt_loops=}" ;;
        rt_duration_sec=*) duration_sec="${arg#rt_duration_sec=}" ;;
        rt_interval_us=*) interval_us="${arg#rt_interval_us=}" ;;
        rt_maxlat_us=*) maxlat_us="${arg#rt_maxlat_us=}" ;;
        rt_priority=*) priority="${arg#rt_priority=}" ;;
        rt_trace=*) trace_mode="${arg#rt_trace=}" ;;
        rt_trace_buffer_kb=*) trace_buffer_kb="${arg#rt_trace_buffer_kb=}" ;;
        rt_start_delay_sec=*) start_delay_sec="${arg#rt_start_delay_sec=}" ;;
        rt_hold_after_complete=*) hold_after_complete="${arg#rt_hold_after_complete=}" ;;
    esac
done

cpu_total=$(/bin/busybox grep -c ^processor /proc/cpuinfo)
case "$cpu:$load_cpu" in
    *[!0-9:]*|:*|*:)
        echo "RT_AFFINITY_ERROR invalid cpu=$cpu load_cpu=$load_cpu"
        /bin/busybox poweroff -f
        ;;
esac
if [ "$cpu" -eq "$load_cpu" ] || [ "$cpu" -ge "$cpu_total" ] || [ "$load_cpu" -ge "$cpu_total" ]; then
    echo "RT_AFFINITY_ERROR cpu=$cpu load_cpu=$load_cpu total=$cpu_total"
    /bin/busybox poweroff -f
fi
case "$loops:$duration_sec" in
    *[!0-9:]*|:*|*:)
        echo "RT_DURATION_ERROR invalid loops=$loops duration_sec=$duration_sec"
        /bin/busybox poweroff -f
        ;;
esac
if [ "$loops" -eq 0 ] && [ "$duration_sec" -eq 0 ]; then
    echo "RT_DURATION_ERROR loops and duration cannot both be zero"
    /bin/busybox poweroff -f
fi
case "$trace_mode" in
    disabled|events|timerlat) ;;
    *)
        echo "RT_FTRACE_ERROR invalid mode=$trace_mode"
        /bin/busybox poweroff -f
        ;;
esac
case "$trace_buffer_kb" in
    *[!0-9]*|'')
        echo "RT_FTRACE_ERROR invalid buffer_kb=$trace_buffer_kb"
        /bin/busybox poweroff -f
        ;;
esac
if [ "$trace_buffer_kb" -eq 0 ]; then
    echo "RT_FTRACE_ERROR buffer_kb must be positive"
    /bin/busybox poweroff -f
fi
case "$hold_after_complete" in
    0|1) ;;
    *)
        echo "RT_HOLD_ERROR invalid hold_after_complete=$hold_after_complete"
        /bin/busybox poweroff -f
        ;;
esac

echo "RT_INIT scenario=$scenario cpu=$cpu load_cpu=$load_cpu loops=$loops duration_sec=$duration_sec interval_us=$interval_us maxlat_us=$maxlat_us priority=$priority trace=$trace_mode trace_buffer_kb=$trace_buffer_kb start_delay_sec=$start_delay_sec hold_after_complete=$hold_after_complete"
echo "RT_CPUS total=$cpu_total"
echo "RT_SCHED_PROBE_START"
if /bin/busybox chrt -p $$; then
    echo "RT_SCHED_PROBE_OK"
else
    echo "RT_SCHED_PROBE_ERROR status=$?"
fi
echo "RT_WAIT_BEFORE_TEST seconds=$start_delay_sec"
/bin/busybox sleep "$start_delay_sec"

# Record compact per-CPU counters every 5 seconds. They are emitted only after
# cyclictest prints its histogram so the histogram rows cannot be interleaved.
(
    sample=0
    while :; do
        /bin/busybox awk -v sample="$sample" '
            /^cpu[0-9]+ / {
                printf "RT_CPUSTAT sample=%d cpu=%s user=%s nice=%s system=%s idle=%s iowait=%s irq=%s softirq=%s steal=%s\n",
                    sample, $1, $2, $3, $4, $5, $6, $7, $8, $9
            }
        ' /proc/stat
        sample=$((sample + 1))
        /bin/busybox sleep 5
    done
) > /tmp/rt-cpustat.log &
load_pid=$!

case "$scenario" in
    stress-noiso)
        echo "RT_STRESS_START cpu=$load_cpu workers=2 vm=1"
        /bin/busybox taskset -c "$load_cpu" /bin/stress-ng --cpu 2 --vm 1 --vm-bytes 64M &
        stress_pid=$!
        ;;
    stress-dedicated|stress-rt)
        echo "RT_STRESS_START cpu=$load_cpu workers=2 vm=1"
        /bin/busybox taskset -c "$load_cpu" /bin/stress-ng --cpu 2 --vm 1 --vm-bytes 64M &
        stress_pid=$!
        ;;
esac

trace_dir=/sys/kernel/tracing
if [ "$trace_mode" != disabled ]; then
    /bin/busybox mkdir -p "$trace_dir"
    if ! /bin/busybox mount -t tracefs tracefs "$trace_dir"; then
        echo "RT_FTRACE_ERROR tracefs mount failed"
        /bin/busybox poweroff -f
    fi
    case "$cpu" in
        0) trace_cpumask=1 ;;
        1) trace_cpumask=2 ;;
    esac
    echo 0 > "$trace_dir/tracing_on"
    echo > "$trace_dir/trace"
    echo "$trace_buffer_kb" > "$trace_dir/buffer_size_kb"
    echo "$trace_cpumask" > "$trace_dir/tracing_cpumask"
    echo mono_raw > "$trace_dir/trace_clock"
fi
if [ "$trace_mode" = events ]; then
    for trace_event in \
        irq/irq_handler_entry irq/irq_handler_exit \
        timer/hrtimer_expire_entry timer/hrtimer_expire_exit \
        sched/sched_wakeup sched/sched_switch; do
        if [ ! -e "$trace_dir/events/$trace_event/enable" ]; then
            echo "RT_FTRACE_ERROR missing_event=$trace_event"
            /bin/busybox poweroff -f
        fi
    done
    for trace_event in \
        irq/irq_handler_entry irq/irq_handler_exit \
        timer/hrtimer_expire_entry timer/hrtimer_expire_exit \
        sched/sched_wakeup sched/sched_switch; do
        echo 1 > "$trace_dir/events/$trace_event/enable"
    done
    echo "RT_FTRACE_START mode=events cpu=$cpu buffer_kb=$trace_buffer_kb"
    echo 1 > "$trace_dir/tracing_on"
fi
if [ "$trace_mode" = timerlat ]; then
    for control in \
        current_tracer osnoise/cpus osnoise/timerlat_period_us \
        osnoise/stop_tracing_us osnoise/stop_tracing_total_us; do
        if [ ! -e "$trace_dir/$control" ]; then
            echo "RT_FTRACE_ERROR missing_timerlat_control=$control"
            /bin/busybox poweroff -f
        fi
    done
    echo "$cpu" > "$trace_dir/osnoise/cpus"
    echo "$interval_us" > "$trace_dir/osnoise/timerlat_period_us"
    echo 0 > "$trace_dir/osnoise/stop_tracing_us"
    echo 0 > "$trace_dir/osnoise/stop_tracing_total_us"
    if ! echo timerlat > "$trace_dir/current_tracer"; then
        echo "RT_FTRACE_ERROR timerlat tracer unavailable"
        /bin/busybox poweroff -f
    fi
    echo "RT_FTRACE_START mode=timerlat cpu=$cpu buffer_kb=$trace_buffer_kb period_us=$interval_us"
    echo 1 > "$trace_dir/tracing_on"
fi

echo "RT_CYCLICTEST_START"
IFS=' ' read -r start_uptime_s _ < /proc/uptime
echo "RT_CYCLICTEST_TIMING_START uptime_s=$start_uptime_s"
(
    while :; do
        IFS=' ' read -r progress_uptime_s _ < /proc/uptime
        echo "RT_PROGRESS uptime_s=$progress_uptime_s"
        /bin/busybox sleep 10
    done
) &
progress_pid=$!
all_cpus="0-$((cpu_total - 1))"
if [ "$duration_sec" -gt 0 ]; then
    /bin/busybox taskset -c "$all_cpus" /bin/cyclictest -a "$cpu" \
        -m -p "$priority" -i "$interval_us" -l "$loops" \
        -D "${duration_sec}s" -h "$maxlat_us" -q > /tmp/cyclictest.log 2>&1
else
    /bin/busybox taskset -c "$all_cpus" /bin/cyclictest -a "$cpu" \
        -m -p "$priority" -i "$interval_us" -l "$loops" -h "$maxlat_us" -q \
        > /tmp/cyclictest.log 2>&1
fi
status=$?
if [ "$trace_mode" != disabled ]; then
    echo 0 > "$trace_dir/tracing_on"
    /bin/busybox cat "$trace_dir/trace" > /tmp/rt-ftrace.log
    echo "RT_FTRACE_STOP mode=$trace_mode"
fi
/bin/busybox kill "$progress_pid" 2>/dev/null || true
/bin/busybox wait "$progress_pid" 2>/dev/null || true
/bin/busybox cat /tmp/cyclictest.log
IFS=' ' read -r end_uptime_s _ < /proc/uptime
echo "RT_CYCLICTEST_TIMING_END uptime_s=$end_uptime_s"
if [ "$status" -ne 0 ]; then
    echo "RT_CYCLICTEST_ERROR status=$status"
fi
echo "RT_CYCLICTEST_COMPLETE"

# Signal that the measured workload has completed before emitting the optional
# CPU accounting tail.  Keeping this marker ahead of verbose diagnostics makes
# completion capture deterministic even when fixed-priority scheduling delays
# the final shell commands.
echo "RT_INIT_DONE scenario=$scenario"

if [ -n "${stress_pid:-}" ]; then
    /bin/busybox kill "$stress_pid" 2>/dev/null || true
    echo "RT_STRESS_STOP"
fi
/bin/busybox kill "$load_pid" 2>/dev/null || true
/bin/busybox wait "$load_pid" 2>/dev/null || true
/bin/busybox cat /tmp/rt-cpustat.log

if [ "$trace_mode" != disabled ]; then
    echo "RT_FTRACE_DUMP_READY encoding=gzip-base64"
    IFS= read -r trace_dump_token
    if [ "$trace_dump_token" != dump ]; then
        echo "RT_FTRACE_ERROR invalid_dump_token=$trace_dump_token"
        /bin/busybox poweroff -f
    fi
    echo "RT_FTRACE_DUMP_BEGIN encoding=gzip-base64"
    /bin/busybox gzip -c /tmp/rt-ftrace.log | /bin/busybox base64
    echo "RT_FTRACE_DUMP_END"
fi

if [ "$hold_after_complete" -eq 1 ]; then
    echo "RT_CYCLICTEST_HOLD_READY"
    IFS= read -r release_token
    if [ "$release_token" != release ]; then
        echo "RT_CYCLICTEST_ERROR invalid_release_token=$release_token"
        /bin/busybox poweroff -f
    fi
    echo "RT_CYCLICTEST_RELEASED"
fi

# Keep the console alive briefly so the runner can capture the diagnostic tail.
/bin/busybox sleep 2
/bin/busybox poweroff -f 2>/dev/null || /bin/busybox sleep 30
