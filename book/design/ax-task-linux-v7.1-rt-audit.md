# ax-task Linux v7.1 PREEMPT_RT Audit

## Status and scope

This document is the audit ledger for the task-system migration on
`codex/refactor-ax-task-from-1596`.

The audited base is `origin/dev` at
`37c8f60e81b135c7d997c39630410db418ee192d`. The base already contains the
Starry PID lifecycle fix from PR #1706. Its generation-specific
`ProcessIdentity` and `Live -> Zombie -> Reaping -> Reaped` transition remain
the sole authority for PID visibility and reaping.

The initial branch head is
`962d057785624994d365ea33bb27fdfe0b24ce75`. The range changes 462 files. Every
path in that range is assigned to one of the areas below. Driver work is
limited to drivers and IRQ adapters already changed by the range, plus the
minimum adjacent runtime boundary needed to repair them.

Files that were already large on `dev` are not mechanically split because one
call site changed. New files, substantially expanded files, and objects that
own unrelated concurrency or lifetime invariants must be split by reason to
change.

## Reference configuration

The semantic reference is the local Linux `v7.1` checkout at commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`.

The reference configuration is generated outside that checkout:

```text
CONFIG_NO_HZ=y
CONFIG_HIGH_RES_TIMERS=y
CONFIG_PREEMPT_RT=y
CONFIG_PREEMPT_DYNAMIC=y
CONFIG_EXPERT=y
CONFIG_SMP=y
CONFIG_HOTPLUG_CPU=y
```

The relevant Linux sources are:

- `kernel/sched/core.c`, `fair.c`, `rt.c`, and `deadline.c`;
- `kernel/locking/rtmutex.c`, `rwsem.c`, and `spinlock_rt.c`;
- `kernel/time/hrtimer.c`, `clockevents.c`, and `tick-oneshot.c`;
- `kernel/irq_work.c`, `kernel/softirq.c`, and architecture IPI handlers;
- `kernel/events/core.c`;
- `kernel/exit.c`, `kernel/fork.c`, and `fs/exec.c`;
- `drivers/tty/serial`, `net/core/dev.c`, and the corresponding IRQ adapters.

Linux is prior art for state ownership and ordering. TGOSKits does not copy
Linux object layout, per-CPU implementation, or callback APIs when those would
violate the crate dependency boundaries.

## Audit matrix

| Area | TGOSKits range | Linux reference | Required invariant | Initial finding |
| --- | --- | --- | --- | --- |
| Scheduler placement | `components/ax-task/src/system`, scheduler policy and thread state | `kernel/sched/core.c` | A thread has one checked placement; migration and switch-tail publication cannot expose two owners; every mutable owner access retains one CPU-local rq ownership scope | `queued_cpu`, `running_cpu`, `on_cpu`, and `migration_target` are independent fields with many hand-written updates, and the safe `TaskSystem` surface does not require an outer owner scope |
| Remote delivery | ax-task CPU remote inbox, task-work, ax-runtime IPI glue | scheduler IPI, `irq_work` | Publish payload and epoch before IPI; consume the delivered epoch before admitting a new notification | Busy/claim and boolean doorbells lack complete new-publication race coverage |
| Task deadlines | ax-task timer, park, wait and deferred deadline paths | `hrtimer.c` | Hard IRQ handles bounded value records; cancellation and expiry cannot dereference released owners | The heap stores raw timer-node pointers whose safety depends on caller serialization |
| Physical clockevent | ax-runtime clockevent and platform generic timer | `clockevents.c`, `tick-oneshot.c` | One per-CPU owner; infinity is not a numeric deadline; hardware conversion clamps to min/max delta | `u64::MAX` can truncate during tick conversion, overdue events can program zero, and no offline transition exists |
| Context switch | ax-runtime task/guard and per-architecture context code | scheduler `on_cpu`, `finish_task_switch` | Scheduler baton spans the complete switch; outgoing resources remain live until switch tail clears `on_cpu` | The invariant exists but is spread across oversized runtime modules |
| PI and sleep locks | ax-task PI, ax-sync mutex, scope-local | `rtmutex.c`, `rwsem.c` | Registration, deboost and grant form one bounded transaction; no unbounded spin with preemption disabled | PI unlock waits for registrations by spinning; waiter and lock identities lack generation-based reuse protection |
| IRQ wake lifetime | `IrqWaitCell`, Starry future/wait adapters | `synchronize_irq`, completion/wake queues | An IRQ-visible registration is revoked and quiesced before its owner is freed | Starry `IrqNotify` publishes a raw pointer to retained wake storage |
| Process lifecycle | starry-process and Starry task/process glue | `exit.c`, `forget_original_parent`, `do_wait` | PR #1706 identity owns exit/reap; relationship updates have one lock order and one publication transaction | Reparent and retire can acquire children/group locks in conflicting orders |
| Perf ownership | Starry perf task, hardware and sampling paths | `kernel/events/core.c` | `pid == 0` is a task context; `pid == -1` with a CPU is a CPU context; teardown executes on the owner CPU before storage release | CPU-local sampling slots are registered and removed on the current CPU and contain raw notify pointers |
| Generic timers | Starry POSIX timers and AxVM timer worker | hrtimer soft/threaded callbacks | Arbitrary callbacks run in task context; earlier deadlines use a bounded wake endpoint | Worker design exists and must remain separate from ax-task task deadlines |
| Serial and driver IRQ | rdif-serial, some-serial, ax-runtime serial, changed vsock/USB adapters | serial core, NAPI/event publication | IRQ endpoint only reads/acks/drains a bounded amount and publishes stable events; workers advance flow | Endpoint separation exists but changed paths still require lock, allocation and ownership audit |
| Architecture idle | axcpu idle primitives and trap glue | architecture idle entry, especially LoongArch `genex.S` | IRQ enable plus idle is atomic with pending-work recheck; an interrupt in the enable/idle window returns after the idle instruction and is still dispatched | LoongArch follows the Linux return-address pattern but lacks injected timer/IPI window tests |
| Compatibility | ax-api, ax-posix-api, axstd, Starry syscalls and axvm callers | Linux UAPI and project compatibility contracts | Internal APIs may change; Linux ABI, errno and axstd observable behavior do not | The migration spans many adapters, so compile-only validation is insufficient |

## Structural audit milestone

The behavior-preserving split was completed at branch head
`73957e29b4d15c43555e173a9f383100c7deed6f`. At that point the branch was 54
commits ahead of the audited base and changed 553 paths.

The former 7,230-line `TaskSystem` implementation had already shrunk to 4,192
lines by the audit baseline. It is now a 227-line facade over domain modules
for CPU lifecycle, thread construction, dispatch, owner scheduling, placement
delivery, inboxes, switch transactions, park/switch-tail, lifecycle, deadline,
balance, and deferred task work. The split does not add a second placement or
delivery state source: private helpers continue to mutate the same records
through their existing owner scopes.

The `ax-runtime::task` facade is now 146 lines. CPU bootstrap and online
publication, stack/TLS backing, transactional thread resources, architecture
context switching, runtime thread lifecycle, spawn preparation, executor
integration, scheduler events, and the `TaskRuntime` capability implementation
have separate modules. `TaskSystem`, every per-CPU `CpuLocal`, the physical
context/TLS owner, and the scheduler-switch trace hook still have one storage
owner each.

This stage was source movement and visibility tightening only. Its validation
consisted of all `ax-task` unit, integration, documentation, and 15 loom tests;
the 48 `ax-runtime` IRQ/multitask tests; all 25 `ax-runtime` feature-clippy
checks; formatting; and `git diff --check`. The TLS-enabled host unit binary is
not a supported link target because it deliberately lacks kernel linker
symbols; the TLS feature is covered by the feature-clippy build and will be
covered by the architecture milestone.

Scheduler placement is now structurally closed. `SchedulerPlacement` is the
only placement state and represents detached, queued, running, switching-out,
migrating, and exited-awaiting-tail states. Switch-tail and migration tests
exercise both owner transfer and exit retention. Remote delivery uses an epoch
claim protocol and retains a claim when the transport reports `Busy`. Existing
tests cover epoch replacement and the concrete migration/inbox path; one
end-to-end virtual-runtime test is still required to connect payload
publication, a `Busy` physical delivery, handler acknowledgement, and a newer
publication in one scenario.

Owner runqueue serialization is also closed at this milestone.
`TaskRuntime::validate_owner_cpu_context()` requires every public online
`CpuLocal` mutation to retain either the runtime IRQ pin or the scheduler
baton. The four-CPU Starry affinity stress case and its deterministic owner
scope regression validate that nested lock-preemption restoration cannot enter
the scheduler over a half-committed switch.

Remote affinity completion now follows Linux's shared
`set_affinity_pending` model. Each request advances a generation while holding
the same scheduler-state lock that publishes its mask. A move-only completion
retains an external thread lease, and destination enqueue publishes completion
only after physical placement has committed. Concurrent setters join the
monotonic sequence, so a newer target supersedes an older one without releasing
either caller at an intermediate destination. Exit revokes the outstanding
request and wakes all waiters after releasing scheduler and registry locks.

The deterministic regression
`remote_affinity_completion_waits_for_the_destination_owner` failed on the old
implementation because publication returned `Some(Ok(()))` before the source
owner had detached the task. The companion concurrent-setter and target-exit
tests cover supersession and lifetime teardown. All `ax-task` tests, including
15 loom models, the 48 IRQ/multitask `ax-runtime` tests, all 22
`starry-kernel` feature-clippy checks, and the x86_64 Starry
`affinity-bug-sched-affinity-migrate` QEMU case pass; the QEMU runner reported
`STARRY_GROUPED_TESTS_PASSED`.

## Open audit items after the structural split

These items are not local patch suggestions. Each needs a deterministic red
test and a review of the owning state machine before implementation.

| Area | Open question or defect | Linux v7.1 reference | Required next evidence |
| --- | --- | --- | --- |
| Remote delivery transport | Epoch/claim unit and loom tests do not yet join the real payload queue to a physical transport `Busy` result. | Scheduler IPI and `irq_work` publish work before raising the interrupt and allow a new raise after the old claim is consumed. | One deterministic multi-CPU runtime test must publish payload A, make transport notification report `Busy`, consume A, race publication B, and prove B is drained without an unrelated interrupt. |
| Task deadline domain | `TaskDeadlineQueue::arm` still accepts raw `0` and `u64::MAX`, while `LocalClockEvent` rejects both as physical deadlines. A retained maximum-valued task entry can therefore be logically finite in one owner and unrepresentable in the other. | `hrtimer` stores typed expiry values, while clockevent programming treats “no event” separately and clamps finite deltas to device limits. | Add red tests for zero, maximum, conversion overflow, and cancellation, then introduce one finite monotonic-deadline boundary or an explicit no-deadline value before publication. |
| Clockevent IRQ transaction | A hardware callback always invalidates the armed state and enters `Firing`; the state machine safely re-arms if no task expires, but the audit has not yet injected an early or spurious delivery through the complete ACK/begin/finish path. | `hrtimer_interrupt()` invalidates the delivered event and re-evaluates the next event even when no timer is expired. | Add a fake-device test for an early delivery and require exactly one replacement program with no task expiry. This is currently an evidence gap, not a confirmed semantic failure. |
| Scope-local contention | The low-level lease API is bounded, but Starry's task-context mutation wrapper retries `ScopeCellBusy` with repeated yields. A retained remote reader has no queued waiter, PI donation, interruption, or deadline. | PREEMPT_RT `local_lock` task-context contention blocks through an RT mutex instead of polling with preemption disabled. | Model the outer task-context acquisition as a waitable ownership transition and test remote-reader release, interruption, and cancellation. Keep `scope-local` itself non-sleeping. |
| IRQ waiter owner lifetime | `IrqWaitRegistration` has generation and in-flight notification grace, but Starry `IrqNotify` still relies on a documented unregister/quiesce-before-drop obligation rather than an owner type that makes the teardown sequence unavoidable. | IRQ action removal revokes publication and synchronizes in-flight handlers before releasing storage. | Trace every retained producer through drop, then add a concurrent notify-versus-owner-drop test. If a producer can outlive the cell, replace the contract-only API with an owning registration/teardown guard. |

## Confirmed red evidence

GitHub Actions run `30330320261`, job `90184755449`, and a local full
aarch64 `qemu/system` run both fail immediately after
`STARRY_PERF_FREQ_OK`. The page fault instruction is at the
`ThreadWakeHandle::wake` entry and the receiver contains released-memory data.

The failure chain is:

1. a system or self sampling event registers a slot in the current CPU's
   registry;
2. the task migrates before disable or drop;
3. teardown removes the slot from the new CPU rather than the owner CPU;
4. the old CPU retains an active slot containing a stale IRQ notification
   pointer;
5. a later PMU interrupt dereferences the released wake registration.

Individual perf cases can pass because they do not force this cross-test,
cross-CPU lifetime. The fix must model task and CPU perf targets explicitly;
the SMP perf tests must not be skipped or affinity-pinned to hide the defect.

The x86_64 ArceOS `task-irq` case exposed a separate missed-clockevent
failure. Stage markers proved that yielding completed and the first task sleep
never returned. A halted-guest GDB inspection found all of the following on
CPU 0 at the same time:

- the task deadline heap retained one overdue park timeout for the running
  thread;
- the preallocated expired-deadline buffer was empty;
- `need_resched` remained set, so the CPU repeatedly entered the scheduler and
  never reached its final idle wait;
- the LAPIC one-shot had counted down, but no timer interrupt remained pending
  or in service.

The old safe-point path serviced deadlines only after the timer IRQ had
published `deadline_work_pending`. Consequently the physical clockevent was a
single point of correctness rather than an acceleration mechanism. A lost or
late edge could strand a sleeper indefinitely, and idle-side recovery could
not help while another scheduler condition prevented idle entry.

Linux `hrtimer_interrupt()` invalidates the delivered event before processing
and re-evaluates the queue when it programs the next event; clockevent
programming also retries elapsed minimum-delta events instead of treating an
old arm as authoritative. TGOSKits now applies the corresponding invariant at
both boundaries: every changed selected deadline reprograms or stops the
physical owner, and every scheduler safe point promotes one bounded batch of
already-due task deadlines before claiming sticky task work. The deterministic
test
`scheduler_safe_point_recovers_overdue_deadline_without_clock_irq` fails
without the safe-point promotion. The isolated x86_64 QEMU case then completes
its yield, sleep, and wait-queue stages.

The x86_64 Starry `bug-sched-affinity-pid` stress case exposed a separate
owner-runqueue re-entry. Under repeated parent and child affinity changes, GDB
stopped in `execute_switch_plan()` on fatal invariant 1: a nested scheduler
decision required a context switch while `previous_endpoint()` was `None`.
The outer owner transition had already cleared `CpuLocal.current` and
`current_core`, but had not installed the selected successor. An internal
thread-scheduler lock then became the outermost preemption guard; releasing it
restored scheduling eligibility and entered the scheduler over that transient
owner state.

Linux does not make a mutable rq operation safe merely because one nested
object lock happens to disable preemption. `raw_spin_rq_lock_nested()` creates
an explicit rq ownership scope, `lockdep_assert_rq_held()` checks it at helper
boundaries, and `prepare_lock_switch()` carries that ownership across the raw
context switch until `finish_task_switch()` releases it. Ax-runtime already
had the corresponding scheduler baton, but ax-task's direct `TaskSystem`
methods could be invoked after runtime publication without proving that the
caller retained either the baton or an outer IRQ pin.

`TaskRuntime::validate_owner_cpu_context()` is now the capability check for
that boundary. Every public operation borrowing an online `CpuLocal` validates
the exact published `TaskSystem` before touching owner state. Ax-runtime
accepts only a live runtime IRQ scope or an active/transferred scheduler
baton; a lock-local preemption depth alone is rejected. Standalone scheduler
models remain usable before runtime publication, while runtime tests install
an explicit owner scope instead of receiving a test-only exemption.

The deterministic regression
`online_owner_operations_require_an_outer_cpu_pin` configures IRQ-exit
scheduler re-entry and directly calls the formerly safe affinity owner method.
It returned `Ok(false)` before the boundary and now returns
`TaskError::UnsafeContext`; the same call succeeds while the test retains an
IRQ guard. The end-to-end case was raised from 20 to 200 iterations and
completed on four x86_64 CPUs with `STARRY_GROUPED_TESTS_PASSED`, whereas the
old implementation reproduced the transient-state panic within 11 iterations.

Thread construction also exposed a distinct lifetime violation. A rejected
`ThreadSpec` dropped its OS extension before destroying the runtime context,
TLS, and stack. If context destruction returned `Busy`, the remaining handles
were then abandoned because no `TaskSystem` owner retained the failed
transaction. The deterministic tests
`rejected_thread_releases_runtime_resources_before_extension` and
`rejected_thread_retains_extension_until_resource_release_retry` cover the two
failure modes.

The scheduler now owns every consumed specification through a move-only
unpublished-thread guard. A failed validation, admission, slot allocation, or
context binding first attempts runtime-resource teardown. Retryable teardown
keeps the resources and extension together in deferred task work; only a
successful teardown releases the extension. Registry records use the same
resource-before-extension order as a shutdown fallback.

PI lock identity no longer comes from the `RawMutex` address. Each physical
lock owns a lazy `PiLockIdentity` whose process-wide generation is never
reused, so reconstructing a lock in the same storage cannot make a stale
scheduler edge match the new instance. This closes the address-reuse ABA half
of the PI finding. Registration, handoff, cancellation, and quiescence are
tracked separately because identity alone cannot make their publication safe.

PI handoff now follows the transaction boundary used by Linux
`mark_wakeup_next_waiter()` and `rt_mutex_slowunlock()`. Ax-sync holds its local
metadata gate while ax-task validates and retains the scheduler-registry
transaction. All fallible graph reads and generation checks happen before the
local owner or waiter grant changes. Ax-sync then publishes local ownership,
commits the already validated scheduler transition, releases both raw gates,
and only then performs the targeted wake while retaining preemption exclusion.
A dropped preparation changes neither state source.

Registration uses the same `mutex metadata -> task-system registry` lock order
as handoff. The old pending-registration counter, unlock-side spin, local-grant
spin, and test callback between local and scheduler publication are gone. Loom
models registration versus unlock, rejection before publication, and
deboost/local grant/scheduler grant visibility before wake using separate
state sources. Owner-indexed donor traversal, scope-local serialization, and
IRQ-visible waiter lifetime are tracked as separate latency and lifetime
subproblems.

The donor graph is now owner-indexed as an intrusive generation-bearing waiter
list. Registration, cancellation, recomputation, and handoff walk only the
affected owner's waiters and the transitive owner chain; unrelated live thread
slots no longer lengthen the IRQ-disabled metadata transaction. The
deterministic regression populated 128 unrelated records and observed 260
whole-registry donor-record visits before the change, then only the one
registered donor afterward. Each chain preflight also checks link direction
and the cached waiter count before any mutation.

Scope-local mutation now follows the PREEMPT_RT `local_lock` boundary: the
low-level component only performs bounded lease transitions while migration is
disabled, while Starry serializes task-context resource-scope reads and writes
with a PI mutex before entering the pinned section. Upgrading the current
activation is one `shared -> exclusive` compare-exchange, so writer intent is
visible before the active lease is withdrawn. A retained remote read or a
second CPU activation returns `ScopeCellBusy`; it cannot make the caller spin
or panic with IRQs disabled. The deterministic regression exercises both the
old admission window and a read-then-mutate self-deadlock.

IRQ-visible waiters now distinguish `Attached`, `Notifying`, and `Detached`
for each registration generation. Removing the cell pointer is only the
park-abort condition; it is not permission to release the wake payload.
`IrqWaitToken` borrows the owning cell, identifies one generation, and must be
detached and quiesced in task context. Reuse cannot confuse an older token
with a later generation. The deterministic regression blocks inside the wake
operation and proves that detachment does not authorize reclamation until the
notifier returns. Loom covers notify versus cancellation, payload grace, and
stale-generation rearm.

The registration now owns a typed `ThreadWakeHandle`. Production code can no
longer construct an arbitrary raw hard-IRQ callback or separately release its
wake target. Serial, block, network, AxVM timer, task-work, evdev, KPU, and TPU
workers all branch `ConsumedPending` directly to service work; token-bearing
results park on generation ownership and then use the common detach/quiesce
boundary. Starry `IrqNotify` uses the same cell instead of publishing an
`AtomicPtr<ThreadWakeHandle>`.

Starry perf now follows Linux's task-context versus CPU-context split. The
`PerfTarget` parser preserves the full syscall-width flag word, maps `pid == 0`
to the current task, and accepts a CPU target only for `pid == -1 && cpu >= 0`.
Task events carry generation-bearing scheduler ownership; CPU events execute
configure, enable, disable, read, reset, and unregister through one fixed
worker on the selected CPU. The direct local fast path pins the CPU and masks
local IRQs before validating ownership, so migration cannot occur between the
check and a PMU sysreg write.

The sampling registry is per CPU and generation checked. A slot owns its ring
and `IrqNotify` references by value; teardown masks overflow, stops the
counter, clears pending state, removes the exact generation under local IRQ
exclusion, and only then releases output storage. Task-bound events use a
`Detached -> Arming -> Registered -> Running -> StopRequested -> Stopping`
lifecycle. A close request upgrades an in-flight disable, switch-out and the
fixed worker claim one exact generation, and an architecture stop failure
returns the same generation to `StopRequested` for a bounded later retry.

Output ownership mirrors the ordering in Linux
`perf_event_set_output()` and `perf_output_begin()`, without importing Linux's
callback-shaped ring API. An event retains its own mmap output weakly while the
VMA retains the complete ring object. Redirects retain a typed output strongly,
and `SET_OUTPUT(-1)` removes that redirect. Self, cross-task, and cross-CPU
relationships are rejected; a source with its own active mmap cannot be
redirected. All overflow and side-band producers share one non-blocking CAS
gate in the output object. A contender drops and accounts the record rather
than spinning in hard IRQ, so inherited or redirected writers cannot race
`data_head` from different CPUs.

The original aarch64 `perf-hw-freq` stale-wake panic remains the end-to-end
regression. Deterministic host tests cover target parsing, registry generation
reuse, close versus switch-out, failed owner-stop retry, redirect/detach
selection, shared output lifetime, and bounded multi-producer admission.

Starry process exit now closes child publication in the same relationship
transaction that reparents existing children. This mirrors Linux
`copy_process()` and `exit_notify()`, which publish a child and splice an
exiting parent's child list under `tasklist_lock`. Previously Starry took a
child snapshot, reparented it, and only later published
`ProcessIdentity::Zombie`; a prepared fork could publish in that interval and
remain permanently attached to the zombie. The deterministic regression
prepares a fork, starts parent exit relationships, and then proves that late
publication is rejected. The transaction returns the exact moved child
snapshot, so parent-death notification cannot miss a child admitted between a
separate snapshot and reparent step.

All parent, child, process-group, and session-group writes now pass through
`ProcessRelationTxn`. Production Starry builds use `ax_sync::PiMutex` for these
task-context locks. This follows Linux v7.1 RT, where `rwlock_t` is backed by
`rwbase_rt` and `write_lock_irq(&tasklist_lock)` acquires the RT lock without
actually disabling local interrupts. The isolated `starry-process` host tests
retain a non-sleeping backend because they do not install an ax-task runtime.

Writers reserve replacement storage before acquiring relation locks, acquire
parent child sets by stable PID and process-group member sets by stable PGID,
and release removed `Arc`/`Weak` storage after the transaction guards. Group
migration does not remove the source membership until both group locks are
owned. The deterministic regression holds the destination lock and proves the
process remains visible in its source group; the old remove-then-lock sequence
made it absent from both groups.

Reparenting also validates the destination's child-publication state while
holding both child sets. A selected subreaper that completes its own
relationship exit cannot accept a later orphan batch. Starry re-evaluates the
live ancestor chain and retries the transaction; a second deterministic
regression closes the candidate first and proves the child falls back to an
open reaper. This closes the selection-to-publication window that Linux avoids
by doing reaper selection and list splicing under the same `tasklist_lock`.

The branch-touched USB hosts now make the PREEMPT_RT execution boundary
explicit. Linux v7.1 `xhci_irq()`, `ehci_irq()`, and
`dwc2_handle_common_intr()` combine status acknowledgement with event
processing because the RT IRQ core force-threads eligible handlers. TGOSKits
does not yet have that generic forced-threading layer, so CrabUSB exposes
bounded `acknowledge_irq()` separately from task-context `drain_event()` and
`rearm_irq()`. Starry's shared hard-IRQ callback only claims the device status,
masks the source, and publishes an `IrqWaitCell` notification. A fixed-batch
worker owns command, transfer, port, and topology processing.

Controller enable/disable and deferred rearm share one task-context
`ControllerIrqState`; hard acknowledgement observes only its atomic enabled
publication and never acquires the control gate. This prevents a stale worker
from reopening a disabled controller while preserving bounded hard-IRQ work.
The xHCI handler additionally owns a non-blocking register gate: hard IRQ uses
`try_lock`, and every safe access to its register and event-ring `UnsafeCell`s
requires the resulting guard.
The USBFS lifecycle gate rejects new work before IRQ callback removal and waits
for an in-flight worker before relinquishing callback ownership; the stable
registry slot itself remains allocated. Shared IRQ callbacks return `Unhandled`
when the controller did not claim the status.
The deterministic regressions
`stale_task_rearm_cannot_reenable_a_disabled_controller` and
`hard_irq_ack_preserves_controller_disable` reproduce the two controller
shutdown races, while USBFS gate tests cover teardown quiescence and fixed
batch exhaustion.

Starry user waits now have one typed terminal boundary:
`Ready / Interrupted / TimedOut`. The old executor park callback treated a
sticky interruption only as a reason to yield. A pending future that did not
independently poll `task.interrupted` could therefore yield forever without
producing `EINTR`. The deterministic state test presents a pending operation
with an already-published interruption; the old transition remained
`Pending`, while the new transition completes as `Interrupted`.

The operation future is polled before signal and deadline so an already-ready
result wins, matching Linux wait-condition ordering. A signal is consumed
before the timer, and every timed syscall retains a distinct timeout mapping
(`0`, `EAGAIN`, or `ETIMEDOUT`) at its ABI boundary. Task-neutral I/O readiness
polling no longer consumes Starry signal state. The executor's park callback
only performs the predicate handshake; it never loops through
`yield_current_cpu` on a sticky interruption.

## Completion rules

Each confirmed defect receives a deterministic failing test at the lowest
useful layer before its fix. Pure source moves are exempt but must preserve the
same test results.

A phase is complete only after its affected packages compile, their tests and
feature clippy checks pass, formatting is clean, and `git diff --check` passes.
Full QEMU runs occur only at subsystem milestones. A QEMU failure is first
reduced to a grouped subcase or deterministic virtual-runtime test before
another full run.

Every confirmed finding inside the range is fixed. A pre-existing issue
outside the agreed range is recorded separately with evidence rather than
silently expanding the refactor.
