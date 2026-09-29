# resume858: ktimer fresh-wake classification

Source is `69a33650763538692fafea27c869870ed0313642` on
`dev@05175ca38823b631a73777b0130226ddfa558439`. Reuse the
generation-paired `resume826` qperf-only probe, adding separate counts for
the underlying `WakeResult::Notified` and `WakeResult::AlreadyPending` at the
CPU1 hard-IRQ `ktimers/%u` notification. The existing coarse `Notified` count
must equal their sum; `Pending` remains separate. No scheduling, timer, or
notification behavior changes are allowed. Archive both probe layers and
remove them from the production worktree after the run.

Build one release qperf image with no PGO and no cpufreq feature. Verify source,
toolchain/build configuration, image and frozen benchmark SHA-256. On the same
OrangePi-5-Plus-2 boot, run FIFO/OTHER `absolute_timer_same_cpu` twice each,
with 1000 warmups and 10000 attempted samples per process. Require 10000/10000,
zero `not_parked` and missed deadlines, exit zero, unchanged policy/CPU, valid
PLL check, all raw before/after snapshots and logs, and released board lease.
Invalid rounds are retained but never spliced into valid results.

Report per-round fresh/AlreadyPending/Pending counts and fractions among
coarse `Notified`, matched publish/notify/claim generation counts, and the
existing 2 us-bin notify-to-claim histogram. The histogram is pooled over
both fresh and AlreadyPending outcomes, so it can characterize fresh wakes
only if the fresh share is high; otherwise a per-kind generation-paired stage
probe is needed. These timings are probe-inclusive and not a native p50 gain.
No Linux RT or full20 acceptance comparison is made. A high fresh share would
focus next work on IRQ-return/worker dispatch; a high AlreadyPending share
would focus on coalescing and worker-claim state. Do not change the PREEMPT_RT
ParkSoft/worker contract based on this diagnostic alone.
