//! Operations bound to the calling scheduler thread.

use alloc::sync::Arc;
use core::marker::PhantomData;

pub use crate::{
    runtime::switch::dispatch::{
        ExitPermit, commit_current_exit, exit_current_thread, prepare_current_exit,
        yield_current_cpu,
    },
    sync::wait_queue::{sleep, sleep_until},
    thread::{
        current::park::{
            CurrentParkDisposition, CurrentParkResume, CurrentParkStart, PreparedCurrentPark,
            begin_current_park,
        },
        execution::exit_current,
    },
};
use crate::{
    runtime::{
        context::{
            RuntimeSchedulerFrameGuard, runtime_current_cpu_mut, runtime_task_system,
            validate_schedule_context,
        },
        switch::{RuntimeScheduleOrigin, RuntimeSchedulerEntry, dispatch::execute_switch_plan},
        task_runtime,
    },
    sched::CpuSet,
    thread::{
        CurrentThreadToken, TaskError, ThreadCore, ThreadExtensionLease, ThreadHandle, ThreadId,
    },
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
    crate::diagnostics::counters::record_current_thread_handle_query();
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
pub(crate) fn current_thread_identity()
-> Result<crate::runtime::switch::ThreadIdentityV1, TaskError> {
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

pub(crate) fn current_thread_publication()
-> Result<crate::runtime::switch::CurrentThreadPublication, TaskError> {
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

pub(crate) fn current_thread_core_arc() -> Result<Arc<ThreadCore>, TaskError> {
    let publication = current_thread_publication()?;
    // SAFETY: the runtime publication belongs to this architecture context.
    // The returned Arc is scheduler-internal and remains in the synchronous
    // current-thread operation; it does not acquire an external lease.
    unsafe { publication.acquire_scheduler_core() }
}

/// Validates that the caller may publish a waiter or block its current thread.
///
/// Sleeping synchronization primitives should call this before changing any
/// waiter, owner, donation, or thread-lifecycle state.
pub fn validate_blocking_context() -> Result<(), TaskError> {
    acquire_blocking_permit().map(|_| ())
}

/// RT-lock contention may schedule inside another RT critical section.
pub(crate) fn validate_rt_lock_context() -> Result<(), TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Block)
}

pub(crate) fn validate_sleeping_lock_context() -> Result<(), TaskError> {
    if crate::runtime::task_runtime::in_hard_irq() || current_thread_core_arc()?.holds_rt_lock() {
        return Err(TaskError::UnsafeContext);
    }
    Ok(())
}

/// One validated opportunity to publish a blocking handshake.
pub(crate) struct BlockingPermit {
    _not_send: PhantomData<*mut ()>,
}

pub(crate) fn acquire_blocking_permit() -> Result<BlockingPermit, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Block)?;
    let current = current_thread_core_arc()?;
    if current.holds_rt_lock() && !current.in_rt_lock_wait() {
        return Err(TaskError::UnsafeContext);
    }
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
    let current = scheduler_frame.current_thread_ref()?;
    let system = scheduler_frame.task_system();
    let mut outcome = {
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
        unsafe { system.yield_current_in_scheduler_frame(cpu.as_mut()) }.unwrap_or_else(|_| {
            // Affinity publication cannot be rolled back safely after another CPU
            // may have observed the migration target. Scheduler commit failures are
            // therefore runtime invariants, like failures after exit publication.
            task_runtime::fatal_invariant(0x4558_0021, current.id().as_u64() as usize);
        })
    };
    let decision = outcome.decision_mut().unwrap_or_else(|| {
        task_runtime::fatal_invariant(0x4558_0022, current.id().as_u64() as usize)
    });
    execute_switch_plan(&mut scheduler_frame, decision);
    Ok(())
}
pub(crate) mod park;
