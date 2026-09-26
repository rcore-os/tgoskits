#!/bin/sh
set -u

bench="${RESUME754_BENCH:?}"
log="${RESUME754_LOG:?}"
: > "$log" || exit 1

# Cover the frozen matrix once, then bias training toward its largest p50 gap.
status=0
"$bench" --policy all --case all >> "$log" 2>&1 || status=$?
for attempt in 1 2 3 4 5 6 7; do
    "$bench" --policy other --case thread_futex_same_cpu >> "$log" 2>&1 || status=$?
done

cat "$log"
echo "RESUME817_WORKLOAD_DONE $status"
exit "$status"
