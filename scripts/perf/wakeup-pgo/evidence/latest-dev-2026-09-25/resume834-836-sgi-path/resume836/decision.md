# resume836: CPU0 SGI issue to CPU1 IRQ entry

## 1. Measurement

### 1.1 Scope

This is a diagnostic build of source `69a33650763538692fafea27c869870ed0313642` on `dev@05175ca38823b631a73777b0130226ddfa558439`, using the cpufreq-off, `qperf-metrics` configuration. `probe.patch` is the complete temporary source diff (SHA-256 `b798b9f98df978e14a6a475136ce6e1e1c759d263d7738329acba21bec8c3a38`); image SHA-256 is `e803441a0d600b9d02d11899d1f028fb9c1c620e516901a6ed3b05877bf903ff`. The frozen benchmark SHA-256 is `94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`. The image and benchmark were delivered as session files to OrangePi-5-Plus-2; the session was released. The image itself remains in the local artifact directory, not in the PR.

`qperf_mark_ipi_issue_to_cpu1()` records a timestamp immediately before the physical `somehal::irq::send_ipi()` call, after target validation. `qperf_record_ipi_entry_on_cpu1()` records the first identified CPU1 SGI after `somehal::irq::begin_irq()` and before action dispatch. A single atomic slot pairs issue and entry, while overwrite, unmatched and backward-clock counters expose pairing failures. The CPU0-only paired histogram uses 250 ns buckets, saturating bucket 63 at 15750 ns. The interval includes send-wrapper execution, controller delivery, trap entry and `begin_irq()`; it is not isolated SGI flight or the complete benchmark forward path.

### 1.2 Results

One boot ran two FIFO and two OTHER `thread_futex_cross_cpu` rounds at 20000 samples each. All four have 20000/20000 samples, zero `not_parked` and zero `missed_deadlines`. The raw `run1/results.json`, four benchmark logs, per-round before/after snapshots and `serial.log` are retained.

| Policy | Round | CPU0 pairs | Issue / entry / pair | Overwrite / unmatched / backward | Mean paired interval | Median 250 ns bucket | Saturated bucket |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| FIFO | 1 | 21138 | 21765 / 21765 / 21765 | 0 / 0 / 0 | 3149.6 ns | 2750-2999 ns | 207 |
| FIFO | 2 | 21182 | 22084 / 22084 / 22084 | 0 / 0 / 0 | 3198.9 ns | 2750-2999 ns | 184 |
| OTHER | 1 | 21014 | 21375 / 21375 / 21375 | 0 / 0 / 0 | 3144.0 ns | 3000-3249 ns | 63 |
| OTHER | 2 | 21001 | 21360 / 21360 / 21360 | 0 / 0 / 0 | 3129.6 ns | 3000-3249 ns | 41 |

Counts exceed benchmark attempts because other CPU0-to-CPU1 IPIs occur during each window; pairing is not tagged to individual futex attempts. Snapshot boundaries can race with counter updates, although the four recorded deltas happened to match exactly. The 15750 ns bucket includes all larger intervals, so the histogram does not recover an exact tail percentile. Instrumented benchmark p50 values of 38792–42583 ns are diagnostic only and must not be compared with the frozen Linux RT acceptance table.

## 2. Decision

### 2.1 Interpretation

The paired issue-to-entry interval is around 3 us for the mixed CPU0-origin traffic. It does not by itself explain the approximately 5.9-7.4 us reductions needed by the four cross-CPU futex p50 rows in the latest uninstrumented G1/G2 screen. Subtracting medians would be invalid because the pair stream includes background IPIs and the probe changes the measured path. The next useful stage is entry-to-scheduler-selection and scheduler-to-user-return under a shared generation identifier; only an actual runtime candidate can be judged by source-matched, uninstrumented full20 A/B and the p50/p99/p99.9 guardrails.

### 2.2 Acceptance Boundary

No runtime optimization or new full20 acceptance result was produced. Latest valid uninstrumented G1/G2 remains 11/20 at the matched-frequency 90% threshold, worst OTHER same-CPU futex 58.00%. Three valid candidate boots, all source-matched `<3%` regression checks and production-build reproduction remain open. `probe.patch` must be removed from the worktree before the PR update; this result is evidence only and PR #2477 remains Draft.
