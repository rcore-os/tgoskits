#!/bin/sh
# Isolation smoke: run perf incrementally so a hang localizes to one step.
P=/usr/bin/perf
echo "PERF_SMOKE_BEGIN"
echo "S1 version";  "$P" --version 2>&1; echo "S1_DONE"
echo "S2 list";     "$P" list 2>&1 | head -3; echo "S2_DONE"
echo "S3 stat";     "$P" stat true 2>&1 | tail -8; echo "S3_DONE"
echo "S4 record";   "$P" record -o /tmp/p.data -- /bin/true 2>&1 | tail -3; echo "S4_DONE"
echo "S5 report";   "$P" report -i /tmp/p.data --stdio 2>&1 | head -12; echo "S5_DONE"
echo "PERF_SMOKE_END"
exit 0
