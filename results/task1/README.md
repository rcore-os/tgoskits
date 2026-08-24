# Task1 Real-Time Partition Evidence Index

> Branch: `openrace/task1-rt-partition`
>
> Scope: QEMU AArch64 TCG unless explicitly stated. QEMU results are relative
> software-in-the-loop evidence, not physical-board hard-real-time bounds.

> **Status note:** Files explicitly marked `HISTORICAL` or `historical` are
> retained for archaeology and must not override the current closure report.

## Final submission entry points

- Overall score mapping: `results/final-submission-scorecard-20260821.md`
- Current Task1 closure: `two-gap-closure-20260820.md`
- IRQ-tail design and negative evidence: `irq-tail-preemption-design.md`
- Task2 final network summary: `../task2-final-run-20260821.md`
- Task3 AI loop design/results: `../../book/design/task3-ai-design.md` and
  `../task3/README.md`

The current branch has completed the QEMU software-in-the-loop mechanisms. The
remaining work is submission closure (canonical documents, conflict-free PR,
one integrated current-HEAD run and demo video), not another unbounded scheduler
experiment.

## Current score and scope decision

The two previously identified Task1 deductions are no longer open defects in the
validated QEMU path: the shared-pCPU result-collection race is closed, and the
IRQ-tail/GIC ordering defect is fixed with a bounded priority-aware policy. The
remaining deductions are evidence-boundary deductions (literal upstream `dev`
comparability, physical-board worst-case evidence, and the fact that not every
IRQ is allowed to trigger an immediate switch). The current submission estimate
is **26–27/30**, so Task1 is frozen for now while higher-yield delivery work
continues in Task2, documentation, and StarryOS/STERRORS.

## Evidence Map

| Item | Purpose | Data / status |
|---|---|---|
| `task1-final-closure-20260820/` | final equal-protocol RR vs bounded-service FP-RR comparison and score estimate | accepted 90-second shared-pCPU A/B; expected 26-27/30 |
| `24h-exploration-retrospective.md` | full exploration, failures, fixes, experiments, scoring, and remaining work | current end-to-end retrospective |
| `phase2-plan.md` | Phase 2 execution record: statistics, contention, WFI, timer lock, preemption, rejected candidates, and closure | closed for the QEMU mainline; board port remains external |
| `SESSION-SUMMARY.md` | current implementation and remaining work | updated after code audit |
| `WORKLOG.md` | chronological implementation/debug record | includes failed preempt/IPI paths |
| `allocation-table.md` | CPU, memory, device, interrupt ownership | current measurement/load topology |
| `locking-discipline.md` | T3.3 trace and lock rules | complete |
| `vgic-maintenance.md` | T3.2 LR/EOI refill verification | complete |
| `native-zephyr/` | Zephyr directly on QEMU, no AxVisor | 300/300, sha256 verified |
| `overload/` | old unbounded vs current bounded queue replay | complete, source snapshots included |
| `virq-ab/` | recovered historical Task1 vIRQ assets | local assets verified; experiment `/tmp` logs missing |
| `matrix/` | formal idle/stress/RT matrix and stall diagnostics | four 1800-second duration scenarios passed |
| `stability/` | one-hour stability | functionally passed; long-tail latency stability not established |
| `priority-scheduler/` | fixed-priority FIFO, deferred kick priority, and ready-path A/B | formal software-mechanism result; Zephyr P99 10.494x |
| `wfi-fastpath-ab/` | dedicated Zephyr CNTV/WFI fast path | mechanism count proof plus single-pair latency observation |
| `percpu-timer-wheel/` | per-CPU timer-wheel lock A/B | host-only lock/throughput result; no end-to-end latency claim |
| `linux-guest-trace-gate/` | optional Linux Guest ftrace diagnosis | identifies Guest wake-to-switch tail; not a performance A/B |
| `timer-worker-priority/` | bounded priority-91 timer-worker candidate | rejected for formal improvement; measurement-only |

## Final Mechanism Results

These are the numbers that may be used for the Task1 software-mechanism claim.
The A/B variables are held at the same topology and offered load unless the
row explicitly says otherwise.

| Mechanism | Measured result | What it proves | Evidence |
|---|---:|---|---|
| Fixed-priority FIFO + ready-path preemption | Zephyr P99 `10.721 ms -> 1.022 ms`, **90.47% lower / 10.494x**; 1 ms misses `56 -> 5` | A real scheduler, target-vCPU wake, deferred-kick priority, and same-core low-priority interference improvement; not static partitioning | `priority-scheduler/final-kick91-ababab-90s/` |
| Dedicated Zephyr WFI/CNTV fast path | WFI exits `1952 -> 0`; single-pair P99 **19.2% lower**; 1 ms misses `3 -> 0` | Deterministically removes the trapped-WFI plus software park/wake path; the latency percentage remains a one-pair observation | `wfi-fastpath-ab/` |
| Per-CPU timer wheel | Register/cancel throughput **1.774x**; total lock wait **90.10% lower**; max lock wait **63.44% lower** | Global IRQ-safe timer-wheel lock was removed from the cross-pCPU path | `percpu-timer-wheel/formal-host-lock-ab-priority89-aggregate.md` |

The `10.494x` figure is the strongest independent software result in this
repository. It is a Zephyr/common-core interference result, not a Linux-wide
P99 result and not a static CPU-partition result. The `18.29x` result in
`p1-interleaved-2026-08-16/` is reported separately because it changes CPU
placement/dedicated no-tick isolation relative to the official DEV shared-core
baseline. The timer-wheel `1.774x` is throughput, not latency acceleration.

## Task1 Requirement Coverage

| Requirement | Current evidence | Status |
|---|---|---|
| AxVisor real-time path changes | WFI/CNTV fast path, fixed-priority FIFO + ready wake, deferred-kick priority contract, per-CPU timer wheel, bounded vIRQ and no-tick isolation | **Delivered**; Linux tail remains a documented limitation |
| At least 2-vCPU Linux Guest | 2-vCPU Linux on pCPU2/3, QEMU AArch64 SMP4, reproducible config/build/start commands | **Delivered** |
| vCPU/pCPU, memory, devices, IRQ routing, boot args | `allocation-table.md`, generated TOML and per-run `meta.txt` | **Delivered** |
| Jitter, scheduling, IRQ response, max latency, long stability | formal 1800 s four-scenario matrix, timerlat/ftrace gates, one-hour `stress-rt` stability | **Delivered with limits**: QEMU TCG max is not WCET; one-hour P99 worsens |
| RTOS baseline | native Zephyr, 300/300 samples, reproducible image/hash/command | **Delivered** |
| Reproducibility | runner scripts, configs, build logs, raw logs, CSV/statistics, manifests and SHA256 | **Delivered for tracked QEMU evidence** |
| Physical-board hard bound | OrangePi-5-Plus port/run | **External blocker**: no board allocation in this environment |

## Result Classification

The following Linux candidates are intentionally **not** included in the
formal improvement percentage: direct per-vCPU CNTV/WFI (IRQ P99 worsened
67.32% in the repeated pilot), unconditional post-VM-exit yield, timer-worker
priority 91, and timer-contract-only pilots. They demonstrate real mechanism
effects or useful diagnosis, but fail directional or worst-tail acceptance.
The Linux Guest trace gate shows `sched_wakeup -> sched_switch` is 95.6% of the
observed IRQ-to-switch P99, so further Linux improvement requires a separate
Guest scheduling/IRQ investigation rather than relabelling the current AxVisor
numbers.

## Native Zephyr

Purpose: establish the same periodic sampler without AxVisor.

Command:

```bash
QEMU_BIN=/home/huhu/.local/bin/qemu-system-aarch64 \
  scripts/test/rt-partition/run-native-zephyr.sh
```

Configuration: Cortex-A72, QEMU `virt`, GICv3, no network, wall-clock TCG,
and no `-icount` or VM RTC override. The 30-second command is only a capture
window; the guest completes 300 samples in its first few seconds.

| Metric | Value |
|---|---:|
| samples | 300 |
| mean jitter | 405.783 us |
| p99 | 599.056 us |
| p99.9 / max | 836.048 us |

Files: `raw.log`, `zephyr.csv`, `stats.txt`, `meta.txt`, `qemu-command.txt`,
ELF/BIN, build manifest, and `sha256sums`. The reproducible ELF/BIN remain
local build artifacts; their manifests and hashes are tracked in Git.

## vIRQ Overload

Purpose: show the queue-level difference between the historical unbounded
`Vec::push` implementation and the current capacity-64 implementation.

Command:

```bash
python3 scripts/test/rt-partition/virq_overload_model.py
```

Load: one edge every 50 us, one service every 1000 us, arrivals for 100 ms.

| Model | Accepted | Overflow | Max depth | Max queue latency |
|---|---:|---:|---:|---:|
| old unbounded | 2000 | 0 | 1901 | 1900.05 ms |
| current bounded | 163 | 1837 | 64 | 64 ms |

Files: source snapshots, per-event CSV, summary CSV, comparison plot, metadata,
README, and `sha256sums`. This is a deterministic queue-contract replay, not
an end-to-end QEMU or hardware timing result.

## Short Matrix Smoke

Purpose: validate orchestration, Linux CPU affinity, stress placement,
cyclictest overflow accounting, Zephyr completion, and vmexit capture before
long runs.

Command shape:

```bash
RT_LOOPS=5000 \
RT_OUTPUT_ROOT=tmp/rt-partition/validation-results \
RT_SCENARIO=<idle|stress-noiso|stress-dedicated|stress-rt> \
  scripts/test/rt-partition/run-cyclictest.sh
```

All four scenarios completed 5000/5000 Linux samples and 300/300 Zephyr samples
in the short smoke set; the fourth scenario uses a virtualized Zephyr guest on
the dedicated host pCPU.
These temporary smoke artifacts are not the formal performance result.

## Formal Matrix

Purpose: compare idle, stressed without host RT partition, stressed with a
dedicated host pCPU but virtualized Zephyr, and the passthrough RT profile for
the same 30-minute guest duration. Fixed loop count was
rejected after stressed TCG failed to finish 1,800,000 loops within 3900 wall
seconds while the guest remained alive.

Long duration runs also showed that a 20-second smoke does not predict
long-run guest-clock progress: idle timed out at both 2100 and 3900 wall
seconds without a guest panic. Those attempts are archived under
`matrix/failed-idle-duration-scale{1,2}/`. The runner now reads a per-scenario
calibration file, with a scale-3 fallback when a scenario has not calibrated.

Calibration command (120 guest seconds per scenario, sequentially):

```bash
scripts/test/rt-partition/run-runtime-calibration.sh
```

The 2026-08-15 idle attempt stopped at guest uptime `659.70 s`. A later
1800-second diagnostic run stopped after `1710.75 s`; watchdog QMP snapshots
showed all four physical CPUs spinning on the same
`somehal::irq::IRQ_ROUTES` lock while QEMU remained `running`. The sync
migration had left two read paths using preempt-only `SpinLock.lock()` instead
of the former IRQ-disabling semantics. Both route lookups now use the existing
IRQ-save helper, with a source-contract regression preventing direct
`IRQ_ROUTES.lock()` access.

The post-fix idle loop validation in `matrix/idle/` completed all
`1,800,000/1,800,000` samples and reached `1832.21 s` guest progress, crossing
the old `1710.75 s` failure point. Zephyr completed `300/300`, all archived
hashes verify, and no watchdog artifacts were produced. This closes the stall
root cause but does not replace the four required duration-mode formal runs.

The uniform post-fix duration matrix is complete for all four scenarios under
`matrix/formal-postfix-2026-08-16/`:

| Scenario | Linux samples | Linux avg/max | Zephyr mean/p99/max | Guest progress |
|---|---:|---:|---:|---:|
| `idle` | 1,713,691 | 534/304645 us | 601.023/866.560/914.768 us | 1792.17 s |
| `stress-noiso` | 1,746,156 | 510/303853 us | 665.170/848.032/919.488 us | 1794.90 s |
| `stress-dedicated` | 1,727,468 | 578/302045 us | 711.746/1029.120/1067.168 us | 1794.86 s |
| `stress-rt` | 1,720,603 | 604/303101 us | 614.024/810.224/882.672 us | 1795.09 s |

The first completed `stress-noiso` workload exposed a runner-only failure:
roughly 720 final `RT_CPUSTAT` lines took longer than the hard-coded 30-second
wait for `RT_INIT_DONE`. That attempt is preserved as
`stress-noiso-failed-result-drain-30s/`. The runner now provides
`RT_RESULT_DRAIN_TIMEOUT_SEC` with a 180-second default and records it in
`meta.txt`; the successful rerun needed about 39.36 seconds for this drain.
None of the accepted runs produced watchdog artifacts.

Both dedicated runs satisfy the no-tick acceptance policy: pCPU1 has
`count=0, delta=0` in the workload-before, Zephyr-after, and Linux-final host
periodic scheduler tick snapshots. All archived hashes pass and no accepted
run produced watchdog artifacts.

For the Zephyr path, `stress-rt` mean/p99/max jitter improves by
7.69%/4.46%/4.00% relative to `stress-noiso`, and by
13.73%/21.27%/17.29% relative to `stress-dedicated`. Linux cyclictest average
latency is 18.43% higher than `stress-noiso`, so this is not presented as a
whole-system latency improvement.

The current idle and `stress-noiso` calibrations recommend scale 2. The
dedicated virtualized calibration reached guest uptime 109.83 s and then
stalled; passthrough `stress-rt` stalled at 49.36 s in a 60 s run. Those are
retained as failed calibration evidence, not converted into scale values.

```bash
RT_SCENARIO=idle RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-noiso RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-dedicated RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-rt RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
```

Accepted evidence per scenario must contain: build log, raw run log,
cyclictest CSV and summary, Linux per-CPU load, Zephyr CSV and statistics,
vmexit workload-before/Zephyr-after/Linux-final snapshots, generated configs,
host-periodic-ticks.csv, metadata, and hashes. Dedicated scenarios additionally
require cumulative and delta host periodic scheduler ticks on pCPU1 to remain
zero. The Zephyr image waits at a UART gate until Linux emits
`RT_CYCLICTEST_START`, so the 300-sample window overlaps the Linux workload.
Build inputs are copied into each scenario before hashing; hashes never point
at mutable `tmp/` or `target/` paths. See `matrix/README.md` for accepted and
failed attempts.

## Stability

Purpose: one-hour stressed RT-partition run with exact sample accounting and
the same evidence contract.

```bash
RT_SCENARIO=stress-rt \
RT_DURATION_SEC=3600 \
RT_OUTPUT_ROOT=results/task1/stability \
  scripts/test/rt-partition/run-cyclictest.sh
```

The accepted run is in `stability/stress-rt/`: 3,347,556 complete Linux
samples, 3598.96 s guest elapsed time, Zephyr 300/300, zero pCPU1 host periodic
ticks in all snapshots, no watchdog artifacts, and all hashes passing.

Linux avg/P90/P95/P99/P99.9/max is
679/872/950/2061/9212/304760 us. P99 is 94.43% higher than the accepted
30-minute `stress-rt` run, while max is 0.55% higher. This proves one-hour
functional stability and isolation, not an absence of long-tail latency
degradation. The Zephyr sampler covers only an early 300-sample window, so it
cannot support a first-window versus last-window claim. See
`stability/README.md`.

The review checks total samples, elapsed time, first/last-window
latency, vmexit deltas, queue overflow markers, and all sha256 entries.

The current Linux guest kernel lacks `CONFIG_NO_HZ_FULL` and prints
`Housekeeping: nohz unsupported`. `isolcpus` and IRQ/load placement are active,
but guest full-dynticks isolation is not part of the current claim.

## Historical vIRQ Assets

`virq-ab/` contains the recoverable day4-6 CSV/log assets and their hashes.
The older experiment-machine files `/tmp/ab-*.log` and `/tmp/e1-*.log` are not
present locally. Their published 311-to-301 us numbers are not treated as
recomputed evidence until those files are supplied.

## Verification Commands

```bash
python3 scripts/test/rt-partition/test_rt_linux_affinity.py
python3 scripts/test/rt-partition/test_host_periodic_ticks.py
python3 scripts/test/rt-partition/test_runtime_calibration.py
python3 scripts/test/net-dual-guest/test_serial_console.py
python3 scripts/test/rt-partition/test_cyclictest_hist_to_csv.py
python3 scripts/test/rt-partition/test_rt_build_scripts.py
python3 scripts/test/rt-partition/test_virq_overload_model.py
cargo test -p axvm --features host-test
cargo xtask ktest qemu -p axvisor --test axtest --arch aarch64
cargo fmt --all -- --check
git diff --check
```
