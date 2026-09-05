#!/bin/sh
# board-server.sh — SG2002 iperf3 server lifecycle with /proc/net/dev snapshots.
#
# Deployed to the board at /tmp/board-server.sh.  The PC-side controller
# (board-controller.py) opens an SSH channel and runs:
#
#   sh /tmp/board-server.sh <port> <warmup_flag> <duration>
#
# Semantics:
#   1. Emit NET_STATS_BEGIN, cat /proc/net/dev, NET_STATS_END — before snapshot.
#   2. Emit SERVER_READY so the PC knows it is safe to start iperf3 -c.
#   3. iperf3 -s -1 handles one client and exits (self-terminating).
#      Wrapped with `timeout` (busybox) as a safety net — if iperf3 hangs
#      for any reason, it is killed after <duration>+<pad> seconds and the
#      after-snapshot is still collected.
#   4. Emit NET_STATS_BEGIN, cat /proc/net/dev, NET_STATS_END — after snapshot.
#
# iperf3 -s -1 is the key primitive: the server serves exactly one client
# and exits, so the after-snapshot is gated by an OS-visible process exit
# rather than a blind sleep.  Requires iperf3 >= 3.10.
#
# Written for busybox ash — no bash-isms.

PORT="${1:-5201}"
WARMUP="${2:-0}"
DURATION="${3:-5}"
PAD="${4:-10}"

TIMEOUT=$((DURATION + PAD))

fail() {
    echo "BOARD_SERVER_ERROR: $*"
    exit 1
}

# ---- sync point 1: before snapshot ----------------------------------------

echo "NET_STATS_BEGIN warmup=${WARMUP}"
cat /proc/net/dev
echo "NET_STATS_END"

# ---- notify controller ----------------------------------------------------

echo "SERVER_READY"

# ---- sync point 2: serve one client, self-terminate -----------------------

# iperf3 -s -1 exits after handling one client.  `timeout` is a safety net:
# if the client hangs or the network stalls, the server is killed and the
# after-snapshot is still collected (rather than blocking forever).
timeout "$TIMEOUT" iperf3 -s -1 -p "$PORT" >/dev/null
IPERF_RC=$?

# ---- sync point 3: after snapshot -----------------------------------------

echo "NET_STATS_BEGIN warmup=${WARMUP}"
cat /proc/net/dev
echo "NET_STATS_END"

# Report iperf3 exit status to the controller for diagnostics.
if [ "$IPERF_RC" -eq 124 ]; then
    echo "BOARD_SERVER_ERROR: iperf3 killed by timeout after ${TIMEOUT}s"
elif [ "$IPERF_RC" -ne 0 ]; then
    echo "BOARD_SERVER_ERROR: iperf3 -s -1 exited with code ${IPERF_RC}"
fi
