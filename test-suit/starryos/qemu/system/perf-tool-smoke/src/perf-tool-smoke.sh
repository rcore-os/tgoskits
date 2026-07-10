#!/bin/sh
# Spike harness: run upstream `perf` on StarryOS and echo everything so the
# serial log shows exactly what works and what breaks. Never fails the group
# (exit 0) — this is a diagnostic smoke, not a gate yet.
P=/usr/bin/perf

echo "PERF_SMOKE_BEGIN"
echo "== uname =="; uname -a 2>&1
echo "== /proc/sys/kernel/perf_event_paranoid =="; cat /proc/sys/kernel/perf_event_paranoid 2>&1
echo "== /sys/bus/event_source/devices =="; ls /sys/bus/event_source/devices/ 2>&1
echo "== perf --version =="; "$P" --version 2>&1
echo "== perf list (head) =="; "$P" list 2>&1 | head -25
echo "== perf stat true =="; "$P" stat true 2>&1
echo "== perf stat -e cycles,instructions true =="; "$P" stat -e cycles,instructions true 2>&1
echo "== perf record -o /tmp/p.data true =="; "$P" record -o /tmp/p.data -- /bin/true 2>&1; echo "record rc=$?"
echo "== perf.data =="; ls -la /tmp/p.data 2>&1
echo "== perf report --stdio =="; "$P" report -i /tmp/p.data --stdio 2>&1 | head -15
echo "PERF_SMOKE_END"
exit 0
