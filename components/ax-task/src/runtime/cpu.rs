//! Pinned CPU capabilities and scheduler observations.

pub use crate::{
    runtime::clock::{RqClockSample, SchedulerDeadlineUpdate, SchedulerRuntimeDeadline},
    sched::system::{
        CpuLifecycleState, CpuLoadSummary, CpuLocal, CpuLocalOwnerBorrow, CpuRemote, CpuSnapshot,
    },
};
use crate::{
    runtime::{
        context::{current_cpu_remote, runtime_current_cpu, validate_schedule_context},
        lock::PreemptScope,
        switch::RuntimeScheduleOrigin,
        task_runtime,
    },
    thread::TaskError,
};

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

/// Tests the sticky reschedule state of the calling CPU.
pub fn current_cpu_needs_resched() -> Result<bool, TaskError> {
    let _pin = PreemptScope::enter();
    // SAFETY: `_pin` prevents migration through the remote reschedule-state
    // observation. Stronger IRQ/scheduler owner scopes are inherited.
    unsafe { current_needs_reschedule_pinned() }
}

/// Observes only the immediate preemption bit in real-runtime regression tests.
/// Owner maintenance and lazy preemption remain separate scheduler requests.
#[cfg(feature = "fault-injection")]
pub fn current_immediate_preemption_requested() -> Result<bool, TaskError> {
    let _pin = PreemptScope::enter();
    Ok(current_cpu_remote()
        .ok_or(TaskError::NotInitialized)?
        .immediate_preemption_requested())
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
use crate::runtime::handle::opaque_handle;

opaque_handle!(
    /// Opaque address of the current CPU's pinned owner-only scheduler object.
    ///
    /// Consumers must claim the corresponding [`crate::runtime::cpu::CpuRemote`] owner gate
    /// before reconstructing any reference from this address.
    CurrentCpuLocalHandle,
    "runtime::cpu"
);
opaque_handle!(
    /// Opaque pointer-sized handle to one Arc-backed remote CPU endpoint.
    ///
    /// Remote and owner-only CPU handles are intentionally not interchangeable:
    ///
    /// ```compile_fail
    /// use ax_task::runtime::cpu::{CpuRemoteHandle, CurrentCpuLocalHandle};
    ///
    /// fn borrow_owner(_handle: CurrentCpuLocalHandle) {}
    /// borrow_owner(CpuRemoteHandle::NONE);
    /// ```
    CpuRemoteHandle,
    "runtime::cpu"
);
opaque_handle!(
    /// Token returned by the nested IRQ guard service.
    IrqGuardToken,
    "runtime::cpu"
);
opaque_handle!(
    /// Token returned by the nested task-preemption guard service.
    PreemptGuardToken,
    "runtime::cpu"
);

/// Runtime-defined raw local-IRQ state saved by a synchronization guard.
///
/// Unlike [`IrqGuardToken`], this value does not own a scheduler publication
/// scope. It only transports the architecture interrupt state back to the
/// runtime that produced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct LocalIrqState(usize);

impl LocalIrqState {
    /// Creates a saved local-IRQ state at the runtime provider boundary.
    ///
    /// # Safety
    ///
    /// `raw` must be a state value accepted by the linked runtime's matching
    /// local-IRQ restore operation.
    pub const unsafe fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns the runtime-owned representation of this saved state.
    pub const fn into_raw(self) -> usize {
        self.0
    }
}

/// Logical CPU identifier exchanged with the operating-system runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RuntimeCpuId(u32);

impl RuntimeCpuId {
    /// Creates a logical CPU identifier.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric logical CPU identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Runtime-owned capability snapshot for one pinned scheduler CPU.
///
/// The paired fields are captured in one runtime operation, mirroring Linux's
/// direct `this_rq()` lookup. The remote endpoint is the sole owner identity;
/// its embedded CPU ID prevents a second architecture or registry lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CurrentCpuOwnerHandles {
    local: CurrentCpuLocalHandle,
    remote: CpuRemoteHandle,
}

impl CurrentCpuOwnerHandles {
    /// Empty capability used when a scheduler-frame entry is rejected.
    pub const NONE: Self = Self {
        local: CurrentCpuLocalHandle::NONE,
        remote: CpuRemoteHandle::NONE,
    };

    /// Creates one pinned current-CPU capability snapshot.
    ///
    /// # Safety
    ///
    /// `local` and `remote` must identify the paired owner-only and Arc-backed
    /// scheduler endpoints for the pinned CPU. Every non-empty handle must
    /// remain live until shutdown, and the caller must keep migration excluded
    /// while the snapshot is used.
    pub const unsafe fn new(local: CurrentCpuLocalHandle, remote: CpuRemoteHandle) -> Self {
        Self { local, remote }
    }

    /// Returns the current CPU's owner-only scheduler handle.
    pub const fn local(self) -> CurrentCpuLocalHandle {
        self.local
    }

    /// Returns the current CPU's Arc-backed remote endpoint.
    pub const fn remote(self) -> CpuRemoteHandle {
        self.remote
    }
}

pub use crate::sched::system::OwnerControlDrain;

/// Failed prerequisite observed by the serial real-idle test probe.
#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum IdleOfflineRejection {
    /// No instrumented prerequisite failed.
    Unclassified         = 0,
    /// A publisher still owns the placement endpoint.
    PlacementPublication = 1,
    /// A thread cannot leave this CPU's placement domain.
    ThreadTarget         = 2,
    /// Owner-directed delivery has not relinquished publication.
    OwnerPublication     = 3,
    /// Runqueue, timer, handoff or remote work remains.
    CpuState             = 4,
    /// A thread retains CPU ownership or a migration pin.
    ThreadOwnership      = 5,
}

#[cfg(feature = "fault-injection")]
static IDLE_OFFLINE_REJECTION: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

#[cfg(feature = "fault-injection")]
pub(crate) fn record_idle_offline_rejection(reason: IdleOfflineRejection) {
    IDLE_OFFLINE_REJECTION.store(reason as u8, core::sync::atomic::Ordering::Release);
}

/// Reads the last serial probe's rejection after its locks have been released.
#[cfg(feature = "fault-injection")]
pub fn idle_offline_rejection() -> IdleOfflineRejection {
    match IDLE_OFFLINE_REJECTION.load(core::sync::atomic::Ordering::Acquire) {
        1 => IdleOfflineRejection::PlacementPublication,
        2 => IdleOfflineRejection::ThreadTarget,
        3 => IdleOfflineRejection::OwnerPublication,
        4 => IdleOfflineRejection::CpuState,
        5 => IdleOfflineRejection::ThreadOwnership,
        _ => IdleOfflineRejection::Unclassified,
    }
}

/// Exercises scheduler CPU offline/online on the real idle owner.
///
/// This test-only transaction retains IRQ exclusion and the exclusive owner
/// borrow across both transitions. It never returns to scheduling while offline
/// and does not implement platform power-off or an externally parked CPU.
#[cfg(feature = "fault-injection")]
pub fn probe_idle_cpu_round_trip() -> Result<(), TaskError> {
    use crate::runtime::context::{RuntimeIrqGuard, runtime_current_cpu_mut, runtime_task_system};
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    if cpu.remote().current_thread() != cpu.remote().idle_thread() {
        return Err(TaskError::NotReady);
    }
    record_idle_offline_rejection(IdleOfflineRejection::Unclassified);
    system.take_cpu_offline(cpu.as_mut())?;
    assert_eq!(cpu.remote().lifecycle_state(), CpuLifecycleState::Offline);
    assert!(system.cpu_remote(cpu.owner()).is_none());
    // Returning an error here would strand the executing idle owner offline.
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("idle CPU re-online failed");
    assert_eq!(cpu.remote().lifecycle_state(), CpuLifecycleState::Online);
    Ok(())
}

/// Publishes ordinary owner work so an idle probe leaves NOHZ sleep.
#[cfg(feature = "fault-injection")]
pub fn notify_idle_cpu_probe(cpu: RuntimeCpuId) -> Result<(), TaskError> {
    let system = crate::runtime::context::runtime_task_system()?;
    let remote = system
        .cpu_remote(crate::sched::CpuId::new(cpu.as_u32()))
        .ok_or(TaskError::CpuOffline(cpu.as_u32()))?;
    if remote.kick_scheduler_work() {
        Ok(())
    } else {
        Err(TaskError::CpuOffline(cpu.as_u32()))
    }
}
