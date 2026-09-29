# resume849: bitset mismatch versus real wake

This is a diagnostic only on unchanged kernels. `protocol.md` was written
before the board boots. The same static AArch64 binary SHA-256
`cb129fc2b48a8af7a7d9c845161bab1f7858750aed70ae9fee6dc83bcab15326`
ran on OrangePi-5-Plus-2: Starry G image SHA-256
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`
and frozen Linux RT Image SHA-256
`aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`.
The one-off Linux initramfs SHA-256 is
`44cdf310cb527e5d78d91fa468d5b4ad74ac2828f21b13524efe55370f1d3345`.
The U-Boot PLL registers were verified on both boots. Earlier independent
diagnostics measured both kernels near 816 MHz, but frequency was not sampled
inside these calls. Both sessions were released; `cargo xtask board ls`
subsequently reported all four OrangePi-5-Plus boards available.

The sender was CPU0 FIFO80, receiver CPU0 FIFO1. Each kernel ran three
complete process rounds on one boot, each with 1000 warmup and 20000 samples
per arm. The empty-word bitset wake returned 0, a mask mismatch against an
actually parked waiter returned 0, and the matching wake returned 1 for
every warmup and measured iteration; receiver setup/exit checks passed.
`python3 check.py` re-parses all raw logs, checks hashes, sample counts, and
normal exit. These three rounds are not independent boot replications.

| Kernel | Round | Empty p50 | Bitset miss p50 | Bitset hit p50 | Miss-empty | Hit-miss |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Starry G | 1 | 2042 | 2042 | 7000 | 0 | 4958 |
| Starry G | 2 | 2042 | 2042 | 7000 | 0 | 4958 |
| Starry G | 3 | 2042 | 2042 | 7000 | 0 | 4958 |
| Linux RT | 1 | 2333 | 2333 | 4084 | 0 | 1751 |
| Linux RT | 2 | 2333 | 2333 | 4084 | 0 | 1751 |
| Linux RT | 3 | 2333 | 2333 | 4084 | 0 | 1751 |

All times are ns. A real waiter present but excluded by the bitset adds no
resolvable marginal p50 versus the empty-word control on either kernel.
The Starry-minus-Linux excess in the difference of marginal p50s for the
hit-specific interval is 3207 ns. This rules out a *large p50 cost in the
common nonmatching bucket scan* under this exact protocol, and directs the
next investigation to the **combined** hit-only path. That combined path
contains queue removal, pending-count update, wait-state CAS, wake-handle
construction, batch handoff, and task/rq scheduler activation. This test
does **not** isolate scheduler activation, nor does an identical marginal
p50 imply zero work. Its differences are not medians of per-attempt paired
differences. The first Linux hit p99 was 17500 ns while the next two were
4667/4666 ns, so no tail inference is taken from this diagnostic.

Decision: proceed only with a new, falsifiable division of the *hit-only*
domain/batch portion from scheduler activation, or a semantics-preserving
candidate supported by source and tail validation. Do not retry the
`resume742` singleton futex fast path (OTHER cross-CPU p99.9 +25.07%) or
weaken `resume604` fences (only tens of ns). No source code changed; latest
valid uninstrumented full20 remains 11/20 at the 90% gate, worst 58.00%.
