# resume844: PMUv3 sample-IP feasibility for the wake window

This is a read-only feasibility audit on source
`69a33650763538692fafea27c869870ed0313642`; no kernel or board state
was changed. A static AArch64 user-space smoke binary was compiled from the
repository's existing `perf-hw-sample.c` (SHA-256
`3ed781ff282ff8009d510597208a4778357a3d56b3b9f7d975767e07c0c6c5d6`)
but not run. It is not an acceptance result.

Starry's `perf_event_open`/PMUv3 backend can record EL1 IP, monotonic time,
CPU and period. The OrangePi DTB has `arm,armv8-pmuv3` and PPI 7; the G ELF
contains `pmu_overflow_handler`. Existing `resume725` already obtained
492/493/491 kernel samples on Plus-1 with zero lost records. However, its
sampled p99 was near 29 us and 67.3% of samples landed immediately after
three IRQ-unmask instructions. This is delayed PMU IRQ delivery, not evidence
that those instructions consume most cycles. The frozen benchmark also does
not export per-handoff interval endpoints, and per-attempt event enable/disable
would change the 10-16 us interval substantially.

Therefore repeating whole-case sample-IP collection is a no-go as the next
source-optimization locator for the issue's forward wake window. It may
diagnose aggregate CPU use, but cannot establish a removable multi-us cost,
and its p50/p99 cannot enter full20 acceptance. A future window attribution
attempt must first expose exact per-attempt boundaries with bounded overhead
and demonstrate that attribution remains valid under IRQ masking; do not
relabel unmask-PC samples as instruction hotspots.

The apparently simpler `sched_switch` boundary has also already been paired
with wake events in `resume738/739`: the instrumented OTHER wake-done-to-switch
interval was about 4.9 us and rq-timestamp-to-switch about 4.2 us. Those
measurements include trace overhead and did not produce a removable source
operation. Repeating the same tracepoint without a new causal discriminator
would duplicate that work.

The read-only source audit and minimal consumer options are in
`subagent/final.txt`; source-matched older board evidence is in
`resume725-pmu-evidence2/decision.md`. This experiment does not supersede
the latest valid G1/G2 full20 status of 11/20 at 90%.
