#!/bin/sh
set -u

duration=10
omit=2
block_size=128K
rounds=3
cooldown=15
result_dir=${TMPDIR:-/tmp}/starry-iperf3-bench
summary_file=$result_dir/summary

fail() {
    printf '\niperf3-bench: %s\n' "$1" >&2
    echo STARRY_IPERF3_BENCH_FAILED
    exit 1
}

read_receiver_mbps() {
    awk -v role="$1" '
        $NF == "receiver" && (role == "" || index($0, "[" role "]") != 0) {
            for (field = 2; field <= NF; field++) {
                unit = $field
                if (unit == "bits/sec" || unit == "Kbits/sec" ||
                    unit == "Mbits/sec" || unit == "Gbits/sec") {
                    rate = $(field - 1)
                    if (unit == "bits/sec") {
                        rate /= 1000000
                    } else if (unit == "Kbits/sec") {
                        rate /= 1000
                    } else if (unit == "Gbits/sec") {
                        rate *= 1000
                    }
                    receiver_mbps = rate
                }
            }
        }
        END {
            if (receiver_mbps != "") {
                printf "%.3f\n", receiver_mbps
            }
        }
    ' "$2"
}

print_command() {
    case "$case_mode" in
        tx) case_direction=TX ;;
        rx) case_direction=RX ;;
        bidir) case_direction=TX/RX ;;
    esac

    printf '类别：%s %s\n' "$case_category" "$case_direction"
    printf 'Command: iperf3 -c %s -t %s -O %s -P %s -l %s' \
        "$server_ip" "$duration" "$omit" "$case_streams" "$block_size"
    case "$case_mode" in
        rx) printf ' -R' ;;
        bidir) printf ' --bidir' ;;
    esac
    printf '\n\n'
}

run_iperf() {
    case "$case_mode" in
        tx)
            iperf3 -c "$server_ip" -t "$duration" -O "$omit" \
                -P "$case_streams" -l "$block_size"
            ;;
        rx)
            iperf3 -c "$server_ip" -t "$duration" -O "$omit" \
                -P "$case_streams" -l "$block_size" -R
            ;;
        bidir)
            iperf3 -c "$server_ip" -t "$duration" -O "$omit" \
                -P "$case_streams" -l "$block_size" --bidir
            ;;
    esac
}

record_rate() {
    record_direction=$1
    record_role=$2
    record_mbps=$(read_receiver_mbps "$record_role" "$round_result")
    [ -n "$record_mbps" ] || round_failed "$round_result"

    echo "$record_mbps" >>"$result_dir/$case_id-$record_direction.samples"
    printf 'Result  %-6s %10s Mbps\n' \
        "DUT $record_direction:" "$record_mbps"
}

round_failed() {
    fail "$case_id round $round did not produce a complete result"
}

run_round() {
    round=$1
    round_result=$result_dir/$case_id-$round.txt

    printf 'Run %s/%s\n\n' "$round" "$rounds"
    run_iperf 2>&1 | tee "$round_result"
    if ! grep -q '^iperf Done\.[[:space:]]*$' "$round_result"; then
        round_failed
    fi

    case "$case_mode" in
        tx) record_rate TX "" ;;
        rx) record_rate RX "" ;;
        bidir)
            record_rate TX TX-C
            record_rate RX RX-C
            ;;
    esac
    printf '\n'

    printf 'Cooldown: %s seconds\n\n' "$cooldown"
    sleep "$cooldown"
}

summarize_direction() {
    summary_direction=$1
    case "$summary_direction" in
        TX) summary_direction_lower=tx ;;
        RX) summary_direction_lower=rx ;;
    esac
    summary_samples=$result_dir/$case_id-$summary_direction.samples
    summary_run_1=$(sed -n '1p' "$summary_samples")
    summary_run_2=$(sed -n '2p' "$summary_samples")
    summary_run_3=$(sed -n '3p' "$summary_samples")
    summary_median=$(sort -n "$summary_samples" | sed -n '2p')

    printf '\nMedian %-6s %10s Mbps\n' "DUT $summary_direction:" "$summary_median"
    printf '%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$case_id" "$case_category" "$case_label" "$summary_direction" \
        "$summary_run_1" "$summary_run_2" "$summary_run_3" "$summary_median" \
        >>"$summary_file"
    printf 'STARRY_IPERF3_BENCH_RESULT case=%s direction=%s median_mbps=%s\n' \
        "$case_id" "$summary_direction_lower" "$summary_median"
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
    printf '============================================================\n\n'
    print_command

    round=1
    while [ "$round" -le "$rounds" ]; do
        run_round "$round"
        round=$((round + 1))
    done

    case "$case_mode" in
        tx) summarize_direction TX ;;
        rx) summarize_direction RX ;;
        bidir)
            summarize_direction TX
            summarize_direction RX
            ;;
    esac
}

print_summary() {
    printf '\n============================================================\n'
    printf 'iperf3 benchmark summary (Mbps)\n'
    printf '============================================================\n\n'
    printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
        Case Category Scenario Dir Run1 Run2 Run3 Median
    printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
        ---- -------- ----------------------------- ---- \
        ---------- ---------- ---------- ----------

    while IFS='|' read -r summary_case summary_category summary_label \
        summary_direction summary_run_1 summary_run_2 summary_run_3 \
        summary_median; do
        printf '%-4s %-8s %-29s %-4s %10s %10s %10s %10s\n' \
            "$summary_case" "$summary_category" "$summary_label" \
            "$summary_direction" \
            "$summary_run_1" "$summary_run_2" "$summary_run_3" "$summary_median"
    done <"$summary_file"

    printf '\n注：TX 表示板端发送、宿主机接收；RX 表示宿主机发送、板端接收。\n'
    printf '\nSTARRY_IPERF3_BENCH_PASSED\n'
}

main() {
    if [ "$#" -ne 1 ] || [ -z "$1" ]; then
        fail "usage: $0 <server-ip>"
    fi
    command -v iperf3 >/dev/null 2>&1 || fail "iperf3 is not installed"

    server_ip=$1
    mkdir -p "$result_dir" || fail "cannot create $result_dir"
    : >"$summary_file"

    printf '\niperf3 benchmark\n'
    printf 'Server: %s:5201\n' "$server_ip"
    printf 'Profile: 10 seconds, 2-second omit, 128K block, 3 rounds\n'
    printf 'Isolation: %s-second cooldown after every connection\n' "$cooldown"

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
