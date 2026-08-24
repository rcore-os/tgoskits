# T3.3 Real-Time Interrupt and Console Locking Discipline

## Trace Segments

The realtime trace now exposes the queue critical path separately from wakeup:

| Segment | Meaning |
|---|---|
| `enqueue_start -> enqueue` | target lookup plus bounded queue lock/push |
| `enqueue -> notify` | lock-free waiter notification |
| `notify -> ipi` | host IPI boundary |
| `running -> inject` | vCPU-side dequeue and architecture injection |

`enqueue_start` is recorded immediately before `VcpuIrqDispatcher::enqueue`.
The stable event name is covered by
`runtime::trace::tests::enqueue_start_has_a_stable_trace_name`.

## Rules

1. Dispatcher locks are acquired sequentially, never nested. Target pCPU
   lookup completes before the bounded queue is locked.
2. Wake, IPI, physical UART writes, and virtual-device callbacks are forbidden
   while a dispatcher, VM machine, wait-queue, or console control-state lock is
   held.
3. A critical section may only update owned state or take a snapshot. Work
   that crosses an ownership boundary runs after the guard is dropped.
4. Guest-facing queues are bounded. Overflow is explicit and traced instead
   of silently growing host memory.
5. A vCPU dequeues one edge at a time. A blocked head stays queued; a backend
   race after dequeue uses the dedicated retry slot.

## vIRQ Queue Rationale

The old batch-drain shape created a lock-free window: several same-vector
edges could leave the queue, the first could occupy the only LR, and later
edges could then fail injection after they had already lost queue ownership.
The current `pop_if` keeps a busy head in place and removes only one injectable
edge under the queue lock.

The retry slot is intentionally outside the capacity-64 producer queue. If an
edge is popped and the backend becomes full before injection, restoring that
edge cannot lose a race with a concurrent producer filling the bounded queue.
The slot is included in `has_pending`, so the vCPU cannot park while retry work
exists. Terminal backend failures are logged and dropped; retryable resource
conflicts survive.

Regression coverage includes queue capacity, per-vCPU isolation, FIFO order,
blocked-head retention, retry-slot retention, pending-state accounting, and
the guarantee that notify/IPI callbacks run after the queue lock is released.

## Deadlocks Found and Removed

### Wait Queue vs VM Machine Lock

A parked vCPU evaluates its wake predicate while holding the wait-queue lock;
that predicate reads VM state through the machine lock. The old interrupt
path could hold the machine lock and then notify the wait queue, forming an
ABBA cycle. `queue_interrupt` now snapshots the runtime handle under the
machine lock and performs dispatch/wake after releasing it.

### Guest Console Global Lock

The guest UART backend previously took the global
`std::sync::Mutex<ConsoleState>` on every read/write. Under dual-guest output,
a vCPU printing through Zephyr `printk` could wait behind attachment,
formatting, or physical UART work and appear to stall inside the guest.

Each backend now owns bounded `IrqSafeMutex` input/output byte rings. Guest
UART exits touch only their endpoint and do not allocate. The housekeeping
flush path snapshots endpoints, drains at most 4 KiB per endpoint, performs
prefix formatting under control state, then writes the physical UART outside
that state lock. Replaced or stopped backend generations are deactivated so
late I/O cannot leak into a new VM instance.

The regressions `guest_write_does_not_wait_for_console_control_state` and
`guest_write_does_not_wait_for_the_physical_host_writer` cover the two
blocking boundaries. Endpoint tests cover the 64 KiB output bound and stale
generation invalidation.

### IRQ Route Registry Re-entry

The `ax-sync` migration changed `IRQ_ROUTES` from `SpinNoIrq` to the generic
`SpinLock`. Its mutation paths were moved to `lock_irqsave`, but the forward
and reverse lookup paths still called `lock`, which disables preemption only.
If ordinary control-plane code was interrupted while holding the reverse
lookup lock, hard-IRQ dispatch re-entered the same lock through
`ActiveIrq::id -> resolve_irq_route`. The interrupted CPU spun forever and
eventually all physical CPUs contended on `IRQ_ROUTES`.

All route registry accesses now use the single `irq_routes` IRQ-save helper.
`irq_route_registry_never_uses_preempt_only_locking` prevents direct
`IRQ_ROUTES.lock()` from returning. The 1800-second diagnostic register dumps
and the post-fix 1,800,000-sample run are preserved under
`results/task1/matrix/diagnostic-1800-2026-08-16/` and
`results/task1/matrix/idle/` respectively.

## Remaining Timing Claim

These rules bound queue occupancy and remove known lock cycles; they do not
claim a hardware worst-case lock latency from QEMU. The trace segments are the
measurement interface for board-level WCET work.
