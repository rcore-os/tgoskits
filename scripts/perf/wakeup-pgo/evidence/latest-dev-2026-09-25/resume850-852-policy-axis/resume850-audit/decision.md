# resume850: matched-wake hit-only source audit

Read-only audit at `69a33650763538692fafea27c869870ed0313642`, base
`dev@05175ca38823b631a73777b0130226ddfa558439`. The independent
source walk is in `subagent/final.txt`; no source, build, board or acceptance
result changed. `resume849` remains three valid process rounds per kernel,
not three independent boots or a new full20 result.

The matched wake's hit-only work includes `VecDeque::remove`, pending count
decrement, wait-state CAS, transfer of the existing owning task handle,
`ThreadWakeBatch` push/pop/drain, and the task/rq wake transaction. The
`into_wake_handle()` conversion moves ownership without cloning Arcs.
The nonmatching bitset arm in `resume849` confirms that a nonempty bucket
scan adds no *resolvable p50* on this workload; it does not prove the scan
has zero cost. The earlier `resume604` fence probe and `resume742` rejected
singleton fast path further weaken small domain/batch micro-optimization
claims. Linux and Starry operation counts alone are not a cost or
correctness proof; the agent's statement that Starry's domain+batch path
is lighter than Linux must not be used as a measured conclusion.

No semantics-preserving, source-backed candidate with a plausible
multi-microsecond gain emerged. The proposed same-address requeue arm is
not a clean substitute for a matched wake: it modifies cleanup/key route
but does not execute waiter removal, `mark_woken`, wake-handle transfer or
batch publication. Its latency cannot bound those missing operations, and
success on both kernels has not been established. Do not run its six-arm
protocol or use its D1 thresholds as attribution evidence without a new
contract and independent validation.

There is a useful separate policy discriminator. In the checked G1/G2
full20 medians, OTHER and FIFO same-CPU thread futex p50 are 14583.5 and
11958 ns, whereas frozen Linux RT gives 8458 ns for both. The difference
changes both sender and receiver policy and includes park/switch work, so
it cannot be assigned to Fair wake activation. A same-binary continuation
of the `resume849` lower-priority, no-immediate-preemption protocol can
compare pinned FIFO1 versus SCHED_OTHER receivers behind the *same* FIFO80
sender. That will test only the policy-dependent sender-side matched-wake
interval. If a stable Fair excess appears there, inspect Fair activation;
if not, inspect park/switch and other full20-only work. Neither outcome
would by itself prove a 90% optimization.

Decision: close source-only candidate search for this exact hit interval;
pre-register and run the narrower policy-axis discriminator as a separate
numbered diagnostic. Latest valid uninstrumented full20 is still 11/20
at the 90% gate, worst 58.00%.
