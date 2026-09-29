# resume903: archived scheduler-rq transaction counts

This is a read-only reanalysis of `b292a098bb` qperf-only board records.
There is no new build, board run, production change, or native p50 result.
The archived `resume854` and `resume889` SHA256 manifests and their check
scripts passed before reusing the counters. All cited rounds have 20,000 of
20,000 samples and zero `not_parked`.

`OwnerRqTxn::begin_scheduler` increments
`owner_rq_scheduler_transactions` after acquiring the rq lock. The ordinary
`yield_current_in_scheduler_frame` path enters via this constructor, so each
`context_switches_yield` consumes at least one such transaction. The valid
OTHER `resume854` rounds show:

| Round | Scheduler-rq transactions | Yield switches | Other transactions at most |
| --- | ---: | ---: | ---: |
| 2 | 42,739 | 42,005 | 734 |
| 3 | 42,743 | 42,005 | 738 |
| 6 | 42,758 | 42,007 | 751 |

The residual is a whole-round upper bound shared by all non-yield scheduler
work and any no-switch rq pass. It is at most 3.755% of 20,000 timed samples,
far below the proposed >=50% extra full rq pass per sample. This falsifies
the `resume902` pass-multiplicity hypothesis for OTHER yield's costly rq
transaction. It does not count an early scheduler-frame exit that never
acquires rq, nor identify which part of a timed event owns any residual.

For contrast, valid OTHER same-CPU futex `resume889` rounds have
`owner_rq_scheduler_transactions - context_switches` of 21,196 (run1 round2),
21,189 (run1 round6), and 21,216 (run2 round2). This is not a no-switch
count: some switch reasons may use a different rq entry, and all counters
include background activity and both directions of the benchmark. The
surplus does justify a *futex-specific* generation-paired classification
before attempting to elide a pass. It does not support a shared yield/futex
shortcut or a native performance claim.

Decision: diagnostic only. Stop treating an extra full rq pass in OTHER yield
as a plausible 5-us common bottleneck. Any future futex pass probe must pair
the target gate generation with the sender/receiver scheduler entry and
distinguish before versus after the receiver's userspace timestamp. The
valid five-crate PGO screen remains 11/20 at 90%, worst 57.999%.
