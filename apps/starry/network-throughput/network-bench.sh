#!/bin/sh
set -u

duration=${STARRY_NETWORK_BENCH_DURATION:-10}
cooldown=${STARRY_NETWORK_BENCH_COOLDOWN:-15}
rounds=3
result_dir=${TMPDIR:-/tmp}/starry-network-bench
summary_file=$result_dir/summary
active_pids=

cleanup() {
    for cleanup_pid in $active_pids; do kill "$cleanup_pid" 2>/dev/null || :; done
    for cleanup_pid in $active_pids; do wait "$cleanup_pid" 2>/dev/null || :; done
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fail() {
    printf '\nnetwork-bench: %s\n' "$1" >&2
    echo STARRY_NETWORK_BENCH_FAILED
    exit 1
}

round_failed() { fail "$case_id round $round: $1 (see $round_dir)"; }

parse_direction() {
    awk -v wanted="$2" '
        {
            key = "\"" wanted "\":{"
            start = index($0, key)
            if (!start) exit 1
            body = substr($0, start + length(key))
            last = index(body, "}")
            if (!last) exit 1
            count = split(substr(body, 1, last - 1), fields, ",")
            for (i = 1; i <= count; i++) {
                separator = index(fields[i], ":")
                if (!separator) continue
                name = substr(fields[i], 1, separator - 1)
                value = substr(fields[i], separator + 1)
                gsub(/["[:space:]]/, "", name)
                gsub(/["[:space:]]/, "", value)
                if (name == "status") status = value
                if (name == "bytes") bytes = value
                if (name == "elapsed_ms") elapsed = value
            }
            if (status == "" || bytes !~ /^[0-9]+$/ || elapsed !~ /^[0-9]+$/) exit 1
            print status, bytes, elapsed
            exit
        }
    ' "$1"
}

create_test() {
    created=$(curl -fsS --connect-timeout 5 --max-time 20 \
        -X POST "$base_url/v1/tests") || return 1
    created_id=$(printf '%s\n' "$created" |
        sed -n 's/.*"test_id":"\([0-9a-fA-F-]*\)".*/\1/p')
    case "$created_id" in
        ????????-????-????-????-????????????) printf '%s\n' "$created_id" ;;
        *) return 1 ;;
    esac
}

start_transfer() {
    start_index=$1
    start_id=$2
    start_direction=$3
    start_prefix=$round_dir/$start_index-$start_direction
    if [ "$start_direction" = TX ]; then
        (
            "$upload_source" "$duration" 2>"$start_prefix.source.err"
            printf '%s\n' "$?" >"$start_prefix.source.status"
        ) | curl -fsS --connect-timeout 5 --max-time "$request_timeout" \
            -T - -o "$start_prefix.response" -w '%{size_upload}\n' \
            "$base_url/v1/tests/$start_id/upload" \
            >"$start_prefix.client_bytes" 2>"$start_prefix.curl.err" &
    else
        curl -fsS --connect-timeout 5 --max-time "$request_timeout" \
            -o /dev/null -w '%{size_download}\n' \
            "$base_url/v1/tests/$start_id/download?duration_secs=$duration" \
            >"$start_prefix.client_bytes" 2>"$start_prefix.curl.err" &
    fi
    start_pid=$!
    printf '%s\n' "$start_pid" >>"$round_dir/pids"
    active_pids="$active_pids $start_pid"
}

query_test() {
    curl -fsS --connect-timeout 5 --max-time 20 \
        "$base_url/v1/tests/$1" -o "$2"
}

snapshot_fields() {
    fields=$(parse_direction "$1" "$2") || round_failed "invalid $2 snapshot"
    set -- $fields
    snapshot_status=$1
    snapshot_bytes=$2
    snapshot_ms=$3
}

record_direction() {
    result_direction=$1
    if [ "$result_direction" = TX ]; then result_api_direction=upload
    else result_api_direction=download; fi
    : >"$round_dir/$result_direction.rates"
    while read -r result_index result_id; do
        snapshot_fields "$round_dir/$result_index.final.json" "$result_api_direction"
        [ "$snapshot_status" = completed ] ||
            round_failed "$result_direction stream $result_index ended as $snapshot_status"
        result_after_bytes=$snapshot_bytes
        result_after_ms=$snapshot_ms

        result_wire_bytes=$(cat "$round_dir/$result_index-$result_direction.client_bytes") ||
            round_failed "missing curl byte count"
        case "$result_wire_bytes" in
            ''|*[!0-9]*) round_failed "invalid curl byte count: $result_wire_bytes" ;;
        esac
        if [ "$result_direction" = TX ]; then
            result_source_status=$(cat "$round_dir/$result_index-TX.source.status") ||
                round_failed "missing upload source status"
            [ "$result_source_status" = 0 ] ||
                round_failed "upload source exited $result_source_status"
            result_client_bytes=$(sed -n 's/^BYTES=\([0-9][0-9]*\)$/\1/p' \
                "$round_dir/$result_index-TX.source.err") ||
                round_failed "missing upload payload count"
            case "$result_client_bytes" in
                ''|*[!0-9]*) round_failed "invalid upload payload count: $result_client_bytes" ;;
            esac
        else
            result_client_bytes=$result_wire_bytes
        fi
        [ "$result_client_bytes" = "$result_after_bytes" ] ||
            round_failed "$result_direction stream $result_index client/server bytes differ: $result_client_bytes/$result_after_bytes"
        [ "$result_client_bytes" != 0 ] ||
            round_failed "$result_direction stream $result_index transferred no bytes"
        result_rate=$(awk -v bytes="$result_after_bytes" \
            -v milliseconds="$result_after_ms" 'BEGIN {
                if (bytes <= 0 || milliseconds <= 0) exit 1
                printf "%.6f\n", bytes * 8 / (milliseconds * 1000)
            }') || round_failed "$result_direction stream $result_index has no measurable transfer"
        printf '%s\n' "$result_rate" >>"$round_dir/$result_direction.rates"
        printf '  %s stream %s: client/server=%s bytes, average=%s Mbps\n' \
            "$result_direction" "$result_index" "$result_client_bytes" "$result_rate"
    done <"$round_dir/ids"
    result_total=$(awk '{ sum += $1 } END { printf "%.3f\n", sum }' \
        "$round_dir/$result_direction.rates")
    printf '%s\n' "$result_total" >>"$result_dir/$case_id-$result_direction.samples"
    printf 'Result  DUT %-2s: %10s Mbps\n' "$result_direction" "$result_total"
}

run_round() {
    round=$1
    round_dir=$result_dir/$case_id-$round
    mkdir -p "$round_dir" || fail "cannot create $round_dir"
    : >"$round_dir/ids"
    : >"$round_dir/pids"
    printf 'Run %s/%s\n' "$round" "$rounds"
    stream_index=1
    while [ "$stream_index" -le "$case_streams" ]; do
        stream_id=$(create_test) || round_failed "cannot create test ID"
        printf '%s %s\n' "$stream_index" "$stream_id" >>"$round_dir/ids"
        stream_index=$((stream_index + 1))
    done
    while read -r stream_index stream_id; do
        case "$case_mode" in tx|bidir) start_transfer "$stream_index" "$stream_id" TX ;; esac
        case "$case_mode" in rx|bidir) start_transfer "$stream_index" "$stream_id" RX ;; esac
    done <"$round_dir/ids"
    while read -r stream_pid; do
        wait "$stream_pid" || round_failed "curl stream $stream_pid failed"
    done <"$round_dir/pids"
    active_pids=
    while read -r stream_index stream_id; do
        query_test "$stream_id" "$round_dir/$stream_index.final.json" ||
            round_failed "cannot query final result"
    done <"$round_dir/ids"
    case "$case_mode" in
        tx) record_direction TX ;;
        rx) record_direction RX ;;
        bidir) record_direction TX; record_direction RX ;;
    esac
    printf '\nCooldown: %s seconds\n\n' "$cooldown"
    sleep "$cooldown"
}

summarize_direction() {
    summary_direction=$1
    case "$summary_direction" in TX) summary_lower=tx ;; RX) summary_lower=rx ;; esac
    summary_samples=$result_dir/$case_id-$summary_direction.samples
    summary_run_1=$(sed -n '1p' "$summary_samples")
    summary_run_2=$(sed -n '2p' "$summary_samples")
    summary_run_3=$(sed -n '3p' "$summary_samples")
    summary_median=$(sort -n "$summary_samples" | sed -n '2p')
    printf '\nMedian DUT %-2s: %10s Mbps\n' "$summary_direction" "$summary_median"
    printf '%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$case_id" "$case_category" "$case_label" "$summary_direction" \
        "$summary_run_1" "$summary_run_2" "$summary_run_3" "$summary_median" >>"$summary_file"
    printf 'STARRY_NETWORK_BENCH_RESULT case=%s direction=%s median_mbps=%s\n' \
        "$case_id" "$summary_lower" "$summary_median"
}

run_case() {
    case_id=$1
    case_category=$2
    case_label=$3
    case_mode=$4
    case_streams=$5
    : >"$result_dir/$case_id-TX.samples"
    : >"$result_dir/$case_id-RX.samples"
    printf '\n============================================================\n'
    printf '%s  %s\n' "$case_id" "$case_label"
    printf '============================================================\n'
    printf 'Mode: %s, streams per direction: %s\n\n' "$case_mode" "$case_streams"
    round=1
    while [ "$round" -le "$rounds" ]; do run_round "$round"; round=$((round + 1)); done
    case "$case_mode" in
        tx) summarize_direction TX ;;
        rx) summarize_direction RX ;;
        bidir) summarize_direction TX; summarize_direction RX ;;
    esac
}

print_summary() {
    printf '\n============================================================\n'
    printf 'HTTP network throughput benchmark summary (Mbps)\n'
    printf '============================================================\n\n'
    printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
        Case Category Scenario Dir Run1 Run2 Run3 Median
    printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
        ---- -------- ----------------------------- ---- \
        ---------- ---------- ---------- ----------
    while IFS='|' read -r s_case s_category s_label s_direction s_run1 s_run2 s_run3 s_median; do
        printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
            "$s_case" "$s_category" "$s_label" "$s_direction" \
            "$s_run1" "$s_run2" "$s_run3" "$s_median"
    done <"$summary_file"
    printf '\n注：TX 表示板端发送、宿主机接收；RX 表示宿主机发送、板端接收。\n'
    printf '\nSTARRY_NETWORK_BENCH_PASSED\n'
}

main() {
    if [ "$#" -ne 2 ] || [ -z "$1" ] || [ ! -x "$2" ]; then
        fail "usage: $0 <server-ip> <upload-source>"
    fi
    command -v curl >/dev/null 2>&1 || fail "curl is not installed"
    case "$duration:$cooldown" in *[!0-9:]*|'') fail "invalid timing profile" ;; esac
    [ "$duration" -gt 0 ] && [ "$duration" -le 3600 ] ||
        fail "duration must be between 1 and 3600 seconds"
    server_ip=$1
    upload_source=$2
    base_url=http://$server_ip:3000
    request_timeout=$((duration + 60))
    mkdir -p "$result_dir" || fail "cannot create $result_dir"
    : >"$summary_file"
    printf '\nHTTP network throughput benchmark\n'
    printf 'Server: %s\n' "$base_url"
    printf 'Profile: %s seconds, 128K upload blocks, %s rounds\n' \
        "$duration" "$rounds"
    printf 'Isolation: %s-second cooldown after every round\n' "$cooldown"
    run_case T01 "单流单向" "Single-stream DUT TX" tx 1
    run_case T02 "单流单向" "Single-stream DUT RX" rx 1
    run_case T03 "单流双向" "Single-stream bidirectional" bidir 1
    run_case T04 "双流单向" "2-stream DUT TX" tx 2
    run_case T05 "四流单向" "4-stream DUT TX" tx 4
    run_case T06 "八流单向" "8-stream DUT TX" tx 8
    run_case T07 "四流单向" "4-stream DUT RX" rx 4
    print_summary
}

main "$@"
