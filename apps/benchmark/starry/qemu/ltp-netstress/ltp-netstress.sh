#!/bin/sh
set -eu

netstress=/usr/bin/ltp-netstress
mode=${1:-smoke}
work_dir=
server_pid=
completed=0

stop_server() {
    if [ -n "$server_pid" ]; then
        # netstress owns a watchdog child; terminate this invocation's group.
        kill -TERM -- "-$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
        server_pid=
    fi
}

cleanup() {
    status=$?
    trap - EXIT INT TERM
    stop_server
    if [ "$completed" -ne 1 ]; then
        echo "LTP_NETSTRESS_APP_FAILED: status=$status logs=$work_dir" >&2
        [ "$status" -ne 0 ] || status=1
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
    echo "ltp-netstress: $*" >&2
    exit 1
}

case "$mode" in
    smoke) requests=20; rounds=1; warmup=0 ;;
    bench) requests=1000; rounds=3; warmup=1 ;;
    *) fail "usage: $0 [smoke|bench]" ;;
esac
[ -x "$netstress" ] || fail "prebuild must provide $netstress"
[ -r /usr/share/ltp-netstress/Version ] || fail "missing netstress version"
command -v setsid >/dev/null || fail "setsid is required"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ltp-netstress.XXXXXX")
export LTPROOT=/opt/ltp
export TST_ANSI_COLOR=0

printf 'LTP_NETSTRESS_SOURCE version=%s\n' "$(cat /usr/share/ltp-netstress/Version)"
printf 'LTP_NETSTRESS_ENV mode=%s topology=loopback requests_per_client=%s rounds=%s logs=%s\n' \
    "$mode" "$requests" "$rounds" "$work_dir"
uname -a

run_sample() {
    sample_dir=$work_dir/$case_id-$sample
    mkdir "$sample_dir"
    # Read the upstream ephemeral port publication after listen is ready.
    # Keep the server in the foreground so its process group remains ours.
    setsid "$netstress" -T "$protocol" -R "$close_after" \
        >"$sample_dir/server.log" 2>&1 &
    server_pid=$!
    attempt=0
    port=
    while [ "$attempt" -lt 100 ]; do
        if [ "$protocol" = udp ] || grep -q 'Listen on the socket' "$sample_dir/server.log"; then
            port=$(sed -n 's/.*bind to port \([0-9][0-9]*\).*/\1/p' "$sample_dir/server.log")
            [ -z "$port" ] || break
        fi
        kill -0 "$server_pid" 2>/dev/null || break
        sleep 0.1
        attempt=$((attempt + 1))
    done
    if [ -z "$port" ]; then
        cat "$sample_dir/server.log"
        fail "$case_id server did not become ready"
    fi

    if "$netstress" -l -H 127.0.0.1 -g "$port" -T "$protocol" \
        -a "$clients" -r "$requests" -n "$size" -N "$size" \
        -c "$sample_dir/elapsed-ms" >"$sample_dir/client.log" 2>&1; then
        client_status=0
    else
        client_status=$?
    fi
    cat "$sample_dir/client.log"
    stop_server
    [ "$client_status" -eq 0 ] || fail "$case_id client exited $client_status"
    grep -q 'TPASS: test completed' "$sample_dir/client.log" || fail "$case_id missing LTP completion"
    if grep -Eq 'TFAIL:|TBROK:|TCONF:|TWARN:' "$sample_dir/client.log"; then
        fail "$case_id has an unsuccessful LTP result"
    fi
    elapsed=$(cat "$sample_dir/elapsed-ms")
    case "$elapsed" in ''|*[!0-9]*) fail "$case_id invalid elapsed time" ;; esac
    printf 'LTP_NETSTRESS_SAMPLE case=%s sample=%s elapsed_ms=%s clients=%s requests_per_client=%s payload_bytes=%s\n' \
        "$case_id" "$sample" "$elapsed" "$clients" "$requests" "$size"
    if [ "$sample" -gt 0 ]; then
        printf '%s\n' "$elapsed" >>"$work_dir/$case_id.samples"
    fi
}

run_case() {
    case_id=$1; protocol=$2; clients=$3; size=$4; close_after=$5
    : >"$work_dir/$case_id.samples"
    sample=$((1 - warmup))
    while [ "$sample" -le "$rounds" ]; do
        run_sample
        sample=$((sample + 1))
    done
    # Report upstream elapsed time, not inferred wire PPS or packet loss.
    sort -n "$work_dir/$case_id.samples" | awk -v id="$case_id" \
        -v middle="$((rounds / 2 + 1))" \
        'NR == middle { printf "LTP_NETSTRESS_RESULT case=%s median_ms=%s\n", id, $0 }'
}

run_case tcp-rr tcp 1 64 "$((requests + 1))"
run_case tcp-rr-parallel tcp 4 64 "$((requests + 1))"
run_case tcp-large tcp 1 16384 "$((requests + 1))"
run_case tcp-connect-rr tcp 1 64 1
run_case udp-rr udp 1 64 "$((requests + 1))"
run_case udp-rr-parallel udp 4 64 "$((requests + 1))"
completed=1
printf 'LTP_NETSTRESS_APP_PASSED mode=%s\n' "$mode"
