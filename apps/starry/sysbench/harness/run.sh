#!/bin/sh
# A common workload and output schema for Linux, StarryOS and QEMU.
set -eu
export LC_ALL=C
mode=${1:-board}
case "$mode" in board|matrix|smoke) ;; *) echo 'usage: run.sh [board|matrix|smoke]' >&2; exit 2 ;; esac
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "$mode" = board ]; then
    bench="$dir/sysbench"
    cpus=$("$dir/cpuprobe" --list)
    set -- $cpus
    count=$#
else
    bench="$dir/sysbench"
    cpus=unprobed
    count=4
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
seconds=3
prime=20000
[ "$mode" != smoke ] || seconds=1
printf 'SYSBENCH_BEGIN schema=1 mode=%s seconds=%s prime=%s cpus=%s\n' "$mode" "$seconds" "$prime" "$count"
printf 'ENV uname=%s\n' "$(uname -a)"
printf 'ENV affinity=%s\n' "$cpus"
if [ "$mode" = board ]; then
    printf 'ENV bundle_sha256=%s\n' "${SYSBENCH_BUNDLE_SHA256:?set the archive SHA256 before running}"
    sha256sum "$dir/sysbench.bin" "$dir/cpuprobe" "$dir/membw"
fi
"$bench" --version
run() {
    name=$1
    shift
    printf 'CASE_BEGIN %s\n' "$name"
    if "$@" > "$work/result" 2>&1; then
        cat "$work/result"
    else
        status=$?
        cat "$work/result"
        printf 'SYSBENCH_FAILED case=%s status=%s\n' "$name" "$status"
        exit "$status"
    fi
    # These are completed operations per wall second, not an inferred clock rate.
    metric=$(awk '/events per second:/ {print $NF; exit}' "$work/result")
    if [ -n "$metric" ]; then
        printf 'METRIC %s events_per_second %s\n' "$name" "$metric"
    fi
    printf 'CASE_END %s\n' "$name"
}
if [ "$mode" = board ]; then
    for cpu in $cpus; do
        run "probe-$cpu" "$dir/cpuprobe" "$cpu"
        run "membw-$cpu" "$dir/membw" "$cpu" 32
        run "pinned-$cpu" taskset -c "$cpu" "$bench" cpu --cpu-max-prime="$prime" --threads=1 --time="$seconds" run
    done
fi
for threads in 1 2 4 8; do
    [ "$threads" -le "$count" ] || continue
    run "cpu-$threads" "$bench" cpu --cpu-max-prime="$prime" --threads="$threads" --time="$seconds" run
    [ "$mode" != smoke ] || break
done
if [ "$mode" != smoke ]; then
    run threads "$bench" threads --threads="$count" --time="$seconds" run
    run mutex "$bench" mutex --threads="$count" --mutex-num=64 --mutex-locks=10000 --mutex-loops=100 run
    run memory "$bench" memory --threads="$count" --memory-block-size=1M --memory-total-size=256M --memory-oper=write --memory-access-mode=seq run
fi
printf 'SYSBENCH_DONE\n'
