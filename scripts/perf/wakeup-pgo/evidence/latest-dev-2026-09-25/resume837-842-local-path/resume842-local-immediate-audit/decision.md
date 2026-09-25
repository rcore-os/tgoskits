# resume842: local immediate-preemption source audit

Exact source `69a33650763538692fafea27c869870ed0313642`, based on
`dev@05175ca38823b631a73777b0130226ddfa558439`. This is a read-only
audit of the new, same-board forced-FIFO diagnostic in resume840/841. It does
not change production source or establish a full20 performance result.

The forced FIFO wake runs through private futex lookup, `ThreadWakeBatch`,
`wake_thread_source`, `activate_waking_thread_locked`, local immediate
reschedule publication, `PreemptScope` exit, one owner-rq scheduling pass,
`execute_switch_plan`, the architecture switch, and the incoming switch tail.
The equal-priority FIFO control keeps the current thread until it enters its
own `FUTEX_WAIT`; therefore the two benchmark windows differ in more than the
preemption decision. The forced-priority result rules out a Fair-only
explanation, but does not identify a removable operation.

No transaction-level change with a defensible >=4 us native p50 gain was
found. The wake-side task/rq ownership, `on_cpu` release/acquire pair,
lost-wake ordering, runtime accounting, context switch, and incoming rq baton
completion remain required. Old switch-stage probes were produced with
different source and board conditions and contain probe overhead; their means
cannot be subtracted from the current native p50 gap.

The subagent's proposed one-boundary W1/W2 split could classify where time
is spent, but its suggested criterion does not prove that time is removable.
In particular, a 10.792 us total window must have a half of at least
5.396 us, even when every operation is mandatory. No production candidate or
board test is authorized by this audit alone. The next experiment needs a
specific suspected redundant operation, an invariant-preserving replacement,
and a source-matched native ABBA comparison before a gain is claimed.

The full source-path report is in `subagent/final.txt`. It is a read-only
exploration report; any function-cost estimate or suggested probe design is
diagnostic, not acceptance evidence.
