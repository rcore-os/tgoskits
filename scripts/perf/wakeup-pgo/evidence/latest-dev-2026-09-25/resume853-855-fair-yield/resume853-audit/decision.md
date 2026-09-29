# resume853: park/switch transaction audit

This was a read-only audit of `69a33650763538692fafea27c869870ed0313642`
against `dev@05175ca38823b631a73777b0130226ddfa558439`. The independent
report is `subagent/final.txt`; its process exited successfully. No source,
image, board session, or full20 result changed.

The sender wake and subsequent schedule/park are different owner-rq
transactions. Fair current accounting occurs at different clock instants;
task and rq ownership, publication, and next-task selection cannot be
carried across the intervening window without invalidation for remote
wake, timer IRQ, migration, PI and policy changes. The audit found one
potentially redundant Fair virtual-time recomputation within a single
selection transaction, but existing stage data bound this to a small
fraction of a microsecond and the direction overlaps rejected `exp120`.
There is no supported multi-microsecond source candidate from this audit.

The report's roughly additive split of the OTHER same-CPU futex gap into
"wake" and "Fair switch" is not a causal decomposition: full20 yield and
futex cases use different user and kernel paths, and their marginal p50s
are not paired per-attempt components. Its claim that forced FIFO81 removes
the sender's measured park is also a source inference, not established by
that benchmark. Keep the checked G1/G2 matrix and the `resume846` boundary
instead of treating those subtractions as removable costs.

One distinct next question remains: current source already has qperf
yield-rq slots 0/1/2/4/6, while `resume777` measured OTHER futex preemption
leaves rather than a within-image FIFO/OTHER yield pair. A separate
pre-registered diagnostic could measure both `sched_yield_handoff` modes
on one qperf image and compare stage counts/means, excluding invalid
rounds. It would only select a code region for a future semantics-preserving
candidate, not prove a native p50 gain. Avoid reusing the old qperf image
as an exact-head result because it predates the current `memset` commit.

Decision: diagnostic only. No production change or new acceptance result.
Latest valid uninstrumented G1/G2 remains 11/20 at the Linux RT 90% gate,
worst OTHER same-CPU futex 58.00%; all tail and three-boot gates remain open.
