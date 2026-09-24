#!/usr/bin/env bash
set -euo pipefail

command -v iperf
command -v ss
command -v flock

# Reserve a free host port while competing board jobs are starting. The lock
# is released as soon as this server is listening, not after the board test.
exec 9>/tmp/starry-iperf2-port.lock
flock -x 9
port=
for candidate in {15001..15064}; do
  if ! ss -H -lnt "( sport = :$candidate )" | grep -q LISTEN; then
    port=$candidate
    break
  fi
done
if [[ -z "$port" ]]; then
  echo "no free iperf2 server port in 15001..15064" >&2
  exit 1
fi

server_log="${RUNNER_TEMP:-/tmp}/starry-iperf2-${port}.log"
iperf --server --port "$port" --interval 1 >"$server_log" 2>&1 &
server_pid=$!

cleanup() {
  status=$?
  if ((status != 0)); then
    tail -n 80 "$server_log" || true
  fi
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
}
trap cleanup EXIT

ready=0
for _ in {1..50}; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "iperf2 server exited before listening on port $port" >&2
    exit 1
  fi
  if ss -H -lnt "( sport = :$port )" | grep -q LISTEN; then
    ready=1
    break
  fi
  sleep 0.1
done
if ((ready == 0)); then
  echo "iperf2 server did not listen on port $port" >&2
  exit 1
fi
flock -u 9
exec 9>&-

echo "iperf2 board server ready on TCP $port"
STARRY_IPERF_PORT="$port" "$@"
