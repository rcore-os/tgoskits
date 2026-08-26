# axpoll Linux waitqueue semantics and registration ownership

## Status

This document defines the target architecture for the destructive `axpoll`
registration redesign. The migration intentionally removes the fire-and-forget
`Pollable::register` and `PollSet::register` interfaces. There is no compatibility
adapter and no parallel legacy registration path.

## Problem

The existing `PollSet` stores at most 64 anonymous wakers. Registration has no
owned result, so a caller cannot unregister an abandoned wait. Once the array is
full, a new registration replaces an unrelated entry and wakes the displaced
waker. `wake()` drains every matching entry, while individual users select
between `wake()` and `wake_one()` by convention. The registration itself does
not record whether the waiter observes readiness or competes to consume it.

Those properties violate three required invariants:

1. cancellation and object teardown must remove the exact registration rather
   than leave a stale waker until a later event;
2. capacity pressure must never manufacture a readiness notification;
3. one readiness transition must wake every non-exclusive observer and at most
   one exclusive consumer, as a single atomic waitqueue decision.

Starry pipe blocking I/O exposes the consequence directly. All blocked readers
or writers are ordinary poll registrations, and every successful transfer wakes
all matching tasks. The result is a thundering herd. The current LTP hackbench
run at commit `8895ecd2e4` timed out at the fixed 1800-second budget; its completed
process-mode samples had a 69.156 s single-CPU median and a 59.235 s four-CPU
median. That evidence does not by itself assign all runtime cost to `axpoll`, but
it establishes the workload and the fixed-budget regression boundary.

## Users and success criteria

The direct users are reusable `axpoll` sources, ArceOS and Starry blocking I/O,
Starry `poll`/`select`/`epoll`, ax-net transports, device IRQ readiness, process
and signal waits, and long-lived readiness routers.

The redesign succeeds when:

- waiter mode is explicit in the type-directed registration API;
- every registration has one RAII owner and an exact unregister operation;
- a readiness wake selects all matching shared observers and at most one
  matching exclusive consumer;
- wake callbacks never execute while the poll-state lock is held;
- hard IRQ handlers publish readiness and use the typed IRQ-to-task bridge;
  the deferred task then runs the same common wake-selection implementation;
- a callback that re-registers cannot be consumed by the wake operation already
  in progress;
- there is no fixed waiter capacity, replacement wake, raw-pointer lifetime, or
  legacy registration entry point;
- all architectures share the same upper-layer implementation. A platform may
  override a lower-level wake-delivery capability without changing registration
  or selection semantics;
- Starry pipe readiness transitions match Linux v7.1, including the deliberate
  pipe-poll observer exception;
- deterministic tests cover selection, cancellation, teardown, reentrancy,
  more than 64 waiters, and IRQ wake behavior before performance comparison.

## Crate and runtime layering

`axpoll` is the single, pure `no_std` readiness capability crate. It owns the
event type, `Pollable`, source/lease contracts, typed shared and exclusive
registration sinks, and `PollRegistrar`. It does not depend on a scheduler,
runtime, lock implementation, or concrete wait queue.

`axpoll-set` implements the common owned registration queue and wake selection
algorithm using `axpoll` contracts. Its registry uses the repository's ordinary
preemption-safe non-sleeping lock because registration and wake selection are
task/deferred-only; it does not claim a hard-IRQ entry point. It does not depend
on `ax-runtime` or `ax-task`. An
architecture may replace a lower wake-delivery primitive without changing the
queue contract or any upper-layer caller.

`PollSet::wake_with()` keeps that separation explicit: selection, registration
ownership, and cancellation remain in `axpoll-set`, while an OS adapter may
consume each selected standard `Waker` through a scheduler-aware delivery
function. Unknown Waker implementations must retain ordinary wake behavior.

Filesystem nodes implement `axpoll::Pollable` directly. `axfs-ng-vfs` and
`ax-fs-ng` do not define parallel filesystem event bits or a second poll trait,
and their production dependency graph does not depend on `ax-runtime`. Host
tests still link `ax-runtime` as the repository's external `ax-sync` provider;
that test-only linkage does not expose runtime types through the VFS API.
Making the runtime implement a VFS-owned poll interface would either create the
reverse dependency
`ax-runtime -> ax-fs-ng -> axfs-ng-vfs -> ax-runtime` or require a global
callback adapter. Both forms duplicate ownership and are rejected.

Portable drivers use the same split: readiness contracts come from `axpoll`,
the common task-context registry comes from `axpoll-set`, and hard IRQ endpoints
publish only preallocated atomic state. In particular, AIC8800 no longer vendors
a fixed-capacity poll set; its task workers retain owned registrars while the
SDIO IRQ path continues to use its single-waiter atomic notification endpoint.

Each OS composes the same source and registrar implementation with its own task
blocking boundary. `ax-task` only supplies standard task wakers and activation;
it does not own I/O registrations or Linux poll/epoll policy. A `Future` is used
only at the top task wait boundary, where Rust's cancellation lifetime is useful;
the source, VFS, and queue layers remain synchronous capabilities.

The syscall layer must also preserve Linux's synchronous I/O fast path. A direct
read or write attempts the source operation before constructing a task future or
allocating registration state. Only a real `WouldBlock` result enters the
`PollRegistrar` future below. This keeps cancellation ownership on the blocking
path without charging ready I/O for an executor, and it does not create a second
waiter-selection algorithm beside `PollSet`.

## Non-goals

- This change does not add scheduler-specific policy to `axpoll`. The scheduler
  still decides when an activated task runs.
- It does not invoke arbitrary Rust `Waker`s from hard IRQ. Registration and
  wake selection are task/deferred-context operations; an IRQ handler publishes
  state and notifies the owning service through the typed IRQ-to-task bridge.
- It does not infer exclusivity from event bits, file type, architecture, or the
  fact that a future blocks. Callers choose a typed capability.
- It does not make `poll`, `select`, or target-file epoll interest exclusive.
  They are observers even though the syscall task itself sleeps.
- It does not widen timeouts to hide wake or scheduling regressions.

## Linux v7.1 PREEMPT_RT reference semantics

Linux stores a waitqueue entry with `WQ_FLAG_EXCLUSIVE`. In
`kernel/sched/wait.c::__wake_up_common()`, matching non-exclusive entries are
woken without consuming the exclusive quota, while a callback that successfully
wakes an exclusive entry consumes one unit. With the ordinary pipe wake helper,
the quota is one.

`fs/pipe.c` uses `wait_event_interruptible_exclusive()` for blocking pipe reads
and writes. A read wakes a writer on the full-to-not-full transition. A write
wakes a reader on the empty-to-nonempty transition. `pipe_poll()` registers
non-exclusive observers before checking state and sets `poll_usage`; once poll
has observed the pipe, Linux intentionally preserves broader reader notification
for epoll compatibility even when a write begins with a non-empty pipe.

Those data/space handoffs use `wake_up_interruptible_sync_poll()`. Its `WF_SYNC`
hint makes the scheduler discount a waker that promises to sleep soon and keeps
EEVDF from forcing an early ping-pong preemption inside the migration-cost
window. Terminal close notifications remain ordinary wake-all transitions.

These are waitqueue semantics, not x86 behavior. Architecture code may optimize
the final task notification, but it must not select different waiters.

## Type and ownership model

### Waiter modes

`axpoll` exposes two public, uninhabited marker modes:

- `SharedObserver`: readiness observers such as `poll`, `select`, target-file
  epoll interests, monitoring APIs, and long-lived router registrations;
- `ExclusiveConsumer`: blocking reads, writes, accepts, sends, receives, and
  epoll-wait tasks that compete to consume one readiness transition.

`PollRegistrar<M>` is `#[must_use]`, non-cloneable, and owns every registration
made during one polling attempt. A registrar stores its current waker and a set
of erased registration leases whose mode was fixed by the capability used to
create them. Dropping or resetting the registrar unregisters every exact ID.
Ready, timeout, signal interruption, future cancellation, epoll deletion, and
object shutdown therefore use the same release mechanism.

The public mode types prevent a shared-only caller from registering an exclusive
waiter. An exclusive registrar can deliberately accept shared registrations for
composite observer boundaries. This is required for `poll` and `select`: their
outer future is one sleeping task, but every target-file registration remains a
shared observation.

### Object-safe `Pollable`

`Pollable` remains usable behind `dyn FileLike` and socket trait objects. Its two
registration methods accept sealed object-safe sinks:

```rust,ignore
pub trait Pollable {
    fn poll(&self) -> IoEvents;

    unsafe fn register_shared(
        &self,
        sink: &mut dyn SharedRegistrationSink,
        events: IoEvents,
    );

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.register_shared(sink.as_shared(), events) };
    }
}
```

The default is the common, semantically safe observer implementation. Sources
whose state is consumed by one operation override `register_exclusive()` and
use the sink's exclusive capability. This is a default implementation, not a
compatibility path: the old method no longer exists, and the selected mode is
recorded in the only registry.

Composite sources propagate the capability required by their abstraction:

- direct pipe/socket/file blocking I/O overrides to exclusive;
- `FdPollSet` always fans out shared registrations to its files;
- an epoll interest owns a shared registrar for its target file;
- each concurrent `epoll_wait` future owns a separate exclusive registrar for
  the epoll ready source;
- network/device routers own shared registrars until their worker shuts down.

### Registry and lease lifetime

`PollSet` lazily creates an `Arc<PollState>`. `PollState` contains one
preemption-safe task-context lock protecting an ordered dynamic entry vector, a
monotonically increasing
registration ID, and a closed flag. A lease holds the same `Arc` and its unique
ID. Exact unregister searches for that ID and removes only that entry.

The `Arc` is not a second owner of the public `PollSet`; it is a lifetime-safe
tombstone for outstanding leases. `PollSet::drop()` first marks the state closed
and drains registered entries. Later lease destruction can safely observe that
its ID has already been removed. No lease contains a borrow or raw pointer back
to the `PollSet`, and no self-referential future is required.

Every sink call creates its own exact lease. The public `PollSource` contract
does not accept caller-defined integer identity keys, so two distinct sources
cannot be accidentally merged. A new polling attempt first drops the previous
attempt's leases, then performs operation/register/recheck. This keeps the Linux
prepare-to-wait ordering without accumulating stale entries.

## Wake transaction

At the start of a wake, the registry snapshots the next registration ID. Only
entries older than that boundary belong to the transaction. The wake loop then:

1. locks the state and removes the next matching shared entry or, if the quota
   remains, the first matching exclusive entry below the boundary;
2. unlocks the state;
3. invokes that entry's waker;
4. repeats until all matching shared entries and one exclusive entry have been
   removed.

New registrations made by a reentrant callback receive IDs at or above the
snapshot boundary and are not consumed by the current wake. Ordered removal
preserves first-exclusive fairness. No temporary vector or stack capacity is
needed. Registration and selection are task/deferred-only. Hard IRQ never calls
an erased `Waker`; it publishes readiness and uses the runtime's typed IRQ-to-task
notification capability, after which the same `wake()` transaction runs.
For Linux pipe handoffs, `wake_with()` invokes the ArceOS executor's synchronous
wake adapter at step 3; it does not change which entries the transaction chose.

The registry lock orders registration visibility and wake selection, but it is
not the readiness data lock. Each source must publish readiness before `wake()`
and make the next readiness check observe it through the same source lock or a
matching Release/Acquire pair. This is the Rust equivalent of Linux's
publish-before-wake ordering; a generic fence inside `axpoll-set` cannot replace
the source's synchronization because the readiness state is owned elsewhere.

Closing uses the same lock-remove-unlock-wake loop with all modes selected. A
close callback cannot register through a dropped `PollSet`; the closed state also
rejects any in-flight attempt before it can create a lease.

## Future protocol

After a synchronous syscall attempt reports `WouldBlock`, the central blocking
helper becomes a concrete future rather than a `poll_fn` whose temporary
registration has no owner. Each slow-path poll follows this sequence:

1. remove the previous attempt's leases;
2. attempt the operation;
3. if it would block, register the current waker with the required mode;
4. retry the operation to close the state/register race;
5. return `Pending` only while the registrar owns the live leases.

Returning `Ready` clears the leases before exposing the result. Dropping the
future on cancellation, timeout, or signal interruption performs the same exact
cleanup through `PollRegistrar::drop()`.

Non-central `poll_fn` users must become small named futures or explicitly retain
a registrar in their future state. Fire-and-forget helper methods that only
accept `&Waker` are removed or changed to register into the caller's sink.

## Epoll ownership

An `EpollInterest` owns the shared registrar that watches one target file. Add
creates it; rearm replaces it in task context; modify and delete explicitly drop
the previous registrar outside topology and target locks. Ready-queue callbacks
hold only weak epoll/interest references and publish epoll-owned state. They do
not poll, register, unregister, or acquire target locks.

The epoll object contains the readiness source, not a single waiter lease.
Every concurrent `epoll_wait` future owns its own exclusive registrar. Storing a
single lease in `EpollInner` would make waiters replace one another and violate
the one-owner rule.

## Starry pipe transitions

The pipe keeps separate readiness sources for readers and writers, but the wake
decision is derived from state transitions under the pipe lock:

- commit data after observing empty: publish `IN`, wake all shared reader
  observers and one exclusive reader;
- free capacity after observing full: publish `OUT`, wake all shared writer
  observers and one exclusive writer;
- a successful read from an already non-full pipe does not wake writers;
- a successful write to an already non-empty pipe does not wake blocking
  readers, except that a prior shared poll/epoll registration marks poll usage
  and preserves Linux's broader observer notification rule.

The state is published before wake, and no pipe buffer lock is held while wakers
run. EOF/error transitions use the same mode-aware wake transaction.

Pipe `read` and `write` first execute this state transition synchronously with
an unselected wake token. A completed operation returns directly. Only an empty
read or full write with a live peer enters the exclusive `PollSet` future and
retries after registration. Pipe-local scheduler wait queues are deliberately
absent: adding them would give the same readiness source two independent owners
for waiter selection and cancellation.

## Rejected alternatives

### Keep `register` and add `register_exclusive`

This leaves fire-and-forget lifetime, stale cancellation entries, ambiguous
defaults, and two caller conventions. It is a compatibility layer and is
rejected.

### Keep 64 slots and reserve part for exclusive waiters

Partitioning capacity only changes which correct waiter is discarded. Waking a
displaced waiter remains a fabricated event. It also makes behavior depend on
load rather than readiness and is rejected.

### Store raw pointers or borrowed unregister tokens

The future and `PollSet` lifetimes are not naturally nested across trait objects,
epoll interests, and cancellation. Raw pointers move the lifetime proof into
unsafe teardown paths. Shared state plus a unique ID expresses the proof in safe
ownership and is preferred.

### Wake under the registry lock

It would avoid extracting entries but permits waker reentry into the same poll
source and creates lock-order cycles with files, sockets, epoll, and the runtime.
Callbacks must remain outside locks.

### Add a second waiter-selection algorithm for IRQ context

Two algorithms invite semantic drift and retain a hard capacity at the most
sensitive boundary. Hard IRQ therefore performs no waiter selection: it only
publishes readiness and notifies the typed IRQ-to-task bridge. The deferred
service runs the single generation-bounded `PollSet::wake()` implementation.

## Validation plan

Lowest-layer deterministic tests must first fail with the old semantics, then
pass through the same production API:

1. one wake notifies every matching shared registrar and exactly one of several
   exclusive registrars; a second wake selects the next exclusive registrar;
2. dropping or resetting a registrar removes the exact entry and it is never
   woken later;
3. repeated re-poll and cancellation do not grow the registry;
4. dropping a `PollSet` wakes and drains each entry once, and later registrar
   destruction is safe;
5. a reentrant waker can register again without deadlock and the new entry is
   not consumed by the in-progress transaction;
6. more than 64 live registrars remain registered without displacement wake;
7. QEMU integration tests verify that the runtime IRQ-to-task handoff publishes
   readiness before the deferred service runs the same shared/exclusive
   selection transaction.

Starry QEMU regressions then verify pipe reader and writer exclusivity, shared
poll/epoll fanout alongside one exclusive consumer, transition-only wakeups, and
the `poll_usage` exception. Network, process, signal, and epoll lifecycle tests
cover cancellation and deletion. Final validation uses the requested three-OS
clippy configurations, the four-architecture target matrix, the unchanged LTP
hackbench workload, and the current-head remote CI.

Linux RT and `dev` fixed benchmark results will be recorded once in the scheduler
audit document. Temporary rootfs and runner artifacts used to obtain them are
removed after recording; later work compares against the document instead of
rerunning those fixed baselines.
