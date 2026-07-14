//! Runtime-backed scheduler facade for crates below `ax-runtime`.

use core::{marker::PhantomData, mem::align_of, pin::Pin, ptr};

use crate::{
    CpuLocal, CpuSet, Nice, ParkCommit, ParkPrepare, PiLockId, PiWaitToken, RtPriority,
    ScheduleDecision, SchedulePolicy, TaskError, TaskSystem, ThreadExtensionLease, ThreadHandle,
    ThreadId, ThreadRuntimeSnapshot, ThreadWakeHandle, WakeResult,
    inbox::PublishResult,
    reclaim::DeferredReclaimNode,
    runtime::{AddressSpaceHandle, IrqGuardToken, RuntimeCpuId, SchedSwitchRecord, task_runtime},
    timer::{ExpiredTimer, TimerToken},
};

/// Returns a strong handle for the calling scheduler thread.
///
/// # Errors
///
/// Returns [`TaskError::NotInitialized`] before runtime object publication and
/// [`TaskError::StaleThreadId`] if the current slot contains a stale identity.
pub fn current_thread_handle() -> Result<ThreadHandle, TaskError> {
    let system = runtime_task_system()?;
    let cpu = runtime_current_cpu()?;
    let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
    system.thread_handle(current)
}

/// Returns the generation-bearing identity of the calling scheduler thread.
pub fn current_thread_id() -> Result<ThreadId, TaskError> {
    Ok(current_thread_handle()?.id())
}

/// Returns the opaque extension of the calling scheduler thread.
///
/// Runtime entry trampolines use the callback-table address as a type identity
/// before recovering an OS-owned closure or process object from `data`.
pub fn current_thread_extension() -> Result<Option<ThreadExtensionLease>, TaskError> {
    let system = runtime_task_system()?;
    let handle = current_thread_handle()?;
    system.thread_extension_lease(handle)
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
    let _irq = RuntimeIrqGuard::enter();
    runtime_task_system()?.replace_current_address_space(runtime_current_cpu_mut()?, address_space)
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
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let cpu = runtime_current_cpu()?;
    cpu.acknowledge_scheduler_ipi();
    if cpu.prepare_idle_wait() {
        task_runtime::wait_for_interrupt();
        cpu.finish_idle_wait();
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
    complete_current_context_switch_tail()?;
    task_runtime::finish_initial_context_switch();
    Ok(())
}

/// Performs bounded timer-IRQ accounting without allocation or callbacks.
pub fn timer_interrupt_current_cpu(
    _elapsed_runtime_ns: u64,
    reclaimed_ns: u64,
) -> Result<TimerInterruptOutcome, TaskError> {
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut()?;
    let now_ns = task_runtime::monotonic_ns();
    let charge = system.charge_current_until(cpu.as_mut(), now_ns, reclaimed_ns)?;
    let batch = cpu
        .as_mut()
        .expire_timers(now_ns, task_runtime::timer_resolution_ns());
    let scheduler_due = cpu.take_due_scheduler_deadline(now_ns);
    let next_deadline_ns = match (batch.next_deadline_ns(), cpu.scheduler_deadline_ns()) {
        (Some(timer), Some(scheduler)) => Some(timer.min(scheduler)),
        (Some(timer), None) => Some(timer),
        (None, Some(scheduler)) => Some(scheduler),
        (None, None) => None,
    };
    Ok(TimerInterruptOutcome {
        slice_expired: charge.slice_expired(),
        deadline_overrun: charge.deadline_overrun(),
        expired: batch.expired(),
        pending: batch.pending() || scheduler_due,
        next_deadline_ns,
    })
}

/// Copies the last IRQ's expired timer events for task-context processing.
pub fn take_current_expired_timers(output: &mut [ExpiredTimer]) -> Result<usize, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    runtime_current_cpu_mut().map(|cpu| cpu.take_expired_timers(output))
}

pub(crate) fn prepare_current_park() -> Result<ParkPrepare, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    system.prepare_park(runtime_current_cpu_mut()?)
}

pub(crate) fn commit_current_park(token: crate::ParkToken) -> Result<(), TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    drain_current_expired_timers(system)?;
    let mut cpu = runtime_current_cpu_mut()?;
    let now_ns = task_runtime::monotonic_ns();
    match system.commit_park(cpu.as_mut(), token)? {
        ParkCommit::Notified => Ok(()),
        ParkCommit::Blocked(decision) => {
            execute_switch_plan(decision, now_ns);
            Ok(())
        }
    }
}

pub(crate) fn cancel_current_park(token: crate::ParkToken) -> Result<(), TaskError> {
    let _irq = RuntimeIrqGuard::enter();
    runtime_task_system()?.cancel_park(runtime_current_cpu_mut()?, token)
}

pub(crate) fn arm_current_sleep_timer(
    thread: &ThreadHandle,
    deadline_ns: u64,
) -> Result<TimerToken, TaskError> {
    let _irq = RuntimeIrqGuard::enter();
    if runtime_current_cpu()?.current() != Some(thread.id()) {
        return Err(TaskError::StaleThreadId);
    }
    let now_ns = task_runtime::monotonic_ns();
    let resolution_ns = task_runtime::timer_resolution_ns();
    let mut cpu = runtime_current_cpu_mut()?;
    let owner = cpu.owner();
    let node = thread.sleep_timer();
    let token = unsafe {
        // The registry and this strong handle retain ThreadCore until the timer
        // is physically cancelled or published to the safe-point buffer.
        cpu.as_mut().timer_queue().arm(node, deadline_ns)
    }
    .map_err(|_| TaskError::TimerCapacity)?;
    thread.core.register_sleep_timer(owner, token.generation());
    let next = cpu
        .as_mut()
        .timer_queue()
        .next_deadline_ns(now_ns, resolution_ns);
    if let Some(next) = next {
        let status = task_runtime::program_oneshot_timer(next);
        if status != crate::runtime::RuntimeStatus::Success {
            let _removed = cpu.as_mut().timer_queue().cancel(node, token);
            thread.core.complete_sleep_timer(token.generation());
            return Err(TaskError::RuntimeFailure(status as u32));
        }
    }
    Ok(token)
}

pub(crate) fn cancel_current_sleep_timer(
    thread: &ThreadHandle,
    token: TimerToken,
) -> Result<bool, TaskError> {
    let _irq = RuntimeIrqGuard::enter();
    let actual = runtime_current_cpu()?.owner();
    let Some(expected) = thread.core.sleep_timer_cpu_for(token.generation()) else {
        return Ok(false);
    };
    if actual != expected {
        return Err(TaskError::CpuOwnerMismatch {
            expected: expected.as_u32(),
            actual: actual.as_u32(),
        });
    }
    let mut cpu = runtime_current_cpu_mut()?;
    let removed = cpu
        .as_mut()
        .timer_queue()
        .cancel(thread.sleep_timer(), token);
    thread.core.complete_sleep_timer(token.generation());
    Ok(removed)
}

/// Bounded timer IRQ result used by the runtime to reprogram one-shot time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerInterruptOutcome {
    slice_expired: bool,
    deadline_overrun: bool,
    expired: usize,
    pending: bool,
    next_deadline_ns: Option<u64>,
}

impl TimerInterruptOutcome {
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
    /// Returns the next representable one-shot deadline.
    pub const fn next_deadline_ns(self) -> Option<u64> {
        self.next_deadline_ns
    }
}

/// Registers uncontended ownership of a kernel PI mutex.
pub fn pi_mutex_acquired(lock: PiLockId, owner: ThreadId) -> Result<(), TaskError> {
    runtime_task_system()?.pi_mutex_acquired(lock, owner)
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
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut()?;
    if cpu.current() != Some(token.waiter()) {
        return Err(TaskError::InvalidPiState);
    }
    let now_ns = task_runtime::monotonic_ns();
    system.drain_policy_updates(cpu.as_mut(), now_ns)?;
    if token.is_granted() {
        return Ok(());
    }
    let park = match system.prepare_park(cpu.as_mut())? {
        ParkPrepare::Notified => return Ok(()),
        ParkPrepare::Prepared(park) => park,
    };
    if token.is_granted() {
        system.cancel_park(cpu.as_mut(), park)?;
        return Ok(());
    }
    let decision = match system.commit_park(cpu.as_mut(), park)? {
        ParkCommit::Notified => return Ok(()),
        ParkCommit::Blocked(decision) => decision,
    };
    execute_switch_plan(decision, now_ns);
    if token.is_granted() {
        Ok(())
    } else {
        Err(TaskError::InvalidPiState)
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

/// Runs one scheduler decision at a task/IRQ-return safe point.
///
/// The function returns `Ok(None)` when no sticky request is pending. It never
/// clears `need_resched` before entering [`TaskSystem::schedule`].
///
/// # Errors
///
/// Returns [`TaskError::UnsafeContext`] in hard IRQ context and object-handle
/// errors when runtime initialization is incomplete or inconsistent.
pub fn schedule_current_cpu() -> Result<Option<ScheduleDecision>, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _scheduler_frame = RuntimeSchedulerFrameGuard::enter();
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    drain_current_expired_timers(system)?;
    let mut cpu = runtime_current_cpu_mut()?;
    let _reclaimed = system.drain_deferred_reclaims(cpu.batch_limit())?;
    if !cpu.needs_reschedule() {
        return Ok(None);
    }
    let now_ns = task_runtime::monotonic_ns();
    system.drain_remote_wakes(cpu.as_mut(), now_ns)?;
    system.drain_policy_updates(cpu.as_mut(), now_ns)?;
    let decision = system.schedule_if_requested(cpu.as_mut(), now_ns)?;
    system.dispatch_deadline_overruns(cpu.batch_limit());
    let Some(decision) = decision else {
        return Ok(None);
    };
    execute_switch_plan(decision, now_ns);
    Ok(Some(decision))
}

fn drain_current_expired_timers(system: &TaskSystem) -> Result<usize, TaskError> {
    let mut drained = 0;
    while let Some(event) = runtime_current_cpu_mut()?.take_thread_expired_timer() {
        let Some(thread) = event.owner_thread() else {
            continue;
        };
        match system.thread_handle(thread) {
            Ok(handle) => {
                handle.core.complete_sleep_timer(event.token().generation());
                let _wake_result = handle.wake_handle().wake();
            }
            Err(TaskError::StaleThreadId) => {}
            Err(error) => return Err(error),
        }
        drained += 1;
    }
    Ok(drained)
}

/// Yields the calling thread and executes the resulting context switch.
pub fn yield_current_cpu() -> Result<ScheduleDecision, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut()?;
    let now_ns = task_runtime::monotonic_ns();
    system.drain_policy_updates(cpu.as_mut(), now_ns)?;
    let decision = system.yield_current(cpu.as_mut(), now_ns)?;
    execute_switch_plan(decision, now_ns);
    Ok(decision)
}

/// Exits the calling thread and switches to its replacement.
pub fn exit_current_thread() -> Result<(), TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let _irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut()?;
    let now_ns = task_runtime::monotonic_ns();
    let decision = system.exit_current(cpu.as_mut())?;
    execute_switch_plan(decision, now_ns);
    // An exited context is never re-enqueued, so returning here indicates a
    // broken architecture switch contract.
    task_runtime::fatal_invariant(4, decision.previous().map_or(0, ThreadId::as_u64) as usize)
}

fn execute_switch_plan(decision: ScheduleDecision, now_ns: u64) {
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
    let _switch_guard = RuntimeSchedulerFrameGuard::enter();
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
    task_runtime::trace_sched_switch(SchedSwitchRecord {
        cpu: RuntimeCpuId::new(task_runtime::current_cpu_id().as_u32()),
        previous_thread: previous.thread().as_u64(),
        next_thread: next.thread().as_u64(),
        timestamp_ns: now_ns,
        reason: decision.switch_reason() as u32,
    });
    // SAFETY: the scheduler committed both endpoint states before releasing its
    // locks. Runtime handles remain live, and local IRQs stay disabled here.
    unsafe { task_runtime::switch_context(previous.context(), next.context()) };
    if complete_current_context_switch_tail().is_err() {
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

fn complete_current_context_switch_tail() -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let cpu = runtime_current_cpu_mut()?;
    let batch_limit = cpu.batch_limit();
    system.complete_context_switch(cpu)?;
    system.reap_unreferenced_exited(batch_limit)?;
    Ok(())
}

pub(crate) fn runtime_task_system() -> Result<&'static TaskSystem, TaskError> {
    let handle = task_runtime::task_system_handle();
    let raw = handle.into_raw();
    validate_handle::<TaskSystem>(raw)?;
    // SAFETY: TaskRuntime requires this handle to identify a pinned TaskSystem
    // that remains live until shutdown. The scheduler's mutable state is behind
    // its internal IRQ ticket lock, so creating this shared reference aliases no
    // unprotected mutable access.
    Ok(unsafe { &*ptr::with_exposed_provenance::<TaskSystem>(raw) })
}

fn runtime_current_cpu() -> Result<&'static CpuLocal, TaskError> {
    let handle = task_runtime::current_cpu_local_handle();
    let raw = handle.into_raw();
    validate_handle::<CpuLocal>(raw)?;
    // SAFETY: TaskRuntime publishes only pinned CpuLocal objects with shutdown
    // lifetime. Shared fields accessed by this facade are atomic or immutable.
    let cpu = unsafe { &*ptr::with_exposed_provenance::<CpuLocal>(raw) };
    validate_cpu_owner(cpu)?;
    Ok(cpu)
}

pub(crate) fn runtime_current_cpu_mut() -> Result<Pin<&'static mut CpuLocal>, TaskError> {
    let handle = task_runtime::current_cpu_local_handle();
    let raw = handle.into_raw();
    validate_handle::<CpuLocal>(raw)?;
    // SAFETY: the runtime's current-CPU handle is pinned and valid to shutdown.
    // `schedule_current_cpu` holds a nested local IRQ guard and is restricted to
    // scheduler safe points, providing unique owner-CPU access for this borrow.
    let cpu = unsafe { &mut *ptr::with_exposed_provenance_mut::<CpuLocal>(raw) };
    validate_cpu_owner(cpu)?;
    // SAFETY: CpuLocal is allocated pinned by construction and runtime handles
    // never permit moving it.
    Ok(unsafe { Pin::new_unchecked(cpu) })
}

pub(crate) fn cpu_local_for_wake(cpu: crate::CpuId) -> Option<&'static CpuLocal> {
    let handle = task_runtime::cpu_local_handle(crate::runtime::RuntimeCpuId::new(cpu.as_u32()));
    let raw = handle.into_raw();
    if validate_handle::<CpuLocal>(raw).is_err() {
        return None;
    }
    // SAFETY: TaskRuntime guarantees every published per-CPU handle is pinned
    // and remains valid until shutdown. Producers access only atomics/inboxes.
    let cpu = unsafe { &*ptr::with_exposed_provenance::<CpuLocal>(raw) };
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

impl Drop for RuntimeIrqGuard {
    fn drop(&mut self) {
        // SAFETY: this guard consumes its same-CPU token exactly once.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}

struct RuntimeSchedulerFrameGuard {
    _not_send: PhantomData<*mut ()>,
}

impl RuntimeSchedulerFrameGuard {
    fn enter() -> Self {
        task_runtime::scheduler_frame_guard_enter();
        Self {
            _not_send: PhantomData,
        }
    }
}

impl Drop for RuntimeSchedulerFrameGuard {
    fn drop(&mut self) {
        task_runtime::scheduler_frame_guard_exit();
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{
        CpuId, SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadSpec,
        runtime::AddressSpaceHandle, test_runtime,
    };

    static ORDERING_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: assert_address_space_installed,
        on_switch_out: ignore_switch_out,
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
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_ref());
        let ParkPrepare::Prepared(park) = prepare_current_park().unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let timer = arm_current_sleep_timer(&running, 0).unwrap();

        assert_eq!(timer_interrupt_current_cpu(0, 0).unwrap().expired(), 1);
        assert_eq!(
            drain_current_expired_timers(system.as_ref().get_ref()).unwrap(),
            1
        );
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
        assert!(
            schedule_current_cpu().unwrap().is_none(),
            "timer IRQ-return scheduling must defer until the park token commits"
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking
        );
        assert!(system.snapshot(cpu.as_ref()).need_resched());

        commit_current_park(park).unwrap();
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        assert!(system.snapshot(cpu.as_ref()).need_resched());
        assert!(!cancel_current_sleep_timer(&running, timer).unwrap());
    }

    #[test]
    fn scheduler_frame_guard_covers_work_before_the_context_switch() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_ref());
        test_runtime::reset_scheduler_frame_state();

        let _decision = schedule_current_cpu().unwrap();

        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 1),
            "IRQ-off scheduler preparation must run inside one non-recursive frame"
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

    unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn ignore_drop(_data: usize) {}

    struct InstalledTaskHandles;

    impl InstalledTaskHandles {
        fn new(system: Pin<&TaskSystem>, cpu: Pin<&CpuLocal>) -> Self {
            test_runtime::install_task_handles(
                (system.get_ref() as *const TaskSystem).expose_provenance(),
                (cpu.get_ref() as *const CpuLocal).expose_provenance(),
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
