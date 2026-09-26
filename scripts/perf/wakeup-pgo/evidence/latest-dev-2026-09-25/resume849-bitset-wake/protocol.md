# resume849 bitset wake discriminator

This is a diagnostic on unchanged kernels, not a production optimization or
frozen full20 acceptance run. The source is
`69a33650763538692fafea27c869870ed0313642` on
`dev@05175ca38823b631a73777b0130226ddfa558439`.

## 1. Measurement contract

The static `wake_cost.aarch64` binary is SHA-256
`cb129fc2b48a8af7a7d9c845161bab1f7858750aed70ae9fee6dc83bcab15326`.
The uninstrumented Starry G image remains SHA-256
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`;
the frozen Linux RT Image remains SHA-256
`aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`.
The one-off Linux initramfs is SHA-256
`44cdf310cb527e5d78d91fa468d5b4ad74ac2828f21b13524efe55370f1d3345`.
Both images have previously been measured near 816 MHz independently; this
experiment will verify the U-Boot PLL registers but not sample frequency
inside each timed syscall.

### 1.1 Three arms

The CPU0 FIFO80 sender and CPU0 FIFO1 receiver run 1000 warmup and 20000
measured iterations per process, three processes per kernel boot. The receiver
publishes readiness only after successfully setting policy and affinity.
After `armed`, the sender settles for 50 us outside the timing window. The
receiver uses `FUTEX_WAIT_BITSET_PRIVATE` with bitset 1 and an absolute
monotonic timeout; all three timed calls use `FUTEX_WAKE_BITSET_PRIVATE`:

| Arm | Word and mask | Required return |
| --- | --- | ---: |
| empty | separate empty word, mask 1 | 0 |
| miss | actual parked waiter's word, mask 2 | 0 |
| hit | same parked waiter's word, mask 1 | 1 |

The sender records the duration of each syscall plus the same raw monotonic
clock calls. `miss` leaves the waiter parked; only `hit` releases it. The
sender then waits for the receiver's completion outside all three timing
windows.

### 1.2 Validity gate

Reject any process round with an incorrect wake return during warmup or
measurement, receiver setup or wait error, incomplete thread join, abnormal
exit, missing marker, or any arm other than 20000 samples. The runner also
requires the exact binary/image hashes, expected board identity, U-Boot PLL
registers, complete logs, and released sessions. Invalid rounds are retained
but never used for interpretation. Three valid process rounds on each kernel
are required; they are not three independent boot measurements.

## 2. Interpretation

For each process round, report the three marginal p50s E, M and H in ns.
`M-E` is the extra cost of looking through a nonempty bucket without
selecting the waiter. `H-M` is only a *combined interval*: it also contains
waiter removal and state transition, wake-handle creation and batch handoff,
plus scheduler activation. It is not an isolated scheduler duration.

### 2.1 Predeclared contrast

For each kernel, compare the three rounds' `M-E` and `H-M` values, and then
compare Starry and Linux across all nine round pairings. A consistent
cross-kernel excess greater than 2000 ns in `M-E`, with `H-M` excess no more
than 1000 ns, prioritizes the bucket/PI/scan path. The reverse pattern
prioritizes the combined hit-specific domain and activation interval. Other
patterns are inconclusive. The cutoffs only select the next investigation;
they are not performance acceptance thresholds or proof of a removable cost.

### 2.2 Causal limits

These are differences of marginal p50s, not medians of per-attempt paired
differences. Adding the two differences to recover `H-E` is an algebraic
identity, not an independent protocol check. Bitset wait/wake is a different
opcode from `resume847`; its absolute p50 need not reproduce that run.
Unmatched bitset wake excludes hit-only waiter removal and scheduler work,
so it cannot isolate the scheduler from the matched arm. No direct source
edit should follow without a semantics-preserving candidate and full20
ordinary-release/candidate validation.
