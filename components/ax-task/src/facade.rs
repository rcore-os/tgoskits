//! Runtime-backed scheduler facade for crates below `ax-runtime`.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{marker::PhantomData, mem::align_of, ops::Deref, pin::Pin, ptr};

use crate::{
    CpuId, CpuLocal, CpuLocalOwnerBorrow, CpuRemote, CpuSet, IrqRegisterResult,
    IrqUnregisterResult, IrqWaitCell, IrqWaitRegistration, IrqWaitToken, Nice, ParkCommit,
    ParkPrepare, PiLockId, PiMutexClaim, PiMutexHandoff, PiMutexRelease, PiWaitToken, RtPriority,
    ScheduleDecision, SchedulePolicy, SchedulerOutcome, TaskError, TaskSystem, ThreadBuilder,
    ThreadCore, ThreadExtensionLease, ThreadHandle, ThreadId, ThreadRuntimeSnapshot, ThreadState,
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

mod deadline;
mod pi;
mod runtime_cpu;
mod scheduling;
mod task_work;

pub use deadline::{
    TaskClockEventOutcome, on_clock_event, on_clock_event_with_scheduler_tick,
    take_current_expired_task_deadlines,
};
pub(crate) use deadline::{
    arm_current_park_deadline, cancel_current_park, cancel_current_park_deadline,
    commit_current_park, prepare_current_park,
};
pub use pi::{
    pi_block_current, pi_wait_cancel, pi_wait_start, pi_wait_start_pending, pi_wake,
    prepare_pi_mutex_claim, prepare_pi_mutex_handoff, prepare_pi_mutex_release,
};
use runtime_cpu::{
    RuntimeCpuPin, RuntimeSchedulerFrameGuard, runtime_current_cpu, validate_schedule_context,
    validate_task_context,
};
pub(crate) use runtime_cpu::{
    RuntimeIrqGuard, cpu_local_for_wake, current_cpu_remote, runtime_current_cpu_mut,
    runtime_task_system, try_wake_current_cpu_from_task,
};
pub use scheduling::{
    ExitPermit, commit_current_exit, exit_current_thread, prepare_current_exit,
    schedule_current_cpu, schedule_current_cpu_from_irq_guard_exit,
    schedule_current_cpu_from_preempt_exit, yield_current_cpu,
};
use scheduling::{complete_current_context_switch_tail, execute_switch_plan};
#[cfg(test)]
use scheduling::{
    drain_current_expired_timers, prepare_next_context, service_scheduler_safe_point_deadlines,
};
#[cfg(test)]
use task_work::{TaskWorkServiceAction, service_task_work_pass, task_work_service_action};
pub(crate) use task_work::{drain_deferred_reclaims, publish_deferred_reclaim};
pub use task_work::{quiesce_irq_wait, start_deferred_task_work_service};

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

/// Returns the calling scheduler thread while the caller retains a CPU pin.
///
/// This is the scheduler-adjacent fast path used by primitives that already
/// hold migration exclusion. It reads the generation-bearing current identity
/// from the CPU's remote publication endpoint without recursively entering the
/// IRQ-guarded mutable owner facade.
///
/// # Safety
///
/// The caller must prevent migration from before this call until it has
/// completed the local state transition associated with the returned identity.
/// Task-context callers normally satisfy this with a preemption guard or an
/// IRQ-aware metadata lock.
pub unsafe fn current_thread_id_pinned() -> Result<ThreadId, TaskError> {
    current_cpu_remote()
        .ok_or(TaskError::NotInitialized)?
        .current_thread()
        .ok_or(TaskError::NoRunnableThread)
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
    validate_task_context()?;
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
    validate_schedule_context(RuntimeScheduleOrigin::Yield)?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
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
        let now_ns = task_runtime::monotonic_ns();
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

/// Executes one lossless idle publication/recheck/WFI iteration.
pub fn idle_current_cpu_once() -> Result<(), TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    let may_wait = {
        let cpu = runtime_current_cpu()?;
        let may_wait = cpu.prepare_idle_wait();
        if may_wait {
            // Linux clears TIF_POLLING_NRFLAG before the architecture sleep
            // commit. Work published before this transition is observed by
            // the runtime's final IRQ-disabled recheck; work published after
            // it must create a physical interrupt edge.
            cpu.finish_idle_wait();
        }
        may_wait
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
    complete_current_context_switch_tail(&mut irq)?;
    drop(irq);
    task_runtime::finish_initial_context_switch();
    Ok(())
}

include!("facade/tests.rs");
