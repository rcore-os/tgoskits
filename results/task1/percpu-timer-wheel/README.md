# Per-CPU AxVM timer-wheel lock

Date: 2026-08-16

The previous implementation stored CPU-bucketed timer heaps behind one global
`IrqSafeMutex`. A timer register, cancel, or expiry on any Linux/RTOS vCPU could
therefore delay the same operation on every other pCPU while local interrupts
were disabled.

The new implementation uses one static IRQ-safe lock and one published deadline
per host CPU. Timer tokens encode their owner CPU, so the legacy token-only
cancel API and handle-based remote cancellation go directly to the owning wheel
without a global owner map.

Functional verification:

- `cargo test -p ax-timer-list`: 2 unit tests and 1 doctest passed.
- `cargo test -p axvm --features host-test`: 291 unit tests and 1 boundary test passed.
- Remote cancel, stale owner handles, timer migration, deadline publication, and
  expiry/cancel idempotence are covered by focused tests.

## Timer-worker ordering and the final liveness root cause

The fixed-priority scheduler first exposed an equal-priority FIFO ordering
problem. A Linux vCPU and its timer worker both ran at priority 90. After a
no-deadline WFI park, the worker could notify the vCPU without yielding the CPU
soon enough for that equal-priority vCPU to resume.

Production assigns the timer worker priority 89, one level below the
latency-critical vCPU priority 90. The worker runs while the vCPU is blocked;
the notified vCPU then immediately preempts it. A deterministic
priority-contract test prevents this ordering from regressing. This change
fixed timer-worker domination, but it did not by itself prove Linux SMP
liveness.

Two short production runs initially appeared to demonstrate the fix:

- `liveness-priority89/stress-noiso`: 15-second two-vCPU Linux workload, vCPU1
  reached 12069 parks, cyclictest and init completed, Linux powered off, and
  Zephyr completed 300/300 samples.
- `liveness-priority89-repeat30/stress-noiso`: 30-second repeat, vCPU1 reached
  26228 parks with the same completion markers and no RCU stall.

Longer repeated testing later reproduced a freeze near 100 seconds of guest
uptime. The actual remaining dependency was the deferred VGIC vCPU kick worker,
which still ran at default priority 0. A priority-90 Linux vCPU could wait for a
remote guest CPU while keeping that worker from turning an already-pending IRQ
into a wakeup for the target vCPU sleeping in no-deadline WFI.

The final production contract therefore uses deferred kick worker priority 91,
vCPU priority 90, and timer worker priority 89. Architecture-controller IRQs
also wake only their target vCPU instead of broadcasting to the whole VM. The
three fixed-priority runs in
`../priority-scheduler/final-kick91-ababab-90s` each completed the 90-second
Linux workload, Zephyr samples, and clean shutdown without a watchdog or RCU
stall. The earlier priority-89 short runs remain useful diagnostic evidence,
but they are not the final liveness proof.

## Independent lock A/B

The CNTV-only Zephyr fast path performs no AxVM software timer-wheel operations
on dedicated pCPU1. Its jitter cannot be used to claim a timer-lock speedup.
The lock mechanism was therefore measured with an explicit host-only timer storm
under identical 4-pCPU load, with no guest VMs and no topology change between
the two builds. The only A/B variable is the `global-timer-wheel` feature.

Across three independent runs:

| Metric | Median | Range |
|---|---:|---:|
| Register/cancel throughput speedup | **1.774x** | 1.553x-1.779x |
| Total lock-wait reduction | **90.10%** | 88.23%-90.45% |
| Maximum lock-wait reduction | **63.44%** | 59.52%-71.07% |
| Expiry P99 reduction | inconclusive | -55.03%-4.51% |

The full protocol and per-run table are in
`formal-host-lock-ab-priority89-aggregate.md`. Throughput and lock-wait results
are repeatable software-mechanism improvements. Expiry P99 is not claimed as an
improvement because one of the three TCG runs regressed by 55.03%.

## Compatibility boundary

The measurement-only global timer wheel did not complete the Linux integration
workload reliably at timer-worker priority 89. In
`formal-priority89-cooperative-ab-rerun/global-lock/stress-noiso`, Linux vCPU1
stopped near 205 parks, the progress watchdog fired after roughly 300 seconds,
and Linux later reported an RCU stall before `RT_CYCLICTEST_START`.

That failed global-lock integration run is not mixed into the performance
claim. Production Linux/Zephyr runs establish per-CPU implementation stability;
the host-only single-variable A/B independently measures lock contention.

The short AArch64 smoke used an invalid combination of a 10-second Linux run and
the fixed 45-second Zephyr settle interval. The VMs and per-CPU wheels started
and ran correctly, but the runner failed during collection because Linux had
already shut down. The formal 90-second regression avoids that harness issue.
