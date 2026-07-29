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
| CPU lifecycle | ax-task root domain, CPU remote endpoint, ax-runtime clockevent | `sched_cpu_deactivate()`, `sched_cpu_wait_empty()`, `sched_cpu_dying()` | Close producer admission before proving the runqueue, deadlines, inboxes and switch tail quiescent; never publish an offline CPU while work can still target it | Only online publication exists, and a failed migration on a disappearing CPU is a fatal invariant |
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

The first behavior-preserving split was completed at branch head
`73957e29b4d15c43555e173a9f383100c7deed6f`. At that point the branch was 54
commits ahead of the audited base and changed 553 paths. The split continued
through `03e030ccc` and `d4ec647b9`: at the pre-documentation core milestone
`a8a4ebd3b`, the branch was 78 commits ahead of the audited base and changed
587 paths.

The former 7,230-line `TaskSystem` implementation had already shrunk to 4,192
lines by the audit baseline. Its orchestration root is now a 225-line module
over domain modules for CPU lifecycle, thread construction, dispatch, owner
scheduling, placement delivery, inboxes, switch transactions,
park/switch-tail, lifecycle, deadline, balance, and deferred task work. The
separate public facade still contains its API forwarding surface and unit
tests; it is not counted as part of that 225-line orchestration root. The split
does not add a second placement or delivery state source: private helpers
continue to mutate the same records through their existing owner scopes.

The `ax-runtime::task` facade is now 148 lines and the crate root is 189 lines.
CPU bootstrap and online publication, boot-memory layout, interrupt
registration, shared IPI delivery, the per-CPU physical clockevent, stack/TLS
backing, transactional thread resources, architecture context switching,
runtime thread lifecycle, spawn preparation, executor integration, scheduler
events, and the `TaskRuntime` capability implementation have separate modules.
`TaskSystem`, every per-CPU `CpuLocal`, the physical clockevent, the
context/TLS owner, and the scheduler-switch trace hook still have one storage
owner each. In particular, moving the 408-line clockevent runtime and the
127-line IPI transport did not introduce a second cache or doorbell state
source.

This stage was source movement and visibility tightening only. Its validation
consisted of all `ax-task` unit, integration, documentation, and 15 loom tests;
the 48 `ax-runtime` IRQ/multitask tests; all 25 `ax-runtime` feature-clippy
checks; formatting; and `git diff --check`. The TLS-enabled host unit binary is
not a supported link target because it deliberately lacks kernel linker
symbols; the TLS feature is covered by the feature-clippy build and the
architecture milestone below.

Scheduler placement is now structurally closed. `SchedulerPlacement` is the
only placement state and represents detached, queued, running, switching-out,
migrating, and exited-awaiting-tail states. Switch-tail and migration tests
exercise both owner transfer and exit retention. Remote delivery uses an epoch
claim protocol and retains a claim when the transport reports `Busy`, matching
Linux `irq_work_claim()` and the handler-side pending-bit clear before callback
execution. The deterministic virtual-runtime test
`coalesced_busy_ipi_drains_real_payloads_and_accepts_new_epoch` now connects
real retained-`Arc` inbox payloads, a coalesced `Busy` transport result,
handler acknowledgement, bounded drain remainder, and a publication that must
claim a newer physical epoch. The final safe point observes no stranded
payload or sticky scheduler work.

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

The task-deadline heap now admits only a validated finite logical deadline.
Unlike the physical clockevent type, zero remains valid and expires
immediately, matching Linux hrtimer semantics. `u64::MAX` is the explicit
no-deadline value: it is rejected before a heap slot or timer generation is
consumed, while a saturated park timeout remains a notification-only park with
no queue registration. The red regression
`no_deadline_sentinel_cannot_consume_queue_capacity` previously left a
maximum-valued entry resident after the runtime had published no physical
clockevent. It now passes together with explicit immediate-zero and
notification-only park coverage.

The clockevent IRQ transaction is also closed. Platform `begin_irq()` claims
the controller event before dispatch (and LoongArch acknowledges the timer
source there); the retained `ActiveIrq` completes EOI only after the runtime
handler returns. Within that interval `LocalClockEvent` invalidates the old
arm, accounts one bounded task-deadline batch, merges updates received during
`Firing`, and commits one replacement before EOI. Deterministic tests prove
that an early IRQ reprograms the unchanged deadline exactly once and that a
spurious IRQ in `Idle` is a bounded no-op. The physical conversion uses
ceiling arithmetic, clamps elapsed or sub-tick deadlines to one tick, clamps
unrepresentable deltas to the device argument width, and programs the interval
before unmasking the IRQ.

The core CPU lifecycle now has one packed state owner. Its high bits represent
`Online`, `Draining`, and `Offline`; its low bits count producers admitted
across payload publication and the scheduler doorbell. The
`Online-with-zero-producers -> Draining` compare-exchange closes new remote
publication before the owner checks its current/idle slot, runqueue, task
deadline heap, expired buffer, switch handoff, inboxes, IPI claim, and every
live thread's affinity and placement ownership. A failed check returns to
`Online`; a successful check removes the CPU from the root domain before
publishing `Offline`. Re-online publication reverses that ordering under the
topology sequence.

This follows the ordering rather than the object layout of Linux v7.1
`sched_cpu_deactivate()`: clear active placement admission, enable balance
push, wait for prior producers, and only then mark the runqueue offline.
Linux's later `sched_cpu_wait_empty()` and `sched_cpu_dying()` verify that the
outgoing runqueue has been vacated. TGOSKits currently exposes the stricter
quiescent transition: callers must migrate or retire work before
`take_cpu_offline()` succeeds. It does not yet claim Linux-style automatic
runqueue evacuation or a complete physical CPU hot-unplug control path.

The runtime clockevent now participates in that same lifecycle transaction.
After remote admission is changed to `Draining` and quiescence is proven,
`TaskRuntime::prepare_cpu_offline()` stops the owner CPU's physical oneshot
before the root-domain bit and final `Offline` publication are cleared. A
runtime failure cancels draining and leaves the scheduler online for retry.
The reverse hook brings `LocalClockEvent` from `Offline` to `Idle` or `Armed`
while local IRQs remain disabled and before the root domain or remote endpoint
is published online. This follows Linux v7.1's `tick_cpu_dying()`,
`tick_offline_cpu()`, and `tick_shutdown()` ordering: detach the per-CPU
clockevent before the scheduler completes the dying transition.

The deterministic regression
`quiescent_cpu_can_cycle_offline_and_online` observed no runtime lifecycle
event on the old implementation even though the scheduler completed
`Online -> Offline -> Online`. It now proves the matching physical sequence.
`runtime_failure_leaves_cpu_lifecycle_transition_retryable` covers both
failure directions. Other unit tests retain last-CPU rejection, pending remote
work, and a live thread without a remaining affinity destination; Loom
exhaustively covers publication racing the draining transition. ArceOS still
has no service that coordinates IRQ-framework removal and physical CPU power
off, so this change intentionally does not invent a platform hot-unplug
interface.

## Architecture idle and core validation closure

Every supported architecture now has a live pending-work test for its
IRQ-masked idle primitive instead of relying on source-text assertions. The
target CPU disables local IRQs, the sender publishes a scheduler IPI, and the
target enters `wait_for_irqs_disabled()`. x86_64 uses the atomic `sti; hlt`
region; RISC-V and AArch64 enter WFI before restoring interrupt admission; and
LoongArch uses the Linux-style trap return-address fast-forward when an
interrupt lands in its idle region.

The LoongArch regression masks the local timer line during the experiment, so
an incidental periodic interrupt cannot satisfy the wake condition. Removing
the trap-side `fast_forward_idle_interrupt()` call makes the isolated
`task-ipi` case remain hung after `ARCEOS_TEST_BEGIN` for more than 90 seconds.
Restoring it completes the same case in 22 ms. The final targeted case passes
on all four architectures: x86_64 in 21 ms, RISC-V in 39 ms, AArch64 in 31 ms,
and LoongArch in 22 ms.

The core milestone also runs the complete ArceOS `rust/all` QEMU group
serially on x86_64, RISC-V, AArch64, and LoongArch. Each runner reports all
17 cases passed and the formal `ArceOS test suite run OK!` marker. The exact
repository standard-test command, `cargo xtask test`, passes all 49 packages.

That standard-test run first provided a deterministic host-link red regression
for axvm: after the runtime migration, its test binary lacked the host
definitions for stack/page sizing and the per-CPU template bounds. Production
features were unaffected, but relying on callers to select an aggregate
`host-test` feature made the package's own test target invalid. axvm now
enables the existing ax-hal, ax-kernel-guard, and ax-kspin host capabilities
through dev-dependency feature unification. The original
`cargo test -p axvm --no-run` link failure is green; 116 unit, 18 integration,
and 4 error tests pass, as do all six axvm feature-clippy configurations.

## PI and waiter audit closure

The two follow-up questions found after the structural split have now been
resolved against their complete ownership graphs. The scope-local conclusion
and regression are recorded below. IRQ waiter storage still has one retained,
generation-bearing owner and a quiescence boundary; no borrowed producer or
reclaim-before-grace path remains.

A later three-party regression did find a separate lost-event race in the
cell's old two-atomic publication protocol. After the registration published
its waiter, the first IRQ could remove it and begin the wake. A second IRQ then
set `pending`, but the still-running registration path could clear that bit
before discovering that the first notifier owned its token. The second event
was consequently absent from the next wait. `IrqWaitCell` now encodes
`Empty / Pending / Waiter` in one atomic pointer state using a non-dereferenced
aligned sentinel. Register, unregister, and notify each linearize one state
transition; a notifier uses a compare-exchange even when coalescing an existing
pending state, so a concurrent consumer cannot erase the new notification.
The deterministic blocked-wake regression and the corresponding Loom model
cover the exact second-IRQ interleaving. This follows Linux v7.1
`include/linux/wait.h`'s requirement that condition publication and waiter
observation share an ordering boundary instead of relying on unrelated
lockless loads.

Architecture, Starry lifecycle, and driver findings remain tracked by their
dedicated matrix rows and later milestones.

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

The x86_64 Starry `test-futex-wake-op-smp` stress case later exposed a
process CPU-time accounting livelock rather than a futex defect. Two halted
GDB snapshots found all four CPUs at the same reader loop in
`poll_interval_timers()`: `ProcessCpuTimeAccounting::writers` remained one,
while no CPU was executing the matching writer. `record_transition()` had
published that writer count before its callback entered a non-preemptible
thread-accounting transition. The task could therefore be switched out in
that window, after which every syscall-return path in the thread group spun
waiting for it and prevented useful scheduler progress.

Linux v7.1 does not put a retrying reader behind such a writer. Active
thread-group CPU timers are updated through `account_group_user_time()` and
`account_group_system_time()` into `cputime_atomic`; expiry samples them with
`proc_sample_cputime_atomic()` and runs the heavyweight work through
`CONFIG_POSIX_CPU_TIMERS_TASK_WORK`. `thread_group_cputime()` updates the
current task but deliberately relies on ticks and later scheduler actions for
pending runtime on remote siblings.

Starry now follows that ownership model without giving up task-context
visibility of a running sibling. Scheduler and user/kernel boundary
transitions publish process user/system deltas directly into atomic group
counters. A reader samples the committed counters before aggregating live
per-thread residuals. A concurrent transition can therefore only make that
sample temporarily low: it cannot count the same interval in both sources.
Separate monotonic high-water counters prevent the handoff window from making
an already observed process clock move backwards, while the next poll observes
the newly committed delta. No reader waits for a preempted writer.
Timer-state transitions commit the old state's delta before polling interval
timers. Behavioral regressions cover running siblings, transition handoff
without double counting, and a transition callback held in a simulated
preempted state while a concurrent snapshot must return.

With the old implementation, an isolated four-CPU
`test-futex-wake-op-smp` run failed to produce a completion marker before the
240-second command deadline, matching the full-suite GDB livelock. With the
live-residual/high-water accounting path, the same 80,000 atomic
`FUTEX_WAKE_OP` operations complete in 162 seconds and the runner reports both
`STARRY_SYSTEM_TEST_PASSED` and `STARRY_GROUPED_TESTS_PASSED`. All 22
`starry-kernel` feature-clippy configurations pass. The 136-check
`test-timer-family` case also passes, including process and thread CPU clocks
plus `ITIMER_VIRTUAL` and `ITIMER_PROF`.

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
visible before the active lease is withdrawn.

The follow-up ownership audit corrected an earlier assumption in this ledger.
Linux v7.1 RT `__local_lock()` first calls `migrate_disable()` and
`migrate_disable_switch()` pins a preempted task to its current runqueue; it
does not permit one task-local owner to remain activated on two CPUs and then
wait for itself. Starry's ordinary remote scope reads all hold the same outer
PI mutex, so after the current task acquires that mutex a retained ordinary
reader can no longer explain `ScopeCellBusy`. The only remaining causes are a
duplicate scheduler activation or a caller bypassing `ThreadScope`.

`ScopeCell` therefore admits exactly one scheduler activation and rejects a
second CPU before publishing its per-CPU pointer. Starry no longer treats
`ScopeCellBusy` as a condition to poll with repeated yields: the outer PI mutex
handles legal task-context contention, and a failed bounded upgrade is an
explicit scheduler-ownership invariant violation. The deterministic red test
observed the old implementation publish the same task-owned scope on CPU 1
while CPU 0 still owned it; the new implementation reports
`ScopeCellBusy` without publishing on CPU 1, and the sole activation upgrades
and unwinds without losing its lease.

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

The follow-up Starry ownership trace found seven `IrqNotify` constructions.
Tracepoint and USB notification owners are immortal global state. BPF workers
retain an `Arc<IrqNotify>` until their stop publication is consumed. PMU
sampling slots store their `Arc<IrqNotify>` by value in the owner CPU registry;
teardown masks overflow, stops the counter, clears pending state, removes the
exact registry generation under local IRQ exclusion, and only then releases
the sampling anchors. No producer retains a borrowed `&IrqNotify` across its
owner lifetime.

This matches Linux v7.1 `__free_irq()`: remove publication and shut down the
line first, call `__synchronize_irq()`, stop the threaded handler, and only
then free the action. Linux's hard-IRQ grace path
`__synchronize_hardirq()` itself waits for the in-progress bit to clear;
TGOSKits' task-context `quiesce_irq_wait()` yields while the bounded direct
wake finishes, rather than spinning with preemption disabled. The existing
blocking-wake regression proves that detachment alone cannot authorize
payload reclamation, and loom covers unregister-versus-notify plus stale
generation reuse. With the complete producer trace, the prior contract-only
lifetime concern is closed without adding a second registration owner.

Starry perf now follows Linux's task-context versus CPU-context split. The
`PerfTarget` parser preserves the full syscall-width flag word, maps `pid == 0`
to the current task, and accepts a CPU target only for `pid == -1 && cpu >= 0`.
Task events carry generation-bearing scheduler ownership; CPU events execute
configure, enable, disable, read, reset, and unregister through one fixed
worker on the selected CPU. The direct local fast path pins the CPU and masks
local IRQs before validating ownership, so migration cannot occur between the
check and a PMU sysreg write.

Task-target parsing and authorization are separate typed phases. A positive
TID is resolved once to a strong `UserTaskRef` before its optional CPU filter
is validated, preserving Linux's `ESRCH` precedence over an invalid CPU. That
same lease is retained through hardware-event construction instead of looking
the TID up again. Probe attributes and a value-only ARM PMU construction plan
are validated before authorization or allocation. Malformed attributes
therefore keep their Linux error precedence without temporarily publishing an
unauthorized event.

For a task target, Starry holds the process `exec_lock` from the credential
check through event construction, fd installation, and sampling-registry
publication. This is the local equivalent of Linux retaining
`signal->exec_update_lock` across `perf_check_permission()` and
`perf_install_in_context()`: exec cannot replace credentials or retire the
task context between authorization and attachment.

Following Linux v7.1 `perf_check_permission()` and
`PTRACE_MODE_READ_REALCREDS`, callers in the same thread group and callers
with `CAP_PERFMON` (including Linux's `CAP_SYS_ADMIN` compatibility fallback)
are accepted. Other callers must have `CAP_SYS_PTRACE`, or their real UID/GID
must match every real/effective/saved target ID while the target remains
dumpable. The pure policy also requires `CAP_KILL` for the `CAP_PERFMON`
bypass of a `sigtrap` event, matching Linux's upgrade to attach access.
Synchronous perf SIGTRAP delivery is not implemented yet, so the syscall
currently rejects that capability explicitly rather than silently accepting
an event with missing signal behavior. A denial is `EACCES`; a PID below the
`-1` CPU-target sentinel is `ESRCH`.

The deterministic x86_64 `perf-task-permission` QEMU regression drops the
target and caller to distinct credentials while leaving the target dumpable.
Before authorization was introduced, the cross-credential
`perf_event_open()` succeeded; it now returns `EACCES`. A follow-up red run
showed malformed attributes returning that `EACCES` before their own `EINVAL`.
The same QEMU case now also requires malformed-attribute `EINVAL` and missing
TID `ESRCH` even when the CPU filter is invalid, and the runner reports
`STARRY_GROUPED_TESTS_PASSED`. Pure policy tests cover the complete ID-slot,
thread-group, dumpability, `CAP_PERFMON`, `CAP_KILL`, and `CAP_SYS_PTRACE`
matrix. The aarch64 `perf-hw-fork-exit` case also passes, proving that the
authorized strong target survives the value-only PMU validation plan,
hardware construction, and fork/exit teardown.

ARM PMU counter selection and task-context installation now follow the
corresponding Linux v7.1 ownership rules instead of treating every task event
as a programmable counter. Linux `armv8pmu_get_event_idx()` gives
`ARMV8_PMUV3_PERFCTR_CPU_CYCLES` first claim on `PMCCNTR`, while
`armv8pmu_user_event_idx()` publishes the hardware counter as the perf mmap
index plus one. Starry therefore represents the physical reservation as
`Counter::{Cycle, Programmable(_)}` all the way through allocation,
owner-CPU register access, scheduler binding, sampling validation, and mmap
metadata. Sampling remains restricted to programmable counters, while a
non-sampling architectural or raw CPU-cycle event prefers the dedicated
64-bit cycle counter and falls back to a programmable counter only when it is
already reserved.

Allocating a physical counter is not sufficient to install a task event.
Linux `perf_install_in_context()` uses `task_function_call()` to cross the
target task's scheduling boundary and install or reschedule its context while
it is already running. Starry's fixed per-CPU perf worker now provides the
same boundary: attach and family enable publish intent first, then request a
generation-bearing scheduler-context synchronization on the CPU currently
selected for the target. If the task was running there, executing the fixed
worker proves it switched out after the publication; if it moved first, that
move itself crossed the switch hook and its next sched-in observes the
published list. A stale generation is an already-dead context rather than a
request to touch hardware. This also removes the former local fast path in
which a caller could configure another task's PMU state without crossing its
scheduling boundary.

The deterministic aarch64 `perf-hw-rdpmc` regression exposed both defects in
sequence. The old implementation reported `index=1`, `caps=0x4`, and
`pmc_width=32`, proving that a CPU-cycle event was placed on a programmable
counter. Counter typing changed that to `index=32` and `pmc_width=64`, but the
first red run then observed a nonzero direct read with `read(fd)==0`, proving
that an already-running target had never installed the event. After
scheduler-boundary synchronization, the same case observed nonzero,
comparable direct and fd reads (for example `61853925` and `77531120`) and
reported `STARRY_PERF_RDPMC_OK`. Task sampling, frequency sampling, and
fork/exit PMU regressions pass with the same physical ownership model.

Linux `perf_event_update_userpage()` brackets each metadata update with the
`perf_event_mmap_page.lock` sequence, publishes `index == 0` while inactive,
and sets active `offset` to the accumulated count minus the current hardware
slice base. Starry now has the same single-writer protocol in a focused
`rdpmc` module. Sched-out first disables and reads the terminal physical slice,
folds it into the accumulator, then publishes inactive metadata. Sched-in
programs a fresh zero-based slice and publishes its physical index with the
completed total as offset. RESET stops an exact generation, clears the total,
and only then republishes enable intent, so migration cannot leave a
pre-reset slice running on a different CPU.

The mmap page is strongly owned by its VMA and weakly published by the event.
Only one live, exactly-one-page mapping is accepted; `munmap` drops the anchor
and permits a later mapping. The old implementation allocated one page but
accepted an 8192-byte mapping, exposing adjacent physical memory, and accepted
multiple unrelated live metadata pages. A deterministic red run failed with
“oversized metadata mmap unexpectedly succeeded.” The repaired case requires
`EINVAL` for the oversized request, `EBUSY` for a second live mapping, and a
successful remap after `munmap`. It then forces sched-out/in with `usleep` and
observes a nonzero offset, for example `offset=127041424`, before comparing
`offset + rdpmc` with `read(fd)`.

Preserving those errno values also closed an adjacent mmap probe ambiguity.
`DeviceMmap::None` is now the explicit ordinary-file fallback; a device
implementation's `Err` is committed instead of being discarded and replaced
by an unrelated `file_mmap` error. Regular files still take the same fallback,
while perf metadata validation reaches userspace as Linux `EINVAL`/`EBUSY`.

Linux `find_get_context()` rejects `PF_EXITING` while holding
`task->perf_event_mutex`; a dead perf context is a tombstone that cannot accept
new events. Starry currently treats a stale generation as a completed
synchronization, and rollback quiesces any hardware generation before
withdrawing scheduler publication. The remaining task-exit/open admission
race is tracked in this audit as the next lifecycle stage: admission and
exit-start must share one per-thread perf-context gate so an event cannot
attach after exit cleanup has passed.

The inheritance regression found an analogous unpublished-child boundary.
Previously `on_clone_inherit()` ran before `PreparedUserTask` existed, so 40
sequential children logged “scheduler identity unavailable” and produced only
one distinct descendant EXIT TID. Inheritance now runs immediately after
`prepare_user_thread*()` creates the scheduler identity but before publication
of the Linux task. Prepared-task rollback calls the scheduler-side, idempotent
PMU release path without emitting a Linux-visible EXIT record. The original
case now reports 42 distinct EXIT TIDs for the child, grandchild, and 40
sequential descendants.

The former 1,545-line PMU implementation is now a stable facade over focused
allocation, owner-CPU request, sampling storage, event-state, and open-validation
modules. Counter reservation remains process-wide in one allocator, CPU-local
register access remains behind value-only owner requests, and the event object
remains the sole owner of control and teardown state. This split changes no
publication, enable, stop, or reclamation ordering; it makes those boundaries
independently auditable without introducing a second PMU state source.

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

Task-event inheritance now has one fd-owned `PerfInheritanceFamily`. This
matches Linux v7.1 `inherit_event()`, which flattens every descendant onto the
original event's `child_list` under `child_mutex`; `_perf_ioctl()` applies
ENABLE, DISABLE, and RESET with `perf_event_for_each_child()`;
`__perf_event_read_value()` aggregates live and retired child counts; and
`perf_event_release_kernel()` marks the root dead before repeatedly removing
every child. TGOSKits' family relationship lock similarly rejects a child that
races close, publishes the current enable and output generations to a joining
member, and snapshots bounded member references before releasing the
relationship lock and waiting on owner-CPU work.

An exited descendant is removed from the bounded live-member relation after
its PMU generation is quiescent. Its value and enabled/running times are
saturating-folded into fd-owned retired totals before the strong member
reference is dropped. This mirrors Linux's child synchronization and prevents
sequential forks from exhausting a capacity intended to bound simultaneously
live events.

The previous implementation skipped a sampling child when the root ring had
not yet been mmaped. It also stored the root output on the monitored task
counter, so task exit could stop the poll worker and withdraw the ring while
the root fd and inherited descendants were still live. The deterministic
aarch64 regression now forks the child and grandchild before mmap, then maps
and enables only the root fd. Before the family change it completed with one
EXIT TID; after the change both descendant EXIT records share the root output.
It then creates 40 more descendants sequentially and requires 42 distinct EXIT
TIDs in total, proving that retired members do not consume the 32-entry live
relation capacity.
Pure state tests cover output publication to pre-existing members, ENABLE and
DISABLE inheritance for existing and future members, close versus join, and
exactly-once reservation release.

GDB on the same regression also found a separate owner-stop deadlock. A
`SpinNoIrq` guard created directly in a Rust `match` scrutinee remained alive
through the selected arm; `stop_requested_on_owner()` then tried to reacquire
that same `run_state` after the hardware stop. One CPU self-spun on the lock,
while concurrent fd teardown spun on the identical lock byte. Every stop action
is now copied out in an explicit short scope before hardware access, an
owner-CPU worker rendezvous, or completion publication. The scheduler
switch-out path publishes only the generation state; it never fans out a
`WaitQueue` while the scheduler baton and IRQ-off region are active. The
per-CPU task-context worker serializes contenders and observes an
`AlreadyComplete` fence after a switch-out winner, while a separate atomic
reclamation claim ensures the PMU slot and global active count are released
once.

The original aarch64 `perf-hw-freq` stale-wake panic remains the end-to-end
regression. Deterministic host tests cover target parsing, registry generation
reuse, close versus switch-out, failed owner-stop retry, redirect/detach
selection, shared output lifetime, and bounded multi-producer admission.

The first post-refactor x86_64 full `qemu/system` milestone passed the
scheduler, futex, timer, PID, exec, and perf groups; one `test-ptrace-gdb`
failure was not reproduced in 20 targeted reruns and remains recorded as an
intermittent observation rather than receiving a speculative scheduler
change. The first aarch64 full milestone passed those same critical groups and
failed only the deterministic `perf-hw-rdpmc` counter-selection assertion
described above. A final full aarch64 rerun is required after the remaining
exit-admission work.

A separate pidfd audit question is closed as Linux-compatible rather than
changed. Linux v7.1 keeps an unreaped zombie addressable through its pid object:
`pidfd_send_signal(..., 0)` and a nonzero signal return success until
`waitpid()` reaps the child, after which they return `ESRCH`. Starry's
generation-bearing `ProcessIdentity` has the same boundary. The existing
`syscall-test-pidfd-send-signal` QEMU case already asserts success for the
unreaped zombie and `ESRCH` after reaping, so no second PID/zombie authority or
compatibility patch was introduced.

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

PID namespace reaping no longer falls through to the global init process.
Linux v7.1 `find_child_reaper()` starts from
`task_active_pid_ns(father)->child_reaper`, and `find_new_reaper()` stops its
subreaper walk at the exiting task's PID namespace level. Starry now retains
the immutable PID namespace membership in the generation-bearing
`ProcessIdentity`, chooses subreapers only from that namespace, and falls back
to the namespace's stable init identity. The relationship layer has a distinct
namespace-shutdown transaction: it rejects new fork publication while still
allowing already-existing namespace members to be adopted during teardown.

The grouped PID namespace regression forks an intermediate parent and an
orphan below namespace PID 1, waits until the intermediate exit transaction is
complete, and then releases the orphan to observe its new parent. Before the
fix the orphan reported parent 0 and namespace init received `ECHILD`; after
the fix it reports parent 1 and is reaped by that init. The same change removes
the old `ProcessData` construction window in which every new identity was
temporarily associated with the root namespace before clone replaced its
`NsProxy`.

PID allocation and namespace shutdown now share the same final-publication
gate. This is the Starry task-context equivalent of Linux v7.1
`copy_process()` checking `PIDNS_ADDING` while holding `tasklist_lock` and
declaring that no failure is permitted after task visibility. Clone reserves
all namespace IDs first, prepares every fallible resource, and then holds the
gate while it publishes the PID entries, process relationships, task
registries, cgroup membership, and runnable scheduler thread. Namespace init
holds the same gate while it disables allocation and closes child publication.
A shutdown that wins the gate therefore revokes and rolls back a prepared
clone; a clone that wins is completely signalable before shutdown can enumerate
it.

Reservations intentionally remain visible to namespace shutdown before the
final-publication gate. Linux `alloc_pid()` likewise increments
`pid_allocated` before `copy_process()` takes `tasklist_lock`; after
`disable_pid_allocation()` clears `PIDNS_ADDING`, `zap_pid_ns_processes()` waits
for a failed in-flight clone to call `free_pid()`. Starry counts `Reserved`
entries in the shutdown membership predicate, rejects them at the guarded
publication check, and advances the member epoch when rollback removes them.
Moving the entire fallible clone preparation under the sleeping publication
gate would serialize resource allocation without removing this required
in-flight lifetime.

Each PID namespace now owns an immutable parent identity outside its mutable
allocation state. A process identity retains the complete innermost-to-root
lineage, and clone reserves a local ID in every non-root ancestor, matching
Linux `alloc_pid()`'s per-level `upid` allocation. Ancestor namespace shutdown
therefore includes tasks in nested namespaces. Thread exit and non-leader
`execve` release every corresponding ancestor TID; final zombie reap releases
every process PID. The deterministic host tests cover immutable namespace init
identity, reservation rollback after shutdown, nested ancestor membership, and
generation-safe PID retirement.

Linux `zap_pid_ns_processes()` also has to service zombies that become children
of namespace init only after their previous parent exits. Starry now snapshots
a monotonic member epoch before each reap pass, services newly reparented
zombies, and sleeps only if neither the membership nor the epoch changed.
Every terminal thread publication advances the epoch, including a process
leader whose namespace entry remains a process PID until reap. The grouped
namespace regression retains a child with `waitid(..., WNOWAIT)`, exits PID 1,
and proves shutdown kills its live parent, reaps the newly adopted zombie, and
only then releases namespace init.

Full namespace-relative numeric PID translation remains a pre-existing,
separate compatibility finding. Starry currently translates `getpid()` and
`getppid()`, while several PID/TID-returning and PID/TID-consuming syscall
paths still use the global registry key. This lifecycle stage deliberately
does not change one return path in isolation because doing so would make
`clone()` return a local PID that existing `wait`, signal, scheduling, and
ptrace lookups cannot yet resolve. The follow-up must introduce one typed
namespace PID resolver and migrate those ABI boundaries together.

The serial emergency path now preserves the hard-IRQ ownership boundary even
when panic output wins the non-blocking register gate. Linux v7.1
`serial8250_console_write()` uses a try-lock during oops, but independently
saves and clears IER before touching the TX FIFO, then restores it before
returning. TGOSKits keeps the stricter bounded/drop-on-contention panic API:
each concrete `UartEmergencyTx` must save and mask every device source before
its fixed-size FIFO pass and restore the saved mask through an RAII guard.
Therefore an IRQ endpoint that loses the gate can publish one task-context
retry without leaving a level-triggered UART source continuously asserted.
The ordinary port remains worker-owned under same-CPU IRQ exclusion, and the
registered IRQ callback still owns the only `UartIrq` endpoint.

The NS16550 regression records the IER value observed at the TX register and
requires zero while preserving the worker's original mask afterward. The
PL011 regression observes IMSC throughout the mask-guard lifetime and after
drop. Together with the existing fixed RX/TX budgets, queue-overflow, gate
contention, and worker-doorbell tests, this closes the serial gate-busy IRQ
storm without introducing a hard-IRQ lock or callback.

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
The xHCI handler no longer makes hard acknowledgement contend with task event
draining on one register gate. It owns three capability views: a non-blocking
USBSTS/IMAN acknowledgement endpoint, an ERDP/event-ring endpoint serialized
under task ownership, and a task-only IMAN rearm endpoint. A single
`Unmasked / Masking / Masked / Rearming` phase orders the two IMAN views. If a
new acknowledgement wins while rearm is writing IMAN, the rearm tail masks the
source again. As in Linux `xhci_enable_interrupter()` and
`xhci_disable_interrupter()`, every IMAN mask or unmask is read back before the
software state is published so posted MMIO cannot escape the transition.
Rearm also writes the RW1C IP field as zero, preserving an event that arrived
while masked instead of consuming it in the same write that opens IE.
The USBFS lifecycle gate rejects new work before IRQ callback removal and waits
for an in-flight worker before relinquishing callback ownership; the stable
registry slot itself remains allocated. The worker drops its event permit
before device rearm, so an IRQ delivered immediately by the IMAN write can
enter the callback and acknowledge the level source. Shared IRQ callbacks
return `Unhandled` when the controller did not claim the status.
The deterministic regressions
`stale_task_rearm_cannot_reenable_a_disabled_controller` and
`hard_irq_ack_preserves_controller_disable` reproduce the controller shutdown
races. `acknowledgement_during_rearm_requires_a_final_hardware_mask` covers the
IMAN race, and `rearm_write_preserves_a_new_hardware_pending_bit` covers the
RW1C encoding. USBFS gate tests cover teardown quiescence, fixed batch
exhaustion, and permit release before rearm.

Vsock transmit readiness now follows Linux
`virtio_transport_notify_poll_out()` rather than treating every connected
socket as writable. For events surfaced by `virtio-drivers`, the virtio
adapter publishes a task-context credit snapshot from the same device-owner
transaction that consumes protocol events and completes sends. Its
per-connection window is
`min(peer_buf_alloc, local_buf_alloc) - in_flight`, uses wrapping protocol
counters, and saturates to zero when a peer shrinks its window. The rdif
boundary exposes only the resulting send capacity.

`VsockStreamTransport::poll()` publishes `OUT` only for a live transmit half
with nonzero transport capacity, and connected sockets retain a TX `PollSet`.
Every surfaced connection event re-evaluates the credit snapshot after
releasing the device and connection-manager gates; a positive window wakes
registered writers. Device send performs one bounded attempt and reports
`WouldBlock`. The ordinary socket poller owns timeout, `MSG_DONTWAIT`,
registration, and retry, replacing the old ten-iteration timed wait inside the
device helper. The deterministic credit-window tests cover exhaustion, peer
forwarding, counter wrap, and peer-window shrink. The worker tests prove both
publish-before-wake gate release and one device attempt under backpressure.

The upstream manager still consumes `CREDIT_REQUEST` internally even though
its header carries valid peer credit. TGOSKits therefore cannot yet make the
manager's private credit state the sole readiness source without copying the
third-party connection manager. Issue
[#1724](https://github.com/rcore-os/tgoskits/issues/1724) records the required
observer/capacity boundary and a zero-window `CREDIT_REQUEST` regression test.
This branch deliberately does not infer credit from `poll() == None`, because
that would restore unconditional false `POLLOUT` readiness.

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
