#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
output_dir=${1:-"$workspace/tmp/competition/ivc/host-loopback"}
count=${IVC_COUNT:-100}
drop_every=${IVC_DROP_EVERY:-5}
port=${IVC_PORT:-45500}
toolchain=nightly-2026-07-15

mkdir -p "$output_dir"
cd "$workspace"
cargo "+$toolchain" build -p ivcproto
controller="$workspace/target/debug/ivcproto"

"$controller" rtos-sim "127.0.0.1:$port" "$count" "$drop_every" \
    >"$output_dir/rtos.log" 2>&1 &
server_pid=$!
cleanup() {
    if kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid"
    fi
}
trap cleanup EXIT

sleep 0.2
"$controller" controller "127.0.0.1:$port" "$count" neural 0 \
    >"$output_dir/controller.log" 2>&1
wait "$server_pid"
trap - EXIT

grep 'IVC-CONTROLLER-RESULT' "$output_dir/controller.log"
grep 'IVC-RTOS-RESULT' "$output_dir/rtos.log"
sha256sum "$output_dir/controller.log" "$output_dir/rtos.log"
