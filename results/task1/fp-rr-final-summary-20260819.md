# FP-RR Direct Guest-Shared Result

## Scope

Linux vCPU0 and Zephyr vCPU0 both run on pCPU1. Linux vCPU1 runs on pCPU2.
No dedicated CPU mask and no host burner are used. The only scheduler variable
in the clean three-arm run is `rr-scheduler`, `rt-scheduler` (legacy fixed FIFO),
or `fp-rr-scheduler`.

The accepted matrix is in
`results/task1/guest-shared-three-arm-clean-r1-20260819/` and all three arms
use commit `8531e9fc6`.

## Clean Three-Arm Matrix

| Arm | Linux avg | Linux P99 | Linux P99.9 | Linux max | Zephyr P99 | Zephyr max |
|---|---:|---:|---:|---:|---:|---:|
| RR | 1.608 ms | 1.695 ms | 4.649 ms | 3.800 s | 1.710 ms | 3.723 ms |
| Fixed FIFO | 2.376 ms | 1.640 ms | 2.292 ms | 50.257 s | 1.726 ms | 9.722 ms |
| FP-RR, 5 ticks | 1.603 ms | 2.224 ms | 8.213 ms | 1.719 s | 1.849 ms | 3.664 ms |

Relative to Fixed FIFO, FP-RR reduces Linux maximum latency from 50.257 s to
1.719 s: **96.58% lower, 29.23x lower**. Zephyr maximum jitter falls 62.31%.
This is a real shared-pCPU scheduling result, not static partitioning.

The trade-off is visible: Linux P99 rises 35.61% and P99.9 rises 258.33% in
this single run. Therefore the defensible claim is improved worst-case
starvation bound, not universal improvement of every percentile.

## Mechanism Evidence

The instrumented 60-second FP-RR run recorded:

```text
quantum_ticks=5
quantum_expiries=449
same_priority_rotations=196
slice_preserving_preemptions=107245
voluntary_requeues=4571652
```

The non-zero quantum-expiry and same-priority-rotation counts prove that the
software mechanism executed while Linux and Zephyr competed on pCPU1.

## Quantum Boundary

* 1 tick (10 ms) was rejected: 38,083 quantum expiries and 2,177 rotations in
  one run, with excessive switching and failed acceptance.
* 2 ticks (20 ms) completed, but Linux P99.9 reached 19.875 ms.
* 5 ticks (50 ms) is retained as the validated default because it removes the
  50-second FIFO starvation tail while producing a lower P99.9 than q=2.

## Reproduction

```bash
RT_GUEST_SHARED_THREE_ARM_OUTPUT_ROOT=results/task1/guest-shared-three-arm \
RT_GUEST_SHARED_THREE_ARM_DURATION_SEC=60 \
RT_GUEST_SHARED_THREE_ARM_REPEATS=3 \
RT_GUEST_SHARED_THREE_ARM_SAMPLE_COUNT=3000 \
scripts/test/rt-partition/run-guest-shared-three-arm.sh
```

The runner accepts both legal shutdown orderings: completion markers captured
before Linux powers off, or a completed Linux guest that has already emitted
`PSCI_SYSTEM_OFF`.
