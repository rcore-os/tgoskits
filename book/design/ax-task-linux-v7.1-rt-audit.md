# ax-task Linux v7.1 PREEMPT_RT Audit

## Status and scope

This document is the audit ledger for the task-system migration on
`codex/refactor-ax-task-from-1596`.

The current audited base is `origin/dev` at
`07f221e6880fc5e2f88a81185b2647f5f7500f02`. The base already contains the
Starry PID lifecycle fix from PR #1706. Its generation-specific
`ProcessIdentity` and `Live -> Zombie -> Reaping -> Reaped` transition remain
the sole authority for PID visibility and reaping.

The implementation snapshot immediately before this ledger update is
`0a8758ca95f1176a38ea5cd8ad69f424af0b5893`: 128 commits and 646 paths relative
to that base, with 82,502 insertions and 19,103 deletions. Every path in that
range is assigned to one of the areas below. Driver work is limited to drivers
and IRQ adapters already changed by the range, plus the minimum adjacent
runtime boundary needed to repair them.

The older base `37c8f60e81b135c7d997c39630410db418ee192d`, initial branch head
`962d057785624994d365ea33bb27fdfe0b24ce75`, and structural milestone hashes
retained below are pre-rebase evidence identifiers. They are not claimed to be
ancestors of the current rebased branch. Likewise, a QEMU result described as
a previous or pre-rebase milestone is historical evidence, not a current-head
pass; current-head validation is recorded separately.

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
| Starry signal return | user-task interrupt state and signal safe point | `recalc_sigpending_tsk()`, `complete_signal()`, `signal_wake_up_state()` | A return-to-user acknowledgement can clear only wake publications observed before its signal scan | The boolean interruption flag is cleared unconditionally after the scan, so a concurrently queued process signal can lose its only wake |
| Process lifecycle | starry-process and Starry task/process glue | `exit.c`, `forget_original_parent`, `do_wait` | PR #1706 identity owns exit/reap; relationship updates have one lock order and one publication transaction | Reparent and retire can acquire children/group locks in conflicting orders |
| Perf ownership | Starry perf task, hardware and sampling paths | `kernel/events/core.c` | `pid == 0` is a task context; `pid == -1` with a CPU is a CPU context; teardown executes on the owner CPU before storage release | CPU-local sampling slots are registered and removed on the current CPU and contain raw notify pointers |
| Tracepoint publication | Starry tracepoint registry and perf/BPF attachment | `kernel/tracepoint.c` | Publish a complete callback generation before enabling its fast-path gate; callback execution holds no raw writer lock; retired callback data remains live through a read-side grace period | The registry held `SpinNoPreempt` across arbitrary callbacks, and callback registration patched executable text after SMP startup |
| Generic timers | Starry POSIX timers and AxVM timer worker | hrtimer soft/threaded callbacks | Arbitrary callbacks run in task context; earlier deadlines use a bounded wake endpoint | Worker design exists and must remain separate from ax-task task deadlines |
| CPU interval timers | Starry `ITIMER_VIRTUAL`/`ITIMER_PROF`, ax-runtime periodic clockevent, ax-task task work | `update_process_times()`, `run_posix_cpu_timers()`, `TWA_RESUME` task work | A real periodic tick performs only bounded generation/timestamp publication in hard IRQ; accounting, expiry, and signal work run in task context; writer contention is retried without spinning; disable/rearm cannot replay an old generation | After wall polling was removed, CPU timers were checked only at user/kernel transition safe points, so a long-running syscall could delay expiry; the first runtime composition also hid inner OS tick work behind its outer extension |
| Serial and driver IRQ | rdif-serial, some-serial, ax-runtime serial, changed vsock/USB adapters | serial core, NAPI/event publication | IRQ endpoint only reads/acks/drains a bounded amount and publishes stable events; workers advance flow | Endpoint separation exists but changed paths still require lock, allocation and ownership audit |
| Architecture idle | axcpu idle primitives and trap glue | architecture idle entry, especially LoongArch `genex.S` | IRQ enable plus idle is atomic with pending-work recheck; an interrupt in the enable/idle window returns after the idle instruction and is still dispatched | LoongArch follows the Linux return-address pattern but lacks injected timer/IPI window tests |
| Compatibility | ax-api, ax-posix-api, axstd, Starry syscalls and axvm callers | Linux UAPI and project compatibility contracts | Internal APIs may change; Linux ABI, errno and axstd observable behavior do not | The migration spans many adapters, so compile-only validation is insufficient |

The last column is the baseline finding, not an open-finding list. The current
disposition of every row is:

| Area | Current disposition |
| --- | --- |
| Scheduler placement | Closed by the single `SchedulerPlacement` state machine, owner-scoped runqueue operations, switch-tail retention, and the offline-target recovery carrier. |
| Remote delivery | Closed by generation-bearing scheduler work, claim-before-drain acknowledgement, publish-before-IPI ordering, and re-kick after a newer epoch. |
| CPU lifecycle | Closed at the scheduler/runtime boundary by producer draining, quiescence validation, clockevent offline/online hooks, and `min(platform_cpu_count, CPU_CAPACITY)` admission. A complete platform CPU hot-unplug service remains a stated non-goal. |
| Task deadlines | Closed by three generation-checked per-thread timer classes: park timeout, CBS/miss/replenishment, and GRUB zero-lag. Hard IRQ copies only typed values; owner safe points validate the exact registration before changing task state. |
| Physical clockevent | Closed by the sole per-CPU `Offline / Idle / Armed / Firing` owner, typed finite deadlines, saturating tick conversion, overdue recovery, and one hardware commit per transaction. |
| Context switch | Closed by the scheduler baton, runtime switch-tail hook, physical `on_cpu` release ordering, and transactional stack/TLS/context/address-space ownership. |
| PI and sleep locks | Closed by generation-bearing PI identities, bounded waiter/grant transactions, deboost-before-wake, quiescent destruction, and task-context sleeping waits. |
| IRQ wake lifetime | Closed by generation-bearing `IrqWaitCell` state, revocation plus quiescence, and no consumer-visible raw wake pointer. |
| Starry signal return | Closed by monotonic interruption publication/acknowledgement generations and typed `Ready / Interrupted / TimedOut` waits. |
| Process lifecycle | Closed around dev's sole `ProcessIdentity` authority, `ProcessRelationTxn`, and PI-backed task/PID/group/session registries; no competing zombie/PID state machine or preemptible raw registry lock is present. |
| Perf ownership | Closed by typed task/CPU targets, fixed owner-CPU workers, generation-checked sampling registrations, IRQ grace before release, and a physical scheduler-owner fence during migration. |
| Tracepoint publication | Closed by side-effect-free callback generations, an atomic callback gate, raw-lock-only snapshot acquisition, two-epoch reader leases, and task-context retirement after the last IRQ/scheduler reader. |
| Generic timers | Closed by CPU-affine task workers and bounded wake endpoints; ax-task contains no arbitrary timer callback API. |
| CPU interval timers | Closed by real-periodic-expiry detection, hard-IRQ-only generation/timestamp publication, a serialized task-context accounting writer, bounded retry ownership, O(1) aggregate snapshots, retained delivery, explicit ax-runtime forwarding, and Starry task-context expiry/signals. |
| Serial and driver IRQ | Closed for the branch-touched serial, vsock, and USB/xHCI paths: hard IRQ work is bounded and non-sleeping, while manager/topology work is task-owned. The upstream vsock credit observer limitation is tracked separately as #1724. |
| Architecture idle | Closed by pending-work recheck and live injected timer/IPI window tests on all four architectures. |
| Compatibility | Closed by source/feature validation and the final four-architecture ArceOS and Starry QEMU milestones recorded below. |

## Structural audit milestone

The following hashes describe the pre-rebase structural audit. The first
behavior-preserving split was completed at branch head
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

Idle-pull source selection preserves the same class split as Linux instead of
using one total scheduling key as a load metric. Deadline and RT candidates
retain their globally comparable absolute-deadline or fixed-priority ordering,
and therefore take precedence over Fair work. Fair and Idle candidates first
select the busiest published runqueue because EEVDF virtual deadlines are
local to each runqueue and cannot order load across CPUs. Linux v7.1 expresses
the same separation through RT pull selection and Fair
`sched_balance_newidle()`/`sched_balance_rq()` load balancing. The deterministic
regression
`idle_pull_uses_load_not_cross_cpu_eevdf_deadline_within_fair_class` failed
when a two-task high-weight Fair queue had a numerically earlier local virtual
deadline than a five-task low-weight queue; the idle CPU now requests the
five-task source while the existing RT-over-Fair regression remains green.

The asynchronous owner-to-owner pull also has a target-owned reservation
transaction. `Pending -> Claimed -> Committed` shares one atomic word with the
count of remote work publishers. Local enqueue cancels before mutating the
target runqueue; remote wake and migration publication increment that count
and cancel an uncommitted pull in the same compare-exchange. The source can
therefore commit only before newer target work starts, while work arriving
after commit is ordered as an ordinary post-balance wake. This replaces the
old unused source-summary epoch, which could not protect target idleness.
Linux closes the equivalent window while `sched_balance_newidle()` rechecks
the target rq and pending wake work under scheduler locking; the reservation
is the owner-only message-passing equivalent. The deterministic stale-request
regression and Loom's
`idle_pull_commit_orders_against_target_work_publication` cover both sides of
the commit race.

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

A later placement audit closed the remaining gap between target selection and
remote publication. A balancing or wake owner could select an online CPU, then
detach the Ready thread after that target had entered `Draining`; the target
inbox rejected the publication after the wake transition was already
consumed. Returning `CpuOffline` at that point left no physical runqueue owner
to retry the task. Linux v7.1 closes placement admission before
`sched_cpu_deactivate()` drains the runqueue and flushes pending scheduler and
IRQ work. TGOSKits now treats the still-running source owner as the recovery
carrier: a rejected target publication is republished to the source inbox,
whose safe-point drain revalidates the committed target and either enqueues
locally or forwards to another allowed online CPU. The source retry is a
placement reconciliation message and may therefore name the same source and
inbox CPU. The deterministic
`migration_publication_recovers_through_source_when_target_starts_draining`
regression previously failed with `CpuOffline(1)` after the thread entered
`Migrating`; it now completes with exactly one queued source owner.

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

The pre-rebase core milestone also ran the complete ArceOS `rust/all` QEMU group
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
reader cannot block its task-context mutation. Scheduler activation still has
to coexist with an already admitted compatible remote reader, however; that
case is distinct from both an exclusive writer and duplicate CPU ownership.

`ScopeCell` therefore admits exactly one scheduler activation and rejects a
second CPU before publishing its per-CPU pointer. Starry no longer treats
`ScopeCellBusy` as a condition to poll with repeated yields: the outer PI mutex
handles legal task-context contention, and a failed bounded upgrade is an
explicit scheduler-ownership invariant violation. The deterministic red test
observed the old implementation publish the same task-owned scope on CPU 1
while CPU 0 still owned it; the new implementation reports
`ScopeActivationError::AlreadyActive` without publishing on CPU 1, and the sole
activation upgrades and unwinds without losing its lease.

The full RISC-V system sequence later exposed a separate false-conflict path
while `test-openat-umask-smp` was running. The raw shared gate loaded the reader
count once and attempted one compare-exchange. If another compatible reader
changed that count between the two operations, activation returned the same
error as an exclusive writer or a duplicate scheduler owner. Linux v7.1 RT's
`rwbase_read_trylock()` retries compare-exchange while the observed state still
admits readers; simply copying that loop would remove the false conflict but
would not preserve scope-local's fixed hard-path bound.

The gate now reserves one reader count with a single atomic fetch-add, checks
the writer bit from the returned state, and rolls the reservation back without
accessing protected data when a writer was already present. Exclusive unlock
preserves any such in-flight failed reservations, and downgrade publishes its
active shared lease before clearing the writer bit. The deterministic
interleaving regression inserts a compatible reader between the outer
reader's reservation and decision: the former load/CAS implementation rejected
the outer reader, while the reservation protocol admits both. Scheduler
activation reports `ExclusiveLease` and `AlreadyActive` separately, so a future
QEMU failure cannot turn ordinary reader movement into evidence of duplicate
CPU execution. The reduced four-CPU RISC-V `test-openat-umask-smp` case then
completed all eight workers and 2,400 file iterations without an activation
failure.

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
new events. Starry now gives each thread one `ThreadPerfContext` containing
both the fixed scheduler list and its admission state under the same bounded
IRQ-safe lock. Exit tombstones admission and takes its complete snapshot in
one transaction. A concurrent open therefore either publishes its counter and
global fast-path key before that snapshot, or observes the tombstone and
returns `ESRCH`; it cannot attach after cleanup has selected the ownership set.
The scheduler hook still borrows the fixed list without allocation, while
side-band and inheritance paths take bounded `Arc` snapshots and release the
gate before task-context work.

The deterministic state test initially failed because the old exit operation
only cloned the list: attaching `2` after an exit snapshot containing `1`
returned success instead of `Closed`. The same test is green after
tombstoning. A loom race covers both legal linearizations and also models the
global counter publication inside the list lock: if attach wins, exit owns and
releases exactly that entry; if close wins, attach is rejected and the active
count remains zero. Failed-open rollback distinguishes a never-attached PMU
reservation from an attached one, preventing a global-count underflow, and a
later opener may reclaim a quiescent failed entry without making final detach
panic. The reservation itself now transitions
`Reserved -> Published -> Released`; only a `Published` release decrements the
global scheduler fast-path key, so every admission error uses the same
idempotent `free_hw()` transaction instead of a hand-written side path.

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

The pre-rebase first post-refactor x86_64 full `qemu/system` milestone passed the
scheduler, futex, timer, PID, exec, and perf groups; one `test-ptrace-gdb`
failure was not reproduced in 20 targeted reruns and remains recorded as an
intermittent observation rather than receiving a speculative scheduler
change. The first aarch64 full milestone passed those same critical groups and
failed only the deterministic `perf-hw-rdpmc` counter-selection assertion
described above. The targeted RDPMC, fork/exit, and inheritance regressions
now pass after the dynamic metadata and exit-admission repairs; a final full
aarch64 rerun remains a milestone check for their combined integration.

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

Two follow-up regressions close the task-side latency and handoff boundaries.
First, an emergency writer could acquire and release the register gate after
the maintenance worker had consumed the original TX doorbell. The worker
would observe the busy gate and park, while the emergency path returned
without republishing progress; buffered normal output then depended on an
unrelated interrupt. Linux v7.1 `nbcon_device_release()` similarly checks for
pending records after releasing device ownership and either flushes them or
defers console output. TGOSKits now drops the register guard with Release
ordering before issuing one existing `IrqWaitCell` notification. The panic
path remains allocation-free, bounded, non-waiting, and drop-on-contention.
The deterministic
`emergency_release_notifies_the_deferred_worker` regression observed zero
handoffs before the change and one handoff after gate release afterward.

Second, PL011 runtime `startup()` and `set_config()` inherited the early-console
`FR_BUSY` polling loop while ax-runtime held `NoPreemptIrqSave`; a permanently
busy transmitter could therefore execute `BUSY_POLL_BUDGET` iterations with
local interrupts disabled. Linux v7.1 `pl011_startup()` and
`pl011_set_termios()` perform bounded register transactions under the port
lock and do not reuse the polling-console busy drain. The runtime endpoint now
does the same: it masks/clears the device IRQ state, preserves an in-flight
boot-console TX FIFO, commits configuration, and enables the UART in a finite
register sequence. Only the early-console `open()` path retains the bounded
busy takeover and restores the previous control register on timeout. The old
runtime behavior returned `ConfigError::Timeout` in
`runtime_config_does_not_wait_for_the_transmitter_to_become_idle`; the same
busy hardware state now completes without changing the enabled TX/RX paths.

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

The transport credit lifetime is now tied to an explicitly opened connection.
Previously, a successful local disconnect left its `TxCreditBook` entry live,
so the rdif transport could continue to advertise stale send capacity after
the socket had published local shutdown. Removing that entry alone was
insufficient because a late peer credit update used `entry().or_default()` and
silently recreated it. Linux v7.1 `vsock_shutdown()` publishes the local
shutdown state before calling the transport, and the virtio poll-out path
requires an active, send-capable socket before reporting space. TGOSKits now
opens credit only for connection establishment events, updates and accounts
bytes only for a live entry, and retires the entry after the transport accepts
local disconnect. The deterministic
`successful_local_disconnect_retires_transmit_credit` and
`late_peer_credit_cannot_reopen_a_locally_closed_connection` regressions
failed respectively with stale capacity and a resurrected zero-capacity entry
before the change.

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

The full RISC-V system sequence subsequently exposed a second boundary in the
same path. A process-directed `SIGKILL` was present in the shared pending queue
and the chosen thread had been woken, but the target returned through its
signal scan and unconditionally cleared the boolean interruption flag. When
the signal arrived between the final dequeue attempt and that clear, the wake
of the still-running thread was consumed, the new publication was erased, and
the target's next `pause()` parked permanently. GDB showed the shared
`SIGKILL` bit set while both target threads had a clear local interruption
flag.

Linux v7.1 avoids this split-brain state under the sighand lock:
`complete_signal()` queues shared pending state, `signal_wake_up_state()` sets
the selected task's `TIF_SIGPENDING` before waking it, and
`recalc_sigpending_tsk()` recomputes the flag from both private and shared
queues before it may be cleared. Starry retains its scheduler-independent
signal queues, but now applies the same publication invariant through a
monotonic `InterruptState`. Producers publish their reason, advance the
generation with release ordering, and then wake the scheduler thread. The
owner snapshots that generation before scanning timers, exit requests, and
signals; it acknowledges only that snapshot and repeats the safe point when a
newer generation exists. A nested consumer cannot move the acknowledged
generation backwards.

The deterministic state regression
`publication_after_snapshot_survives_acknowledgement` failed with the former
boolean clear and passes with the generation protocol. The two companion tests
cover ordinary acknowledgement and nested-consumer monotonicity. All 22
`starry-kernel` feature-clippy checks pass, and the reduced
`syscall-test-aspace-teardown-reclaim` RISC-V QEMU case completed all twelve
`SIGKILL`/`waitpid`/address-space-reclaim iterations in 104 seconds.

The subsequent pre-rebase full RISC-V `qemu/system` run passed the scheduler-sensitive
address-space reclaim, robust futex, SMP futex wake-op, four-CPU umask, and
page-cache pressure cases without a panic or grouped failure marker. The
runner then terminated the guest at exactly its suite-wide 1800-second limit
near the 353rd of roughly 402 installed binaries. Because the grouped script
prints its timing summary only after the final binary and the runner does not
persist the serial stream on timeout, this is not evidence that the current
binary hung. Issue
[#1767](https://github.com/rcore-os/tgoskits/issues/1767) tracks splitting the
group, retaining incremental timing artifacts, and distinguishing a per-case
stall from aggregate wall-clock exhaustion. The task/runtime branch does not
delete cases or loosen the success expression; final milestone runs use the
existing timeout-scaling facility until the test infrastructure is split.

## Current post-rebase integration closure

The current `fb399d055`-based sequence retains the correctness changes above
and adds focused integration/module commits:

- `5929e3e50` adapts the AxVM timer worker to the current device runtime;
- `7afee9c8b` finishes the ax-task scheduler ownership split;
- `d871c319d` moves ax-net to the core scheduler yield API;
- `94e14299d` makes Starry user-task references scheduler-backed;
- `e5a41f08b` splits Starry timer ownership;
- `6a3558bd3` splits task PMU model, scheduling, lifecycle, control,
  attachment, and read ownership;
- `b9b9a218c` replaces serial's arbitrary IRQ RX sink with the fixed-value,
  64-byte `SerialIrqReport`;
- `75d775bc2` splits PL011 control, IRQ, RX, emergency TX, register, runtime,
  event, and test ownership; and
- `faefd1ad2` fences task perf publication against the physical scheduler
  owner rather than the ahead-of-switch direct-wake destination;
- `8b44f2875` moves sigwait wake publication into the signal managers that own
  pending-state publication and replaces the stale source-shape assertion with
  behavior tests;
- `ac4ab7a7d` replaces the obsolete zombie/PID source contract with a grouped
  Linux-ABI regression that retains the exited leader's nice value; and
- `30de59fe2` replaces task-layout assertions with typed scope cloning,
  move-only user-memory access guards, a value-only bounded observer stack,
  and directly compiled behavior tests;
- `ed10b4b54` permits a legacy clone parent only when it is a live member of
  the new PID namespace publication transaction;
- `330818a20` moves cgroup membership into the stable process generation and
  removes the global callback-under-spinlock path; and
- `353355c9b` publishes fatal process signals before releasing ptrace or
  job-control stops.

The final perf correction closes a migration-specific context-install window.
Direct wake placement may be changed to the destination CPU as soon as
migration is requested, while `SchedulerPlacement::on_cpu()` intentionally
remains on the source through switch tail. Waiting for a fixed worker on the
wake destination could therefore complete before the still-running task had
crossed a scheduler boundary and observed the new event. This is the same
distinction Linux v7.1 preserves in `task_function_call()` and
`perf_install_in_context()`: either rendezvous with the CPU that physically
runs the task or prove a concurrent schedule transition serialized the
installation. `ThreadHandle::scheduler_fence_cpu()` now exposes only that
read-only fence snapshot. The deterministic migration regression first
observed wake target CPU 1 and an incorrect fence CPU 1 while physical
ownership remained on CPU 0; the corrected implementation returns CPU 0.

Current module sizes reflect the ownership split rather than facade growth:
the ax-task orchestration root remains 225 lines; its largest production
submodule is the 900-line registry, while the 2,832-line scheduler test module
is intentionally separate. `ax-runtime::task` remains a 148-line facade and
the runtime crate root 189 lines. Starry task perf is split into a 109-line
facade over 20- to 558-line domain modules. The remaining Starry `Thread`
orchestrator is 697 lines; its fixed observer stack and user-memory access
lease now live in focused 34- and 56-line value modules. PL011 is split into a
35-line facade over 51- to 368-line production modules plus its dedicated test
module.

The final static IRQ audit found no arbitrary callback, allocation, sleeping
lock, unbounded loop, or unowned wake lifetime in the branch-touched serial,
vsock, or USB/xHCI hard-IRQ paths. Serial now returns a bounded value report,
publishes it to a preallocated SPSC queue, and signals only after publication.
Vsock protocol/manager work remains in a bounded task worker. xHCI hard IRQ
uses only non-blocking acknowledgement/masking; event-ring processing and
rearm remain task-owned under the stable registry protocol.

The final clockevent audit retained the existing Idle spurious-interrupt
behavior. Like Linux v7.1 `hrtimer_interrupt()`, a delivered physical
interrupt may enter the bounded service transaction even if no deadline
expires; the state test proves the transaction is a no-op and returns to
`Idle`. Offline CPUs remain an explicit invariant violation at the handler
boundary because runtime offlining disables the physical source before final
scheduler publication. No evidence justified a speculative second phase or
timer-accounting cache.

The non-QEMU validation feeding the final milestone includes the
deterministic perf migration red/green test, `cargo xtask clippy --package
ax-task`, `cargo check -p starry-kernel --tests`, and all 22
`starry-kernel` feature-clippy configurations. The directly compiled task-state
tests cover fixed-capacity LIFO overflow and nested move-only user-memory
access; the scope-local two-CPU behavior test covers sole activation,
duplicate-CPU rejection, writer exclusion, bounded upgrade, and unwind. The
remaining worker contract static check is explicitly limited to forbidden API
names, where source text is the lint subject rather than a substitute for
runtime behavior.

The x86_64 grouped
`zombie-bugfix-bug-zombie-syscalls` regression passes all ten ABI checks and
the formal `STARRY_GROUPED_TESTS_PASSED` marker. Its multithreaded child now
also proves that a pthread inherits the leader's nice value, changing the
worker to nice 12 does not change the leader's nice 7, and
`/proc/self/task/<tid>/stat` reports those two thread-local values before the
leader exits. The complete package, formatting, static-symbol, and
four-architecture QEMU results are recorded in the final validation section
below; historical results above are not substituted for them.

## Current-head Starry process closure and RISC-V milestone

The first current-head RISC-V run stopped reproducibly in
`test-fcntl-deadlock-smp`. GDB showed a cgroup membership operation holding the
global `SpinNoIrq<MembershipState>` while invoking a process-provider callback.
That callback acquired `PROCESS_TABLE`, whose writer had been preempted on
another CPU. The readers could not sleep or allow the writer to run, so the
callback boundary converted ordinary registry contention into a system-wide
IRQ-off spin. This was an ownership defect, not an fcntl defect.

Linux keeps a task's cgroup membership on the stable task identity and performs
fork, migration, and exit as explicit membership transactions; it does not
invoke an arbitrary process-registry callback while holding a global cgroup
membership spinlock. TGOSKits now follows that boundary with a
generation-specific `ProcessMembership`, a task-context
`ProcessCgroupState<PiMutex<_>>`, and a move-only `CgroupForkGuard`. Clone
publishes membership before the child becomes visible and rolls it back with
the other prepared resources. Final exit retires membership from the stable
`ProcessData` object without a PID lookup. The focused component regressions
cover migration rollback, missing and exited identities, fork rollback versus
scheduler publication, and the absence of a global provider lock. The
original SMP case now completes all 32 rounds in five seconds.

The next current-head run exposed a distinct ptrace-stop race. Both
`PTRACE_TRACEME` and the initial `SIGSTOP` had succeeded, but a subsequent
process-directed `SIGKILL` first released the ptrace stop and only then inserted
the fatal signal into the process pending queue. The tracee could therefore
resume through the stop, consume the wake, and execute `_exit(0)` before the
fatal publication became authoritative. `waitpid()` then correctly reported
normal status zero for the state that Starry had incorrectly published.

Linux v7.1 makes the opposite ordering part of its signal and exit protocol:

- [`ptrace_traceme()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/ptrace.c#L497-L518)
  installs tracing under the task-list relationship lock;
- [`ptrace_stop()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c#L2352-L2442)
  publishes the stop before waking the tracer;
- [`get_signal()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c#L2923-L2927)
  makes `SIGKILL` bypass ptrace interception; and
- [`do_group_exit()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c#L3001-L3038)
  publishes the fatal group exit code before the exit/wait handoff.

Starry now has one `publish_before_release()` boundary. It inserts the fatal
signal, wakes the selected process target and the exact ptrace-stopped thread,
and only then clears ptrace or job-control stop state. A thread interrupted out
of its initial ptrace stop rescans pending signals before reaching the first
user instruction. Stop release no longer creates a window in which a
non-interceptable `SIGKILL` can be replaced by a racing `_exit(0)`.

The ordering regression was first compiled directly against the real
`signal_publication.rs` module with the old release-before-publish order. It
failed deterministically because the release callback observed phase zero.
The same test passes with publication at phase one. The grouped
`test-proc-status-tracerpid` regression additionally pins the tracer to CPU 0
and the tracee to CPU 1 for 64 rounds; every round requires
`WIFSIGNALED(status)` and `WTERMSIG(status) == SIGKILL`.

The affected ABI boundaries and their retained Linux behavior are:

| ABI boundary | Starry path | Linux standard and implementation | Required result |
| --- | --- | --- | --- |
| `kill(2)` and process-group signal delivery | `sys_kill()` -> `send_signal_to_process()` | [`kill(2)`](https://man7.org/linux/man-pages/man2/kill.2.html), [`complete_signal()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c#L963-L1033) | Publish a process signal before waking or releasing a stopped target; signal zero remains an existence/permission probe. |
| process and process-group `pidfd_send_signal(2)` | pidfd target -> `send_signal_to_process()` | [`pidfd_send_signal(2)`](https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html) | Use the same stable process identity and fatal publication order as `kill(2)`. Thread-target pidfds retain the separate thread-signal path. |
| `ptrace(2)` stop and kill control | `ptrace_kill()`, attach/interrupt stops, ptrace wait state | [`ptrace(2)`](https://man7.org/linux/man-pages/man2/ptrace.2.html), Linux sources linked above | A ptrace stop is observable before tracer wake, while `SIGKILL` remains un-interceptable and cannot resume the tracee into user code. |
| `wait4(2)` and `waitid(2)` | stable process/ptrace/job state -> child-exit event | [`wait(2)`](https://man7.org/linux/man-pages/man2/wait.2.html), [`do_wait()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c#L1174-L1297) | Observe the terminal `SIGKILL` status rather than a later normal exit. These wait paths consume published lifecycle state; they do not invoke the signal publisher. |

`tkill(2)`, `tgkill(2)`, and thread-target pidfds continue to use
`send_signal_to_thread()` and are not routed through the new process
publication boundary.

At `353355c9b`, the complete RISC-V Starry milestone passed without exclusions:

- `system`: 1,738.44 seconds, including `test-fcntl-deadlock-smp`,
  `test-proc-status-tracerpid`, and `test-ptrace-traceme-stop`;
- `tty-console-input-burst`: 40.22 seconds;
- runner result: `2/2 case(s) passed`;
- formal markers: `STARRY_GROUPED_TESTS_PASSED`,
  `STARRY_TTY_INPUT_BURST_PASSED`, and `all starry qemu tests passed`.

This supersedes the historical aggregate-timeout observation for RISC-V.

## Final architecture and tooling validation

The first aarch64 milestone attempt exposed a compile-only defect in the Starry
perf module split. The standard 22-check feature clippy matrix used the host
target, so `cfg(target_arch = "aarch64")` excluded every task-PMU module. The
formal qemu-rga build compiled those modules and found 24 visibility/path
errors: implementation items had remained `pub(super)` inside their new child
modules even though sibling modules consumed them through the task facade, and
one lifecycle helper resolved `super::hw_allocation` from the wrong parent.

The repaired boundary uses `pub(in crate::perf)` for capabilities shared only
inside the perf subsystem. Task-private time helpers remain private, and no
PMU implementation type was made public outside the crate. The exact
qemu-rga release build then passed. All 22 standard feature-clippy checks
passed, and an explicit
`aarch64-unknown-none-softfloat` clippy check with
`dynamic_debug,input,rga,smp,vsock` compiled the previously omitted ARM PMU
code with warnings denied. Issue
[#1773](https://github.com/rcore-os/tgoskits/issues/1773) tracks making this
target-aware check a permanent CI capability rather than relying on the QEMU
build phase.

The final Starry QEMU matrix completed serially with no exclusions:

| Architecture | Tested implementation | Runner cases | Formal result |
| --- | --- | --- | --- |
| RISC-V | `353355c9b` | system 1,738.44 s; tty burst 40.22 s | 2/2 passed |
| AArch64 | `60e2b9101` | qemu-rga system 4.89 s; system 1,320.44 s; tty burst 15.96 s | 3/3 passed |
| LoongArch64 | `60e2b9101` | system 943.42 s; tty burst 16.43 s | 2/2 passed |
| x86_64 | `60e2b9101` | system 1,079.68 s; tty burst 14.15 s | 2/2 passed |

Every runner reported `STARRY_GROUPED_TESTS_PASSED`,
`STARRY_TTY_INPUT_BURST_PASSED`, and `all starry qemu tests passed`. The
RISC-V implementation predates only the documentation commit and the
AArch64-only perf visibility correction; neither changes code compiled into
the RISC-V target.

Together with the core-milestone ArceOS `rust/all` matrix, which reported all
17 cases and `ArceOS test suite run OK!` on x86_64, RISC-V, AArch64, and
LoongArch64, this closes the architecture release gate. The Starry runs cover
the scheduler, remote wake, SMP futex, timer/clockevent, affinity, ptrace,
clone/exec/exit, PID/zombie/pidfd/wait, signal interruption, PMU ownership,
UART burst, and branch-touched USB IRQ paths in their integrated environments.

The standard `starry-kernel` ktest path remains a separate infrastructure
defect: both x86_64 and RISC-V bare-metal axtest builds incorrectly compile
`log 0.4.33` with its std/default feature and fail before linking. Production
Starry builds and all system QEMU groups above do not use that invalid
dependency graph. Issue
[#1772](https://github.com/rcore-os/tgoskits/issues/1772) records the exact
commands, feature-graph evidence, and four-architecture acceptance criteria.
The fatal-signal ordering regression remains compiled directly from its real
source module until the standard ktest target is repaired; no test was skipped
or weakened to hide the tooling failure.

## Current-head CPU interval timer closure

Removing wall-clock polling from `ITIMER_VIRTUAL` and `ITIMER_PROF` corrected
their clock source, but exposed a scheduler integration gap. Starry checked
those timers while switching between user and kernel accounting states. A
thread that remained inside one long-running syscall continued to accrue CPU
time in ax-task, but did not necessarily reach another Starry polling point
before the timer should have expired.

Linux v7.1 separates virtual-time ownership from CPU-timer expiry:

- `vtime_user_enter()`, `vtime_user_exit()` and
  `vtime_task_switch_generic()` are the task-vtime writers. They update one
  task's state and start time under its `vtime.seqcount`; the scheduler
  switch-out marks the task inactive and clears its CPU before switch-in
  publishes the new owner CPU.
- `task_cputime()` is a read-side seqcount snapshot. For a running task it adds
  the residual from `sched_clock()` without becoming a vtime writer.
- `account_process_tick()` returns immediately when vtime accounting is
  enabled. The periodic tick is therefore not a second task-vtime writer.
- `run_posix_cpu_timers()` performs a bounded fast-path check with interrupts
  disabled. Group expiry uses `thread_group_cputimer::cputime_atomic` and
  `expiry_active` rather than walking every sibling on each tick.
- with `CONFIG_POSIX_CPU_TIMERS_TASK_WORK`, expiry is coalesced into
  `TWA_RESUME` task work, and the PREEMPT_RT path rechecks ticks that raced
  expiry collection before reopening the fast path.

TGOSKits follows the same ownership split without putting Starry timer objects
or arbitrary callbacks in ax-task:

1. `LocalClockEvent` reports whether the physical firing transaction actually
   crossed the periodic deadline. An earlier task deadline does not masquerade
   as a scheduler tick, and delayed ticks catch up without deadline drift.
2. The ax-task hard-IRQ path observes the shared generation-bearing
   `SchedulerTickGate`, updates a monotonic `observed_ns` watermark, retains the
   generation-bearing carrier `ThreadId`, and publishes at most one intrusive
   task-work record. It does not invoke OS code, allocate, acquire a Starry
   lock, run expiry logic, or publish a signal.
3. Starry task vtime has one writer: the running task's CPU. User/kernel
   transitions hold only a short `NoPreempt` scope, and scheduler switch hooks
   run under the owner baton. Both update an odd/even sequence counter so
   readers can retry a mixed snapshot. Migration naturally hands ownership
   over at switch-out/switch-in; there is no cross-CPU writer lock.
4. Repeated ticks in one enabled generation coalesce and retain the latest
   timestamp. The already-pending path still performs an AcqRel generation
   RMW: either a concurrent claim observes the timestamp publication, or the
   failed RMW installs a new physical record after the claim. This mirrors the
   publish/claim pairing in Linux `irq_work_claim()` instead of treating a
   plain pending-bit load as a publication barrier. Disabling the gate
   invalidates queued work from that generation; re-enabling cannot replay it.
5. ax-runtime explicitly composes an inner OS extension's tick capability into
   its scheduler-owned outer extension. The task-work callback takes a stable
   read-only vtime snapshot at the IRQ watermark and advances per-task
   user/system high-water marks. Owner transitions use the same high-water
   publication, so concurrent readers and writers cannot double-add a delta to
   the process aggregate.
6. A running thread's base-policy change is accounted at the ax-task
   owner-apply boundary, not on the syscall caller's CPU. The bounded hook runs
   after the thread-state lock is released while the owner still retains its
   scheduler baton. Queued and inactive threads do not invoke this execution
   hook: their applied base policy is copied into the typed switch endpoint and
   passed directly to `on_switch_in`. This removes the old side-band
   `realtime_policy` atomic and closes the inactive-to-running race between
   policy publication and vtime start. PI donation remains separate from the
   Linux scheduling policy used by `RLIMIT_RTTIME`.
7. The task-work callback then reads the process aggregate in O(1), polls only
   Starry's Virtual and Prof timers under their task-context `PiMutex`, then
   publishes signals after releasing timer metadata. Ordinary process-clock
   and `getitimer` reads retain the precise live-sibling residual scan. Real
   remains owned by its alarm generation. An inbox delivery lease pins the
   extension across a concurrent carrier-thread exit; the shared process gate,
   rather than the carrier thread state, decides whether the work is still
   relevant.

The previous `SpinNoPreempt` writer gate and `Complete/Retry` Starry accounting
protocol are obsolete. They serialized a scheduler switch on the task owner
with a deferred worker on an unrelated CPU, but that serialization preserved
the wrong ownership model. It also put a lock acquisition plus ordinary
preemption-guard bookkeeping on every user/kernel transition. The corrected
model prevents the global worker from writing task vtime at all. Generic
ax-task scheduler-tick work retains `Retry` for other task-context consumers,
but Starry CPU accounting now always completes from a read-side snapshot.

The older callback had two separate defects. First, the typed IRQ hook still
allowed arbitrary OS code to execute in hard IRQ and raced task-context
writers. Second, the original deferred callback called
`ProcessData::cpu_time_snapshot()`, copied the whole thread-ID vector, and
performed one global task-registry lookup per sibling. A process with N running
threads could publish N callbacks per physical tick and each callback performed
O(N) work, producing O(N²) scans plus repeated process and registry lock
contention. Small timer-family correctness cases did not exercise either the
cross-CPU writer conflict or this scaling boundary.

The lowest-layer regressions were deliberately observed red before the fixes:

- `scheduler_tick_os_work_is_deferred_with_latest_irq_timestamp` observed the
  old OS accounting hook execute three times in hard IRQ instead of zero;
- `scheduler_tick_sampling_is_read_only` produced one deterministic failure in
  the 390-case x86_64 axtest run because the old callback tried to acquire the
  target writer gate and mutated committed task vtime. The fixed callback
  leaves both the sequence and committed counters unchanged.
- `scheduler_tick_retry_republishes_one_bounded_task_work_attempt` observed
  zero events on the required second worker pass before retry ownership was
  implemented;
- `scheduler_tick_retry_defers_until_a_later_service_pass` observed 64 callback
  attempts in one 64-item batch before the worker began suppressing same-pass
  retries;
- `coalesced_scheduler_tick_publishes_its_timestamp_before_claim` found a weak
  memory execution where the consumer observed the old timestamp while the
  producer's plain pending-generation load suppressed the replacement
  publication;
- the pre-existing gate, wrapper-composition, and carrier-exit tests retain
  coverage for stale epochs, lost runtime capabilities, and extension
  reclamation.

The same tests are green after hard-IRQ callback removal, owner-only vtime,
read-side high-water publication, and explicit runtime composition.
`newer_tick_owns_delivery_when_it_races_a_callback_retry` proves that a new IRQ
and a retry cannot both publish the intrusive node, while
`scheduler_tick_retry_cannot_cross_a_gate_disable_epoch` proves that retry
cannot resurrect disabled work. The corresponding loom model checks the
single delivery owner, and the coalescing model checks the timestamp's
publish-before-claim edge. The complete ax-task suite has 220 unit tests, all
integration/doc tests, and 20 loom models; the ax-runtime host IRQ/multitask
suite has 62 tests. The post-fix x86_64 Starry axtest run passed all 390 cases
with `AXTEST_SUITE_OK`; all 25 `starry-kernel` clippy configurations pass.
These results are not inferred from host linking, whose
bare-metal symbols are intentionally supplied only by the xtask build.

## Current-head reschedule ownership closure

The runtime timer and IPI adapters used to complete owner-local scheduler
state transitions themselves. The timer adapter interpreted four
`TaskClockEventOutcome` fields and called `CpuRemote::request_reschedule()`;
the IPI adapter acknowledged the ax-task epoch and then made the same direct
write. That public method only set sticky owner state. It did not ring a remote
doorbell, so a caller holding another CPU's public `CpuRemote` could create a
permanently sleeping reschedule request.

Linux v7.1 keeps this distinction inside `__resched_curr()`:

- for the current runqueue it sets the task and preemption flags directly;
- for a remote runqueue it first publishes need-resched and then calls
  `smp_send_reschedule()` unless the target is polling; and
- `sched_tick()` owns the current runqueue lock while scheduling-class tick
  logic decides whether to call that boundary.

TGOSKits now applies the same ownership rule. The ax-task clockevent facade
publishes owner preemption before returning when class accounting, a task
deadline, bounded backlog, or a scheduler deadline requires a safe point.
Scheduler IPI acknowledgement releases the delivered epoch and promotes any
remaining owner work to preemption in the same core facade. ax-runtime now
transports only the physical clockevent update and IPI edge; it no longer
interprets scheduler policy or writes `CpuRemote`. The owner-only
`request_reschedule()` and `acknowledge_scheduler_ipi()` methods are
crate-private, while remote wake, migration, and policy producers continue to
use the payload-before-sticky-before-IPI publication state machine.

Both ownership gaps were captured before the fix:

- a due deferred scheduler deadline returned `pending` without publishing
  current-CPU preemption; and
- acknowledging a scheduler IPI with sticky work left
  `preempt_requested == false` unless ax-runtime performed a second write.

The two deterministic tests are green after the ownership transfer. The
complete ax-task suite now passes 190 unit tests, all integration/doc tests,
and 17 loom models. The ax-runtime IRQ/multitask unit configuration passes all
50 tests, and the full ax-task plus 25-combination ax-runtime feature-clippy
matrices pass with warnings denied. The timer-expiry facade regression also
asserts that the resulting scheduler decision preserves the current execution
context when no runnable peer exists.

## Current-head wake-consumption boundary closure

The scheduler previously exposed `TaskSystem::consume_wake()` as a public
shortcut. It resolved a generation-bearing wake handle through the global
thread registry and changed the target lifecycle under the per-thread
scheduler lock. Production IRQ wakeups did not use it: they already published
an intrusive record into the owner CPU inbox and rang the scheduler doorbell.
The shortcut nevertheless made a global scheduler lock look like an
IRQ-capable wake boundary and let an external caller bypass owner-CPU enqueue,
placement, and bounded-drain ordering.

Linux v7.1 keeps wake activation within the scheduler's ownership protocol:

- `try_to_wake_up()` serializes task state with `p->pi_lock` and publishes
  `TASK_WAKING`;
- `ttwu_queue_wakelist()` publishes into the target runqueue wake list and
  sends the target CPU an IPI when remote activation should be deferred; and
- the target CPU performs activation under its runqueue ownership instead of
  offering an unrelated public routine that consumes only half of the wake
  transaction.

TGOSKits now has the same single path. A producer can only call
`ThreadWakeHandle::wake()`. The owner CPU's bounded
`drain_remote_wakes()` consumes the transferred reference, validates the
generation-bearing identity, performs the lifecycle transition, enqueues the
thread if needed, and preserves the executor's lost-wakeup predicate. Direct
wake consumption is private to that owner drain.

A compile-fail API regression was first observed red while the shortcut
remained public. The executor regression was then changed from manually
calling the shortcut to draining the real owner inbox and asserting that
exactly one wake record was consumed while the park predicate remained
abortable. Both tests are green after removing the bypass.

## Current-head switch-tail retry closure

The runtime switch-tail contract requires a failed architecture handoff to
leave the outgoing context physically bound and unreclaimable so the
scheduler can retry. ax-runtime previously removed its staged
`RuntimeSwitchTail` before calling `PreviousThreadBinding::finish()`, while
the cpu-local API consumed that binding token by value. A validation failure
therefore panicked after permanently discarding the only exact binding epoch;
the scheduler's existing retry and `ThreadBusy` protection could not run.

Linux v7.1 keeps the corresponding ordering inside `finish_task_switch()`:
it observes the outgoing state before `finish_task()` performs the release
store that clears `prev->on_cpu`, and only then permits task-stack or task
object release. The incoming continuation is the sole owner of this tail;
failure must not be reported after silently consuming its ownership proof.

The cpu-local finish operation now borrows its move-only binding token
mutably. ax-runtime retains the staged tail while validation or epoch
withdrawal fails, returns a typed runtime failure to ax-task, and removes the
slot only after the exact previous binding is successfully withdrawn. The
core can consequently keep the outgoing placement and resources
unreclaimable and retry the same transaction.

A host CPU-local regression constructs a real prepared switch, deliberately
pairs the tail with the wrong previous header, and was first observed to
panic in the old implementation. It now receives `InvalidHandle`, verifies
that the binding transaction remains staged, corrects the injected header,
and completes the same tail successfully.

## Current-head preemption guard fast-path boundary

The runtime lock hook used to reconstruct a full `CpuPin` and
`ExclusiveCpu` merely to read or update the scheduler's current per-CPU guard
word. The preemption-exit path also queried the generic IRQ-context helper
before it had proved that the exiting guard was the outermost depth. That
helper creates its own `NoPreempt` guard, so moving the query ahead of the
depth check made a nested guard drop re-enter itself until x86_64 raised a
double fault.

Linux v7.1 keeps this path deliberately direct:

- `preempt_enable()` calls `preempt_count_dec_and_test()` and reaches
  `__preempt_schedule()` only when the final nesting level exposes
  `need_resched`;
- `hardirq_count()` and `in_hardirq()` read the already-current task's
  preemption word instead of acquiring another preemption guard; and
- `preempt_schedule_common()` retains the preemption-disabled ownership proof
  across `__schedule()` and uses the no-reschedule decrement on its tail.

TGOSKits retains a narrow scheduler-only current-thread register read for
per-CPU objects whose address still depends on the running task's validated
binding. Ordinary preemption no longer uses that path. Its depth and inverted
`need_resched` bit live in the fixed `CpuRuntimeAnchor`, so x86_64 changes the
word directly through GS and the other architectures recover only the fixed
CPU base. `CurrentThreadHeader` again contains task identity, binding epoch and
architecture task state only; a context switch neither copies nor resets the
CPU guard word.

The final guard exit follows the Linux `preempt_count_dec_and_test()` boundary:
`FinalPending` itself is the reschedule observation. ax-runtime disables local
IRQs, validates hard-IRQ and scheduler-baton constraints, and converts that
exact retained depth into the scheduler baton without querying `CpuRemote`
again. IRQ-exit paths may still reconcile the remote sticky request into the
local word, but ordinary guard release never turns a current-CPU scalar into a
registry or current-thread lookup.

The deterministic host regression first observed one CPU-base register read
plus one current-thread register read for a scheduler-current per-CPU access.
It now observes only the current-thread read. A second regression proves that
dropping a nested preemption guard invokes neither the IRQ-context nor the
reschedule query; the buggy ordering failed by entering both callbacks. The
new fixed-anchor regression additionally observed two current-thread reads for
one ordinary guard pair on the intermediate implementation and now observes
zero. A final-pending regression proved that the old path re-read the
reschedule endpoint after the local word had already selected scheduling. The
corrected path performs no such callback. Four-CPU `task-yield` runs pass on
x86_64, AArch64, RISC-V and LoongArch64, and x86_64 `task-irq` covers the
IRQ-to-baton path.

A DHCP-to-shell qperf window confirms that the old CPU-local symbol-offset
hotspot is gone, but also keeps the remaining performance finding explicit:
the current branch executes 13,113 sampled instruction intervals in 13.43
seconds versus dev's 4,606 intervals in 4.99 seconds. The remaining excess is
concentrated in owner scheduling, remote/policy inbox drains, and preemption
safe points. It is not evidence against the direct register boundary; it is a
separate scheduler-work amplification finding that must be resolved before
the performance milestone is complete.

## Current-head empty owner-inbox fast path

The owner safe point previously entered both scheduler inbox drains even when
neither inbox contained a publication. An empty drain still acquired the
single-consumer gate, exchanged the detached-list head, crossed the epoch
queue's grace observation, and released the gate. This work was repeated for
every scheduler-only sticky request.

Linux v7.1 uses the cheaper predicate at the equivalent boundary:
`sched_ttwu_pending()` returns immediately when its detached llist argument is
empty. When work exists, it still owns the runqueue transaction, updates the
runqueue clock, and activates every detached wake before clearing
`ttwu_pending`. ax-task now applies only that empty-list optimization. The
owner samples the wake, policy, and reclaim inbox heads, enters a bounded drain
only for a present wake or policy publication, and keeps the existing
claim-before-drain acknowledgement for policy-only or reclaim-only work.
Publication racing the snapshot remains visible in the final inbox recheck and
is carried by a fresh or retained scheduler doorbell.

The deterministic regression first observed one wake-inbox drain and one
policy-inbox drain for a synthetic scheduler-only request. Both counts are now
unchanged. A companion policy-only regression proves that the policy inbox is
still drained exactly once, the empty wake inbox is not entered, and the
delivery is not stranded behind its consumed IPI epoch. The complete ax-task
suite passes 191 unit tests, every integration and documentation test, and 20
loom models; the package clippy check also passes with warnings denied.

This optimization deliberately does not skip dispatch commit, Deadline
service, or dispatch reinstallation merely because the final selection keeps
the same thread. `CurrentDispatch` owns running runtime, PI/CBS baton, policy
generation, and Deadline entity state. Linux `__schedule()` likewise executes
`hrtick_schedule_enter()`, `update_rq_clock()`, and `pick_next_task()` before
testing `prev != next`; only the physical context-switch tail is conditional.
Removing those state transitions would trade a measured performance problem
for incorrect accounting and timer semantics.

The post-change DHCP-to-shell qperf window remains a negative performance
result: 13,980 sampled instruction intervals in 14.33 seconds versus dev's
4,606 intervals in 4.99 seconds. `scheduler_wait_preempt` still accounts for
46.27% of the candidate samples. The empty-inbox fast path is therefore a
correct bounded-work reduction, not closure of the end-to-end regression; the
remaining scheduler-work amplification stays open.

## Current-head direct current-CPU reschedule endpoint

The remaining preemption-exit path still converted the already-current CPU
into a logical identifier and then resolved that identifier through the
generic remote-CPU registry. This happened after every outermost preemption
guard release, even though the caller already held migration exclusion and
needed only the current CPU's `need_resched` bit.

Linux v7.1 does not turn this local observation into a remote runqueue lookup.
The architecture `preempt_count` word embeds the inverted reschedule flag,
`preempt_count_dec_and_test()` reads that current-CPU word directly, and
`preempt_enable()` invokes the scheduler only for the outermost transition.
Explicit CPU lookup remains a remote-operation boundary, for example
`cpu_rq(cpu)` in wake and migration paths.

ax-runtime now caches each CPU's Arc-backed `CpuRemote` endpoint in its
architecture-owned per-CPU area before online publication. The
`TaskRuntime::current_cpu_remote_handle()` capability reaches that immutable
endpoint through the scheduler-current thread register. It neither asks for a
logical CPU ID nor enters the task-system registry. The endpoint allocation
remains stable until shutdown; online, draining and offline are states of that
same allocation, and the ax-task facade rejects it whenever it is not online.
Generic `cpu_remote_handle(cpu)` remains the only path for actual remote
producers.

The deterministic regression was first observed with current-handle read
counts `(0, 1)`: the pinned reschedule query performed no owner-handle read but
did perform one generic remote lookup. It now observes `(0, 0)`. A CPU
lifecycle regression additionally retains the bootstrap-time handle across
offline and re-online transitions, verifies that its address never changes,
and observes only the endpoint's publication state changing.

The earlier qperf slowdown was then checked without the instruction plugin,
while retaining the exact `qperf` Cargo feature set, NVMe root filesystem and
virtio network configuration. The current branch reached DHCP at
3.328285 seconds and the latest dev baseline at 3.321183 seconds, a
7.1-millisecond difference. This does not support a material real-time
performance regression. The much larger plugin-window difference is therefore
kept as evidence of instrumentation-sensitive scheduler work, not used as an
end-to-end wall-time claim. A later plugin run that missed both workload
markers was rejected rather than compared with valid DHCP-to-shell windows.

The x86_64 full-system workload now provides a stronger counterexample to that
boot-only result. Against the same latest-dev CI lane, 388 common tests show
large workload-specific regressions: ext4 inode creation takes 159 seconds
instead of 76, page-cache population 112 instead of 55, SMP futex wake-op 46
instead of 4, and the AVX forced-context-switch test 44 instead of 2. Network
dataplane remains 63 seconds in both runs, so the result is not a uniform QEMU
host slowdown. The common signature is high-frequency wake, yield, or
multi-CPU runnable work. This supersedes DHCP-to-shell as the scheduler
performance acceptance signal; the placement and owner-scheduling paths
remain open until those common subcases are near the dev baseline.

The first scheduler hot-path correction separates cross-CPU serialization from
hard-IRQ exclusion. A deterministic yield regression observed eight nested
runtime IRQ-guard entries while the owner already held its scheduler baton.
The task registry, thread scheduler state, wait queues, and kernel-thread entry
are not acquired by the hard-IRQ deadline or IPI paths, so they now use a
runtime-provided preempt-only ticket lock. Hard IRQ publication retains the
bounded `IrqScope` protocol. The same yield performs zero nested IRQ entries
while retaining balanced preemption depth, matching PREEMPT_RT's distinction
between task-only scheduler metadata and IRQ-owned publication. Initial Fair
placement also accounts for the running dispatch and in-flight migrations so
new work does not remain concentrated on the spawning CPU.

## Current-head typed Deadline event closure

The earlier bounded `deadline_members` scan fixed a finite-pass livelock but
still made every ordinary scheduler entry inspect unrelated reservations.
With two future Deadline jobs and a batch limit of one,
`schedule_if_requested()` returned `OwnerWorkPending` even though no event was
due. The deterministic regression failed before this phase and is now
`Quiescent` without touching either reservation. `deadline_members` remains
only the stable GRUB ownership and CPU-offline registry; it is no longer a
timer-dispatch structure.

Linux v7.1 gives each `sched_dl_entity` separate `dl_timer` and
`inactive_timer` ownership. `start_dl_timer()` arms the CBS boundary under the
runqueue transaction, `dl_task_timer()` validates the task and current
runqueue before replenishment, and `inactive_task_timer()` performs the
zero-lag bandwidth transition. Cancellation removes the physical timer and
the task reference is released only after callback ownership ends.

ax-task now models the same ownership without arbitrary callbacks:

- every thread embeds independent park, CBS, and zero-lag timer nodes;
- the fixed heap assigns each physical node a process-lifetime identity and
  combines it with the node-local arm generation in the value-owned token;
  replacement is keyed by node identity, so distinct nodes cannot cancel one
  another even when they carry the same `ThreadId` and typed class;
- each class has its own preallocated capacity, and rearm physically removes
  only the matching physical node before publishing a new generation;
- `ThreadSchedState` move-owns the live CBS and zero-lag registrations;
- enqueue, throttle, yield, block, wake, migration, policy change, and detach
  refresh or cancel the exact registration in the owner-CPU transaction; and
- hard IRQ copies `ThreadId`, token, deadline, and kind into bounded storage.
  The safe point accepts an event only when all fields still match the
  move-owned registration.

CBS expiry updates only the named entity, while zero-lag expiry removes only
that reservation's running bandwidth. A remote PI borrower cannot mutate the
donor heap. Returning its CBS baton publishes a retained, generation-bearing
`DeadlineRefresh` message to the donor CPU; the owner then arms the current
CBS state. This replaces the old implicit “kick and eventually find the donor
by scanning every member” protocol.

The physical scheduler deadline no longer carries a second
`deferred_scheduler_deadline_ns` cache. Runqueue/current scheduling boundaries
and the typed task heap are the two explicit inputs to the runtime
clockevent. A bounded pass retains work only when another expired value
remains; future reservations do not manufacture scheduler work.

The scheduler's late doorbell claim also follows Linux `__schedule()` ordering.
Linux updates the runqueue clock and selects the next task before clearing
`TIF_NEED_RESCHED`. ax-task now settles dispatch and typed deadline work before
the final claim, so an RR quantum observed during the same forced yield does
not leak a stale reschedule into the selected task. The independent
deterministic model was updated to that Linux ordering, and a dedicated RR
regression fixes the previous stale request explicitly.

Coverage includes independent typed-slot capacity and rearm, two same-thread
same-class nodes retaining independent registration identity, stale-token
cancellation, bounded due-event continuation, no work for future members,
zero-lag before later replenishment, exact CBS replenish/miss behavior, and
remote PI baton return. A dedicated coalescing test proves that an older
retained `DeadlineRefresh` reconciles the latest CBS generation instead of
replaying its old state. The hard-IRQ allocation contract remains zero
allocation, zero free, and zero callback.

## Current-head PI cycle policy boundary

The reusable PI graph exposed `TaskError::PiCycle`, but `pi_wait_start()`
intercepted that result and invoked the runtime fatal hook before publishing
the new waiter edge. This mixed two Linux policies at the wrong layer. Linux
ordinary `rt_mutex_lock()` treats an actual kernel mutex deadlock as a fatal
programming error, while the PI-futex/proxy registration path performs a full
chain walk and returns `-EDEADLK` so its caller can apply ABI policy.

`ax-task::pi_wait_start()` is the reusable graph/proxy boundary because it
accepts explicit waiter and owner identities and already returns a typed
`Result`. It now returns `TaskError::PiCycle` before starting the waiter
generation or changing the donation graph. `ax-sync::RawMutex` remains free to
turn that error into a kernel programming failure through its infallible lock
API. The deterministic regression first observed the old fatal hook, then
proved the typed error leaves the pre-existing edge cancellable.

## Current-head Starry registry lock boundary

The Starry task, PID identity, process-group, and session registries were
task-context data structures but used `ax_kspin::SpinRwLock`, whose `NoOp`
guard neither disables preemption nor provides priority inheritance. Their
write critical sections insert into or remove from `BTreeMap` and `WeakMap`,
and read-side operations can allocate result vectors or upgrade scheduler
handles. A lock holder could therefore be preempted on its CPU while another
task spun on the same raw word, and a higher-priority waiter could not donate
priority to the owner.

Linux v7.1 declares `tasklist_lock` as an ordinary `rwlock_t` in
`kernel/fork.c:158`, but under `CONFIG_PREEMPT_RT` the type is replaced by an
`rwbase_rt`-backed lock (`include/linux/rwlock_types.h:54-76`).
`rt_read_lock()` and `rt_write_lock()` enter the rtmutex-based wait path rather
than spinning (`kernel/locking/spinlock_rt.c:229-247`).

TGOSKits has no PI read/write lock, so the four registries now use exclusive
`PiMutex` ownership. This trades parallel read access for a bounded,
priority-inheriting sleep path and keeps allocation, weak-handle upgrade, and
table traversal out of a raw critical section. The per-identity lifecycle word
remains a short `SpinNoIrq` transaction because it is bounded, allocation-free,
and may nest only after the registry lock. The explicit lock order is registry
then identity state; identity-state code does not acquire a registry lock.

The deterministic lock-boundary regression first failed against all four raw
registry declarations and then passed after the migration. Starry's complete
24-configuration feature clippy validates normal, `axtest`, SMP, and
architecture builds with the new sleepable boundary.

## Current-head perf control and IRQ-output boundary

The generic Starry `PerfEvent` wrapper serialized every dynamic event
operation with `SpinNoPreempt<Box<dyn PerfEventOps>>`. That lock was also held
across `PERF_EVENT_IOC_SET_BPF`, event enable/disable/read callbacks, and
`mmap(perf_fd)`. Those callbacks can construct a BPF VM, register a
tracepoint, allocate and zero contiguous ring pages, or wait for a CPU-owned
PMU worker. The wrapper therefore made task-context allocation and sleeping
control operations execute with preemption disabled.

Linux v7.1 keeps these ownership classes separate. `_perf_ioctl()` resolves a
BPF fd before installing the program (`kernel/events/core.c:6598-6669`), and
the context operation is serialized by the sleepable perf context mutex
(`kernel/events/core.c:11657-11717`). `perf_mmap()` performs ring creation and
mapping under the event's `mmap_mutex` (`kernel/events/core.c:7420-7520`).
Only the bounded buffer publication/write state remains reachable from
IRQ/NMI-side producers.

Starry now uses a `PiMutex<Box<dyn PerfEventOps>>` for the generic task-control
plane. Software BPF events additionally publish a dedicated `BpfPerfOutput`
capability containing only a `SpinNoPreempt` ring state and the IRQ-safe worker
notification. `bpf_perf_event_output` resolves that capability directly and
never acquires the sleepable control mutex. Ring page allocation, zeroing, and
`Arc` construction happen before the raw gate; the gate only revalidates
single-mmap ownership, installs the already-allocated ring, writes bounded
records, and observes enabled state. A concurrent second mmap may allocate
speculatively but revalidates under the raw state and drops its unused pages
after releasing the gate.

The deterministic x86_64 QEMU regression invokes a real scheduler yield from a
mock `PerfEventOps::enable` callback. It failed under the old wrapper because
the scheduler rejected the `SpinNoPreempt` context, and passes after the
control-plane split. The complete Starry axtest suite passes 389/389,
Starry's 24 feature/target clippy configurations pass, and the 186-package
synchronization-boundary lint remains green.

## Current-head tracepoint callback publication boundary

The Starry tracepoint registry previously stored each mutable
`ExtTracePoint` behind `SpinNoPreempt` and held that gate while invoking every
trace, perf, and raw-BPF callback. A callback that yielded, allocated through
a sleepable path, or changed tracepoint control state therefore ran in an
atomic context or recursively acquired the same raw gate. The upstream
`ktracepoint` registration path also enabled and disabled `static-keys`
directly, so constructing an unpublished replacement could change the global
fast path before the replacement state was visible. Runtime static-key text
patching is not a suitable SMP publication primitive here; Starry retains it
only for the pre-existing dynamic-debug subsystem.

Linux v7.1 publishes complete probe arrays with `rcu_assign_pointer()` before
enabling the static branch (`kernel/tracepoint.c:286-352`). Final removal
disables the branch before publishing the empty pointer
(`kernel/tracepoint.c:361-423`). Old probe arrays are released through
`call_srcu()` or `call_rcu_tasks_trace()` rather than while the trace callback
is returning (`kernel/tracepoint.c:115-129`). A caller that must destroy module
storage separately invokes `tracepoint_synchronize_unregister()`
(`include/linux/tracepoint.h:103-123`).

The workspace-local `ktracepoint` now separates immutable event metadata from
runtime callback generations. `ExtTracePoint::register()` and
`unregister()` only edit an unpublished value. Starry serializes writers with
a `PiMutex`, clones a complete replacement, and uses `SpinNoPreempt` only to
acquire or replace one `Arc` snapshot. The shared event descriptor has an
Acquire/Release atomic callback gate: the writer publishes a non-empty
snapshot before opening it and closes it before retiring the last callback.
Scheduler and IRQ readers increment one of two generation counters while
acquiring the snapshot, release the raw gate before dispatch, and retain the
snapshot through the callback.

Retired snapshots whose readers have not left are queued to a dedicated
task-context reclaimer. The last reader only decrements its epoch counter and
publishes an IRQ-safe sticky notification, so neither an IRQ nor the scheduler
path performs the final callback allocation drop. This is the Arc/epoch
equivalent of Linux's deferred SRCU release. It intentionally permits an
already-started callback to finish after unregister returns while keeping all
callback data alive; callers do not need a separate module-storage grace
operation because the retired snapshot owns that data.

The deterministic x86_64 regression yielded from the tracepoint read closure.
It failed on the old implementation with exactly one failure in the 389-case
suite (`388/389`) because the raw registry gate still disabled preemption. The
same test also exercises read-callback-to-update re-entry and deferred
retirement; the new implementation passes the complete `389/389` suite. The
local crate additionally tests that callback-list edits have no gate side
effects until the owner publishes them, and its host example executes both
cooked and raw callbacks through the new atomic gate.

## Current-head Starry clone activation boundary

Starry clone previously inserted the child into its public task and process
identity registries and only then called the fallible scheduler
`make_ready()`/`place_ready()` path. CPU placement, Deadline timer capacity, or
runtime clockevent failure could therefore expose a live PID/TID to concurrent
lookup and later remove it while returning a clone error. The PID publication
mutex serialized clone against namespace shutdown, but ordinary task, signal,
ptrace, pidfd, and process readers do not acquire that mutex, so it did not
make the visible-then-rollback window atomic.

Linux v7.1 performs cgroup and scheduler preparation before task visibility
(`kernel/fork.c:2368-2383`). After the last PID-namespace and fatal-signal
checks, `copy_process()` states “No more failure paths after this point”
(`kernel/fork.c:2442-2453`), publishes PID and relationship state under
`tasklist_lock` (`kernel/fork.c:2464-2514`), and later invokes the infallible
`wake_up_new_task()` (`kernel/fork.c:2722-2753`).

ax-runtime now exposes the same two-phase boundary:

- `PreparedThread::stage()` performs `make_ready()` and runqueue placement,
  returning every recoverable error before OS identity publication;
- the staged context may be selected, but its runtime trampoline waits on a
  private atomic start gate and cannot execute the caller-owned entry;
- dropping a staged token publishes `Aborted` and wakes the trampoline, which
  exits without consuming the caller entry; and
- `StagedThread::activate()` publishes `Active` and wakes the trampoline
  without a fallible operation.

Starry stages the child before taking its publication mutex, then publishes
namespace PID mappings, cgroup membership, process relationships, TASK_TABLE,
and PROCESS_TABLE while the entry gate remains closed. It commits every
rollback token, releases the publication mutex, and only then activates the
thread. Scheduler placement no longer runs under the broad Starry publication
lock, and no recoverable failure remains after public identity commit.

The deterministic x86_64 QEMU regression stages a real runtime thread, yields
four times, and requires that its entry remain unexecuted until activation.
With the old scheduler semantics it failed immediately; the same test passes
with the runtime gate. A second regression drops a staged token, waits for its
logical scheduler exit through an independently retained handle, and proves
that abort never consumes the caller entry.

## Current-head futex wake and deadline safe-point hot paths

Starry previously called `yield_current_cpu()` after every successful
`FUTEX_WAKE`, and after requeue or wake-op whenever any waiter was woken.
Linux v7.1 instead builds the wake queue while holding the futex hash-bucket
lock and invokes `wake_up_q()` after unlocking
(`kernel/futex/waitwake.c:155-199`). The woken task's scheduler publication
sets the owning runqueue's reschedule state; the syscall does not add an
unconditional second scheduling point. Starry now follows that ownership
boundary for wake, requeue, and wake-op. Fault and atomic-retry paths retain
their explicit retry yield.

The old ax-task scheduler facade also entered `expire_task_deadlines()` on
every schedule, yield, park, affinity, and exit safe point, then entered the
same expiry engine again inside `TaskSystem`. Linux runs hrtimer queues from
the clockevent interrupt (`kernel/time/hrtimer.c:2083-2138`); scheduler entry
consumes already-published work and uses the timer state only as a lost-edge
recovery predicate.

ax-task now checks the sticky deadline-work bit and the owner heap's cached
minimum before entering expiry. Empty and future-only queues perform no expiry
pass. The clockevent remains the normal expiry owner; an overdue cached head
allows exactly one bounded recovery pass at a scheduler safe point. A full IRQ
buffer is drained before that pass so each safe point still makes progress.
Typed `DeadlineEntry::{Service, AlreadyServiced}` entry paths preserve direct
`TaskSystem` runtimes while preventing the ax-runtime facade from repeating
the same pass.

The deadline service returns its coherent entry clock sample when no work was
processed and resamples only after actual timer work. Thus the ordinary
schedule/yield path performs one monotonic read instead of two without using a
stale timestamp after callbacks. Deterministic regressions proved the prior
futex yield, idle expiry pass, and double clock read before their respective
fixes.

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
