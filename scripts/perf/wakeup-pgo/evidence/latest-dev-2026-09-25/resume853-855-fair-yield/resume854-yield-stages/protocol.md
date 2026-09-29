# resume854: paired FIFO/OTHER yield-rq stage diagnostic

Prerecorded before image build or board lease. Source HEAD is
`69a33650763538692fafea27c869870ed0313642`, base `dev@05175ca38823b631a73777b0130226ddfa558439`.
Production source stays clean. Build `cargo xtask starry build -c build.toml`
with ten OrangePi features plus `qperf-metrics`, no cpufreq and no PGO.
Record image, config, source and frozen benchmark SHA256. The frozen
benchmark SHA256 must be
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`.

On one OrangePi-5-Plus-2 boot, check U-Boot PLL registers, upload only
session-scoped image/script/benchmark, and run six separate benchmark
processes in order FIFO, OTHER, OTHER, FIFO, FIFO, OTHER. Each runs only
`sched_yield_handoff` with frozen 1000 warmup and 20000 measured samples.
Capture raw `scheduler_metrics` before and after every process, its full
stdout/stderr and exit status, the full serial log and release state.

Valid pair: six complete processes, 20,000/20,000 samples per process,
zero `not_parked` and `missed_deadlines`, policy/case identity and exit
status zero, monotonic counter deltas, all qperf yield-rq slots 0/1/2/4/6
have count at least 20,000 per process, and the count for each slot is
within 3% of slot 0. If any condition fails, retain all raw data and
reject the whole six-round policy contrast; do not pick successful rows.

Compare per-event `total_ns/count` for each slot by policy using median
of three process rounds. Predeclared directional discriminator: if the
OTHER-minus-FIFO sum for slots 1 (put-prev) and 2 (pick) is at least
2000 ns and exceeds the corresponding sum for slots 4 (rq commit) and 6
(selection tail), focus a later source audit on Fair put-prev/pick.
If slots 4+6 dominate instead, inspect class-sensitive publication and
selection tail. Otherwise the gap is not localized by these slots.
These are probe-inclusive aggregate means, not an additive native p50
breakdown or a removable-cost estimate. The instrumented p50 values and
one-boot process rounds do not count toward the issue's full20 gate.
Any source candidate requires a separate numbered A/B with ordinary
uninstrumented release, full20 p50/p99/p99.9 and correctness checks.
