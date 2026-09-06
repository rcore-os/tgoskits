#!/bin/sh
# StarryOS network benchmark core (guest side).
#
# Shared by net-bench.sh (SLIRP) and net-bench-tap.sh (TAP). The caller must
# export HOST_IP before exec-ing this script. Each measurement emits its raw
# iperf3 JSON wrapped in parseable markers so the host-side summarizer
# (summarize.py) can aggregate mean/stddev across repeated iterations without
# guessing block boundaries.
#
# Marker contract (consumed by summarize.py):
#   NET_BENCH_BEGIN test=<id> iter=<n> warmup=<0|1>
#   <iperf3 -J output>
#   NET_BENCH_END test=<id> iter=<n>
#
# warmup=1 iterations are recorded but excluded from statistics (cache/route
# warmup per methodology §3.4). The whole run ends with NET_BENCH_PASSED.

HOST_IP="${HOST_IP:-10.0.2.2}"
PORT="${NET_BENCH_PORT:-5201}"
# Per-iteration duration (seconds) and repetition count. Kept short so the full
# matrix fits inside the QEMU test timeout; stats come from ITERS repetitions.
DURATION="${NET_BENCH_DURATION:-5}"
ITERS="${NET_BENCH_ITERS:-5}"
WARMUP="${NET_BENCH_WARMUP:-1}"
RESULT_PREFIX="NET_BENCH"

fail() {
    echo "${RESULT_PREFIX}_FAILED: $*"
    exit 1
}

# run_test <test-id> <extra iperf3 args...>
# Runs WARMUP warmup iterations followed by ITERS measured iterations, each
# wrapped in BEGIN/END markers. Warmup failures are tolerated; measured
# failures abort the whole benchmark.
#
# /proc/net/dev snapshots are emitted before and after each iperf3 call so
# summarize.py can compute per-interface L2 byte/packet deltas.
run_test() {
    test_id="$1"
    shift
    total=$((WARMUP + ITERS))
    iter=0
    while [ "$iter" -lt "$total" ]; do
        if [ "$iter" -lt "$WARMUP" ]; then
            warm=1
        else
            warm=0
        fi
        echo "${RESULT_PREFIX}_BEGIN test=${test_id} iter=${iter} warmup=${warm}"
        echo "NET_STATS_BEGIN warmup=${warm}"
        cat /proc/net/dev
        echo "NET_STATS_END"
        if iperf3 -c "$HOST_IP" -p "$PORT" -t "$DURATION" -J "$@"; then
            echo "${RESULT_PREFIX}_END test=${test_id} iter=${iter}"
        else
            echo "${RESULT_PREFIX}_END test=${test_id} iter=${iter}"
            if [ "$warm" -eq 0 ]; then
                fail "${test_id} iteration ${iter}"
            fi
            echo "${RESULT_PREFIX}_WARN: ${test_id} warmup iteration ${iter} failed (ignored)"
        fi
        echo "NET_STATS_BEGIN warmup=${warm}"
        cat /proc/net/dev
        echo "NET_STATS_END"
        iter=$((iter + 1))
    done
}

# Wait for the iperf3 server to be reachable (QEMU usermode ICMP is unreliable,
# so probe with a short TCP test; smoltcp sends ARP on the first TAP packet).
i=0
while ! iperf3 -c "$HOST_IP" -p "$PORT" -t 1 >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -gt 15 ] && fail "iperf3 server $HOST_IP:$PORT unreachable after 15 retries"
    sleep 2
done

echo "=== StarryOS iperf3 network benchmark ==="
echo "target=$HOST_IP:$PORT duration=${DURATION}s iters=${ITERS} warmup=${WARMUP}"

# Throughput, single TCP stream (uplink: guest -> host).
run_test tcp1

# Throughput, 4 parallel TCP streams.
run_test tcp4 -P 4

# Throughput, single TCP stream reverse (downlink: host -> guest).
run_test tcp1r -R

# UDP throughput at 1 Gbit/s target (large packets).
run_test udp1g -u -b 1G

# UDP small-packet PPS: 64-byte payloads bounded to 100 Mbit/s to avoid
# flooding the smoke setup while still exposing per-packet overhead.
run_test udp64 -u -l 64 -b 100M

echo "${RESULT_PREFIX}_PASSED"
