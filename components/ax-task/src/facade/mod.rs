//! Runtime-backed scheduler capabilities for crates below `ax-runtime`.

use alloc::{string::String, sync::Arc};
use core::{marker::PhantomData, mem::align_of, ops::Deref, pin::Pin, ptr};

use crate::{
    CpuId, CpuLocal, CpuLocalOwnerBorrow, CpuRemote, CpuSet, CurrentExitPermit, CurrentThreadToken,
    IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, IrqWaitToken, ParkCommit, ParkPrepare,
    PiMutexClaimOutcome, PiMutexLockResult, PiMutexRef, PiWaitCancelOutcome, PiWaitToken,
    RtPriority, ScheduleDecision, SchedulePolicy, SchedulerOutcome, SchedulerRequestScope,
    TaskError, TaskSystem, ThreadBuilder, ThreadCore, ThreadExtensionLease, ThreadHandle, ThreadId,
    ThreadRuntimeSnapshot, ThreadState, ThreadWakeHandle, WaitQueue, WaitWakeClaim,
    WaitWakeDelivery, WakeResult,
    executor::CoroutineHeader,
    inbox::PublishResult,
    lock::PreemptScope,
    runtime::{
        CurrentThreadRef, IrqGuardToken, MonotonicDeadline, MonotonicInstant, RuntimeCpuId,
        RuntimeScheduleOrigin, RuntimeSchedulerEntry, RuntimeSchedulerReturn, RuntimeStatus,
        SchedSwitchRecord, task_runtime,
    },
    timer::{
        HardKernelTimerCallback, KernelTimerCallback, KernelTimerCancelOutcome, KernelTimerEntry,
        KernelTimerHandle, RestartableKernelTimerCallback, TaskDeadlineError, TaskDeadlineKind,
    },
};

mod deadline;
mod irq_worker;
mod kernel_timer;
mod ktimer;
#[cfg(feature = "lockdep")]
mod lockdep;
mod membarrier;
mod pi;
mod runtime_cpu;
mod scheduling;
mod task_work;

pub use deadline::{
    ClaimedSchedulerDeadlines, CurrentParkDisposition, CurrentParkResume, CurrentParkStart,
    PreparedCurrentPark, SchedulerTickStamp, TaskClockEventOutcome, begin_current_park,
    on_clock_event, publish_scheduler_tick,
};
pub(crate) use deadline::{
    begin_current_park_with_permit, cancel_current_park, commit_current_park,
};
pub use irq_worker::IrqWorkerWaiter;
pub use kernel_timer::{
    arm_hard_kernel_timer, cancel_kernel_timer, disarm_hard_kernel_timer,
    register_hard_restartable_kernel_timer, register_kernel_timer,
    register_restartable_kernel_timer,
};
pub use ktimer::start_current_ktimer_service;
#[cfg(feature = "lockdep")]
pub(crate) use lockdep::{
    collect_current_task_held_locks, pop_current_task_held_lock, push_current_task_held_lock,
};
pub use membarrier::{
    MembarrierCommand, membarrier, refresh_current_membarrier_run_queue,
    register_current_membarrier,
};
pub(crate) use pi::cancel_prepared_pi_park;
pub use pi::{
    pi_drop_wait_handle, pi_initial_owner_is_on_cpu, pi_mutex_claim, pi_mutex_lock_slow,
    pi_mutex_release_owned, pi_park_current_once, pi_wait_cancel, pi_wait_try_cancel,
    pi_waiter_is_granted, pi_waiter_is_top,
};
use runtime_cpu::{
    RuntimeCpuPin, RuntimeSchedulerFrameGuard, runtime_current_cpu, validate_schedule_context,
    validate_task_context,
};
pub(crate) use runtime_cpu::{
    RuntimeIrqGuard, current_cpu_remote, runtime_current_cpu_mut, runtime_task_system,
    wake_thread_from_current_cpu, wake_wait_claim_from_task,
};
pub use scheduling::{
    ExitPermit, commit_current_exit, exit_current_thread, prepare_current_exit,
    schedule_current_cpu, schedule_current_cpu_from_irq_guard_exit,
    schedule_current_cpu_from_preempt_exit, yield_current_cpu,
};
use scheduling::{complete_current_context_switch_tail, execute_switch_plan};
pub(crate) use task_work::publish_deferred_coroutine_reclaim;
pub use task_work::{
    notify_address_space_reclaim, quiesce_irq_wait, start_deferred_task_work_service,
};

/// Returns a strong handle for the calling scheduler thread.
///
/// # Errors
///
/// Returns [`TaskError::NotInitialized`] before runtime CPU publication,
/// [`TaskError::CpuOwnerBorrowed`] for a reentrant owner query, or
/// [`TaskError::NoRunnableThread`] before a current thread is installed.
pub fn current_thread_handle() -> Result<ThreadHandle, TaskError> {
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_current_thread_handle_query();
    let publication = current_thread_publication()?;
    // SAFETY: the scheduler retains the executing task's owner-side Arc across
    // preemption and migration until this synchronous operation returns.
    unsafe { publication.acquire_handle() }
}

/// Returns the generation-bearing identity of the calling scheduler thread.
#[inline(always)]
pub fn current_thread_id() -> Result<ThreadId, TaskError> {
    let identity = current_thread_identity()?;
    Ok(ThreadId::from_parts(identity.slot, identity.generation))
}

/// Captures the scheduler thread executing this task context.
#[inline(always)]
pub fn current_thread_token() -> Result<CurrentThreadToken, TaskError> {
    Ok(CurrentThreadToken::new(current_thread_id()?))
}

#[inline(always)]
fn current_thread_identity() -> Result<crate::runtime::ThreadIdentityV1, TaskError> {
    let identity = task_runtime::current_thread_identity();
    if identity.is_bound() {
        return Ok(identity);
    }

    let publication = task_runtime::current_thread_publication();
    if publication.identity() != identity || !publication.owner().is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    // Preserve the public distinction between a runtime that has not installed
    // its task system and an initialized bootstrap context without a current
    // scheduler thread. Bound task contexts never enter this cold path.
    let _system = runtime_task_system()?;
    Err(TaskError::NoRunnableThread)
}

/// Returns the scheduler-selected logical address space of the current task.
///
/// This low-level runtime query is intended for the final user-entry
/// validation. The returned opaque handle does not transfer ownership.
#[doc(hidden)]
pub fn current_address_space_handle() -> Result<crate::runtime::AddressSpaceHandle, TaskError> {
    let current = current_thread_id()?;
    let mut irq = RuntimeIrqGuard::enter();
    let cpu = runtime_current_cpu_mut(&mut irq)?;
    // SAFETY: `irq` owns the IRQ-off owner-CPU scope and the architecture
    // current publication proved `current` belongs to this execution context.
    unsafe { cpu.scheduler_current_address_space(current) }
}

fn current_thread_publication() -> Result<crate::runtime::CurrentThreadPublication, TaskError> {
    let publication = task_runtime::current_thread_publication();
    let identity = publication.identity();
    if !identity.is_bound() {
        if !publication.owner().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        // Preserve the public distinction between a runtime that has not
        // installed its task system and an initialized bootstrap context that
        // has not published a scheduler thread. This cold error path does not
        // add a handle lookup to the bound-current fast path.
        let _system = runtime_task_system()?;
        return Err(TaskError::NoRunnableThread);
    }
    if publication.owner().is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(publication)
}

fn current_thread_ref() -> Result<CurrentThreadRef, TaskError> {
    let publication = current_thread_publication()?;
    // SAFETY: the runtime publication was selected from this architecture
    // context. The non-Send borrow remains inside one synchronous facade
    // operation and the operation cannot exit the current thread.
    unsafe { publication.borrow_current() }
}

fn current_thread_core_arc() -> Result<Arc<ThreadCore>, TaskError> {
    let publication = current_thread_publication()?;
    // SAFETY: the runtime publication belongs to this architecture context.
    // The returned Arc is scheduler-internal and remains in the synchronous
    // current-thread operation; it does not acquire an external lease.
    unsafe { publication.acquire_scheduler_core() }
}

/// Tests the current CPU's sticky reschedule request while migration is pinned.
///
/// # Safety
///
/// The caller must prevent migration until it has finished the decision that
/// uses this snapshot. Sleeping-lock owner spinning normally satisfies this
/// with a preemption guard.
pub unsafe fn current_needs_reschedule_pinned() -> Result<bool, TaskError> {
    Ok(current_cpu_remote()
        .ok_or(TaskError::NotInitialized)?
        .needs_reschedule())
}

/// Tests only scheduler work consumed by kernel preempt-enable/IRQ return.
///
/// # Safety
///
/// The caller must prevent migration until it has finished the decision that
/// uses this snapshot.
pub unsafe fn current_needs_immediate_scheduler_work_pinned() -> Result<bool, TaskError> {
    Ok(current_cpu_remote()
        .ok_or(TaskError::NotInitialized)?
        .needs_immediate_scheduler_work())
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
/// token remains scheduler-owned and is returned so the runtime can defer its
/// task-context reclamation after leaving that IRQ-off transaction.
pub fn replace_current_address_space(
    address_space: &mut crate::runtime::AddressSpaceToken,
) -> Result<crate::runtime::AddressSpaceToken, TaskError> {
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.replace_current_address_space(cpu.as_mut(), address_space)
}

/// Detaches the current thread's scheduler-visible user address space.
///
/// The runtime must enter its lazy kernel address-space state before the outer
/// IRQ-off transaction ends, then transfer the returned token to task-context
/// reclamation.
pub fn detach_current_address_space() -> Result<crate::runtime::AddressSpaceToken, TaskError> {
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.detach_current_address_space(cpu.as_mut())
}

/// Transfers an obsolete address-space token to task-context reclamation.
///
/// The runtime may still report the object busy while another CPU retains it
/// as an active mm. The task-work reaper owns every retry after this function
/// accepts the token.
pub fn release_address_space_token(
    address_space: crate::runtime::AddressSpaceToken,
) -> Result<(), TaskError> {
    validate_task_context()?;
    runtime_task_system()?.release_address_space_token(address_space);
    Ok(())
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
    runtime_task_system()?.thread_runtime(thread)
}

/// Returns cumulative non-idle runtime charged by one online CPU.
pub fn cpu_busy_runtime_ns(cpu: CpuId) -> Result<u64, TaskError> {
    runtime_task_system()?.cpu_busy_runtime_ns(cpu)
}

/// Returns successful runtime owner claims observed by one online CPU.
#[cfg(feature = "qperf-metrics")]
pub fn qperf_cpu_owner_claims(cpu: CpuId) -> Result<u64, TaskError> {
    runtime_task_system()?.qperf_cpu_owner_claims(cpu)
}

/// Returns the fixed topology width accepted by scheduler affinity masks.
pub fn cpu_topology_len() -> Result<usize, TaskError> {
    Ok(runtime_task_system()?.cpu_topology_len())
}

/// Returns the CPUs that currently accept runnable placement.
///
/// Unlike [`cpu_topology_len`], this snapshot excludes possible CPUs that have
/// not completed scheduler online publication or no longer accept new work.
pub fn active_cpu_set() -> Result<CpuSet, TaskError> {
    validate_task_context()?;
    Ok(runtime_task_system()?.active_cpu_set())
}

/// Updates a thread scheduling policy through its owner CPU.
///
/// # Errors
///
/// Returns [`TaskError::UnsafeContext`] in hard IRQ context and propagates
/// policy validation, Deadline admission, identity, and CPU publication
/// failures.
pub fn set_thread_policy(thread: ThreadId, policy: SchedulePolicy) -> Result<(), TaskError> {
    validate_task_context()?;
    runtime_task_system()?.set_thread_policy(thread, policy)
}

/// Returns a copy of a thread's CPU affinity.
pub fn thread_affinity(thread: ThreadId) -> Result<CpuSet, TaskError> {
    runtime_task_system()?.thread_affinity(thread)
}

/// Updates a thread CPU affinity after Deadline root-domain validation.
pub fn set_thread_affinity(thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
    validate_task_context()?;
    runtime_task_system()?.set_affinity(thread, affinity)
}

/// Updates a remote thread's affinity and waits for owner-runqueue completion.
///
/// A successful return guarantees that this update was ordered through the
/// target's owner runqueue. If no later setter superseded it, the target no
/// longer executes on, is queued on, or has an in-flight transfer to a CPU
/// excluded by this affinity. Setters that join the same outstanding owner
/// transition share the target's monotonically increasing completion sequence.
pub fn set_thread_affinity_and_wait(thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
    if current_thread_id()? == thread {
        return set_current_thread_affinity(affinity);
    }
    validate_blocking_context()?;
    runtime_task_system()?
        .request_affinity(thread, affinity)?
        .wait()
}

/// Updates the calling thread's affinity and completes a required migration.
///
/// A successful return guarantees that the caller is executing on a CPU in
/// the new mask. Generic remote-thread affinity updates remain asynchronous and
/// are completed by the remote owner's next scheduler safe point.
pub fn set_current_thread_affinity(affinity: CpuSet) -> Result<(), TaskError> {
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let current = current_thread_ref()?;
    let system = runtime_task_system()?;
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        let must_migrate = system.set_current_affinity(cpu.as_mut(), affinity)?;
        if !must_migrate {
            return Ok(());
        }

        // The new mask is now visible and excludes this CPU. Keep the scheduler
        // baton and raw IRQ mask continuously owned until this context has moved;
        // exposing an IRQ-enabled validation window here could let IRQ-return
        // scheduling migrate the caller between publishing the mask and yielding.
        // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
        unsafe { system.yield_current_in_scheduler_frame(cpu.as_mut(), &current) }.unwrap_or_else(
            |_| {
                // Affinity publication cannot be rolled back safely after another CPU
                // may have observed the migration target. Scheduler commit failures are
                // therefore runtime invariants, like failures after exit publication.
                task_runtime::fatal_invariant(0x4558_0021, current.id().as_u64() as usize);
            },
        )
    };
    execute_switch_plan(&mut scheduler_frame, decision);
    Ok(())
}

/// Tests the sticky reschedule state of the calling CPU.
pub fn current_cpu_needs_resched() -> Result<bool, TaskError> {
    let _pin = PreemptScope::enter();
    // SAFETY: `_pin` prevents migration through the remote reschedule-state
    // observation. Stronger IRQ/scheduler owner scopes are inherited.
    unsafe { current_needs_reschedule_pinned() }
}

/// Clears the current CPU's idle-polling state at the runtime sleep boundary.
///
/// # Safety
///
/// The runtime must have disabled local interrupts and must prevent migration
/// through the immediately following sticky-work and clockevent recheck. This
/// is Linux's `current_clr_polling_and_test()` boundary: work published before
/// the clear is found by that recheck, while work published afterwards must
/// own a physical interrupt edge.
#[doc(hidden)]
pub unsafe fn finish_current_cpu_idle_polling() -> Result<(), TaskError> {
    let remote = current_cpu_remote().ok_or(TaskError::NotInitialized)?;
    remote.finish_idle_wait();
    Ok(())
}

/// Executes one lossless idle publication/recheck/WFI iteration.
pub fn idle_current_cpu_once() -> Result<(), TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    let may_wait = {
        let cpu = runtime_current_cpu()?;
        cpu.prepare_idle_wait()
    };
    if may_wait {
        task_runtime::wait_for_interrupt();
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
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    // SAFETY: this trampoline inherits the transferred scheduler baton and
    // the runtime IRQ guard retains its raw IRQ-off state through completion.
    unsafe { complete_current_context_switch_tail(&mut irq)? };
    drop(irq);
    task_runtime::finish_initial_context_switch();
    Ok(())
}
