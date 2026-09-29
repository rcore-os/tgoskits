# resume862: same-CPU futex hit-stage diagnostic

Control source is `b292a098bb60ef604e7677c37cd95d926ff08200` on
`dev@714accd8f636c540b2c3554b0b1e5cb885be42a4`. Apply only a temporary
`qperf-metrics` probe. It must not change futex selection, lock order, wake
publication or production code, and must be removed after evidence capture.
Use a no-PGO, cpufreq-off AArch64 board image and the frozen benchmark SHA256
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

Measure one `ResolvedFutex::wake` transaction with six ordered clock reads:
entry, after bucket selection/pending hint, after acquiring bucket lock,
after waiter collection, after releasing bucket lock, after `wake_batch`.
Only selected-one and enqueued-one calls enter the five stage totals; record
separate counts for skipped, zero-selected, coalesced and multi-selected calls.
Each stage has one denominator, and a snapshot before and after each process
round must show equal stage counts and monotonic totals. Clock reads and the
probe change the observed durations. In particular, `wake_batch` may include
preemption/scheduling before returning, so its span is not exclusive scheduler
work. No stage total or mean may be subtracted from an uninstrumented p50.

Run in a fixed order on one OrangePi-5-Plus-1 boot: OTHER/FIFO/OTHER/FIFO/
OTHER, each as a separate `thread_futex_same_cpu` process (1000 warmups,
20000 attempts). Save original serial, process logs, before/after debugfs
snapshots, board session and image/benchmark SHA. A round is valid only if
exit is zero, 20000/20000 samples, zero `not_parked` and missed deadlines,
correct case/policy/CPU/PLL, and internally consistent counters. Do not splice
valid rows from an invalid process or select favorable rounds. If either
policy has fewer than two valid process rounds, report only raw diagnostics,
not a policy comparison. If all three OTHER rounds are invalid, stop and
reassess settle or protocol rather than repeating the same frozen run.

Report counts and probe-inclusive per-call means for the five stages, plus
coverage relative to attempted samples. Gate and done futex wake calls may
both enter totals; they are not separated by generation, although a
selected-one transaction's five stages refer to the same syscall event.
The separate qperf direct-wake attempt/activation counts can only give a
conservative branch bound when a round itself is valid; they do not time an
on-rq branch. If bucket/hash/hint/collect are collectively sub-microsecond
and the batch span dominates, follow existing switch/return diagnostics,
not a speculative bucket or fence edit. Only an identified source operation
with a semantic argument merits a later no-instrumentation A/B and full20.
