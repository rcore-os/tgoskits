# resume852: FIFO versus OTHER receiver in sender-side wake

This is a diagnostic only, pre-registered in `protocol.md` after the invalid
`resume851` attempt. Both unchanged images ran on OrangePi-5-Plus-2 using
the *same* static AArch64 binary SHA-256
`2cbc88477cd5ff43475505534918c5c2ee9835de33c916d4c8203b1f9babd19b`.
Starry G image SHA-256 was
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`;
frozen Linux RT Image SHA-256 was
`aac6d3c5fa0c4fdf65f987af635f4cd55a06852b23046a4242a184acc2fd563b`.
The one-off Linux initramfs SHA-256 was
`b45d2a993ae4600c099662524a128f175c82c3d38934c4dc93ce9c6d5e9ec41a`.
Both sessions verified the U-Boot PLL registers and were released; a later
`cargo xtask board ls` showed 4/4 OrangePi-5-Plus boards available. Prior
independent PMU checks measured these unchanged kernels near 816 MHz; no
frequency sample was taken inside these syscalls.

Sender policy remained CPU0 FIFO80. The receiver was CPU0 FIFO1 or OTHER,
in the fixed process order FIFO, OTHER, OTHER, FIFO, FIFO, OTHER. Each kernel
ran one boot and six separate process rounds; each round completed 1000
warmup and 20000 measured samples per empty, mismatching, and matching
bitset-wake arm. Every process returned 0/0/1 for all attempts, verified
policy/CPU, joined normally, and exited zero. With 500 us pre-timing settle,
this run did not observe the invalid wake returns seen in `resume851`; it changes the
workload, so these absolute values do not replace `resume849` or full20.
`python3 check.py` re-parses raw serial/guest logs and identities.

| Kernel and receiver | Rounds | Empty p50 | Miss p50 | Hit p50 | Median hit-minus-miss |
| --- | ---: | ---: | ---: | ---: | ---: |
| Starry G, FIFO1 | 3 | 2042 | 2042 | 6709/7000/7000 | 4958 |
| Starry G, OTHER | 3 | 2042 | 2042/2042/2333 | 7875/7875/8166 | 5833 |
| Linux RT, FIFO1 | 3 | 2625 | 2333 | 4375 | 2042 |
| Linux RT, OTHER | 3 | 2625 | 2333 | 5542 | 3209 |

All times are ns. These are differences of *marginal p50s*, not medians of
per-attempt paired differences. The OTHER-minus-FIFO increment of the
hit-minus-miss interval was 875 ns on Starry and 1167 ns on Linux RT.
Their difference is -292 ns, failing the predeclared criterion of at least
1000 ns extra Starry sender-side Fair cost with at most 300 ns Linux class
increment. The Starry-specific OTHER/FIFO gap in frozen full20 thus is not
explained by this *lower-priority, non-preempting sender-side* wake window.
It could lie in sender policy, park, immediate preemption, switch, receiver
resume, or their interaction. This diagnostic does not choose among them.

Decision: close the hypothesis that an extra multi-microsecond Fair-only
sender-side activation explains the full20 worst row. Investigate the
remaining park/switch handoff window or a source-backed common scheduler
optimization, with a separate numbered experiment. No kernel source was
changed, no native full20 was rerun, and the latest valid G1/G2 remains
11/20 at the 90% gate, worst 58.00%. No p99/p99.9 acceptance claim follows
from this diagnostic.
