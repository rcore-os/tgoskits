# Per-CPU timer-wheel lock: three-run aggregate

Date: 2026-08-17

This report aggregates three independent single-variable A/B runs of the AxVM
software timer wheel. It measures the lock mechanism directly and does not use
guest CPU partitioning as the source of the improvement.

## Protocol

- Platform: QEMU AArch64 TCG with 4 pCPUs.
- Guest VMs: none.
- Control and shell activity: pCPU0.
- Timer-storm workers: pCPU1-pCPU3 (`cpu_mask=0xe`), priority 90.
- Per-CPU timer workers: priority 89, one level below latency-critical work.
- Work per storm worker: 5000 register/cancel pairs and 64 expiry samples.
- Expiry delay: 100000 us.
- A/B variable: only the `global-timer-wheel` feature. The global-lock build is
  the measurement baseline; the default build uses per-CPU locks.
- Runner: `scripts/test/rt-partition/run-timer-wheel-lock-ab.sh`.
- Summarizer: `scripts/test/rt-partition/summarize-timer-wheel-ab.py`.

Each result directory archives the AxVisor binary, board and QEMU configs,
commands, serial log, extracted storm data, metadata, and SHA-256 hashes.

## Results

| Metric | Run 1 | Run 2 | Run 3 | Median | Range |
|---|---:|---:|---:|---:|---:|
| Register/cancel throughput speedup | 1.553x | 1.779x | 1.774x | **1.774x** | 1.553x-1.779x |
| Total lock-wait reduction | 88.228% | 90.447% | 90.098% | **90.098%** | 88.228%-90.447% |
| Maximum lock-wait reduction | 59.516% | 71.072% | 63.440% | **63.440%** | 59.516%-71.072% |
| Expiry P99 reduction | 4.493% | -55.030% | 4.505% | 4.493% | -55.030%-4.505% |

Raw summaries:

- `formal-host-lock-ab-priority89/summary.txt`
- `formal-host-lock-ab-priority89-run2/summary.txt`
- `formal-host-lock-ab-priority89-run3/summary.txt`

## Claim boundary

The repeatable result is removal of cross-pCPU lock contention: median
register/cancel throughput improved by 1.774x, total lock wait fell by 90.10%,
and maximum lock wait fell by 63.44%.

Expiry P99 is inconclusive. Two runs improved by about 4.5%, while one regressed
by 55.03%. This metric is sensitive to QEMU TCG scheduling noise and is not used
as evidence of an end-to-end timer-expiry latency improvement.

The global-lock measurement build also failed to complete the Linux integration
workload at timer-worker priority 89. That compatibility failure is documented
separately and is excluded from the lock-performance calculation. Production
per-CPU builds completed both 15-second and 30-second two-vCPU Linux workloads,
including cyclictest completion, init completion, system-off, and all 300
Zephyr samples.
