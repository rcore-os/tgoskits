# resume868: event pairing feasibility audit

Read-only exploration on `b292a098bb` produced an initial design and
a corrected follow-up. No source, image, board run, native full20 result
or performance gain was produced. The exact exploration outputs are
`subagent/final.txt` and `repair/final.txt`.

The useful finding is that the frozen benchmark obtains both A0
(`wake_timestamp_ns`) and A5 (`resumed_ns`) through raw
`SYS_clock_gettime(CLOCK_MONOTONIC)`. Starry's `sys_clock_gettime` can
observe the returned monotonic value and current thread identity without
editing the frozen benchmark. `ResolvedFutex::wake` knows the private
futex key; its local `WakeBatch::push` sees the selected
`ThreadWakeHandle::thread_id()`. The generic `ThreadWakeBatch::wake_all`
does not know the futex key, so the original suggested tag point there
was wrong. The full A0-to-A5 decomposition must include A0->gate
publication as well as publication->claim, claim->switch and
switch->A5. The benchmark prints aggregate stats, not raw A0/A5 pairs,
so aggregate agreement can falsify bad pairing but cannot prove every
individual pair.

The proposed per-CPU single-writer seqlock was rejected: task or IRQ
re-entry can introduce a second writer. The repaired proposal's
`next.fetch_add`-indexed **wrapping** MPSC ring is also insufficient as
written. Unique logical sequence numbers do not prevent two writers
from touching the same physical slot across wrap; a delayed older
writer can publish its sequence after a newer writer has overwritten
fields. A reader could then accept mixed fields under the old sequence.
Before any board probe, use a non-wrapping append-only buffer with a
unique atomic index per event and explicit overflow, or a fully proved
slot claim/release state machine. Every recorded field must be atomic or
exclusive to its reserved slot, and a release-ready flag must be the
only reader publication edge. Diagnostic memory, output size and reset
semantics must be bounded before implementation.

Additional pairing conditions remain: the last sender clock before
gate is A0 only under the benchmark's exact instruction path, which
must be checked in the frozen binary; the first receiver clock after
switch is A5 only for the matching futex return branch. Warmup
`not_parked` is not represented in the printed measured error count,
and a same-offset unrelated futex is possible. The trace must report
all unmatched clocks, selected-zero wakes, other-key pages, migration,
overflows and missing stage transitions, then reject incomplete pairs.
Do not enforce `21000` selected-one gate wakes merely from a printed
`not_parked=0` on 20000 measured attempts: the 1000 warmup attempts are
not covered by that error count.

The prior resume867 data still supports a narrower, verified conclusion:
on two valid OTHER runs the qperf pending observation averaged
178.69/179.05 ns and IRQ unmask to schedule entry 122.00/118.01 ns,
not a multi-microsecond mean. There is no semantically equivalent native
optimization candidate from this audit. Do not implement a guard or
request shortcut on the basis of these measurements. A future trace is
diagnostic only and does not replace same-source, uninstrumented full20
acceptance against the 90% per-row goal.
