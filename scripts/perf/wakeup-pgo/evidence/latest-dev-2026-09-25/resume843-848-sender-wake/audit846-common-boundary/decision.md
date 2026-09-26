# resume846: common wake-to-switch boundary audit

Read-only source and existing-matrix audit at `69a33650763538692fafea27c869870ed0313642`,
base `dev@05175ca38823b631a73777b0130226ddfa558439`. Independent report:
`subagent/final.txt`. No source, build, board, or native acceptance result changed.

The G1/G2 two-boot medians in `resume819-weighted-profile-repeat/analysis.json`
are FIFO yield 4083 ns, FIFO same-CPU futex 11958 ns, OTHER yield 7583 ns,
and OTHER same-CPU futex 14583.5 ns. Relative to Linux RT, the *difference
of the yield and futex gaps* is 3208 ns (FIFO) and 3209.5 ns (OTHER).
This is a useful shape discriminator, not causal attribution: the yield and
futex cases execute different paths. The report's statement that every row
without a blocked-task wake passes is overbroad: OTHER yield fails. It also
listed individual G1 timer/cross-CPU values rather than G1/G2 medians in a
table, so that table must not replace the checked `analysis.json` matrix.

The source audit found no safely removable multi-microsecond operation common
to all nine failures. In particular, ordinary Fair/FIFO preemption uses the
single owner-rq transaction, not the migration/Deadline second transaction;
the task/rq wake ownership and switch remain required. Existing old-base
`memcpy` and leaf probe figures cannot support a current-head candidate.

Next discriminator: on the unchanged G image and frozen Linux RT image, time
one private futex wake of an actually parked same-CPU lower-priority receiver,
with a same-binary empty-wake control. FIFO sender priority 80 and receiver
priority 1 should keep the sender running through the timed syscall. Verify
`wake == 1`, all samples, and no scheduling preemption before treating the
result as sender-side activation. This is a new diagnostic use case, *not* a
full20 acceptance row; perform it as a separate numbered attempt.

Latest valid uninstrumented full20 remains 11/20 at 90%, worst 58.00%.
