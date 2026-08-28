# Unified Host Timer Transport

## Status and scope

This document defines the host-deadline transport shared by ArceOS and AxVM.
It is based on Linux v7.1 KVM, hrtimer, and clockevents ownership, adapted to
TGOSKits crate boundaries. It does not merge guest architectural timer state,
APIC/VGIC pending state, or scheduler policy into one subsystem.

The direct-ACPI VMX timeout reported by PR #1775 is a motivating failure
sample, not a proven timer bug. A claim that this design fixes that timeout
requires a reproduction that records VM exits, guest RIP and RFLAGS.IF,
pending vectors, clockevent generations, timer promotions, vCPU entries, and
wakeups. Without such evidence the result must be reported as not reproduced.

## Ownership model

| Linux v7.1 owner | TGOSKits owner | Invariant |
| --- | --- | --- |
| KVM LAPIC and architectural timer | AxVM and architecture vCPU crates | Own guest-visible registers, masking, periodic state, and interrupt level |
| hrtimer per-CPU bases | `ax-task::PerCpuTimerBase` | Own absolute host deadlines and callback lifecycle |
| clockevents | `ax-runtime::LocalClockEvent` | Exclusively own physical one-shot comparator programming |
| `kvm_vcpu_kick` and vCPU wait condition | AxVM generation plus pre-bound wake capability | Publish completion before waking and recheck after publishing wait state |

Linux KVM arms absolute hard hrtimers for both x86 LAPIC deadlines
(`arch/x86/kvm/lapic.c`) and the Arm virtual timer software fallback
(`arch/arm64/kvm/arch_timer.c`). The generic hrtimer layer arbitrates the next
per-CPU expiry, while `kernel/time/clockevents.c` alone translates an absolute
expiry into device cycles. The split matters: a KVM device owns what expiry
means, but does not own the host comparator.

## Logical timer base

Each online CPU owns one `PerCpuTimerBase`. Its task wakeups, Future wakers,
soft kernel timers, and hard kernel timers remain typed queues because their
payloads and execution contexts differ. They share:

- a single IRQ-safe owner lock;
- absolute `MonotonicDeadline` values with no zero or maximum-value sentinel;
- one non-reused `KernelTimerHandle { owner_cpu, identity }` namespace;
- a single earliest-deadline query and publication path;
- bounded IRQ promotion and worker draining, currently 64 entries per pass.

The timer IRQ may expire task wakeups, promote soft work, and claim a bounded
hard callback while holding the base lock. It releases the lock before invoking
any callback. Ordinary kernel-timer callbacks, Future wakers, and payload
destruction run in the pinned `ktimers/<cpu>` worker. The separately retained
`register_timer_callback` scheduler-tick observers keep their existing timer-IRQ
context and are not deadline-queue entries. If a budget is exhausted, the owner
republishes work and keeps advancing with the platform minimum delta.

Future expiry uses an explicit IRQ-to-worker ownership transition. Once the IRQ
publishes a due Future head to `ktimers/<cpu>`, that head is hidden from the
earliest-deadline query until the worker finishes its bounded drain pass. The
worker either republishes the next live deadline or keeps its work notification
pending. This prevents a due Waker, which cannot execute in IRQ context, from
being fed back into the comparator as an immediate deadline and starving the
worker that must consume it.

Hard callbacks are constructed through an unsafe API. Their safety contract
requires bounded execution with no allocation, destruction, sleeping, registry
lookup, or sleepable lock acquisition. They may use only IRQ-safe atomics,
locks, and capabilities bound before the timer is armed.

## Clockevent state machine

`ax-runtime::LocalClockEvent` is CPU-local and has four phases:

```text
Offline --online--> Armed --IRQ claim--> Firing --finish(deadline)--> Armed
                         ^                   |                         |
                         |                   +--finish(None)--> Idle--+
                         +------- earlier logical publication --------+
```

It carries a CPU epoch and an arm generation. CPU online/offline changes the
epoch; each comparator arm changes the generation. A completion token from an
older epoch or generation cannot rearm the CPU. While `Firing`, logical
publications are accumulated and merged with the post-IRQ recomputation.
Exactly one comparator program is committed after the logical queues advance.

The platform API currently has no portable cancel-pending primitive. Cancelling
or moving the logical head later therefore leaves at most one conservative
stale physical edge. That IRQ claims the current arm, observes no expired
logical timer, and recomputes the live minimum. Logical layers never infer a
hardware pending state and never rewrite another CPU's comparator.

The runtime merges the periodic scheduler tick with the earliest task deadline.
`register_timer_callback` remains a periodic scheduler-tick observer and is not
inserted into the deadline base.

## Handle, cancellation, and migration rules

A registration is permanently owned by the calling CPU. Remote cancel or
disarm changes only that owner's logical queue. It never directly programs the
remote comparator. A callback already claimed may complete, but its tombstone
prevents a restartable action from becoming active again.

An AxVM vCPU migration performs these transitions in order:

1. publish a new vCPU timer generation so the old callback becomes stale;
2. cancel the old CPU registration and preserve one possible stale edge;
3. bind the wake capability and create a stable registration on the new CPU;
4. re-read guest timer state and arm the new registration if still required.

CPU offline invalidates its clockevent epoch before the comparator is disabled.
No new timer may register on an offline CPU. Remaining registrations must be
cancelled or migrated before the per-CPU area is reclaimed.

## vCPU wait and interrupt publication

The blocked-vCPU path follows the same lost-wakeup rule as KVM:

1. the timer callback publishes its completed generation with `Release`;
2. it invokes only a pre-bound vCPU wake capability;
3. the waiter publishes its waiting state;
4. the waiter rechecks completion and architectural pending conditions with
   `Acquire` before sleeping.

The hard callback never takes a VGIC lock. Arm virtual-timer PPI level is
recomputed after the vCPU wakes and immediately before guest entry. x86 LAPIC,
PIT, LoongArch architectural timers, and emulated device timers use soft kernel
timers because their callbacks require ordinary task context.

## Lock ordering and failure handling

The required order is:

```text
guest timer/device state -> timer-base lock -> publish after unlock
clockevent local exclusive state -> platform comparator
```

Callbacks run with neither the guest state lock nor timer-base lock held. No
path holds `LocalClockEvent` state while acquiring a timer-base lock. Capacity,
unsafe context, offline CPU, owner mismatch, stale handle, and generation
exhaustion are typed errors. Identity or epoch exhaustion is never wrapped.

## Migration and validation

Temporary compatibility entry points may delegate to this core during the
single-PR migration, but a timer must never be inserted into two queues or arm
two comparator owners. The final tree removes AxVM's timer wheel, timer worker,
external deadline source, IRQ callback, and direct `ax-timer-list` dependency.
`ax-task` continues to use `ax-timer-list` internally for its typed task-wakeup
lane; it is not a second comparator owner or an AxVM timer wheel.

Required model coverage includes earlier/later rearm, head cancellation,
same-deadline ordering, stale handles and epochs, early/stale IRQ edges, budget
exhaustion, cancel-versus-rearm, and proof that soft callbacks run outside IRQ
and the base lock. Runtime coverage includes notify-versus-timeout, stale task
timer rejection, Future poll/drop after CPU migration, and clockevent idle.

Architecture validation includes Axvisor smoke tests on aarch64, riscv64,
loongarch64, and x86_64; Arm GICv2/GICv3 timer stress; and the x86 VMX sequence
`direct-acpi-vmx`, `mp-fallback-vmx`, and `ovmf-acpi-vmx`. Hardware-dependent
tests must be reported separately when the required KVM or LVZ capability is
not available.
