#!/bin/sh
# Isolation smoke: run the real upstream `perf` incrementally so a hang localizes
# to one step. Every step's exit status is captured directly (NOT through a pipe,
# which would mask it); any failure is propagated so a broken `perf` cannot be
# scored as passing. The final success marker is printed only when all steps pass,
# and the script exits non-zero on any failure.
P=/usr/bin/perf
rc=0
out=/tmp/perf-tool-smoke.out

echo "PERF_SMOKE_BEGIN"

# step NAME MAXLINES CMD...  — run CMD, capture its real status, print a bounded
# slice of its output for diagnosis, and flag a failure marker on non-zero.
step() {
    name=$1
    maxlines=$2
    shift 2
    "$@" >"$out" 2>&1
    st=$?
    head -n "$maxlines" "$out"
    if [ "$st" -ne 0 ]; then
        echo "PERF_SMOKE_STEP_FAILED: $name status=$st"
        rc=1
    fi
    echo "${name}_DONE"
}

step S1 3 "$P" --version
step S2 3 "$P" list
step S3 8 "$P" stat true
step S4 3 "$P" record -o /tmp/p.data -- /bin/true
step S5 12 "$P" report -i /tmp/p.data --stdio

rm -f "$out"

if [ "$rc" -eq 0 ]; then
    echo "PERF_SMOKE_PASSED"
else
    echo "PERF_SMOKE_FAILED"
fi
echo "PERF_SMOKE_END"
exit "$rc"
