# IRQ-tail preemption closure (2026-08-20)

## Status

The acknowledged-host-IRQ path is now implemented and enabled for the
AArch64 AxVM path. The implementation is deliberately narrower than the first
two experimental versions: GIC completion happens before the IRQ/preemption
guards are released, and a fixed-priority scheduler requests an IRQ-tail
switch only when the IRQ woke a strictly higher-priority task (or when the CPU
was idle). RR/FIFO keep their existing eager wake behavior.

This is a software critical-path change. It is not a CPU partition or a
dedicated-pCPU result.

## Original defect

AxVM acknowledges a physical GIC interrupt before deciding whether it belongs
to a Guest or to the host. Host-owned interrupts were dispatched through the
dynamic IRQ framework with `dispatch_irq`, bypassing the common AxHAL IRQ-entry
guard. A handler could wake a vCPU and set `need_resched`, but the VM-exit
return boundary did not reliably consume that request. The other unsafe
extreme was to schedule on every acknowledged IRQ.

## Why the first implementations failed

| Version | Evidence | Diagnosis |
|---|---|---|
| dispatch, then release preemption, then deactivate GIC | Linux P99 reached the 20 ms histogram ceiling | GIC completion was ordered after the scheduling boundary |
| deactivate inside the guard, but every IRQ tail could reschedule | Linux stopped after roughly 258 cyclictest samples; `slice_preserving_preemptions=78906`, `voluntary_requeues=6128290` | timer/console IRQ traffic caused unbounded same-priority scheduler churn |

The failed artifacts remain under:

- `results/task1/irq-tail-preemption-smoke-20260820/`
- `results/task1/irq-tail-preemption-complete-before-resched-smoke-20260820/`

They are negative evidence and are not included as performance wins.

## Implemented mechanism

1. `axhal::irq::dispatch_acknowledged_irq` enters the normal local-IRQ and
   preemption guard sequence without acknowledging the controller a second
   time.
2. It dispatches the already-resolved `IrqId`, executes the caller-owned GIC
   deactivate/EOI closure while IRQ context and preemption protection are
   still active, then withdraws IRQ context and releases preemption.
3. `axvm` resolves the acknowledged GIC token and supplies only the matching
   completion closure; GIC ownership and virtual interrupt routing are
   unchanged.
4. In fixed-priority modes, `axtask` suppresses an IRQ-tail `need_resched` for
   equal- or lower-priority wakeups while a non-idle task is running. A strict
   higher-priority wakeup still preempts immediately. The idle-task exception
   prevents a minimum-priority task from remaining queued on an idle CPU.

The relevant source files are:

- `os/arceos/modules/axhal/src/irq.rs`
- `os/arceos/modules/axtask/src/run_queue.rs`
- `virtualization/axvm/src/arch/aarch64/gic.rs`

## Validation

Static and host checks:

```text
cargo check -p ax-hal -p ax-task -p axvm                 PASS
cargo test -p ax-sched                                   20 passed
cargo test -p ax-task --features test,smp,sched-prio-rr  56 passed
cargo test -p ax-hal --features axtest,host-test         4 passed
Python rt-partition regression tests                     55 passed
shell syntax and git diff --check                        PASS
```

The host `cargo test -p axvm --lib` link step is not a valid bare-metal test
for this package (it lacks the linker-provided `STACK_SIZE`/per-CPU symbols),
while the release AArch64 AxVisor build used by QEMU succeeds.

Current QEMU direct shared-pCPU smoke protocol:

```text
Linux vCPU0 -> pCPU1
Linux vCPU1 -> pCPU2
Zephyr vCPU0 -> pCPU1
dedicated CPUs = none; host burner = disabled
Linux duration = 30 s; Zephyr samples = 3000
```

| Scheduler | Result path | Linux max | Linux P99 | Zephyr P99 / max | Closure |
|---|---|---:|---:|---:|---|
| RR | `irq-tail-priority-filter-rr-smoke-20260820/stress-guest-shared/` | 310,639 us | 7,142 us | 31.582 ms / 67.259 ms | complete; hold/release; QMP 0 |
| FP-RR | `irq-tail-priority-filter-smoke-20260820/stress-guest-shared/` | 349,196 us | 19,549 us (censored) | 35.632 ms / 145.725 ms | complete; hold/release; QMP 0 |

Both logs contain, in order:

```text
PERIODIC LATENCY COMPLETE samples=3000
RT_CYCLICTEST_COMPLETE
RT_CYCLICTEST_HOLD_READY
RT_CYCLICTEST_RELEASED
PSCI_SYSTEM_OFF
```

Neither current smoke emitted `VM[1] is not running`, a progress-watchdog
failure, or the old Linux-stall signature. The latency values still vary
substantially under QEMU TCG; this validation proves the control-flow and
stability gate, not a universal P99 improvement.

## Acceptance boundary

The delivered claim is:

- acknowledged IRQ completion is now ordered correctly;
- fixed-priority IRQ-tail preemption is priority-aware and avoids the proven
  same-priority storm;
- direct shared-pCPU Linux/Zephyr runs complete reproducibly with archived
  markers and counters.

It is not claimed that every IRQ source, every architecture, or physical-board
worst-case latency has been exhaustively proven.
