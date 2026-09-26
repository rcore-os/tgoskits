# resume855: Fair two-peer yield fast-path audit

Read-only audit of `69a33650763538692fafea27c869870ed0313642`.
The independent source report is `subagent/final.txt`; it exited successfully.
No source, image, board session, or full20 result changed.

`yield_current_rq_owned` requeues the Fair current through
`CpuRunQueueState::put_prev_unlinked_current` before `pick_owner_next_in_rq`
selects the next dispatch. The current is deliberately outside the Fair tree;
EEVDF yield request, weighted virtual time, member/index and tree state,
eligibility, slice protection, and publication must still be kept coherent.
A two-peer specialization cannot skip this state transition merely because
the eventual winner looks predictable. The historical retained-node
representation in resume135/136 regressed native performance, and the
small-tree search/virtual-time-refresh directions have already been screened.
The resume854 stage contrast localizes work, but does not establish an
equivalent multi-microsecond operation to remove. Decision: no candidate
patch from this audit; do not repeat retained-node or V-refresh trials.

The scout's numerical upper bound is **not accepted**. It scales
probe-inclusive yield-rq means by a ratio of unrelated native and instrumented
p50s, then treats yield as the worst 58% row; the worst current G1/G2 row is
OTHER `thread_futex_same_cpu`, while OTHER yield has a different p50 and
workload path. Neither cross-case subtraction nor that scaling is a causal
per-attempt bound. Its reference to historical 18/20 PGO does not supersede
the frequency-matched G1/G2 result of 11/20 at 90%, worst 58.00%.

The suggested selected-task hrtick derivation reuse is only a hypothesis.
`finish_owner_selection` may service balance and rederive the local timer
from the committed rq observation; pick-time entity data cannot be reused
without proving identical scheduler/runtime deadline values and publication
ordering across balance and timer-head changes. A source-level proof and
focused diagnostic would be needed before any implementation or native A/B.
No new acceptance result or retained performance optimization exists.
