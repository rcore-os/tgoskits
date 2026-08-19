#!/bin/sh

set -eu

hackbench=/opt/ltp/testcases/bin/hackbench
version_file=/opt/ltp/Version
affinity=/usr/bin/ltp-hackbench-affinity
execution=${1:-smoke}
groups=${LTP_HACKBENCH_GROUPS:-1}
work_dir=
completed=0
failure_reason=unexpected_exit

cleanup_and_report()
{
    status=$?
    trap - EXIT INT TERM
    if [ -n "$work_dir" ] && [ -d "$work_dir" ]; then
        rm -rf -- "$work_dir"
    fi
    if [ "$completed" -ne 1 ]; then
        if [ "$status" -eq 0 ]; then
            status=1
        fi
        printf 'LTP_HACKBENCH_APP_FAILED: %s\n' "$failure_reason" >&2
    fi
    exit "$status"
}

fail()
{
    failure_reason=$1
    exit 1
}

on_signal()
{
    failure_reason=interrupted
    exit 1
}

is_positive_integer()
{
    case "$1" in
        ''|*[!0-9]*|0) return 1 ;;
        *) return 0 ;;
    esac
}

run_case()
{
    task_mode=$1
    case_cpus=$2
    case_phase=$3
    case_round=$4

    if case_output=$("$affinity" run "$case_cpus" "$hackbench" \
        -pipe "$groups" "$task_mode" "$loops" 2>&1); then
        case_status=0
    else
        case_status=$?
    fi
    printf '%s\n' "$case_output"
    if [ "$case_status" -ne 0 ]; then
        fail "hackbench_exit mode=$task_mode cpus=$case_cpus phase=$case_phase round=$case_round status=$case_status"
    fi

    running_count=$(printf '%s\n' "$case_output" \
        | awk '/^Running with .*tasks\.$/ { count++ } END { print count + 0 }')
    if [ "$running_count" -ne 1 ]; then
        fail "unexpected_task_summary mode=$task_mode cpus=$case_cpus phase=$case_phase round=$case_round"
    fi

    if case_elapsed_us=$(printf '%s\n' "$case_output" | awk '
        /^Time: [0-9][0-9]*\.[0-9][0-9]*$/ {
            count++
            split($2, part, ".")
            fraction = part[2] "000000"
            value = part[1] * 1000000 + substr(fraction, 1, 6)
        }
        END {
            if (count != 1 || value <= 0) {
                exit 1
            }
            printf "%.0f\n", value
        }
    '); then
        :
    else
        fail "time_parse_failed mode=$task_mode cpus=$case_cpus phase=$case_phase round=$case_round"
    fi

    last_elapsed_us=$case_elapsed_us
    case "$case_phase" in
        smoke)
            printf 'LTP_HACKBENCH_SMOKE mode=%s cpus=%s elapsed_us=%s\n' \
                "$task_mode" "$case_cpus" "$case_elapsed_us"
            ;;
        warmup)
            printf 'LTP_HACKBENCH_WARMUP mode=%s cpus=%s elapsed_us=%s\n' \
                "$task_mode" "$case_cpus" "$case_elapsed_us"
            ;;
        sample)
            printf 'LTP_HACKBENCH_SAMPLE mode=%s cpus=%s round=%s elapsed_us=%s\n' \
                "$task_mode" "$case_cpus" "$case_round" "$case_elapsed_us"
            ;;
        *) fail "invalid_phase=$case_phase" ;;
    esac
}

run_smoke()
{
    run_case process 1 smoke 0
    run_case process 4 smoke 0
    run_case thread 1 smoke 0
    run_case thread 4 smoke 0
}

summarize()
{
    summary_mode=$1
    summary_cpus=$2
    samples_file=$3
    sample_count=$(wc -l < "$samples_file" | tr -d ' ')
    [ "$sample_count" -eq "$rounds" ] || \
        fail "sample_count mode=$summary_mode cpus=$summary_cpus expected=$rounds actual=$sample_count"

    median_index=$(((rounds + 1) / 2))
    median=$(sort -n "$samples_file" | sed -n "${median_index}p")
    is_positive_integer "$median" || \
        fail "invalid_median mode=$summary_mode cpus=$summary_cpus"
    samples=$(awk '{ printf "%s%s", NR == 1 ? "" : ",", $1 } END { print "" }' \
        "$samples_file")

    printf 'LTP_HACKBENCH_RESULT mode=%s cpus=%s groups=%s loops=%s rounds=%s samples_us=%s median_us=%s\n' \
        "$summary_mode" "$summary_cpus" "$groups" "$loops" "$rounds" \
        "$samples" "$median"
    summary_median=$median
}

run_benchmark_mode()
{
    benchmark_mode=$1
    one_cpu_samples=$work_dir/$benchmark_mode-1.samples
    four_cpu_samples=$work_dir/$benchmark_mode-4.samples
    : > "$one_cpu_samples"
    : > "$four_cpu_samples"

    run_case "$benchmark_mode" 1 warmup 0
    run_case "$benchmark_mode" 4 warmup 0

    round=1
    while [ "$round" -le "$rounds" ]; do
        if [ $((round % 2)) -eq 1 ]; then
            run_case "$benchmark_mode" 1 sample "$round"
            printf '%s\n' "$last_elapsed_us" >> "$one_cpu_samples"
            run_case "$benchmark_mode" 4 sample "$round"
            printf '%s\n' "$last_elapsed_us" >> "$four_cpu_samples"
        else
            run_case "$benchmark_mode" 4 sample "$round"
            printf '%s\n' "$last_elapsed_us" >> "$four_cpu_samples"
            run_case "$benchmark_mode" 1 sample "$round"
            printf '%s\n' "$last_elapsed_us" >> "$one_cpu_samples"
        fi
        round=$((round + 1))
    done

    summarize "$benchmark_mode" 1 "$one_cpu_samples"
    one_cpu_median=$summary_median
    summarize "$benchmark_mode" 4 "$four_cpu_samples"
    four_cpu_median=$summary_median

    speedup_milli=$((one_cpu_median * 1000 / four_cpu_median))
    speedup_whole=$((speedup_milli / 1000))
    speedup_fraction=$((speedup_milli % 1000))
    printf 'LTP_HACKBENCH_SPEEDUP mode=%s one_cpu_median_us=%s four_cpu_median_us=%s speedup_milli=%s speedup=%s.%03dx\n' \
        "$benchmark_mode" "$one_cpu_median" "$four_cpu_median" "$speedup_milli" \
        "$speedup_whole" "$speedup_fraction"
}

trap cleanup_and_report EXIT
trap on_signal INT TERM

if [ "$#" -gt 1 ]; then
    fail "usage=ltp-hackbench-run_[smoke|benchmark]"
fi
case "$execution" in
    smoke)
        loops=${LTP_HACKBENCH_LOOPS:-10}
        ;;
    benchmark)
        loops=${LTP_HACKBENCH_LOOPS:-1000}
        rounds=${LTP_HACKBENCH_ROUNDS:-5}
        ;;
    *) fail "invalid_execution=$execution" ;;
esac

is_positive_integer "$groups" || fail "invalid_groups=$groups"
is_positive_integer "$loops" || fail "invalid_loops=$loops"
if [ "$execution" = benchmark ]; then
    is_positive_integer "$rounds" || fail "invalid_rounds=$rounds"
    if [ "$rounds" -lt 3 ] || [ $((rounds % 2)) -ne 1 ]; then
        fail "rounds_must_be_an_odd_integer_at_least_3"
    fi
fi

[ -x "$hackbench" ] || fail "missing_executable=$hackbench"
[ -r "$version_file" ] || fail "missing_version_file=$version_file"
[ -x "$affinity" ] || fail "missing_executable=$affinity"

raw_version=$(sed -n '1p' "$version_file")
version=$(printf '%s' "$raw_version" | tr -c '[:alnum:]._-' '_')
[ -n "$version" ] || fail "empty_version_file=$version_file"
printf 'LTP_HACKBENCH_SOURCE path=%s version_file=%s version=%s\n' \
    "$hackbench" "$version_file" "$version"
if [ "$execution" = benchmark ]; then
    printf 'LTP_HACKBENCH_CONFIG execution=%s groups=%s loops=%s rounds=%s expected_cpus=4\n' \
        "$execution" "$groups" "$loops" "$rounds"
else
    printf 'LTP_HACKBENCH_CONFIG execution=%s groups=%s loops=%s expected_cpus=4\n' \
        "$execution" "$groups" "$loops"
fi

if ! "$affinity" check 4; then
    fail "topology_or_allowed_cpu_check_failed"
fi

if [ "$execution" = smoke ]; then
    run_smoke
    printf 'LTP_HACKBENCH_SMOKE_PASSED\n'
else
    work_dir=$(mktemp -d /tmp/ltp-hackbench.XXXXXX) || fail "mktemp_failed"
    last_elapsed_us=
    run_benchmark_mode process
    run_benchmark_mode thread
    printf 'LTP_HACKBENCH_APP_PASSED\n'
fi
completed=1
