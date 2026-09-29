# resume847: lower-priority parked waiter wake discriminator

Diagnostic only; the frozen full20 benchmark and kernel sources were unchanged.
The same static AArch64 binary `wake_cost.aarch64` (SHA-256
`a0c02891e5ea9edd8bf132d66edb3923fed6bb1108118f41ddb016adb3e4d21f`)
ran on OrangePi-5-Plus-2 with CPU0 FIFO priority 80 sender and CPU0 FIFO
priority 1 receiver. The receiver must successfully set its policy before
publishing `ready`; each measured wake returned one actual futex waiter, and
the empty control returned zero. The sender is higher priority, so the
receiver cannot force an ordinary FIFO preemption before the timed wake
syscall returns. Both sides use raw `CLOCK_MONOTONIC` syscalls and identical
user code. Every round reported 20000 samples of each kind, no missed waiter,
normal exit, and the board sessions were released.

Starry used unchanged uninstrumented G image SHA-256
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`,
source `69a33650763538692fafea27c869870ed0313642` on
`dev@05175ca38823b631a73777b0130226ddfa558439`. Linux used frozen
PREEMPT_RT Image SHA-256
`aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`
and DTB SHA-256
`316dd15b329756be3887dea22f89fc8d1f5b055f8769761f4144b6b1caaea994`.
Its one-off initramfs SHA-256 is
`3f68116401fa975984e0328a0e6a3216384fd4303a029e2221a4abf3182369dc`.
Both boots passed the U-Boot PLL register check. The archived independent
frequency diagnostics measured both images near 816 MHz; frequency was not
sampled simultaneously with these specific calls.

| System | Round | Empty p50 | Parked p50 | Difference of p50s | Empty p99 | Parked p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Starry G | 1 | 1750 | 7000 | 5250 | 2042 | 7583 |
| Starry G | 2 | 1750 | 7000 | 5250 | 2042 | 7292 |
| Linux RT | 1 | 2916 | 4375 | 1459 | 3500 | 4667 |
| Linux RT | 2 | 2916 | 4084 | 1168 | 3500 | 4667 |

All values are ns. The difference of two marginal p50s is **not** the median
of per-attempt paired differences. Nevertheless, the same-binary contrast is
stable: a real parked wake is 2625–2916 ns slower in absolute p50 on Starry
despite its empty wake being 1166 ns faster. This localizes a substantial
part of the same-CPU deficit to the *sender-side real-waiter wake path*, which
includes bucket collection, wait-state transition, wake batch, rq activation,
and scheduler publication. It does not isolate `activate_waking_thread_locked`
alone or prove any of that work redundant. The Linux p99.9 values are higher
than Starry in this diagnostic; no production tail claim follows from it.

Raw Starry logs are `starry-run1/starry-1.log` and `starry-2.log` (SHA-256
`c68a3b2872dde752c6be36b6cb3bc70fe87fe759977cd8a9f903745d447146f4`,
`a902a5417c92072afc0b11f982f5f87092fb95830874b911e8c25dfc130516e9`).
Linux `linux-run1/boot.log` SHA-256 is
`af33f07e112d8a2a8bcce5115ddb6e7300249e56a886c66e2f6a559ef51362e6`.
Both board runners and all packaging sources are retained beside this note.

Decision: continue only with a stage-specific, semantics-preserving
sender-side candidate. Previous `resume605` found activation enqueue and
publication sizable under heavy probes, but old-head means and probes cannot
be subtracted from this native result. No kernel code was changed and the
latest valid uninstrumented full20 remains 11/20 at 90%, worst 58.00%.
