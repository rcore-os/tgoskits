#!/bin/sh
# StarryOS network benchmark: iperf3 client against host server (via hostfwd)
# Host must be running: iperf3 -s -p 5201

HOST_IP="10.0.2.2"  # QEMU usermode default gateway
PORT=5201
RESULT_PREFIX="NET_BENCH"

fail() {
    echo "${RESULT_PREFIX}_FAILED: $*"
    exit 1
}

# Wait for iperf3 server to be reachable (QEMU usermode ICMP unreliable, use TCP probe)
i=0
while ! iperf3 -c "$HOST_IP" -p "$PORT" -t 1 >/dev/null 2>&1; do
    i=$((i + 1))
    [ $i -gt 10 ] && fail "iperf3 server unreachable after 10 retries"
    sleep 2
done

echo "=== StarryOS iperf3 network benchmark ==="
echo "Target: $HOST_IP:$PORT"

# TCP throughput (10s, single stream)
echo "--- TCP throughput (1 stream, 10s) ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -J > /tmp/tcp1.json 2>&1 || fail "iperf3 TCP 1-stream failed"
cat /tmp/tcp1.json

# TCP throughput (10s, 4 parallel streams)
echo "--- TCP throughput (4 streams, 10s) ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -P 4 -J > /tmp/tcp4.json 2>&1 || fail "iperf3 TCP 4-stream failed"
cat /tmp/tcp4.json

# UDP PPS (1 Gbit/s target, 10s)
echo "--- UDP PPS (1G target, 10s) ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -u -b 1G -J > /tmp/udp.json 2>&1 || fail "iperf3 UDP failed"
cat /tmp/udp.json

echo "${RESULT_PREFIX}_PASSED"
