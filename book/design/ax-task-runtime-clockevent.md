# ax-task Task Deadlines and Runtime Clockevents

## Status

This document defines the timer, interrupt, and CPU-local ownership boundary for
the ax-task migration. It supersedes the earlier requirement that
`components/ax-task` remain byte-identical to PR #1596.

The design is intentionally limited to task scheduling. It does not introduce a
general callback timer service.

## Problem

The previous integration mixed three independent notions of the next timer:

- the scheduler's next task deadline;
- the runtime's periodic tick;
- the deadline last programmed into the physical clockevent.

The values were updated independently, so cancellation, a later replacement,
or an update while the interrupt handler was firing could leave the runtime
with stale state. The ax-task timer heap also retained obsolete rearm entries,
which made capacity depend on historical arms rather than active deadlines.
Finally, a runtime callback registry invoked arbitrary consumers directly from
the timer hard-IRQ path.

## Prior art

The primary reference is Linux `v7.1`, commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`, with
`CONFIG_EXPERT=y`, `CONFIG_PREEMPT_RT=y`, `CONFIG_HIGH_RES_TIMERS=y`,
`CONFIG_SMP=y`, and `CONFIG_HOTPLUG_CPU=y`. The configuration is generated
out-of-tree so the reference checkout remains unmodified.

- `kernel/time/clockevents.c::clockevents_program_event` owns physical event
  programming.
- `kernel/time/hrtimer.c::hrtimer_interrupt` invalidates the fired event,
  performs bounded hard-timer work, and computes one replacement event.
- `kernel/time/tick-oneshot.c::tick_program_event` connects the generic tick
  layer to the selected per-CPU clockevent.
- PREEMPT_RT moves non-hard hrtimer callbacks to soft/threaded processing;
  only explicitly hard timers execute their callback in hard IRQ.
- `kernel/sched/core.c` owns runqueue placement and migration under the
  runqueue lock; `TASK_ON_RQ_MIGRATING` prevents a task from being observed on
  two runqueues during migration.
- `kernel/locking/rtmutex.c` performs PI owner deboost and waiter handoff as one
  ordered transaction, then wakes the selected waiter after releasing the raw
  metadata lock.
- `kernel/irq_work.c` and the scheduler IPI path publish work before ringing
  the target CPU and consume the delivered publication before accepting a new
  coalesced notification.
- `kernel/events/core.c` distinguishes task-owned perf contexts from CPU-owned
  contexts and executes CPU-context changes on the owning CPU.
- `fs/exec.c::exec_mmap` publishes and activates the new `mm` before retaining
  the old `mm` for deferred release; the active hardware page-table root is
  never reclaimed between those operations.
- x86 double faults use a dedicated per-CPU IST stack and the paranoid entry
  path determines the live GS base independently of the saved privilege level.

The corresponding TGOSKits rule is that ax-task publishes scheduling
deadlines, while ax-runtime exclusively owns the physical clockevent.

## Ownership model

### ax-task

Each `CpuLocal` exclusively owns one fixed-capacity `TaskDeadlineQueue`.
Entries are restricted to generation-bearing task identities. An entry cannot
contain an arbitrary callback, OS object, or driver object.

The queue covers:

- sleep, park, and wait timeout deadlines;
- scheduler policy deadlines such as RR, Fair, and Deadline;
- deadlines needed to make bounded deferred task work progress.

Each embedded node has at most one active heap entry. Rearm physically replaces
the old entry and cancel physically removes it, so inactive tombstones never
consume capacity.

`ParkTicket` is the move-only owner of one park generation and its optional
deadline token. Cancellation first validates the token's generation and owner
CPU without consuming it. A retryable owner mismatch therefore leaves both the
ticket and physical queue entry intact; the ticket is cleared only after a
matching queue removal or after the expiry path has already removed that
generation. This makes partial failure unable to orphan an active timer node.

After changing the earliest local task deadline, ax-task publishes a
`TaskDeadlineUpdate` containing:

- a monotonically increasing non-zero generation;
- an optional absolute monotonic deadline;
- a sticky indication that deferred work must be serviced at a scheduler safe
  point.

The runtime may discard an update only when its generation is older than the
last accepted generation.

### ax-runtime

Each CPU owns one `LocalClockEvent`. It is mutable only while an
`ExclusiveCpu` proof covers local IRQ/re-entry exclusion. Its phases are:

- `Offline`: the CPU-local area exists but the clockevent is unavailable;
- `Idle`: no physical event is regarded as armed;
- `Armed`: one absolute deadline is programmed;
- `Firing`: the delivered event was invalidated and updates are being merged.

The object is the only storage for task deadline generation, task deadline,
periodic deadline, deferred-work state, and the deadline regarded as physically
armed. There are no parallel per-CPU scalar caches.

Publishing an earlier deadline while `Armed` may reprogram immediately.
Publishing cancellation or a later deadline leaves the earlier harmless event
armed; when it fires, the handler recomputes the authoritative minimum.
Publishing while `Firing` only updates source state. The handler programs the
minimum exactly once when it finishes.

The runtime wraps `Firing` in an ownership guard. Normal completion merges the
latest task update and finishes the transaction once. An abandoned host-test
transaction recovers to `Idle` and recomputes the arm, so an error path cannot
silently strand the clockevent in `Firing`.

## Interrupt sequence

Platform interrupt controllers and timer devices acknowledge or invalidate the
delivered event before calling the runtime handler. The runtime then performs:

1. enter `LocalClockEvent::Firing` and forget the previously armed deadline;
2. advance the periodic source;
3. call ax-task's bounded clockevent handler;
4. publish reschedule and deferred-work sticky state;
5. merge every source and program one replacement event;
6. return to the platform IRQ layer for its architecture-specific EOI.

Hard IRQ work is allocation-free, non-blocking, and bounded by the caller's
budget. Expired task deadlines are copied into preallocated CPU-local storage;
waking threads and running callbacks happens at a scheduler safe point.

Batch exhaustion publishes both sticky deadline work and `need_resched`.
Beginning a safe-point pass consumes the old sticky publication before
draining, then republishes it if another bounded batch remains. Work arriving
during the pass is therefore not cleared by the older completion, and idle
cannot wait while deferred expiry work is pending.

## Idle and remote delivery

The idle path holds IRQ exclusion while it publishes polling state, checks
remote and timer pending state, observes the current deadline generation, and
commits to the architecture's atomic wait primitive. Pending work prevents the
wait.

Remote producers publish queue state and a sticky bit with release ordering
before sending an IPI. The target consumes the delivered pending indication at
handler entry before rechecking owner work, allowing a concurrent producer to
ring the doorbell again.

The shared physical IPI vector has a separate scheduler doorbell. The runtime
publishes this doorbell before invoking the architecture IPI backend, and the
handler consumes it before acknowledging ax-task's scheduler-delivery epoch.
Unrelated IPI callbacks therefore cannot acknowledge scheduler work that they
did not deliver.

## Generic consumers

VM and POSIX callback timers do not enter ax-task. AxVM owns a CPU-affine
task-context worker. The worker sleeps with a task deadline until the VM timer
wheel's next expiry. A bounded IRQ-safe notification endpoint wakes it when a
new earlier VM deadline is inserted. VM callbacks run only in task context.

If multiple subsystems later need a shared general-purpose deadline engine,
that engine should become a separate component rather than widening ax-task.

StarryOS wall-clock/POSIX timers follow the same task-context rule. Producers
modify the timer queue and then increment an epoch before notifying the fixed
worker. The worker samples the epoch before taking its queue snapshot. A
registration racing either side of that snapshot therefore changes the wait
predicate instead of being absorbed as the worker's baseline. Timer metadata
uses a sleeping PI mutex; only the IRQ-facing coalesced notification endpoint
uses atomics and a generation-checked direct wake.

## UART synchronization boundary

Serial drivers expose three capabilities:

- a task/control endpoint owning ordinary configuration and data flow;
- a hard-IRQ endpoint restricted to bounded status, ACK/mask, FIFO drain, and
  publication;
- a non-blocking emergency-TX endpoint restricted to panic-safe register
  access.

The worker owns the normal port. Task-context control, completion, and
subscription state may use sleepable synchronization. IRQ, scheduler, panic,
and atomic-log paths use bounded queues, atomics, `IrqWaitCell`, or a
non-blocking raw gate. Emergency output drops bytes on contention instead of
waiting.

Normal TX uses a fixed-capacity MPSC ring whose reservation and publication
states are distinct: the sole consumer never spins on a producer that was
preempted after reserving a slot. Start/stop epochs reject frames from an older
device lifecycle. Register contention sets a sticky retry bit and wakes the
worker; it is not treated as a completed IRQ. Emergency TX has a fixed byte
budget and uses the same non-blocking register gate as the hard-IRQ endpoint,
so panic output cannot wait for an interrupted register owner.

## Safety and compatibility

- CPU-local mutable objects require `CpuPin` plus `ExclusiveCpu`.
- The scheduler baton remains the only cross-context switch guard.
- Switch tail clears the outgoing thread's `on_cpu` publication before
  reclamation is allowed.
- CPU bring-up uses `min(platform_cpu_count, CPU_CAPACITY)` consistently.
- StarryOS Linux ABI, POSIX errno, and axstd observable behavior are unchanged.
- Thread creation is transactional: failure unwinds unpublished resources in
  reverse ownership order.
- Starry process/thread publication guards cover scheduler identity, Linux
  PID/TID membership, PIDFD state, address-space ownership, stack, TLS, and
  execution context. A generation-bearing scheduler identity is never reused
  as a Linux-visible TID.
- Starry exec stages the new address-space publication, installs its hardware
  page-table root, and only then releases the old process slot. Releasing the
  old slot before the architecture switch can free the page tables still
  named by CR3 and was observed as an intermittent kernel page fault.
- x86 CPUs install a dedicated double-fault IST before loading the IDT.
  Vector 8 has a fatal, non-returning entry path that reads CR2 before other
  diagnostics and does not reuse the potentially corrupted task stack.
- Remotely sampled load state is a bounded, epoch-validated snapshot. Readers
  return “unavailable” instead of spinning indefinitely while an owner CPU is
  stopped in the middle of publication.

## Validation

Deterministic virtual-runtime tests cover rearm/cancel, stale generations,
updates during `Firing`, batch exhaustion, remote publication, idle
lost-wakeup, owner-mismatch cancellation retry, park notify-versus-timeout,
timer-worker epoch snapshots, IPI pending lifetime, and switch-tail ordering.
Loom tests cover deadline generation, unique park winners, `IrqWaitCell`, and
publish-before-IPI races. UART tests prove bounded, non-allocating hard-IRQ
behavior, producer-reservation progress, sticky register retry, overflow
handling, and non-blocking emergency output.

Targeted crate tests and clippy precede four-architecture ArceOS and StarryOS
QEMU runs. A hang is inspected at the timer begin/finish, IPI consume, idle
commit, switch tail, and UART publish/drain boundaries.

The QEMU harness treats configured success expressions as mandatory
postconditions. A zero process exit without a matching success expression is a
failure; this catches `-no-reboot` exits after x86 triple faults instead of
mistaking firmware shutdown for a passed guest suite.
