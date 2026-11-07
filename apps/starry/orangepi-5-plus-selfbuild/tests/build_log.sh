#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
work="$(mktemp -d -p "$repo_root/tmp" build-log.XXXXXX)"
trap 'rm -r -- "$work"' EXIT

# Exercise the production logger with real pipes and processes. Completion
# must flush output and reap the sampler even when its first interval is pending.
set +e
timeout 5 bash -c '
    set -eu
    source "$1/selfbuild_log.sh"
    begin_build_log "$2/run.log" LOG-TEST
    start_progress_sampler test "$(date +%s)" 0
    # Wait until the real sampler has execed Python or created a child.
    # Record owned children too: killing a shell alone can orphan its sleep.
    children=""
    for ((attempt = 0; attempt < 10000; attempt++)); do
        comm=$(cat "/proc/$progress_pid/comm")
        children=$(cat "/proc/$progress_pid/task/$progress_pid/children")
        case "$comm" in python*) break ;; esac
        [ -z "$children" ] || break
    done
    printf "%s %s %s\n" "$progress_pid" "$logger_pid" "$children" > "$2/processes"
    printf "   Compiling first v1\n   Compiling first v2\n   Compiling second v1\n"
    emit_progress test "$(date +%s)" 0
    printf "last log line\n"
    exit 17
' build-log "$app_dir" "$work" > "$work/outer.log" 2>&1
status=$?
set -e
[ "$status" = 17 ] || { cat "$work/outer.log"; echo "logger changed workload status: $status"; exit 1; }
grep -q 'phase_compile_units=3 total_compile_units=3 distinct_crates=2===' "$work/run.log"
[ "$(tail -n 1 "$work/run.log")" = 'last log line' ]
cmp "$work/run.log" "$work/outer.log"
for pid in $(cat "$work/processes"); do
    if kill -0 "$pid" 2> "$work/process-status"; then
        echo "logger or progress descendant survived workload exit: $pid" >&2
        exit 1
    fi
done
echo 'orangepi5plus_selfbuild_log=PASS'
