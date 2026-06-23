#!/bin/sh
set -u

BENCH_DIR=${BENCH_DIR:-/boot/bench}
BENCH_BIN=${BENCH_BIN:-${BENCH_DIR}/thread_overhead_bench}
CPU=${CPU:-4}

NEXT=${BENCH_DIR}/boot.flag
SKIP=${BENCH_DIR}/skip-bench

mkdir -p "${BENCH_DIR}"

echo "=== run-thread-bench.sh entered ==="
date || true
echo "cmdline: $(cat /proc/cmdline)"
echo "BENCH_BIN=${BENCH_BIN}"
echo "CPU=${CPU}"
echo "online CPUs: $(cat /sys/devices/system/cpu/online 2>/dev/null || echo unknown)"
for cpu in /sys/devices/system/cpu/cpu[0-9]*; do
    id=${cpu##*/cpu}
    gov=$(cat "$cpu/cpufreq/scaling_governor" 2>/dev/null || echo n/a)
    cur=$(cat "$cpu/cpufreq/scaling_cur_freq" 2>/dev/null || echo n/a)
    max=$(cat "$cpu/cpufreq/scaling_max_freq" 2>/dev/null || echo n/a)
    echo "cpu${id}: governor=${gov} cur_freq_khz=${cur} max_freq_khz=${max}"
done

if ! grep -qw bench_thread=1 /proc/cmdline; then
    echo "bench_thread=1 is absent, skip Linux thread benchmark"
    exit 0
fi

if [ -f "${SKIP}" ]; then
    echo "${SKIP} exists, skip Linux thread benchmark"
    exit 0
fi

if [ ! -x "${BENCH_BIN}" ]; then
    echo "Linux thread benchmark binary is missing or not executable: ${BENCH_BIN}"
    exit 1
fi

echo "Running Linux thread benchmark on CPU ${CPU}"
taskset -c "${CPU}" "${BENCH_BIN}"
bench_rc=$?
echo "benchmark exit status: ${bench_rc}"

touch "${NEXT}"
sync

echo "=== Linux thread benchmark finished ==="
echo "Created ${NEXT}; rebooting to ArceOS in 5 seconds"
sleep 5

/sbin/reboot -f
