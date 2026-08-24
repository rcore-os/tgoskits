# Task-1 Real-Time Partition Design

> Branch: `openrace/task1-rt-partition`
>
> Validation environment: QEMU 10.2.1 AArch64 TCG, `virt`, GICv3. QEMU
> results demonstrate reproducibility and relative behavior, not a physical
> hardware worst-case latency guarantee.

## Current delivery status (2026-08-21)

> **CURRENT STATUS / HISTORICAL BODY:** Sections below retain the original
> design narrative for traceability. Where they describe an earlier scheduler
> state, the current closure report and scorecard take precedence.

This document began as the Phase-2 design baseline. The current implementation
and acceptance status supersede the early “cooperative FIFO / preemption not
complete” wording below:

- the final shared-core scheduler is bounded-service FP-RR: strict higher
  priority wakeups may preempt, equal-priority work rotates fairly, and a
  lower-priority runnable Guest receives bounded service;
- acknowledged host IRQs now complete GIC deactivate/EOI before IRQ-context and
  preemption guards are released;
- fixed-priority IRQ-tail scheduling is priority-aware. Unconditional scheduling
  on every IRQ was rejected because it caused same-priority churn and Linux
  liveness failure;
- the direct Linux/Zephyr same-pCPU closure and the IRQ-tail closure are recorded
  in `results/task1/two-gap-closure-20260820.md` and
  `results/task1/irq-tail-preemption-design.md`.

The remainder of this document is retained for path-level design history. For
the final score and claim boundary, use the current closure report and the
submission scorecard rather than the historical caveats in Sections 1 and 3.

## 1. Goal and Claim Boundary

Task1 adds an opt-in real-time partition profile to AxVisor. A Zephyr vCPU is
placed on an exclusive host pCPU, the host periodic tick is removed from that
pCPU, unrelated Linux work stays on other pCPUs, and software vIRQ backlog is
bounded and observable.

The implementation currently establishes:

- stable vCPU identity independent of physical placement;
- a dedicated host pCPU with no periodic ArceOS timer tick;
- explicit Linux measurement/load CPU separation;
- bounded per-vCPU interrupt queues and retry-safe injection;
- measured lock/wakeup trace boundaries;
- VM-exit reason accounting;
- reproducible native-QEMU and AxVisor experiment paths.

It does not claim that fixed-priority preemption is complete. The attempted
ArceOS preempt/IPI integration regressed the vCPU world switch and dedicated
secondary-core startup, so it was reverted. The final scheduler remains
cooperative FIFO.

## 2. End-to-End Latency Path

```text
physical timer / device assertion
        |
        | host IRQ recognition and guest exit
        v
AxVisor IRQ route / timer synchronization
        |
        | enqueue_start -> enqueue: target lookup + queue lock
        | enqueue -> notify: lock-free waiter wake
        | notify -> ipi: optional host kick boundary
        v
per-vCPU bounded pending queue (capacity 64)
        |
        | running -> inject: pop one edge + backend injection
        v
GIC list register / guest timer state
        |
        | guest exception entry
        v
Zephyr ISR and periodic task
```

The latency sources are host scheduling, VM exit/entry, queue lock contention,
vCPU wakeup, ICH LR availability, guest interrupt masking, and guest task
scheduling. Task1 removes a deterministic host source, the 10 ms periodic
tick on the Zephyr pCPU, and bounds the software queue source. It does not
erase QEMU host scheduling noise.

## 3. Architecture Changes

### 3.1 vMPIDR and Physical Placement

The old setup used `phys_cpu_id` as guest MPIDR, so moving a vCPU changed the
guest-visible CPU identity. The current setup uses vCPU index for MPIDR and
keeps placement only for host scheduling.

vGIC affinity required a second split:

- guest affinity: SGI target matching and GICD_IROUTER semantics;
- physical affinity: host SPI/MSI route target.

Guest FDT CPU nodes are renumbered to match virtual MPIDR values. This allows
Linux vCPU0/1 to run on host pCPU2/3 without breaking PSCI/SMP startup.

### 3.2 Dedicated No-Tick Host CPU

`dedicated_cpus=` is parsed from host bootargs into an opt-in runtime mask.
For a dedicated pCPU, AxRuntime:

- does not initialize the periodic scheduler timer;
- does not advance the periodic deadline;
- programs only event-driven axtask/VM deadlines.

The final host mask is `dedicated_cpus=1`. Marking pCPU2/3 dedicated would
also remove timers needed by the Linux guest vCPUs and was the cause of an
earlier `sleep`/cyclictest stall.

### 3.3 WFI and Timer Wake Capability

Dedicated placement alone is insufficient to clear HCR_EL2.TWI. CNTV state
is restored to hardware by the world switch, but guest CNTP is currently
software-emulated and needs trapped WFI to arm a host timer.

The policy is therefore:

```text
trap_wfi = shared_vcpu
        OR virtual_timer_has_no_hardware_wake
        OR physical_timer_has_no_hardware_wake
```

With current capabilities (CNTV true, CNTP false), Zephyr still traps WFI on
the dedicated pCPU. This preserves wake correctness and avoids an invalid
zero-exit claim.

### 3.4 Bounded vIRQ Delivery

Each vCPU has a capacity-64 pending queue. Overflow returns an explicit
resource error and emits `queue_overflow` in realtime trace.

The consumer pops one injectable edge at a time while holding the queue lock.
A busy head remains queued. If the backend becomes full after the edge is
popped, a separate retry slot retains it; the slot is outside producer
capacity and counts as pending work, so the vCPU cannot sleep through it.

This shape fixes two old failure modes:

- batch drain followed by LR contention could lose same-vector edges;
- requeue into a concurrently filled bounded queue could lose the popped edge.

### 3.5 vGIC LR Refill

The shared GICv3 backend already covers LR exhaustion:

- UIE+NPIE for pending work outside LRs;
- UIE+LRENPIE+TDIR for active spill/deactivation;
- a host maintenance PPI discovered from FDT and enabled per CPU;
- ICH save/merge on exit and LR refill before the next guest entry.

The one-LR/two-edge regression proves that completed edges are not repeated
and queued edges are refilled. No Task1-specific EOI retry mechanism is
added. See `results/task1/vgic-maintenance.md`.

### 3.6 Lock Boundaries and Guest Console

Dispatcher locks are sequential and callbacks run outside them. The trace
separates queue lock time from wake time with `enqueue_start`, `enqueue`, and
`notify` events.

The guest console required a separate fix. Guest UART I/O previously acquired
the global console state mutex; under dual-guest output, Zephyr could block in
`printk` behind console control or physical UART work. Each serial backend now
has bounded per-endpoint rings protected by `IrqSafeMutex`. Guest read/write
does not take the global state/output lock or allocate; housekeeping drains,
formats, and writes later. See `results/task1/locking-discipline.md`.

### 3.7 VM-Exit Accounting

Per-CPU cacheline-aligned Relaxed atomics count timer, IRQ, MMIO, WFI,
HVC/SMC, sysreg, GIC interface, SGI, CPU-up, shutdown, no-op, and other exits.
The `vmexit stat` shell command reports cumulative counts and interval rates.
These counters provide the explanation layer between configuration changes
and latency observations.

## 4. Resource Allocation

| Host pCPU | Owner | Activity |
|---|---|---|
| 0 | AxVisor housekeeping | shell, host timer, virtual-device work |
| 1 | Zephyr vCPU0 | exclusive RT partition, no host periodic tick |
| 2 | Linux vCPU0 | stress-ng and guest IRQ housekeeping |
| 3 | Linux vCPU1 | cyclictest measurement, `isolcpus`; `nohz_full` requested but unsupported by this guest kernel |

| Guest memory | Range |
|---|---|
| Linux | `0x8000_0000..0xA000_0000` |
| Zephyr | `0xA000_0000..0xC000_0000` |

Linux bootargs place measurement on guest CPU1 and route load/IRQs to guest
CPU0. The runner derives the load CPU as `1 - RT_CPU` and rejects CPU ids
outside the two-vCPU topology. The full table is in
`results/task1/allocation-table.md`.

## 5. Measurement Design

### 5.1 Linux cyclictest

The guest runs cyclictest at priority 90, 1 ms interval, with a 20,000 us
histogram range. Samples above that bound remain visible as explicit overflow
counts, so a long-tail sample cannot silently reduce the denominator.
stress-ng, when enabled, is launched through an outer BusyBox
`taskset` on the load CPU. cyclictest starts with all online CPUs visible so
musl/libnuma discovers the topology correctly, then pins its worker with
`-a`.

Histogram accounting includes overflow. Loop-mode smoke requires:

```text
bucket_samples + overflow_samples = total_samples = requested loops
```

Formal runs use cyclictest `-D` duration mode because stressed TCG does not
execute a fixed number of 1 ms loops in a predictable wall time. Duration-mode
acceptance checks internal histogram consistency and the difference between
guest `/proc/uptime` values immediately before and after cyclictest. Runner wall
time is metadata only because it also includes build, boot, and pre-test work.

### 5.2 Zephyr periodic sampler

The same application records 300 samples at a 10 ms period and emits:

```text
sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
```

AxVisor and native QEMU runs use the same parser and statistics script.
For the matrix image, the sampler waits at a UART gate. The runner releases it
only after Linux reports `RT_CYCLICTEST_START`, and rejects logs unless Zephyr
start and completion both occur before Linux completion. VM-exit snapshots are
taken immediately before and after this Zephyr window, plus once at Linux end.

### 5.3 Scenarios

| Scenario | Linux load | Host dedicated pCPU | Zephyr mode |
|---|---|---|---|
| `idle` | none | none | virtualized |
| `stress-noiso` | stress on guest CPU0 | none | virtualized |
| `stress-dedicated` | stress on guest CPU0 | pCPU1 | virtualized |
| `stress-rt` | same stress | pCPU1 | passthrough profile |

Each formal matrix scenario requests 1800 seconds via cyclictest `-D`.
Stability requests 3600 seconds. Exact-loop mode remains available for short
smoke tests.

## 6. Evidence to Date

### 6.1 Formal 30-Minute Matrix

All four post-fix duration runs completed and passed their archived hashes.
The dedicated scenarios also kept pCPU1 host periodic scheduler ticks at zero
in all three snapshots.

Linux cyclictest results:

| Scenario | Samples | Min | Avg | Max | Overflow |
|---|---:|---:|---:|---:|---:|
| `idle` | 1,713,691 | 126 us | 534 us | 304,645 us | 12 |
| `stress-noiso` | 1,746,156 | 139 us | 510 us | 303,853 us | 14 |
| `stress-dedicated` | 1,727,468 | 151 us | 578 us | 302,045 us | 12 |
| `stress-rt` | 1,720,603 | 149 us | 604 us | 303,101 us | 12 |

Zephyr 10 ms periodic results:

| Scenario | Samples | Mean jitter | p99 jitter | Max jitter |
|---|---:|---:|---:|---:|
| `idle` | 300 | 601.023 us | 866.560 us | 914.768 us |
| `stress-noiso` | 300 | 665.170 us | 848.032 us | 919.488 us |
| `stress-dedicated` | 300 | 711.746 us | 1,029.120 us | 1,067.168 us |
| `stress-rt` | 300 | 614.024 us | 810.224 us | 882.672 us |

On this QEMU TCG platform, `stress-rt` reduces Zephyr mean/p99/max jitter by
7.69%/4.46%/4.00% relative to `stress-noiso`, and by
13.73%/21.27%/17.29% relative to `stress-dedicated`. Dedicated placement alone
does not improve this virtualized Zephyr path; the passthrough RT profile is
the configuration that recovers the stressed jitter distribution. The Linux
cyclictest average is 18.43% higher in `stress-rt` than `stress-noiso`, so the
claim is deliberately limited to the Zephyr RT partition path.

### 6.2 One-Hour Stability

The accepted `stress-rt` stability run completed 3,347,556 Linux cyclictest
samples over 3598.96 guest seconds. Zephyr completed 300/300, pCPU1 host
periodic scheduler ticks stayed zero in all snapshots, all hashes pass, and no
watchdog artifacts were produced.

| Run | Linux avg | P90 | P95 | P99 | P99.9 | Max |
|---|---:|---:|---:|---:|---:|---:|
| 30-minute `stress-rt` | 604 us | 821 us | 894 us | 1060 us | 8695 us | 303101 us |
| one-hour `stress-rt` | 679 us | 872 us | 950 us | 2061 us | 9212 us | 304760 us |

The one-hour run is functionally stable, but its P99 is 94.43% higher than the
30-minute run. The available histogram is aggregate-only, so it cannot locate
when the tail changed. Zephyr still samples only 300 points near workload
start; it cannot establish first-window versus last-window degradation. The
current evidence therefore supports long-run completion and isolation, not a
claim that the latency distribution remains unchanged over time.

### 6.3 Short Matrix Smoke

All four short scenarios completed Zephyr 300/300 and cyclictest 5000/5000:

| Scenario | Histogram buckets | Overflow | Total |
|---|---:|---:|---:|
| idle | 363 | 4637 | 5000 |
| stress-noiso | 153 | 4847 | 5000 |
| stress-rt | 196 | 4804 | 5000 |
| stress-dedicated | 15,304 | 14 | 15,318 |

This validates orchestration, affinity, completion, and accounting. It is not
the formal 30-minute performance result.

The first formal idle loop-mode run completed 1,800,000 samples. The stressed
loop-mode run timed out twice, at 2040 and 3900 wall seconds, without a guest
crash. This exposed the invalid fixed-loop/fixed-duration assumption and led
to duration mode. An early 10-second duration smoke completed 9349 samples but
still lacked guest-uptime markers. The final gated 20-second smoke recorded
20.80 seconds of guest uptime and enforced the correct workload overlap.

### 6.4 Native Zephyr on QEMU

| Metric | Result |
|---|---:|
| samples | 300 |
| mean jitter | 405.783 us |
| p99 jitter | 599.056 us |
| max jitter | 836.048 us |

The raw log, image, manifest, command, CSV, statistics, metadata, and hashes
are in `results/task1/native-zephyr/`.

The formal `stress-rt` result remains above native QEMU by 51.32% in mean,
35.25% at p99, and 5.58% at max. Virtualization overhead and host scheduling
noise therefore remain measurable even after partitioning.

### 6.5 Queue Overload Replay

Arrival interval is 50 us, service interval is 1000 us, and overload arrivals
last 100 ms:

| Queue | Accepted | Overflow | Max depth | p99 queue latency | Max queue latency |
|---|---:|---:|---:|---:|---:|
| old unbounded | 2000 | 0 | 1901 | 1881.05 ms | 1900.05 ms |
| current capacity-64 | 163 | 1837 | 64 | 64 ms | 64 ms |

This deterministic replay reads the old and current source contracts. It
proves bounded resident backlog and explicit overload reporting; it is not a
QEMU end-to-end timing benchmark. Evidence is in `results/task1/overload/`.

## 7. Failed Paths and Lessons

### Preemptive scheduling

RR/preempt and FIFO/preempt both caused EL2 data abort loops when an IRQ
entered during the guest world switch. Scheduler handoff, IRQ stack, and vCPU
context ownership need a joint design; feature wiring alone is unsafe.

### IPI with dedicated startup

IPI alone passed ordinary tests but combined with a dedicated pCPU caused the
secondary-core enable barrier to stop at 50 percent. Reverting IPI restored
4/4 startup. Combination tests are required for CPU-isolation changes.

### Measurement topology

The first scripts isolated one CPU, ran cyclictest on another, and attempted
stress on the isolated CPU. Individual commands looked plausible, but the
whole experiment measured the opposite topology. Regression tests now assert
the complete role mapping.

### Console observability bias

The guest appeared to stop in a timer-sensitive workload, but the silent
blocking component was global console serialization. A progress log is not a
neutral observer when its write path shares locks with control operations.

## 8. Reproduction

```bash
# Build the Zephyr image for the AxVisor RT partition
scripts/test/rt-partition/build-zephyr-periodic.sh

# Build/stage cyclictest, stress-ng, and the Linux initramfs
scripts/test/rt-partition/build-rt-tools.sh

# Short smoke example
RT_SCENARIO=stress-rt RT_LOOPS=5000 RT_OUTPUT_ROOT=tmp/rt-partition/validation-results \
  scripts/test/rt-partition/run-cyclictest.sh

# Formal 30-minute matrix, one scenario at a time
RT_SCENARIO=idle RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-noiso RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-dedicated RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh
RT_SCENARIO=stress-rt RT_DURATION_SEC=1800 scripts/test/rt-partition/run-cyclictest.sh

# Per-scenario 120-second TCG calibration
scripts/test/rt-partition/run-runtime-calibration.sh

# Native QEMU baseline
ZEPHYR_MEMORY_BASE=0x40000000 \
  ZEPHYR_START_GATED=0 \
  OUT_DIR=tmp/rt-partition/native-zephyr \
  BUILD_DIR=tmp/rt-partition/native-zephyr/build \
  scripts/test/rt-partition/build-zephyr-periodic.sh
QEMU_BIN=/home/huhu/.local/bin/qemu-system-aarch64 \
  scripts/test/rt-partition/run-native-zephyr.sh

# Deterministic overload replay
python3 scripts/test/rt-partition/virq_overload_model.py
```

## 9. Limitations

- QEMU TCG has host scheduling and instruction-count timing artifacts; effects
  below 50 us are not treated as hardware evidence.
- No physical board is available, so physical GIC, cache, and interrupt WCET
  remain unmeasured.
- Fixed-priority preemption and reschedule IPI are not complete in the final
  branch.
- CNTP remains software-emulated, so dedicated placement cannot safely remove
  all WFI exits.
- The guest kernel lacks `CONFIG_NO_HZ_FULL`; it logs `nohz unsupported`.
  CPU affinity and IRQ housekeeping separation are active, but Linux guest
  full-dynticks isolation is not.
- Dedicated pCPU acceptance is based on `host-periodic-ticks.csv`: event-driven
  guest timer IRQ exits are not host periodic scheduler ticks. The earlier
  dedicated calibration stalls were manifestations of the common
  `IRQ_ROUTES` deadlock; the post-fix formal duration runs complete, but remain
  QEMU TCG relative evidence rather than a hardware latency bound.
- Historical `/tmp/ab-*.log` and `/tmp/e1-*.log` must be supplied from the
  experiment machine before their old 311-to-301 us claims can be recomputed.
