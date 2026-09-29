# resume848: sender-side real-waiter wake path audit

Read-only audit of `69a33650763538692fafea27c869870ed0313642` at
`dev@05175ca38823b631a73777b0130226ddfa558439`, after the valid
`resume847` same-board diagnostic. The independent source walk is in
`subagent/final.txt`. No source, build, board image, or native full20 result
changed.

The pinned FIFO80 sender/FIFO1 receiver still executes three distinct locks
on a real wake: the futex bucket PI mutex, receiver task scheduler lock, and
CPU0 rq lock. `reschedule=None` avoids preempt publication and an immediate
switch, but not waiter removal, wake-batch handoff, scheduler activation,
enqueue, or rq summary commit. The audit found no individual operation with
a proven multi-microsecond removable cost. Prior fence, singleton-waiter,
membership, inline, and entity-snapshot attempts remain negative or unsafe
under the acceptance gates.

The only new diagnostic proposal is a same-binary `FUTEX_WAIT_BITSET_PRIVATE`
waiter with three `FUTEX_WAKE_BITSET_PRIVATE` calls: an empty-word control,
a mask mismatch that retains the waiter, then a matching mask that returns 1.
This could test whether scanning a nonempty bucket without waking a thread
already carries a large marginal cost. The control must use the *same wake
opcode* as the other two arms; using ordinary `FUTEX_WAKE_PRIVATE` for only
the empty arm would confound syscall variants. Require successful FIFO policy,
CPU affinity, zero nonmatching/empty wake returns, one matching wake return,
full samples and no receiver error. Use multiple independent rounds and
compare both kernels at the same board/frequency setup. It is not full20.

Important correction to the subagent's proposed interpretation: matched
minus unmatched wake includes `VecDeque::remove`, pending count decrement,
`mark_woken`, wake-handle construction, batch push/drain, and only then
scheduler activation. It is **not** an isolated scheduler transaction.
Likewise, `(hit - miss) + (miss - empty) == hit - empty` is an algebraic
identity of marginal p50s, not an independent consistency check. Fixed
3–4 us branch thresholds are not justified by existing variance data.
Any future experiment must pre-register interpretation and reject invalid
rounds before comparing distributions; a large hit-minus-miss difference
would merely narrow the combined domain-plus-scheduler interval. The new
bitset protocol changes both wait and wake opcodes relative to `resume847`,
so agreement with its absolute p50 is useful context, not a validity gate.

Decision: diagnostic idea only; do not implement a production optimization
from this read-only result. Latest valid frequency-matched uninstrumented
G1/G2 full20 remains 11/20 at 90%, worst 58.00%.
