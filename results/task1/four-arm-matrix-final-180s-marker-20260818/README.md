# Task1 final four-arm matrix

This directory is the reproducible evidence bundle for the Plan 2 separation
matrix. It contains three runs of each condition, with a 180 second Linux
cyclictest workload and 3000 Zephyr periodic samples per run.

## Conditions

| Arm | Topology | Scheduler | Host burner |
|---|---|---|---|
| `shared-rr` | shared | round-robin | pCPU1, 10 ms busy / 53 ms idle |
| `shared-fixed` | shared | fixed-priority FIFO | pCPU1, 10 ms busy / 53 ms idle |
| `partition-rr` | Linux vCPU isolated on pCPU1 | round-robin | disabled on pCPU1 |
| `partition-fixed` | Linux vCPU isolated on pCPU1 | fixed-priority FIFO | disabled on pCPU1 |

The exact interleaving and parameters are in [`protocol.txt`](protocol.txt).
Each run includes `meta.txt`, `run.log`, `zephyr-stats.txt`,
`cyclictest-summary.txt`, VM-exit/tick snapshots and `sha256sums`.

## Final attribution

The three-run median Zephyr P99 jitter is:

| Comparison | P99 median | Interpretation |
|---|---:|---|
| shared RR -> shared fixed | 10.733 ms -> 1.074 ms | 9.99x software scheduling gain under contention |
| shared RR -> partitioned RR | 10.733 ms -> 0.904 ms | 11.87x topology isolation gain |
| partitioned RR -> partitioned fixed | 0.904 ms -> 0.893 ms | 1.01x scheduler gain without contention |
| shared fixed -> partitioned fixed | 1.074 ms -> 0.893 ms | 1.20x topology gain with fixed scheduler |

Therefore the approximately ten-fold software effect is demonstrated by the
`shared RR -> shared fixed` comparison, while the partitioned comparison is a
separate topology effect. The fixed-priority change does not produce a large
gain after the competing work has already been isolated, which is the expected
mechanism-level result.

All 12 runs completed cyclictest, the full Zephyr sample set, `RT_INIT_DONE`,
normal `PSCI_SYSTEM_OFF` and QMP shutdown. The prior `partition-fixed` failure
was a serial tail-capture race; commit `ae2656ef7` moves `RT_INIT_DONE` directly
after measurement completion and the matrix then passed without watchdogs.
