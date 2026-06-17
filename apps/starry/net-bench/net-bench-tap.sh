#!/bin/sh
# StarryOS network benchmark via TAP: guest CLIENT -> host server
# Host must run: iperf3 -s -p 5201 -B 192.168.100.1
# Static ARP is normally unnecessary; add it only when debugging ARP issues.

HOST_IP="192.168.100.1"
PORT=5201
RESULT_PREFIX="NET_BENCH"

fail() { echo "${RESULT_PREFIX}_FAILED: $*"; exit 1; }

# Probe connectivity (smoltcp will send ARP on first packet)
i=0
while ! iperf3 -c "$HOST_IP" -p "$PORT" -t 1 >/dev/null 2>&1; do
    i=$((i + 1)); [ $i -gt 15 ] && fail "host unreachable after retries"; sleep 2
done

echo "=== StarryOS iperf3 benchmark (TAP) ==="
echo "Target: $HOST_IP:$PORT"

echo "--- TCP 1-stream ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -J > /tmp/tcp1.json 2>&1 || fail "TCP 1-stream"
cat /tmp/tcp1.json

echo "--- TCP 4-stream ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -P 4 -J > /tmp/tcp4.json 2>&1 || fail "TCP 4-stream"
cat /tmp/tcp4.json

echo "--- UDP 1G ---"
iperf3 -c "$HOST_IP" -p "$PORT" -t 10 -u -b 1G -J > /tmp/udp.json 2>&1 || fail "UDP"
cat /tmp/udp.json

echo "${RESULT_PREFIX}_PASSED"
