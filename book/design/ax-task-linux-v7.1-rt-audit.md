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
| Scheduler placement | `components/ax-task/src/system`, scheduler policy and thread state | `kernel/sched/core.c` | A thread has one checked placement; migration and switch-tail publication cannot expose two owners | `queued_cpu`, `running_cpu`, `on_cpu`, and `migration_target` are independent fields with many hand-written updates |
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
state sources. The remaining PI work is to replace whole-registry donor scans
with an owner-indexed waiter structure and to finish the scope-local and
IRQ-visible waiter lifetime audits.

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
