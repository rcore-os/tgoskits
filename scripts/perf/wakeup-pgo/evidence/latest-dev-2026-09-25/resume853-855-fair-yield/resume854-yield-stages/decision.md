# resume854: paired yield-rq stage counters

This is a diagnostic, not an uninstrumented performance result. The
unchanged source was `69a33650763538692fafea27c869870ed0313642`
on `dev@05175ca38823b631a73777b0130226ddfa558439`. The release
image was rebuilt with existing `qperf-metrics`, no PGO, and no cpufreq
feature. Image SHA-256:
`255c03696d0de9b808c6c06a9d3cf61303557212c602253016ae500f2e0877fc`.
The frozen benchmark SHA-256 was
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

OrangePi-5-Plus-2 completed one boot and six separate processes in
the preregistered FIFO, OTHER, OTHER, FIFO, FIFO, OTHER order. Each
process ran only `sched_yield_handoff`, with 1000 warmup and 20000/20000
measured samples, zero `not_parked`/`missed_deadlines`, and exit zero.
All five stage counts per process agreed exactly, ranging 42008-42010.
The U-Boot PLL register check passed and the session was released;
`cargo xtask board ls` then showed 4/4 OrangePi-5-Plus available.
Raw serial SHA-256 is
`6fe8b2f6c5ad4b4fa10bd4df151d243d30d2bb809164b17e902a780ec04840f0`.
`python3 check.py` independently re-parses the benchmark logs and every
before/after counter snapshot, checks identities and computes the contrast.

Median probe-inclusive mean per stage event, in ns:

| Yield-rq stage | FIFO | OTHER | OTHER minus FIFO |
| --- | ---: | ---: | ---: |
| account | 188.28 | 891.98 | 703.70 |
| put-prev | 298.93 | 2517.68 | 2218.75 |
| pick | 298.71 | 1236.45 | 937.74 |
| rq commit | 281.85 | 1055.85 | 774.00 |
| selection tail | 155.44 | 813.83 | 658.39 |

The preregistered put-prev+pick contrast is 3156.49 ns/event, versus
1432.40 ns/event for rq commit+selection tail. It meets the directional
criterion and narrows the next source inspection to Fair put-prev/pick.
The qperf image's FIFO/OTHER benchmark p50 medians are 10500/16917 ns,
far above the uninstrumented G1/G2 4083/7583 ns; **neither those p50s
nor the sum of stage means is a native removable cost**. Each benchmark
attempt generated about 2.1 yield-rq stage events, and global counters
can include background scheduling. The equal stage counts make the
within-image comparison interpretable, but not a Linux RT comparison or
a causal per-attempt breakdown.

The first build attempt mistakenly directed host xtask artifacts to a
separate target and was interrupted at 80 MB free space; its log remains
as `build-attempt1-interrupted.log`. Only the new host `target/debug`
directory from that attempt was cleaned after verifying its birth time.
The normal `cargo xtask starry build` then succeeded, without changing
production source. This attempt did not create an A/B candidate or a
full20 run. G1/G2 remains 11/20 at the 90% gate, worst 58.00%; the
three-boot and p50/p99/p99.9 regression gates remain open.
