#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
output_dir=${1:-"$workspace/tmp/competition/ivc/restart-recovery"}
port=${IVC_PORT:-45501}
toolchain=nightly-2026-07-15

mkdir -p "$output_dir"
cd "$workspace"
cargo "+$toolchain" build -p ivcproto
controller="$workspace/target/debug/ivcproto"

"$controller" rtos-sim "127.0.0.1:$port" 2 0 >"$output_dir/rtos.log" 2>&1 &
server_pid=$!
cleanup() {
    if kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid"
    fi
}
trap cleanup EXIT

sleep 0.2
"$controller" controller "127.0.0.1:$port" 1 manual 0 101 \
    >"$output_dir/controller-first.log" 2>&1
"$controller" controller "127.0.0.1:$port" 1 neural 0 202 \
    >"$output_dir/controller-restarted.log" 2>&1
wait "$server_pid"
trap - EXIT

grep -Eq 'acknowledged=1 errors=0 timeouts=0' "$output_dir/controller-first.log"
grep -Eq 'acknowledged=1 errors=0 timeouts=0' "$output_dir/controller-restarted.log"
grep -Eq 'accepted=2 .*session_resets=1 session_rejections=0 .*protocol_errors=0' \
    "$output_dir/rtos.log"

grep 'IVC-CONTROLLER-RESULT' "$output_dir/controller-first.log"
grep 'IVC-CONTROLLER-RESULT' "$output_dir/controller-restarted.log"
grep 'IVC-RTOS-RESULT' "$output_dir/rtos.log"
sha256sum "$output_dir/controller-first.log" "$output_dir/controller-restarted.log" \
    "$output_dir/rtos.log"
