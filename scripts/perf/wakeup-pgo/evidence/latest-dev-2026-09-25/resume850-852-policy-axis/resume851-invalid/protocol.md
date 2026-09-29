# resume851 receiver-policy wake discriminator

Diagnostic only. Both kernels, frozen full20 benchmark, and PGO G image remain
unchanged. Starry source is `69a33650763538692fafea27c869870ed0313642`
on `dev@05175ca38823b631a73777b0130226ddfa558439`. Starry G image
SHA-256 is `9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`;
frozen Linux RT Image SHA-256 is
`aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`.
The same static diagnostic binary SHA-256
`c9e96efb297c64017c55052368a3470cb717f83375c266213b1bb820163addc5`
runs on both. Linux's one-off initramfs SHA-256 is
`a8b7b0ee2020e401c6d91009a473b55cd0b7f2d0ed1fd9d6a830fbf6fce522a6`.

## 1. Protocol

The CPU0 sender is always SCHED_FIFO priority 80. The CPU0 receiver is
SCHED_FIFO priority 1 or SCHED_OTHER priority 0, selected by `fifo|other`
on the *same* binary. The receiver verifies its actual policy before
publishing readiness. Each process has 1000 warmup and 20000 measured
iterations for each of the three `resume849` arms: empty bitset wake (return
0), nonmatching bitset wake with a real parked waiter (return 0), matching
bitset wake (return 1). Every call uses `FUTEX_WAKE_BITSET_PRIVATE`. The
sender settles for 50 us after `armed`; a lower-priority receiver cannot
force ordinary preemption during the timed wake.

### 1.1 Run order

On each kernel boot, run six separate process rounds in the fixed order
FIFO, OTHER, OTHER, FIFO, FIFO, OTHER. This gives three rounds per class,
with both kernels using the identical order. They are independent process
rounds but only one boot per kernel. Verify both board sessions' U-Boot PLL
registers and eventual release; previous independent checks put these
unchanged images near 816 MHz, but frequency is not sampled within each
timed call.

### 1.2 Rejection gate

Reject any process round with policy/affinity setup failure, sender or
receiver on the wrong CPU, empty/miss/hit return other than 0/0/1 in any
warmup or measured iteration, receiver wait error, incomplete join, abnormal
exit, missing marker or less than 20000 samples in any arm. Keep invalid
logs but exclude them from contrasts. The diagnostic requires all three
valid process rounds per class per kernel; no cherry-picked replacement.

## 2. Interpretation

For each valid round compute the *difference of marginal p50s* H-M, where
H is matching wake and M is nonmatching wake. First compare OTHER versus
FIFO within each kernel, then compare those class increments across kernels.
If the Starry OTHER increment is at least 1000 ns above its FIFO increment
across the valid rounds while Linux's class increment is at most 300 ns,
prioritize the policy-dependent sender-side Fair activation path. Otherwise
the earlier full20 OTHER-FIFO gap is not explained by this sender-side
window alone; investigate park/switch and other full20-only work. The
cutoffs choose an investigation, not a production or 90% acceptance gate.

This comparison changes receiver policy while holding the sender FIFO80,
unlike frozen full20 where policy is set for the benchmark scenario and the
window includes park and switch work. It therefore cannot assign the full20
policy difference to a specific function or prove an optimization. All
full20 acceptance and source-matched p50/p99/p99.9 regression gates remain.
