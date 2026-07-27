#!/bin/sh
set -u

fail() {
    echo STARRY_IPERF_SMOKE_FAILED
    exit 1
}

[ "$#" -eq 2 ] || fail
server_ip=$1
server_port=$2

command -v iperf3 >/dev/null 2>&1 || fail

result=/tmp/iperf-smoke.json
if ! iperf3 --client "$server_ip" --port "$server_port" --udp --bitrate 1M --time 2 --json >"$result" 2>&1; then
    cat "$result"
    fail
fi

[ -s "$result" ] || fail
cat "$result"
echo STARRY_IPERF_SMOKE_OK
