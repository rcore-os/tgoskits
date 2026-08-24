# Task 1 final closure report

## Executive conclusion

Task 1 is functionally complete for the QEMU AArch64 evaluation platform. The
final shared-pCPU mechanism is bounded-service fixed-priority round-robin
(FP-RR): high-priority RT work preempts normally, equal-priority work uses RR,
and a lower-priority runnable vCPU that reaches the starvation threshold is
given one bounded host-tick service window. The window survives cooperative
VM-exit yields, then fixed-priority scheduling resumes.

This is a software scheduler/critical-path change under direct Guest-to-Guest
contention. It does not rely on a dedicated pCPU or static CPU partition.

## Baseline qualification

Literal upstream `dev` commit `58272717a` compiled and created both VMs, but
the current Linux/Zephyr test assets repeatedly faulted at EL2 (`ESR_EL2`
`0x96000047`/`0x96000046`) before either measurement marker. Five failed
attempts are retained in `official-dev-rr-offline-20260820/`; they are not
performance samples.

For the runnable scheduler A/B, the baseline therefore selects AxVisor's RR
policy in the Guest-compatible tree. Its `RRScheduler` core algorithm is the
same as upstream `dev`; upstream additionally contains ready-queue inspection
and stealing helpers that are outside the scheduling decision path. This is
reported as an "official RR policy compatible-runtime baseline", not as a
literal successful upstream binary run.

## Equal-protocol final A/B

Both arms use:

- QEMU AArch64 TCG, four pCPUs and 8 GiB RAM;
- Linux vCPU0 and Zephyr vCPU0 on pCPU1 (direct shared-core competition);
- Linux vCPU1 on pCPU2;
- no dedicated CPU and no host burner;
- Linux cyclictest: 1 ms period, 90-second workload;
- Zephyr: 3000 samples, 10 ms period;
- Linux host scheduling priority 89 and Zephyr priority 90 in the modified
  arm; identical Guest images and topology;
- accepted sample accounting and normal QMP shutdown.

| Metric | RR policy baseline | Bounded-service FP-RR | Change |
|---|---:|---:|---:|
| Linux average | 2205 us | 2133 us | **3.27% lower** |
| Linux P99 | 1579 us | 1925 us | **21.91% higher** |
| Linux P99.9 | 6360 us | 7114 us | **11.86% higher** |
| Linux maximum | 3.320090 s | 0.648831 s | **80.46% lower / 5.12x** |
| Zephyr mean jitter | 1.611712 ms | 0.396821 ms | **75.38% lower / 4.06x** |
| Zephyr P99 | 2.706080 ms | 0.659696 ms | **75.62% lower / 4.10x** |
| Zephyr P99.9 | 3.655408 ms | 0.991168 ms | **72.89% lower / 3.69x** |
| Zephyr maximum | 8.211952 ms | 3.371504 ms | **58.94% lower / 2.44x** |
| Zephyr misses above 1 ms | 2399/3000 | 3/3000 | **99.87% fewer** |

The result is a real-time trade-off rather than a universal throughput win:
Linux P99 and P99.9 regress by 21.9% and 11.9%, while Linux worst case falls
by 80.5% and all Zephyr real-time metrics improve strongly. The mechanism
therefore supports a claim about determinism, priority isolation and bounded
worst-case response, not a claim that every Linux percentile improves.

The scheduler counters prove the new path executed:

```text
lower_priority_services=268
idle_quantum_skips=16214
quantum_expiries=15
same_priority_rotations=15
```

## Earlier repeated evidence

The preceding three-run, rotating-order shared-pCPU experiment compared RR,
Fixed FIFO and the pre-service-window FP-RR implementation. Its medians were:

| Strategy | Linux P99 | Linux max | Zephyr P99 | Zephyr max | Zephyr misses >1 ms |
|---|---:|---:|---:|---:|---:|
| RR | 1.734 ms | 2.569 s | 1.633 ms | 6.201 ms | 1085/3000 |
| Fixed FIFO | 1.638 ms | 49.315 s | 1.772 ms | 8.063 ms | 1123/3000 |
| Earlier FP-RR | 2.008 ms | 2.889 s | 1.653 ms | 3.588 ms | 578/3000 |

This repeated experiment established the original starvation problem and the
failure of strict Fixed FIFO. The final bounded service window specifically
closes the remaining multi-second starvation tail.

## Task requirement coverage

| Requirement | Evidence | Status |
|---|---|---|
| Optimize real-time critical paths | fixed-priority scheduling, equal-class RR, bounded lower-priority service, WFI/CNTV fast path, target-vCPU wake, per-CPU timer wheel, bounded vIRQ queue | Complete on QEMU |
| Linux Guest with at least 2 vCPUs | two-vCPU Linux; vCPU0->pCPU1, vCPU1->pCPU2; memory/device/IRQ/boot configuration archived | Complete |
| Jitter, scheduling, interrupt, max and stability measurements | cyclictest, Zephyr sampler, timerlat/ftrace gates, VM-exit/timer statistics, 1800-second matrix and one-hour stability run | Complete with QEMU limitation |
| Native/original RTOS baseline | native Zephyr 300/300, mean 405.783 us, P99 599.056 us, max 836.048 us | Complete on QEMU |
| Reproducibility | source/config/build command, generated TOML, images/manifests, raw log, CSV, summaries and SHA256 archives | Complete |
| Literal upstream `dev` runtime percentage | upstream binary cannot boot the current measurement assets | Blocked and honestly documented |
| Physical-board hard-real-time bound | no board allocation in this environment | Not completed |

## Conservative score estimate

The scoring model totals 30 points.

| Scoring item | Maximum | Estimate | Reason |
|---|---:|---:|---|
| Goal and critical-path analysis | 4 | 4 | scheduler, WFI, timer, IRQ and lock paths have source-level and runtime diagnosis |
| Substantive mechanism changes | 8 | 7 | multiple independent software mechanisms delivered; IRQ-tail arbitrary preemption and physical-board closure remain |
| Multi-vCPU Linux configuration | 4 | 4 | complete topology, memory, device, IRQ and boot evidence |
| Before/after and worst-case data | 5 | 4 | equal-protocol runnable RR A/B and repeated historical A/B exist; literal upstream runtime is blocked and final bounded arm has one formal 90-second run |
| Idle/stress and contention comparison | 4 | 4 | four-scenario 1800-second matrix plus direct shared-pCPU contention |
| Native RTOS baseline | 5 | 4 | reproducible native Zephyr exists on QEMU; no physical-board native baseline |
| **Total** | **30** | **27** | conservative expected range **26-27/30** |

The safest submission claim is **26/30 expected**, with **27/30 plausible** if
the evaluator accepts the compatible-runtime RR baseline and values the full
mechanism/evidence package. Do not claim 28+ without a literal upstream run or
physical-board evidence.

## Evidence paths

- Machine-readable final A/B: `task1-final-closure-20260820/comparison.txt`
- RR baseline: `rr-compatible-final90-20260820/stress-guest-shared/`
- Final bounded FP-RR: `fp-rr-service-window-final90-20260820/stress-guest-shared/`
- Literal upstream failure record: `official-dev-rr-offline-20260820/`
- Three-arm repeated experiment: `guest-shared-three-arm-complete-20260819/`
- Full evidence index and long runs: `README.md`
