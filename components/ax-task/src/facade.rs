//! Runtime-backed scheduler facade for crates below `ax-runtime`.

use alloc::{boxed::Box, string::String};
use core::{marker::PhantomData, mem::align_of, ops::Deref, pin::Pin, ptr};

use crate::{
    CpuId, CpuLocal, CpuLocalOwnerBorrow, CpuRemote, CpuSet, IrqRegisterResult, IrqWaitCell,
    IrqWaitRegistration, Nice, ParkCommit, ParkPrepare, PiLockId, PiWaitToken, RtPriority,
    ScheduleDecision, SchedulePolicy, SchedulerOutcome, TaskError, TaskSystem, ThreadBuilder,
    ThreadExtensionLease, ThreadHandle, ThreadId, ThreadRuntimeSnapshot, ThreadState,
    ThreadWakeHandle, WaitQueue, WakeResult,
    inbox::PublishResult,
    reclaim::DeferredReclaimNode,
    runtime::{
        AddressSpaceHandle, IrqGuardToken, RuntimeCpuId, RuntimeScheduleOrigin,
        RuntimeSchedulerEntry, RuntimeSchedulerReturn, RuntimeStatus, SchedSwitchRecord,
        task_runtime,
    },
    timer::{ExpiredTaskDeadline, TaskDeadlineKind},
};

/// Returns a strong handle for the calling scheduler thread.
///
/// # Errors
///
/// Returns [`TaskError::NotInitialized`] before runtime CPU publication,
/// [`TaskError::CpuOwnerBorrowed`] for a reentrant owner query, or
/// [`TaskError::NoRunnableThread`] before a current thread is installed.
pub fn current_thread_handle() -> Result<ThreadHandle, TaskError> {
    runtime_current_cpu()?.current_thread_handle()
}

/// Returns the generation-bearing identity of the calling scheduler thread.
pub fn current_thread_id() -> Result<ThreadId, TaskError> {
    current_thread_id_from_cpu()
}

fn current_thread_id_from_cpu() -> Result<ThreadId, TaskError> {
    // RuntimeCurrentCpu retains the IRQ pin across handle validation and the
    // owner-state read. The copied generation-bearing ID remains valid after
    // that pin is released.
    runtime_current_cpu()?
        .current()
        .ok_or(TaskError::NoRunnableThread)
}

/// Validates that the caller may publish a waiter or block its current thread.
///
/// Sleeping synchronization primitives should call this before changing any
/// waiter, owner, donation, or thread-lifecycle state.
pub fn validate_blocking_context() -> Result<(), TaskError> {
    acquire_blocking_permit().map(|_| ())
}

/// One validated opportunity to publish a blocking handshake.
pub(crate) struct BlockingPermit {
    _not_send: PhantomData<*mut ()>,
}

pub(crate) fn acquire_blocking_permit() -> Result<BlockingPermit, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Block)?;
    Ok(BlockingPermit {
        _not_send: PhantomData,
    })
}

/// Returns the opaque extension of the calling scheduler thread.
///
/// Runtime entry trampolines use the callback-table address as a type identity
/// before recovering an OS-owned closure or process object from `data`.
pub fn current_thread_extension() -> Result<Option<ThreadExtensionLease>, TaskError> {
    let handle = current_thread_handle()?;
    Ok(handle
        .extension_view()
        .map(|view| ThreadExtensionLease::new(view, handle)))
}

/// Replaces the current thread's scheduler-visible address-space token.
///
/// The runtime must update its architecture context and hardware page table in
/// the same outer IRQ-off transaction after this function returns. The old
/// token is returned for runtime bookkeeping; ax-task owns no address-space
/// destruction right.
pub fn replace_current_address_space(
    address_space: AddressSpaceHandle,
) -> Result<AddressSpaceHandle, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.replace_current_address_space(cpu.as_mut(), address_space)
}

/// Looks up a generation-valid thread through the runtime-owned task system.
pub fn thread_handle(thread: ThreadId) -> Result<ThreadHandle, TaskError> {
    runtime_task_system()?.thread_handle(thread)
}

/// Returns a thread scheduling policy snapshot.
pub fn thread_policy(thread: ThreadId) -> Result<SchedulePolicy, TaskError> {
    runtime_task_system()?.thread_policy(thread)
}

/// Returns a cumulative charged-runtime snapshot for a live thread.
pub fn thread_runtime(thread: ThreadId) -> Result<ThreadRuntimeSnapshot, TaskError> {
    runtime_task_system()?.thread_runtime(thread, task_runtime::monotonic_ns())
}

/// Returns cumulative non-idle runtime charged by one online CPU.
pub fn cpu_busy_runtime_ns(cpu: CpuId) -> Result<u64, TaskError> {
    runtime_task_system()?.cpu_busy_runtime_ns(cpu)
}

/// Updates a non-queued thread scheduling policy.
pub fn set_thread_policy(thread: ThreadId, policy: SchedulePolicy) -> Result<(), TaskError> {
    runtime_task_system()?.set_thread_policy(thread, policy)
}

/// Returns a copy of a thread's CPU affinity.
pub fn thread_affinity(thread: ThreadId) -> Result<CpuSet, TaskError> {
    runtime_task_system()?.thread_affinity(thread)
}

/// Updates a thread CPU affinity after Deadline root-domain validation.
pub fn set_thread_affinity(thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
    runtime_task_system()?.set_affinity(thread, affinity)
}

/// Updates the calling thread's affinity and completes a required migration.
///
/// A successful return guarantees that the caller is executing on a CPU in
/// the new mask. Generic remote-thread affinity updates remain asynchronous and
/// are completed by the remote owner's next scheduler safe point.
pub fn set_current_thread_affinity(affinity: CpuSet) -> Result<(), TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Yield)?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let (decision, now_ns) = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        let must_migrate = system.set_current_affinity(cpu.as_mut(), affinity)?;
        if !must_migrate {
            return Ok(());
        }

        // The new mask is now visible and excludes this CPU. Keep the scheduler
        // baton and raw IRQ mask continuously owned until this context has moved;
        // exposing an IRQ-enabled validation window here could let IRQ-return
        // scheduling migrate the caller between publishing the mask and yielding.
        let thread = cpu.current().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0020, 0);
        });
        let decision = system
            .yield_current(cpu.as_mut(), now_ns)
            .unwrap_or_else(|_| {
                // Affinity publication cannot be rolled back safely after another CPU
                // may have observed the migration target. Scheduler commit failures are
                // therefore runtime invariants, like failures after exit publication.
                task_runtime::fatal_invariant(0x4558_0021, thread.as_u64() as usize);
            });
        (decision, now_ns)
    };
    execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    Ok(())
}

/// Returns the configured RR quantum in nanoseconds.
pub fn thread_round_robin_interval_ns(thread: ThreadId) -> Result<u64, TaskError> {
    runtime_task_system()?.round_robin_interval_ns(thread)
}

/// Returns an RT priority, or `None` for fair/Deadline policies.
pub fn thread_rt_priority(thread: ThreadId) -> Result<Option<RtPriority>, TaskError> {
    Ok(match thread_policy(thread)? {
        SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
            Some(priority)
        }
        _ => None,
    })
}

/// Returns a nice value, or `None` for RT/Deadline policies.
pub fn thread_nice(thread: ThreadId) -> Result<Option<Nice>, TaskError> {
    Ok(match thread_policy(thread)? {
        SchedulePolicy::Fair { nice, .. } => Some(nice),
        _ => None,
    })
}

/// Tests the sticky reschedule state of the calling CPU.
pub fn current_cpu_needs_resched() -> Result<bool, TaskError> {
    Ok(runtime_current_cpu()?.needs_reschedule())
}

/// Acknowledges the current CPU's coalesced scheduler IPI epoch.
pub fn acknowledge_current_scheduler_ipi() -> Result<(), TaskError> {
    runtime_current_cpu()?.acknowledge_scheduler_ipi();
    Ok(())
}

/// Executes one lossless idle publication/recheck/WFI iteration.
pub fn idle_current_cpu_once() -> Result<(), TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    let (owner, may_wait) = {
        let cpu = runtime_current_cpu()?;
        cpu.acknowledge_scheduler_ipi();
        let may_wait = cpu.prepare_idle_wait();
        if !may_wait {
            cpu.finish_idle_wait();
        }
        (cpu.owner(), may_wait)
    };
    if may_wait {
        task_runtime::wait_for_interrupt();
        cpu_local_for_wake(owner)
            .ok_or(TaskError::NotInitialized)?
            .finish_idle_wait();
    }
    Ok(())
}

/// Completes switch tail and consumes the inherited IRQ guard on first entry.
///
/// Fresh context trampolines must invoke this before accessing thread-local
/// state, enabling interrupts, polling futures, or calling user/OS code.
/// Resumed contexts must not call it because their suspended scheduler guard
/// consumes the same baton when the architecture switch returns.
///
/// # Safety
///
/// The caller must be the first instruction sequence of a freshly switched-in
/// context. Exactly one scheduler IRQ guard must be inherited on this CPU, and
/// this function must be called exactly once for that context's first entry.
pub unsafe fn finish_initial_context_switch() -> Result<(), TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let mut irq = RuntimeIrqGuard::enter();
    complete_current_context_switch_tail(&mut irq)?;
    drop(irq);
    task_runtime::finish_initial_context_switch();
    Ok(())
}

/// Performs one bounded task-clockevent pass without allocation or callbacks.
pub fn on_clock_event(now_ns: u64, budget: usize) -> Result<TaskClockEventOutcome, TaskError> {
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let timer_resolution_ns = task_runtime::timer_resolution_ns();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let charge = system.charge_current_until(cpu.as_mut(), now_ns, 0)?;
    let batch = cpu
        .as_mut()
        .expire_task_deadlines(now_ns, timer_resolution_ns, budget);
    let scheduler_due = cpu.take_due_scheduler_deadline(now_ns);
    let update = cpu
        .as_mut()
        .next_task_deadline_update(now_ns, timer_resolution_ns)?;
    Ok(TaskClockEventOutcome {
        slice_expired: charge.slice_expired(),
        deadline_overrun: charge.deadline_overrun(),
        expired: batch.expired(),
        pending: batch.pending() || scheduler_due,
        update,
    })
}

/// Copies the last IRQ's expired timer events for task-context processing.
pub fn take_current_expired_task_deadlines(
    output: &mut [ExpiredTaskDeadline],
) -> Result<usize, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    Ok(cpu.as_mut().take_expired_task_deadlines(output))
}

pub(crate) fn prepare_current_park(_permit: &BlockingPermit) -> Result<ParkPrepare, TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    system.prepare_park(cpu.as_mut())
}

pub(crate) fn commit_current_park(ticket: &mut crate::ParkTicket) -> Result<(), TaskError> {
    validate_blocking_context()?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Block,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let commit = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        system.commit_park(cpu.as_mut(), ticket)?
    };
    match commit {
        ParkCommit::Notified => Ok(()),
        ParkCommit::Blocked(decision) => {
            execute_switch_plan(&mut scheduler_frame, decision, now_ns);
            Ok(())
        }
    }
}

pub(crate) fn cancel_current_park(ticket: &mut crate::ParkTicket) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.cancel_park(cpu.as_mut(), ticket)
}

pub(crate) fn arm_current_park_deadline(
    thread: &ThreadHandle,
    ticket: &mut crate::ParkTicket,
    deadline_ns: u64,
) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    if ticket.thread() != thread.id()
        || ticket.is_resolved()
        || ticket.has_deadline()
        || runtime_current_cpu()?.current() != Some(thread.id())
    {
        return Err(TaskError::StaleThreadId);
    }
    let now_ns = task_runtime::monotonic_ns();
    let resolution_ns = task_runtime::timer_resolution_ns();
    let (registration, update) = {
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let owner = cpu.owner();
        let registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(
                thread.sleep_timer(),
                deadline_ns,
                TaskDeadlineKind::park_timeout(ticket.generation()),
            )
            .map_err(|_| TaskError::TimerCapacity)?;
        let token = registration.token();
        thread.core.register_sleep_timer(owner, token.generation());
        let update = match cpu
            .as_mut()
            .next_task_deadline_update(now_ns, resolution_ns)
        {
            Ok(update) => update,
            Err(error) => {
                let removed = cpu.as_mut().task_deadlines().cancel(&registration);
                let completed = thread.core.complete_sleep_timer(token.generation());
                if !removed || !completed {
                    task_runtime::fatal_invariant(0x5444_0005, thread.id().as_u64() as usize);
                }
                return Err(error);
            }
        };
        (registration, update)
    };
    let status = task_runtime::publish_task_deadline(update);
    if status != crate::runtime::RuntimeStatus::Success {
        let token = registration.token();
        let rollback = {
            let mut cpu = runtime_current_cpu_mut(&mut irq)?;
            let _removed = cpu.as_mut().task_deadlines().cancel(&registration);
            thread.core.complete_sleep_timer(token.generation());
            cpu.as_mut()
                .next_task_deadline_update(now_ns, resolution_ns)?
        };
        let rollback_status = task_runtime::publish_task_deadline(rollback);
        if rollback_status != crate::runtime::RuntimeStatus::Success {
            task_runtime::fatal_invariant(0x5444_0001, rollback_status as usize);
        }
        return Err(TaskError::RuntimeFailure(status as u32));
    }
    if ticket.attach_deadline(registration).is_err() {
        task_runtime::fatal_invariant(0x5444_0002, thread.id().as_u64() as usize);
    }
    Ok(())
}

pub(crate) fn cancel_current_park_deadline(
    thread: &ThreadHandle,
    ticket: &mut crate::ParkTicket,
) -> Result<bool, TaskError> {
    if ticket.thread() != thread.id() {
        return Err(TaskError::StaleThreadId);
    }
    let Some(token) = ticket.deadline().map(|registration| registration.token()) else {
        return Ok(false);
    };
    let mut irq = RuntimeIrqGuard::enter();
    let actual = runtime_current_cpu()?.owner();
    let Some(expected) = thread.core.sleep_timer_cpu_for(token.generation()) else {
        // Expiration physically removes the queue entry and clears the core's
        // matching generation before the owner thread resumes. Only that
        // terminal state permits consuming the ticket without queue access.
        if !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
        }
        return Ok(false);
    };
    if actual != expected {
        return Err(TaskError::CpuOwnerMismatch {
            expected: expected.as_u32(),
            actual: actual.as_u32(),
        });
    }
    let now_ns = task_runtime::monotonic_ns();
    let resolution_ns = task_runtime::timer_resolution_ns();
    let (removed, update) = {
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let removed = cpu.as_mut().task_deadlines().cancel(
            ticket
                .deadline()
                .expect("the deadline registration remains owned until cancellation"),
        );
        thread.core.complete_sleep_timer(token.generation());
        if !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
        }
        let update = cpu
            .as_mut()
            .next_task_deadline_update(now_ns, resolution_ns)?;
        (removed, update)
    };
    let status = task_runtime::publish_task_deadline(update);
    if status != crate::runtime::RuntimeStatus::Success {
        return Err(TaskError::RuntimeFailure(status as u32));
    }
    Ok(removed)
}

/// Bounded task-clockevent result consumed by the runtime clockevent owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskClockEventOutcome {
    slice_expired: bool,
    deadline_overrun: bool,
    expired: usize,
    pending: bool,
    update: crate::runtime::TaskDeadlineUpdate,
}

impl TaskClockEventOutcome {
    /// Returns whether the current scheduling slice or budget expired.
    pub const fn slice_expired(self) -> bool {
        self.slice_expired
    }
    /// Returns whether CBS exhaustion entered PI-critical rescue.
    pub const fn deadline_overrun(self) -> bool {
        self.deadline_overrun
    }
    /// Returns the number of timer events stored for safe-point handling.
    pub const fn expired(self) -> usize {
        self.expired
    }
    /// Returns whether another bounded expiry pass is immediately required.
    pub const fn pending(self) -> bool {
        self.pending
    }
    /// Returns the complete generation-ordered task-deadline publication.
    pub const fn update(self) -> crate::runtime::TaskDeadlineUpdate {
        self.update
    }
    /// Returns the next representable task deadline.
    pub const fn next_deadline_ns(self) -> Option<u64> {
        match self.update.deadline() {
            Some(deadline) => Some(deadline.as_nanos()),
            None => None,
        }
    }
}

/// Creates a PI donation edge for the calling waiter.
pub fn pi_wait_start(lock: PiLockId, owner: ThreadId) -> Result<PiWaitToken, TaskError> {
    let waiter = current_thread_id()?;
    runtime_task_system()?.pi_wait_start(lock, waiter, owner)
}

/// Blocks the calling waiter unless handoff already granted its token.
pub fn pi_block_current(token: &PiWaitToken) -> Result<(), TaskError> {
    if token.is_granted() {
        return Ok(());
    }
    let system = runtime_task_system()?;
    if runtime_current_cpu()?.current() != Some(token.waiter()) {
        return Err(TaskError::InvalidPiState);
    }
    loop {
        {
            let mut irq = RuntimeIrqGuard::enter();
            let now_ns = task_runtime::monotonic_ns();
            let mut cpu = runtime_current_cpu_mut(&mut irq)?;
            system.drain_policy_updates(cpu.as_mut(), now_ns)?;
        }
        if token.is_granted() {
            return Ok(());
        }
        let mut ticket = {
            let permit = acquire_blocking_permit()?;
            match prepare_current_park(&permit)? {
                ParkPrepare::Notified => continue,
                ParkPrepare::Prepared(ticket) => ticket,
            }
        };
        if token.is_granted() {
            cancel_current_park(&mut ticket)?;
            return Ok(());
        }
        commit_current_park(&mut ticket)?;
        if token.is_granted() {
            return Ok(());
        }
    }
}

/// Cancels a PI wait token after a handoff-before-block race.
pub fn pi_wait_cancel(token: PiWaitToken) -> Result<(), TaskError> {
    runtime_task_system()?.pi_wait_cancel(token)
}

/// Completes kernel PI mutex ownership transfer.
pub fn pi_mutex_handoff(
    lock: PiLockId,
    old_owner: ThreadId,
    next_owner: Option<ThreadId>,
) -> Result<(), TaskError> {
    runtime_task_system()?.pi_mutex_handoff(lock, old_owner, next_owner)
}

/// Publishes a targeted task-context wake after PI metadata handoff.
pub fn pi_wake(wake: &ThreadWakeHandle) -> Result<(), TaskError> {
    match wake.wake() {
        WakeResult::Notified | WakeResult::AlreadyPending | WakeResult::Exited => Ok(()),
        WakeResult::Unavailable => Err(TaskError::NotInitialized),
    }
}

pub(crate) fn publish_deferred_reclaim(node: Pin<&'static DeferredReclaimNode>, data: usize) {
    let Ok(system) = runtime_task_system() else {
        // Runtime handles remain published until shutdown. A wake released after
        // teardown cannot safely free in its current context, so leaking the
        // already-inert header is the only UAF-free shutdown fallback.
        return;
    };
    match system.publish_deferred_reclaim(node, data) {
        PublishResult::Published => {}
        PublishResult::AlreadyPending | PublishResult::WrongKind => {
            task_runtime::fatal_invariant(0x4558_0004, data);
        }
    }
}

pub(crate) fn drain_deferred_reclaims(limit: usize) -> Result<usize, TaskError> {
    runtime_task_system()?.drain_deferred_reclaims(limit)
}

/// Creates the shutdown-lifetime service thread for callbacks and reclamation.
///
/// A runtime must call this once after publishing its primary scheduler CPU and
/// before allowing ordinary application threads to exit. The service is the
/// only consumer of deferred Deadline, exit, and destruction work.
pub fn start_deferred_task_work_service() -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    system.begin_task_work_worker_install()?;
    let worker =
        match ThreadBuilder::new(String::from("ax-task-reaper")).spawn(task_work_service_entry) {
            Ok(worker) => worker,
            Err(error) => {
                system.cancel_task_work_worker_install();
                return Err(error);
            }
        };
    worker.detach_permanent();
    Ok(())
}

fn task_work_service_entry() {
    if task_work_service_loop().is_err() {
        task_runtime::fatal_invariant(0x4558_0030, 0);
    }
}

fn task_work_service_loop() -> Result<(), TaskError> {
    const BATCH_LIMIT: usize = 64;

    let system = runtime_task_system()?;
    let doorbell = system.task_work_doorbell();
    let wake_owner = current_thread_handle()?.wake_handle();
    let irq_wake = unsafe {
        // SAFETY: `waiter` below is leaked for the shutdown lifetime and owns
        // this wake handle's strong ThreadCore reference.
        wake_owner.irq_wake_handle()
    };
    let waiter = Box::leak(Box::new(TaskWorkWaiter {
        _wake_owner: wake_owner,
        registration: IrqWaitRegistration::new(irq_wake),
        park: WaitQueue::new(),
    }));
    let registration = unsafe {
        // SAFETY: the leaked allocation never moves or expires, and the sole
        // service thread serializes every register/unregister operation.
        Pin::new_unchecked(&waiter.registration)
    };
    system.finish_task_work_worker_install();

    loop {
        let _published = doorbell.take_pending();
        let batch = service_task_work_pass(system, &doorbell, BATCH_LIMIT)?;
        let pending_after_pass = doorbell.take_pending();
        match task_work_service_action(batch, pending_after_pass, BATCH_LIMIT) {
            TaskWorkServiceAction::Yield => {
                let _decision = yield_current_cpu()?;
                continue;
            }
            TaskWorkServiceAction::Wait => {
                wait_for_task_work(doorbell.event(), registration, &waiter.park)?;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskWorkServiceAction {
    Yield,
    Wait,
}

fn task_work_service_action(
    batch: Option<crate::DeferredTaskWorkBatch>,
    pending_after_pass: bool,
    limit: usize,
) -> TaskWorkServiceAction {
    match batch {
        None => TaskWorkServiceAction::Yield,
        Some(batch) if batch.saturated(limit) || pending_after_pass => TaskWorkServiceAction::Yield,
        Some(_) => TaskWorkServiceAction::Wait,
    }
}

fn service_task_work_pass(
    system: &TaskSystem,
    doorbell: &crate::task_work::TaskWorkDoorbell,
    limit: usize,
) -> Result<Option<crate::DeferredTaskWorkBatch>, TaskError> {
    match system.service_deferred_task_work(limit) {
        Ok(batch) => Ok(Some(batch)),
        Err(TaskError::ThreadBusy) => {
            doorbell.reassert_pending();
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

struct TaskWorkWaiter {
    _wake_owner: ThreadWakeHandle,
    registration: IrqWaitRegistration,
    park: WaitQueue,
}

fn wait_for_task_work(
    event: &IrqWaitCell,
    registration: Pin<&'static IrqWaitRegistration>,
    park: &WaitQueue,
) -> Result<(), TaskError> {
    match event.register(registration) {
        IrqRegisterResult::Occupied => Err(TaskError::InvalidConfiguration),
        IrqRegisterResult::Registered
        | IrqRegisterResult::ConsumedPending
        | IrqRegisterResult::NotificationInFlight => {
            park.try_wait_until(|| !registration.is_attached())?;
            let _removed = event.unregister(registration);
            Ok(())
        }
    }
}

/// Runs one scheduler decision at a task/IRQ-return safe point.
///
/// The typed outcome distinguishes a completed decision, an in-flight park
/// handshake, and bounded owner-work backpressure. It never clears
/// `need_resched` before entering the scheduler.
///
/// # Errors
///
/// Returns [`TaskError::UnsafeContext`] in hard IRQ context and object-handle
/// errors when runtime initialization is incomplete or inconsistent.
pub fn schedule_current_cpu() -> Result<SchedulerOutcome, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    schedule_current_cpu_with_entry(RuntimeSchedulerEntry::Task)
}

/// Services the final preemption-guard exit without exposing a preemptible
/// window before the scheduler owns its CPU-local baton.
///
/// # Safety
///
/// `entry` must match the caller's exact runtime context. The caller must own
/// one final lock-preemption depth and must satisfy the raw IRQ-state contract
/// documented by [`RuntimeSchedulerEntry`].
pub unsafe fn schedule_current_cpu_from_preempt_exit(
    entry: RuntimeSchedulerEntry,
) -> Result<SchedulerOutcome, TaskError> {
    if !matches!(
        entry,
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn
    ) {
        return Err(TaskError::UnsafeContext);
    }
    schedule_current_cpu_with_entry(entry)
}

fn schedule_current_cpu_with_entry(
    entry: RuntimeSchedulerEntry,
) -> Result<SchedulerOutcome, TaskError> {
    let mut scheduler_frame =
        RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Preempt, entry)?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let outcome = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        let current_state = cpu.current_lifecycle_state();
        if !cpu.needs_reschedule() && !cpu.has_remote_work() {
            if current_state == Some(ThreadState::Parking) {
                SchedulerOutcome::ParkingDeferred
            } else {
                SchedulerOutcome::Quiescent
            }
        } else {
            system.schedule_if_requested(cpu.as_mut(), now_ns)?
        }
    };
    if let Some(decision) = outcome.decision() {
        execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    }
    Ok(outcome)
}

fn drain_current_expired_timers(
    system: &TaskSystem,
    pin: &mut impl RuntimeCpuPin,
) -> Result<usize, TaskError> {
    let mut drained = 0;
    loop {
        let event = {
            let mut cpu = runtime_current_cpu_mut(pin)?;
            cpu.as_mut().take_expired_task_deadline()
        };
        let Some(event) = event else {
            break;
        };
        let Some(thread) = event.thread() else {
            continue;
        };
        match system.thread_handle(thread) {
            Ok(handle) => {
                let completed = handle.core.complete_sleep_timer(event.token().generation());
                let park_matches = event
                    .kind()
                    .is_some_and(|kind| kind.park_generation() == handle.core.park_generation());
                if completed && park_matches {
                    let _wake_result = handle.wake_handle().wake();
                }
            }
            Err(TaskError::StaleThreadId) => {}
            Err(error) => return Err(error),
        }
        drained += 1;
    }
    Ok(drained)
}

fn service_current_task_deadline_work(
    system: &TaskSystem,
    pin: &mut impl RuntimeCpuPin,
    now_ns: u64,
) -> Result<usize, TaskError> {
    let should_run = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        cpu.as_mut().begin_deadline_work()
    };
    if !should_run {
        return Ok(0);
    }

    let result = (|| {
        let mut drained = drain_current_expired_timers(system, pin)?;
        let batch = {
            let mut cpu = runtime_current_cpu_mut(pin)?;
            let budget = cpu.batch_limit();
            cpu.as_mut()
                .expire_task_deadlines(now_ns, task_runtime::timer_resolution_ns(), budget)
        };
        drained += drain_current_expired_timers(system, pin)?;
        let mut cpu = runtime_current_cpu_mut(pin)?;
        cpu.as_mut().finish_deadline_work(batch.pending());
        Ok(drained)
    })();
    if result.is_err() {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        cpu.as_mut().finish_deadline_work(true);
    }
    result
}

/// Yields the calling thread and executes the resulting context switch.
pub fn yield_current_cpu() -> Result<ScheduleDecision, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Yield)?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        system.yield_current(cpu.as_mut(), now_ns)?
    };
    execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    Ok(decision)
}

/// Exits the calling thread and switches to its replacement.
pub fn exit_current_thread() -> Result<(), TaskError> {
    let permit = prepare_current_exit()?;
    commit_current_exit(permit)
}

/// A validated, thread-bound opportunity to publish exit completion.
pub struct ExitPermit {
    thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

/// Validates scheduler-side exit prerequisites without changing the current
/// thread's observable lifecycle.
pub fn prepare_current_exit() -> Result<ExitPermit, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Exit)?;
    let mut irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let now_ns = task_runtime::monotonic_ns();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let thread = system.prepare_current_exit(cpu.as_mut(), now_ns)?;
    Ok(ExitPermit {
        thread,
        _not_send: PhantomData,
    })
}

/// Commits a prepared scheduler exit and permanently leaves this context.
///
/// Any failure after completion became externally visible is a fatal runtime
/// invariant; this function therefore has no recoverable return path.
pub fn commit_current_exit(permit: ExitPermit) -> ! {
    let mut scheduler_frame =
        RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Exit, RuntimeSchedulerEntry::Task)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0010, permit.thread.as_u64() as _)
            });
    let system = runtime_task_system().unwrap_or_else(|_| {
        task_runtime::fatal_invariant(0x4558_0011, permit.thread.as_u64() as _)
    });
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0012, permit.thread.as_u64() as _)
        });
    let now_ns = task_runtime::monotonic_ns();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0013, permit.thread.as_u64() as _)
        });
        if cpu.current() != Some(permit.thread) {
            task_runtime::fatal_invariant(0x4558_0014, permit.thread.as_u64() as _);
        }
        system.exit_current(cpu.as_mut()).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0015, permit.thread.as_u64() as _)
        })
    };
    execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    // An exited context is never re-enqueued, so returning here indicates a
    // broken architecture switch contract.
    task_runtime::fatal_invariant(4, decision.previous().map_or(0, ThreadId::as_u64) as usize)
}

fn execute_switch_plan(
    scheduler_frame: &mut RuntimeSchedulerFrameGuard,
    decision: ScheduleDecision,
    now_ns: u64,
) {
    if !decision.requires_context_switch() {
        return;
    }
    let Some(previous) = decision.previous_endpoint() else {
        task_runtime::fatal_invariant(1, decision.next().as_u64() as usize);
    };
    let next = decision.next_endpoint();
    if previous.context().is_none() || next.context().is_none() {
        task_runtime::fatal_invariant(2, next.thread().as_u64() as usize);
    }
    // Match Linux's sched_switch observation point: the trace runs while the
    // previous extension is still the published current task, but after all
    // scheduler locks have been released and the switch decision is final.
    task_runtime::trace_sched_switch(SchedSwitchRecord {
        cpu: RuntimeCpuId::new(task_runtime::current_cpu_id().as_u32()),
        previous_thread: previous.thread().as_u64(),
        next_thread: next.thread().as_u64(),
        timestamp_ns: now_ns,
        reason: decision.switch_reason() as u32,
    });
    if let Some(extension) = previous.extension() {
        // SAFETY: ThreadExtension construction guarantees callback validity;
        // TaskSystem released every internal lock and the outer IRQ guard is held.
        unsafe {
            (extension.ops().on_switch_out)(
                extension.data(),
                previous.thread(),
                decision.switch_reason(),
            )
        };
    }
    prepare_next_context(next.address_space(), next.thread(), next.extension());
    // SAFETY: the scheduler committed both endpoint states before releasing its
    // locks. Runtime handles remain live, and local IRQs stay disabled here.
    unsafe { task_runtime::switch_context(previous.context(), next.context()) };
    if complete_current_context_switch_tail(scheduler_frame).is_err() {
        task_runtime::fatal_invariant(5, 0);
    }
}

fn install_next_address_space(address_space: crate::runtime::AddressSpaceHandle, thread: ThreadId) {
    if task_runtime::install_address_space(address_space) != crate::runtime::RuntimeStatus::Success
    {
        task_runtime::fatal_invariant(3, thread.as_u64() as usize);
    }
}

fn prepare_next_context(
    address_space: crate::runtime::AddressSpaceHandle,
    thread: ThreadId,
    extension: Option<crate::ThreadExtensionView>,
) {
    install_next_address_space(address_space, thread);
    if let Some(extension) = extension {
        // SAFETY: ThreadExtension construction guarantees callback validity;
        // the address space is now active and no scheduler lock is held.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread) };
    }
}

fn complete_current_context_switch_tail(pin: &mut impl RuntimeCpuPin) -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut(pin)?;
    system.complete_context_switch(cpu.as_mut())
}

pub(crate) fn runtime_task_system() -> Result<&'static TaskSystem, TaskError> {
    // SAFETY: the linked TaskRuntime provider is the platform trust root and
    // must publish only the pinned, shutdown-lifetime TaskSystem it owns.
    let handle = unsafe { task_runtime::task_system_handle() };
    let raw = handle.into_raw();
    validate_handle::<TaskSystem>(raw)?;
    // SAFETY: TaskRuntime requires this handle to identify a pinned TaskSystem
    // that remains live until shutdown. The scheduler's mutable state is behind
    // its internal IRQ ticket lock, so creating this shared reference aliases no
    // unprotected mutable access.
    Ok(unsafe { &*ptr::with_exposed_provenance::<TaskSystem>(raw) })
}

struct RuntimeCurrentCpu {
    cpu: CpuLocalOwnerBorrow<'static>,
    _irq: RuntimeIrqGuard,
}

impl Deref for RuntimeCurrentCpu {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        &self.cpu
    }
}

fn runtime_current_cpu() -> Result<RuntimeCurrentCpu, TaskError> {
    let irq = RuntimeIrqGuard::enter();
    let cpu = claim_runtime_current_cpu()?;
    Ok(RuntimeCurrentCpu { cpu, _irq: irq })
}

mod runtime_cpu_pin_sealed {
    pub trait Sealed {}
}

pub(crate) trait RuntimeCpuPin: runtime_cpu_pin_sealed::Sealed {}

pub(crate) struct RuntimeCpuOwnerBorrow<'pin> {
    cpu: CpuLocalOwnerBorrow<'static>,
    _pin: PhantomData<&'pin mut ()>,
}

impl RuntimeCpuOwnerBorrow<'_> {
    pub(crate) fn as_mut(&mut self) -> Pin<&mut CpuLocal> {
        self.cpu.as_pin_mut()
    }
}

impl Deref for RuntimeCpuOwnerBorrow<'_> {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        &self.cpu
    }
}

pub(crate) fn runtime_current_cpu_mut<'pin>(
    _pin: &'pin mut impl RuntimeCpuPin,
) -> Result<RuntimeCpuOwnerBorrow<'pin>, TaskError> {
    Ok(RuntimeCpuOwnerBorrow {
        cpu: claim_runtime_current_cpu()?,
        _pin: PhantomData,
    })
}

fn claim_runtime_current_cpu() -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
    let runtime_cpu = task_runtime::current_cpu_id();
    let remote = cpu_local_for_wake(crate::CpuId::new(runtime_cpu.as_u32()))
        .ok_or(TaskError::NotInitialized)?;
    // SAFETY: callers acquire an IRQ pin or scheduler-frame baton before this
    // helper and retain it for the complete lifetime of the returned claim.
    let handle = unsafe { task_runtime::current_cpu_local_handle() };
    let raw = handle.into_raw();
    validate_handle::<CpuLocal>(raw)?;
    // SAFETY: TaskRuntime guarantees this address identifies the current CPU's
    // pinned shutdown-lifetime CpuLocal. The separately allocated remote gate
    // is claimed before a reference to that allocation is reconstructed.
    let cpu = unsafe { remote.claim_local(ptr::with_exposed_provenance_mut::<CpuLocal>(raw))? };
    validate_cpu_owner(&cpu)?;
    Ok(cpu)
}

pub(crate) fn cpu_local_for_wake(cpu: crate::CpuId) -> Option<&'static CpuRemote> {
    // SAFETY: the linked runtime guarantees that this typed endpoint is the
    // Arc-backed CpuRemote for `cpu` and keeps it alive until shutdown.
    let handle =
        unsafe { task_runtime::cpu_remote_handle(crate::runtime::RuntimeCpuId::new(cpu.as_u32())) };
    let raw = handle.into_raw();
    if validate_handle::<CpuRemote>(raw).is_err() {
        return None;
    }
    // SAFETY: TaskRuntime guarantees every remote endpoint is Arc-backed and
    // remains live until shutdown. It contains no owner-only runqueue state.
    let cpu = unsafe { &*ptr::with_exposed_provenance::<CpuRemote>(raw) };
    cpu.is_online().then_some(cpu)
}

fn validate_handle<T>(raw: usize) -> Result<(), TaskError> {
    if raw == 0 {
        Err(TaskError::NotInitialized)
    } else if !raw.is_multiple_of(align_of::<T>()) {
        Err(TaskError::InvalidRuntimeHandle)
    } else {
        Ok(())
    }
}

fn validate_cpu_owner(cpu: &CpuLocal) -> Result<(), TaskError> {
    let actual = task_runtime::current_cpu_id().as_u32();
    let expected = cpu.owner().as_u32();
    if actual == expected {
        Ok(())
    } else {
        Err(TaskError::CpuOwnerMismatch { expected, actual })
    }
}

fn validate_schedule_context(origin: RuntimeScheduleOrigin) -> Result<(), TaskError> {
    match task_runtime::validate_schedule_context(origin) {
        RuntimeStatus::Success => Ok(()),
        RuntimeStatus::UnsafeContext => Err(TaskError::UnsafeContext),
        status => Err(TaskError::RuntimeFailure(status as u32)),
    }
}

pub(crate) struct RuntimeIrqGuard {
    token: IrqGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl RuntimeIrqGuard {
    pub(crate) fn enter() -> Self {
        Self {
            token: task_runtime::irq_guard_enter(),
            _not_send: PhantomData,
        }
    }
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeIrqGuard {}
impl RuntimeCpuPin for RuntimeIrqGuard {}

impl Drop for RuntimeIrqGuard {
    fn drop(&mut self) {
        // SAFETY: this guard consumes its same-CPU token exactly once.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}

struct RuntimeSchedulerFrameGuard {
    return_to: RuntimeSchedulerReturn,
    _not_send: PhantomData<*mut ()>,
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeSchedulerFrameGuard {}
impl RuntimeCpuPin for RuntimeSchedulerFrameGuard {}

impl RuntimeSchedulerFrameGuard {
    fn enter(
        origin: RuntimeScheduleOrigin,
        entry: RuntimeSchedulerEntry,
    ) -> Result<Self, TaskError> {
        let status = task_runtime::scheduler_frame_guard_enter(origin, entry);
        if status != RuntimeStatus::Success {
            return Err(match status {
                RuntimeStatus::UnsafeContext => TaskError::UnsafeContext,
                status => TaskError::RuntimeFailure(status as u32),
            });
        }
        let return_to = match entry {
            RuntimeSchedulerEntry::Task | RuntimeSchedulerEntry::PreemptExit => {
                RuntimeSchedulerReturn::Task
            }
            RuntimeSchedulerEntry::IrqReturn => RuntimeSchedulerReturn::IrqReturn,
        };
        Ok(Self {
            return_to,
            _not_send: PhantomData,
        })
    }
}

impl Drop for RuntimeSchedulerFrameGuard {
    fn drop(&mut self) {
        let _task_context_safe = task_runtime::scheduler_frame_guard_exit(self.return_to);
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        CpuId, SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadSpec,
        inbox::{InboxKind, InboxMessage, InboxNode, PublishResult},
        runtime::AddressSpaceHandle,
        test_runtime,
    };

    static PARKING_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT: AtomicUsize = AtomicUsize::new(0);

    static ORDERING_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: assert_address_space_installed,
        on_switch_out: ignore_switch_out,
        on_exit: ignore_thread_event,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static PARKING_EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_thread_event,
        on_switch_out: ignore_switch_out,
        on_exit: count_parking_exit,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static REENTRANT_EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_thread_event,
        on_switch_out: ignore_switch_out,
        on_exit: count_reentrant_exit,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static TRACE_ORDER_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_thread_event,
        on_switch_out: record_switch_out,
        on_exit: ignore_thread_event,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    #[test]
    fn kernel_address_space_is_explicitly_installed_before_switch_in() {
        test_runtime::reset_installed_address_space();
        let extension = unsafe {
            // SAFETY: the callback table interprets data only as the expected
            // address-space scalar and owns no external resource.
            ThreadExtension::new(0, &ORDERING_EXTENSION_OPS)
        };

        prepare_next_context(
            AddressSpaceHandle::NONE,
            ThreadId::from_parts(1, 1),
            Some(extension.as_view()),
        );

        assert_eq!(test_runtime::installed_address_space(), Some(0));
    }

    #[test]
    fn timer_expiry_during_parking_is_committed_by_the_owner_thread() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, 0).unwrap();

        assert_eq!(on_clock_event(0, 64).unwrap().expired(), 1);
        let mut irq = RuntimeIrqGuard::enter();
        assert_eq!(
            drain_current_expired_timers(system.as_ref().get_ref(), &mut irq).unwrap(),
            1
        );
        drop(irq);
        assert_eq!(
            system
                .drain_remote_wakes(cpu.as_mut(), 0)
                .unwrap()
                .drained(),
            1
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking,
            "timer wake must leave the owner thread to finish its PARKING handshake"
        );
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        cpu.request_reschedule();
        let mut irq_return_passes = 0;
        while current_cpu_needs_resched().unwrap() {
            assert!(
                schedule_current_cpu().unwrap().parking_deferred(),
                "timer IRQ-return scheduling must defer until the park token commits"
            );
            irq_return_passes += 1;
            assert!(irq_return_passes < 2, "PARKING must not spin at IRQ return");
        }
        assert_eq!(irq_return_passes, 1);
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking
        );
        assert!(!system.snapshot(cpu.as_ref()).need_resched());

        commit_current_park(&mut ticket).unwrap();
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        assert!(system.snapshot(cpu.as_ref()).need_resched());
        assert!(!cancel_current_park_deadline(&running, &mut ticket).unwrap());
    }

    #[test]
    fn irq_budget_exhaustion_defers_backlog_without_resolution_rate_rearm() {
        let system =
            Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1).with_batch_limit(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let first = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let second = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let _first_registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(first.sleep_timer(), 10, TaskDeadlineKind::park_timeout(0))
            .unwrap();
        let _second_registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(second.sleep_timer(), 10, TaskDeadlineKind::park_timeout(0))
            .unwrap();

        let outcome = on_clock_event(10, 1).unwrap();

        assert_eq!(outcome.expired(), 1);
        assert!(outcome.pending());
        assert!(outcome.update().deferred_work());
        assert!(
            outcome
                .update()
                .deadline()
                .is_none_or(|deadline| deadline.as_nanos() > 11),
            "an expired bounded backlog must be advanced by sticky task work, not by a timer \
             interrupt rearmed at the 1ns hardware resolution"
        );
        assert!(
            system.snapshot(cpu.as_ref()).need_resched(),
            "the deferred owner pass must remain a scheduler-visible safe point"
        );
    }

    #[test]
    fn park_deadline_owner_mismatch_preserves_ticket_for_retry() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, 10).unwrap();
        let token = ticket
            .deadline()
            .expect("armed park deadline token")
            .token();

        running
            .core
            .register_sleep_timer(CpuId::new(1), token.generation());
        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::CpuOwnerMismatch {
                expected: 1,
                actual: 0,
            })
        );
        assert!(
            ticket.has_deadline(),
            "a retryable owner mismatch must not consume the move-only deadline token"
        );
        assert_eq!(
            cpu.as_mut().task_deadlines().next_deadline_ns(0, 1),
            Some(10),
            "a failed cancellation must leave the physical queue entry intact"
        );

        running
            .core
            .register_sleep_timer(CpuId::new(0), token.generation());
        assert!(cancel_current_park_deadline(&running, &mut ticket).unwrap());
        assert!(!ticket.has_deadline());
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn stale_expiration_cannot_notify_a_new_park_generation() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let first_permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut first) = prepare_current_park(&first_permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = first_permit;
        arm_current_park_deadline(&running, &mut first, 10).unwrap();
        assert_eq!(on_clock_event(10, 1).unwrap().expired(), 1);
        assert!(!cancel_current_park_deadline(&running, &mut first).unwrap());
        cancel_current_park(&mut first).unwrap();

        let second_permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut second) = prepare_current_park(&second_permit).unwrap()
        else {
            panic!("the next park generation must be independently prepared");
        };
        let _ = second_permit;
        arm_current_park_deadline(&running, &mut second, 100).unwrap();

        test_runtime::set_monotonic_ns(10);
        schedule_current_cpu().unwrap();
        assert!(
            !running.core.take_park_notification(),
            "an expiration buffered for the preceding park generation must not wake the rearm"
        );

        assert!(cancel_current_park_deadline(&running, &mut second).unwrap());
        cancel_current_park(&mut second).unwrap();
    }

    #[test]
    fn failed_deadline_update_rolls_back_the_unpublished_park_timer() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        cpu.as_mut().set_task_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            arm_current_park_deadline(&running, &mut ticket, 10),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(!ticket.has_deadline());
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);

        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn failed_cancel_update_consumes_the_physically_removed_deadline_ticket() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, 10).unwrap();
        cpu.as_mut().set_task_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(
            !ticket.has_deadline(),
            "a physically removed timer must not remain represented by a live ticket"
        );
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);

        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn parking_safe_point_is_bounded_and_does_not_run_task_work() {
        PARKING_EXIT_CALLBACKS.store(0, Ordering::Release);
        let remote_wake_nodes = [
            Box::pin(InboxNode::new(InboxKind::RemoteWake)),
            Box::pin(InboxNode::new(InboxKind::RemoteWake)),
        ];
        let system =
            Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1).with_batch_limit(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let extension = unsafe {
            // SAFETY: the static callback table interprets no extension data.
            ThreadExtension::new(0, &PARKING_EXIT_EXTENSION_OPS)
        };
        let exited = system
            .create_thread(
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_extension(extension),
            )
            .unwrap();
        system.make_ready(exited.id()).unwrap();
        system.enqueue(cpu.as_mut(), exited.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu.as_mut(), 0).unwrap().next(),
            exited.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
        let exit_decision = system.exit_current(cpu.as_mut()).unwrap();
        assert_ne!(exit_decision.next(), exited.id());
        assert_eq!(PARKING_EXIT_CALLBACKS.load(Ordering::Acquire), 0);

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;

        for (index, node) in remote_wake_nodes.iter().enumerate() {
            let slot = (index + 1) as u32;
            let message = InboxMessage::remote_wake(ThreadId::from_parts(slot, 1), CpuId::new(0));
            let node = unsafe {
                // The pinned fixture is declared before the task system, so it
                // outlives the CPU inbox even when one bounded batch remains.
                Pin::new_unchecked(&*(node.as_ref().get_ref() as *const InboxNode))
            };
            assert_eq!(
                cpu.remote().publish_remote_wake(node, message),
                PublishResult::Published
            );
        }

        assert!(schedule_current_cpu().unwrap().parking_deferred());
        assert!(
            cpu.has_remote_work(),
            "one owner-work batch must remain pending"
        );
        assert!(
            cpu.needs_reschedule(),
            "remaining work must retain its doorbell"
        );
        assert_eq!(
            PARKING_EXIT_CALLBACKS.load(Ordering::Acquire),
            0,
            "task-work must not run while current owns a park token"
        );
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn scheduler_frame_guard_covers_work_before_the_context_switch() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_scheduler_frame_state();

        let _decision = schedule_current_cpu().unwrap();

        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 0),
            "an empty safe point needs only the scheduler baton"
        );
    }

    #[test]
    fn irq_exit_scheduler_reentry_does_not_nest_task_work_on_one_thread_stack() {
        REENTRANT_EXIT_CALLBACKS.store(0, Ordering::Release);
        REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.store(0, Ordering::Release);
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let extension = unsafe {
            // SAFETY: the callback table owns no external data and records only
            // whether task work ran inside the configured scheduler reentry.
            ThreadExtension::new(0, &REENTRANT_EXIT_EXTENSION_OPS)
        };
        let exiting = system
            .create_thread(
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_extension(extension),
            )
            .unwrap();
        system.make_ready(exiting.id()).unwrap();
        system.enqueue(cpu.as_mut(), exiting.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu.as_mut(), 0).unwrap().next(),
            exiting.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
        assert_eq!(
            system.exit_current(cpu.as_mut()).unwrap().next(),
            bootstrap.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::configure_irq_exit_schedule_reentry(1);
        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));

        assert_eq!(
            REENTRANT_EXIT_CALLBACKS.load(Ordering::Acquire),
            0,
            "scheduler frames must only publish task work for the service thread"
        );
        let batch = system.service_deferred_task_work(1).unwrap();
        assert!(batch.made_progress());
        assert_eq!(REENTRANT_EXIT_CALLBACKS.load(Ordering::Acquire), 1);
        assert_eq!(
            REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.load(Ordering::Acquire),
            0,
            "nested scheduler completion must not recursively run task work on the active stack"
        );
    }

    #[test]
    fn busy_task_work_consumer_is_retryable_for_the_service_thread() {
        let system = TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap();
        let doorbell = system.task_work_doorbell();
        let _consumer = doorbell.try_claim_consumer().unwrap();

        assert_eq!(service_task_work_pass(&system, &doorbell, 1).unwrap(), None);
        assert!(
            doorbell.is_pending(),
            "the worker must retain a sticky retry after losing consumer ownership"
        );
    }

    #[test]
    fn repeated_task_work_publication_yields_before_retry() {
        assert_eq!(
            task_work_service_action(Some(crate::DeferredTaskWorkBatch::default()), true, 64,),
            TaskWorkServiceAction::Yield,
            "a sticky publication after an under-budget pass must not spin the service thread"
        );
    }

    #[test]
    fn scheduler_safe_point_drains_owner_work_after_resched_bit_was_consumed() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
        assert!(cpu.has_remote_work());
        assert!(cpu.needs_reschedule());

        // Forced schedule paths used to consume the sticky bit without first
        // draining owner work. Claiming scheduler entry must re-observe the
        // published inbox and preserve a doorbell for the next bounded drain.
        cpu.as_mut().scheduler_enter();
        assert!(cpu.needs_reschedule());

        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
        assert!(
            !cpu.has_remote_work(),
            "pending owner work must be sufficient to enter the scheduler safe point"
        );
    }

    #[test]
    fn context_switch_uses_one_scheduler_frame() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap_resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::from_raw(2),
                TlsHandle::from_raw(3),
                AddressSpaceHandle::NONE,
            )
        };
        system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(bootstrap_resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let next_resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(4),
                StackHandle::from_raw(5),
                TlsHandle::from_raw(6),
                AddressSpaceHandle::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();
        test_runtime::reset_scheduler_frame_state();

        let decision = schedule_current_cpu().unwrap().decision().unwrap();

        assert!(decision.requires_context_switch());
        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 1),
            "one scheduling operation must use exactly one scheduler baton"
        );
        assert_eq!(
            test_runtime::irq_guards_at_context_switch(),
            0,
            "ordinary same-CPU IRQ tokens must be released before raw switch"
        );
    }

    #[test]
    fn switch_trace_observes_previous_before_switch_out_callback() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
            test_runtime::SwitchObservation,
        };

        test_runtime::reset_switch_observations();
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let previous_resources = unsafe {
            // SAFETY: the unit runtime treats these unique scalar handles as
            // inert identities for the duration of this switch.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(11),
                StackHandle::from_raw(12),
                TlsHandle::from_raw(13),
                AddressSpaceHandle::NONE,
            )
        };
        let extension = unsafe {
            // SAFETY: the static callback records only value-type switch data.
            ThreadExtension::new(0, &TRACE_ORDER_EXTENSION_OPS)
        };
        let previous = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default())
                    .with_resources(previous_resources)
                    .with_extension(extension)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let next_resources = unsafe {
            // SAFETY: this bundle owns distinct inert handles.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(21),
                StackHandle::from_raw(22),
                TlsHandle::from_raw(23),
                AddressSpaceHandle::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();

        let outcome = schedule_current_cpu().unwrap();

        assert!(outcome.decision().unwrap().requires_context_switch());
        let observations = test_runtime::take_switch_observations();
        assert_eq!(observations.len(), 2);
        let SwitchObservation::Trace(record) = observations[0] else {
            panic!("the allocation-free trace must run before OS switch-out");
        };
        assert_eq!(record.previous_thread, previous.id().as_u64());
        assert_eq!(record.next_thread, next.id().as_u64());
        assert_eq!(
            observations[1],
            SwitchObservation::SwitchOut {
                thread: previous.id(),
                reason: SwitchReason::Preempted,
            }
        );
    }

    unsafe extern "Rust" fn assert_address_space_installed(data: usize, _thread: ThreadId) {
        assert_eq!(test_runtime::installed_address_space(), Some(data));
    }

    unsafe extern "Rust" fn ignore_switch_out(
        _data: usize,
        _thread: ThreadId,
        _reason: SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn record_switch_out(
        _data: usize,
        thread: ThreadId,
        reason: SwitchReason,
    ) {
        test_runtime::record_switch_out(thread, reason);
    }

    unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn count_parking_exit(_data: usize, _thread: ThreadId) {
        PARKING_EXIT_CALLBACKS.fetch_add(1, Ordering::AcqRel);
    }

    unsafe extern "Rust" fn count_reentrant_exit(_data: usize, _thread: ThreadId) {
        REENTRANT_EXIT_CALLBACKS.fetch_add(1, Ordering::AcqRel);
        if test_runtime::irq_exit_schedule_reentry_active() {
            REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.fetch_add(1, Ordering::AcqRel);
        }
    }

    unsafe extern "Rust" fn ignore_drop(_data: usize) {}

    #[test]
    fn blocking_context_is_rejected_before_parking_publication() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::set_schedule_context_safe(false);

        let result = acquire_blocking_permit();
        let published_state = system.thread_state(running.id()).unwrap();
        test_runtime::set_schedule_context_safe(true);

        assert!(matches!(result, Err(TaskError::UnsafeContext)));
        assert_eq!(published_state, crate::ThreadState::Running);
    }

    #[test]
    fn affinity_is_not_published_before_the_scheduler_frame_is_owned() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(2)).unwrap());
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
        let original = system.thread_affinity(running.id()).unwrap();
        let mut cpu1_only = CpuSet::empty(2);
        assert!(cpu1_only.insert(CpuId::new(1)));
        test_runtime::set_scheduler_frame_enter_status(RuntimeStatus::UnsafeContext);

        let result = set_current_thread_affinity(cpu1_only);

        test_runtime::set_scheduler_frame_enter_status(RuntimeStatus::Success);
        assert!(matches!(result, Err(TaskError::UnsafeContext)));
        assert_eq!(
            system.thread_affinity(running.id()).unwrap(),
            original,
            "a failed scheduler-frame acquisition must not partially publish affinity"
        );
    }

    #[test]
    fn preparing_exit_keeps_the_current_thread_running_until_commit() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::NONE,
                TlsHandle::NONE,
                AddressSpaceHandle::NONE,
            )
        };
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let permit = prepare_current_exit().unwrap();

        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
        let _ = permit;
    }

    #[test]
    fn current_cpu_reference_keeps_its_irq_pin_alive() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_irq_state();

        let current = runtime_current_cpu().unwrap();

        assert_eq!(current.owner(), CpuId::new(0));
        assert_eq!(
            test_runtime::active_irq_guards(),
            1,
            "the CPU-local reference must retain its migration pin"
        );
        drop(current);
        assert_eq!(test_runtime::active_irq_guards(), 0);
    }

    #[test]
    fn runtime_owner_handle_preserves_mutable_provenance_after_a_shared_query() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let current = runtime_current_cpu().unwrap();
        assert_eq!(current.owner(), CpuId::new(0));
        drop(current);

        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
    }

    #[test]
    fn idle_wait_keeps_polling_published_through_the_runtime_final_recheck() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let idle = system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, crate::FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        assert_eq!(
            system.block_current(cpu.as_mut()).unwrap().next(),
            idle.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::configure_idle_wait(true);

        idle_current_cpu_once().unwrap();

        assert_eq!(
            test_runtime::idle_wait_observation(),
            (1, true),
            "the runtime final recheck must run while polling is still published"
        );
        assert!(
            !cpu.is_idle_polling(),
            "returning from the virtual wait must clear polling publication"
        );
        assert!(
            cpu.needs_reschedule(),
            "work published in the final recheck window must remain sticky"
        );

        idle_current_cpu_once().unwrap();
        assert_eq!(
            test_runtime::idle_wait_observation().0,
            1,
            "pending work must prevent a second idle wait"
        );
    }

    #[test]
    fn runtime_hooks_reject_reentrant_cpu_owner_queries() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let mut irq = RuntimeIrqGuard::enter();
        let mut owner = runtime_current_cpu_mut(&mut irq).unwrap();
        let owner_pin = owner.as_mut();

        assert_eq!(
            current_thread_handle().unwrap_err(),
            TaskError::CpuOwnerBorrowed,
            "a reentrant current-handle query must fail instead of spinning"
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _now = task_runtime::monotonic_ns();
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
        );

        test_runtime::reenter_needs_reschedule_from_next_hook();
        let update = crate::runtime::TaskDeadlineUpdate::try_new(
            1,
            crate::runtime::MonotonicDeadline::from_nanos(1),
            false,
        )
        .unwrap();
        let _status = task_runtime::publish_task_deadline(update);
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _status = task_runtime::send_scheduler_ipi(RuntimeCpuId::new(0));
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
        );
        assert_eq!(owner_pin.as_ref().get_ref().owner(), CpuId::new(0));
    }

    struct InstalledTaskHandles;

    impl InstalledTaskHandles {
        fn new(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
            test_runtime::install_task_handles(
                (system.get_ref() as *const TaskSystem).expose_provenance(),
                // SAFETY: the fixture publishes this pointer only while the
                // owner CPU object is pinned and scheduler access is serialized.
                (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
            );
            Self
        }
    }

    impl Drop for InstalledTaskHandles {
        fn drop(&mut self) {
            test_runtime::clear_task_handles();
        }
    }
}
