//! Runtime-backed owner thread and local hard-IRQ wake capability.

use alloc::{string::String, sync::Arc};
use core::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    time::Duration,
};

use ax_hal::irq::IrqError;
use thiserror::Error;

use super::{
    MaintenanceCauses, MaintenanceClosed, MaintenanceDrain, MaintenanceDrainError,
    MaintenanceLifecycle, MaintenanceLifecycleError, MaintenanceMailbox, MaintenancePublishResult,
    MaintenanceState,
};
use crate::task::{
    CpuId, CpuSet, CurrentCpuLease, FairMode, Nice, SchedulePolicy, TaskError, ThreadHandle,
    ThreadId, ThreadWakeHandle, WaitQueue, WakeResult, current_thread_handle, pin_current_cpu,
};

/// Maintenance-domain construction or owner validation error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MaintenanceError {
    /// Scheduler or runtime operation failed.
    #[error(transparent)]
    Task(#[from] TaskError),
    /// Lifecycle operation violated the registration/close protocol.
    #[error(transparent)]
    Lifecycle(#[from] MaintenanceLifecycleError),
    /// The requested owner drain batch violated the fixed latency bound.
    #[error(transparent)]
    Drain(#[from] MaintenanceDrainError),
    /// IRQ action registration, containment, or teardown failed.
    #[error("maintenance IRQ operation failed: {0:?}")]
    Irq(IrqError),
    /// A CPU outside the runtime topology was requested.
    #[error("maintenance CPU {cpu} is outside the {cpu_count}-CPU topology")]
    InvalidCpu {
        /// Requested logical CPU.
        cpu: usize,
        /// Number of represented logical CPUs.
        cpu_count: usize,
    },
    /// A session operation ran after the owner thread moved to another CPU.
    #[error("maintenance owner CPU changed from {expected} to {actual}")]
    WrongOwnerCpu {
        /// Registered CPU.
        expected: usize,
        /// Observed CPU.
        actual: usize,
    },
    /// A session operation was invoked by another scheduler thread.
    #[error("maintenance operation was invoked by a non-owner thread")]
    WrongOwnerThread,
    /// The owner attempted to extract a close proof before reaching Closed.
    #[error("maintenance close proof requires Closed, observed {0:?}")]
    CloseIncomplete(MaintenanceState),
    /// A run closure returned a close proof created by another domain.
    #[error("maintenance run closure returned a foreign close proof")]
    ForeignCloseProof,
}

impl From<IrqError> for MaintenanceError {
    fn from(error: IrqError) -> Self {
        Self::Irq(error)
    }
}

/// Failure to publish and wake from one hard IRQ callback.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalIrqWakeError {
    /// This capability is restricted to hard IRQ context.
    #[error("maintenance IRQ publication requires hard IRQ context")]
    NotHardIrq,
    /// The IRQ ran outside the CPU-local owner domain.
    #[error("maintenance IRQ ran on CPU {actual}, owner CPU is {expected}")]
    WrongCpu {
        /// Registered owner CPU.
        expected: usize,
        /// CPU handling the IRQ.
        actual: usize,
    },
    /// The close cutoff already rejected this publication.
    #[error("maintenance domain no longer accepts IRQ publications")]
    Closed,
    /// The wake header no longer names the registered owner thread.
    #[error("maintenance IRQ wake header does not match its owner thread")]
    OwnerIdentityMismatch,
    /// The scheduler wake target no longer names the leased owner CPU.
    #[error("maintenance IRQ wake target moved from CPU {expected} to {actual:?}")]
    OwnerPlacementMismatch {
        /// CPU protected by the maintenance owner's placement lease.
        expected: usize,
        /// Scheduler target observed in the stable wake header.
        actual: Option<usize>,
    },
    /// The snapshot was accepted, but its owner can no longer make progress.
    #[error("maintenance event was {publication:?}, but owner wake returned {wake:?}")]
    OwnerUnavailable {
        /// Mailbox outcome that already linearized before the failed wake.
        publication: MaintenancePublishResult,
        /// Direct scheduler wake outcome.
        wake: WakeResult,
    },
}

/// Ordinary task-context publication failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MaintenanceSubmitError {
    /// Hard IRQ callers must use [`LocalIrqWake`].
    #[error("remote maintenance submission is task-context-only")]
    HardIrqContext,
    /// The owner already closed publication admission.
    #[error("maintenance domain no longer accepts requests")]
    Closed,
    /// The request was accepted but its owner can no longer make progress.
    #[error("maintenance request was {publication:?}, but owner wake returned {wake:?}")]
    OwnerUnavailable {
        /// Mailbox result already committed before wake failure.
        publication: MaintenancePublishResult,
        /// Direct scheduler wake outcome.
        wake: WakeResult,
    },
}

/// Result of waiting for maintenance evidence until an absolute deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaintenanceWaitOutcome {
    /// Mailbox evidence arrived or the lifecycle left [`MaintenanceState::Live`].
    ConditionMet,
    /// The absolute monotonic deadline elapsed before the condition became true.
    TimedOut,
}

/// Registration-phase capability factory for one maintenance owner.
///
/// It is intentionally `!Send`: IRQ actions and owner-local cells must be
/// prepared by the CPU-pinned owner thread before [`Self::activate`].
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_runtime::maintenance::MaintenanceRegistrar<u8>>();
/// ```
#[must_use = "activate or abort the maintenance registration before returning"]
pub struct MaintenanceRegistrar<T: Copy + Send + 'static> {
    core: Option<Arc<MaintenanceCore<T>>>,
    wake: Option<ThreadWakeHandle>,
    _not_send: PhantomData<*mut ()>,
}

impl<T: Copy + Send + 'static> MaintenanceRegistrar<T> {
    /// Mints one move-only hard-IRQ publication capability.
    ///
    /// Creation and destruction are task-context operations. The returned
    /// value must be moved into an IRQ callback, then recovered or dropped only
    /// after that IRQ action has been disabled and synchronized.
    pub fn local_irq_wake(&self) -> Result<LocalIrqWake<T>, MaintenanceError> {
        self.validate_owner()?;
        let core = self.core();
        core.lifecycle.register_irq_capability()?;
        Ok(LocalIrqWake {
            core: Arc::clone(core),
            wake: self.wake().clone(),
            _not_sync: PhantomData,
        })
    }

    /// Mints a cross-CPU ordinary-task request handle.
    pub fn remote_handle(&self) -> DeviceMaintenanceHandle<T> {
        DeviceMaintenanceHandle {
            core: Arc::clone(self.core()),
            wake: self.wake().clone(),
        }
    }

    /// Publishes the domain as live after every IRQ action is registered disabled.
    pub fn activate(mut self) -> Result<MaintenanceSession<T>, MaintenanceError> {
        self.validate_owner()?;
        self.core().lifecycle.activate()?;
        Ok(MaintenanceSession {
            core: self.core.take(),
            wake: self.wake.take(),
            _not_send: PhantomData,
        })
    }

    /// Returns the CPU that owns this registration.
    pub fn owner_cpu(&self) -> usize {
        self.core().owner_cpu
    }

    /// Returns the scheduler identity that owns this registration.
    pub fn owner_thread(&self) -> ThreadId {
        self.core().owner_thread
    }

    pub(super) fn core(&self) -> &Arc<MaintenanceCore<T>> {
        self.core
            .as_ref()
            .expect("maintenance registrar core already transferred")
    }

    pub(super) fn wake(&self) -> &ThreadWakeHandle {
        self.wake
            .as_ref()
            .expect("maintenance registrar wake already transferred")
    }

    pub(crate) fn validate_owner(&self) -> Result<(), MaintenanceError> {
        validate_owner_identity(self.owner_cpu(), self.owner_thread())
    }
}

impl<T: Copy + Send + 'static> Drop for MaintenanceRegistrar<T> {
    fn drop(&mut self) {
        if let Some(core) = &self.core {
            core.lifecycle.abort_registration();
        }
    }
}

/// Live owner-thread session and the sole mailbox consumer.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_runtime::maintenance::MaintenanceSession<u8>>();
/// ```
#[must_use = "the owner must explicitly close and drain its maintenance session"]
pub struct MaintenanceSession<T: Copy + Send + 'static> {
    core: Option<Arc<MaintenanceCore<T>>>,
    wake: Option<ThreadWakeHandle>,
    _not_send: PhantomData<*mut ()>,
}

impl<T: Copy + Send + 'static> MaintenanceSession<T> {
    /// Returns the current lifecycle state.
    pub fn state(&self) -> MaintenanceState {
        self.core().lifecycle.state()
    }

    /// Returns the fixed owner CPU.
    pub fn owner_cpu(&self) -> usize {
        self.core().owner_cpu
    }

    /// Returns the fixed owner thread identity.
    pub fn owner_thread(&self) -> ThreadId {
        self.core().owner_thread
    }

    /// Mints a move-only hard-IRQ wake for a replacement endpoint while live.
    ///
    /// The owner must first disable, synchronize, and drop the previous IRQ
    /// endpoint. This task-context operation is restricted to the pinned owner
    /// CPU and scheduler thread. Teardown will wait until the returned
    /// capability is dropped after its endpoint is disabled and synchronized.
    pub fn local_irq_wake(&self) -> Result<LocalIrqWake<T>, MaintenanceError> {
        self.validate_owner()?;
        let core = self.core();
        core.lifecycle.register_live_irq_capability()?;
        Ok(LocalIrqWake {
            core: Arc::clone(core),
            wake: self
                .wake
                .as_ref()
                .expect("maintenance session wake missing before close")
                .clone(),
            _not_sync: PhantomData,
        })
    }

    /// Blocks the owner until mailbox evidence or a close transition is visible.
    pub fn wait_for_pending(&self) -> Result<(), MaintenanceError> {
        self.validate_wait_access()?;
        let core = Arc::clone(self.core());
        core.park.try_wait_until(|| core.pending_or_not_live())?;
        Ok(())
    }

    /// Reports whether mailbox evidence is pending or the live session ended.
    ///
    /// This owner-only observation is intended for composing a scheduler park
    /// predicate with another local source of progress, such as a
    /// [`crate::task::LocalExecutor`] root future. It does not consume causes
    /// or events.
    pub fn has_pending(&self) -> Result<bool, MaintenanceError> {
        self.validate_wait_access()?;
        Ok(self.core().pending_or_not_live())
    }

    /// Reports whether hard-IRQ evidence still precedes an owner decision.
    ///
    /// Device watchdogs use this after acquiring their IRQ-ingress cutoff so
    /// a snapshot already committed to the local mailbox wins over timeout.
    pub fn has_irq_pending(&self) -> Result<bool, MaintenanceError> {
        self.validate_service_access()?;
        Ok(self.core().mailbox.has_irq_pending())
    }

    /// Blocks until maintenance evidence or another owner-local predicate wins.
    ///
    /// The two conditions are evaluated together inside the wait queue's
    /// generation handshake. This is required when a device future and its IRQ
    /// mailbox share one owner thread: checking future readiness before calling
    /// [`Self::wait_for_pending`] would leave a window in which a direct waker
    /// can be consumed immediately before the owner parks on the mailbox alone.
    ///
    /// `predicate` executes in ordinary task context while local IRQs and the
    /// internal wait-queue lock are held. It must be bounded, non-blocking, and
    /// must not re-enter the scheduler or maintenance domain.
    pub fn wait_for_pending_or(
        &self,
        predicate: impl FnMut() -> bool,
    ) -> Result<(), MaintenanceError> {
        self.validate_wait_access()?;
        let core = Arc::clone(self.core());
        let predicate = RefCell::new(predicate);
        core.park
            .try_wait_until(|| core.pending_or_not_live() || (predicate.borrow_mut())())?;
        Ok(())
    }

    /// Blocks until maintenance evidence is pending or `deadline_ns` elapses.
    ///
    /// `deadline_ns` is an absolute timestamp in the task runtime's monotonic
    /// clock domain. A pre-existing publication wins before parking, while the
    /// wait queue's generation handshake covers publications racing with park.
    /// The deadline only detects failure or schedules controller-init progress;
    /// it never probes device completion state.
    pub fn wait_for_pending_until(
        &self,
        deadline_ns: u64,
    ) -> Result<MaintenanceWaitOutcome, MaintenanceError> {
        self.validate_wait_access()?;
        let core = Arc::clone(self.core());
        if core.pending_or_not_live() {
            return Ok(MaintenanceWaitOutcome::ConditionMet);
        }
        let timed_out = core
            .park
            .try_wait_until_deadline(Duration::from_nanos(deadline_ns), || {
                core.pending_or_not_live()
            })?;
        Ok(if timed_out {
            MaintenanceWaitOutcome::TimedOut
        } else {
            MaintenanceWaitOutcome::ConditionMet
        })
    }

    /// Parks this owner until an owner-local IRQ predicate or deadline wins.
    ///
    /// Unlike [`Self::wait_for_pending_until`], already queued mailbox evidence
    /// does not force an immediate return. This is used while an owner-side
    /// driver transaction is synchronously consuming its own lock-free event
    /// latch: every IRQ publication still notifies the same generation-checked
    /// park object, while the outer owner loop retains the mailbox evidence for
    /// its normal bounded drain.
    pub fn wait_for_irq_predicate_until(
        &self,
        deadline_ns: u64,
        predicate: impl FnMut() -> bool,
    ) -> Result<MaintenanceWaitOutcome, MaintenanceError> {
        self.validate_wait_access()?;
        let core = Arc::clone(self.core());
        let predicate = RefCell::new(predicate);
        let timed_out = core
            .park
            .try_wait_until_deadline(Duration::from_nanos(deadline_ns), || {
                core.lifecycle.state() != MaintenanceState::Live || (predicate.borrow_mut())()
            })?;
        Ok(if timed_out {
            MaintenanceWaitOutcome::TimedOut
        } else {
            MaintenanceWaitOutcome::ConditionMet
        })
    }

    /// Consumes at most one fixed batch of events on the owner thread.
    pub fn drain_owner(
        &self,
        limit: usize,
        consume: impl FnMut(T),
    ) -> Result<MaintenanceDrain, MaintenanceError> {
        self.validate_service_access()?;
        self.core()
            .mailbox
            .drain_owner(limit, consume)
            .map_err(MaintenanceError::from)
    }

    /// Closes publication admission before the caller masks and drains IRQ actions.
    pub fn begin_close(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.core().lifecycle.begin_close()?;
        Ok(())
    }

    /// Enters the owner drain phase after IRQ actions and capabilities are gone.
    pub fn try_begin_draining(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.core().lifecycle.try_begin_draining()?;
        Ok(())
    }

    /// Commits Closed after all accepted event and cause evidence was consumed.
    pub fn finish_close(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        self.core()
            .lifecycle
            .finish_close(self.core().mailbox.has_pending())?;
        Ok(())
    }

    /// Retains a failed maintenance domain on its fixed owner forever.
    ///
    /// This is the terminal fail-closed path when an IRQ action, device source,
    /// or DMA owner cannot be detached safely. The session deliberately stays
    /// on this stack so its wake header and lifecycle storage outlive every
    /// quarantined callback. The outer maintenance runner retains the
    /// [`CurrentCpuLease`] until this non-returning call frame is destroyed,
    /// which cannot happen. Publication admission is closed before parking; a
    /// late IRQ must therefore contain its source and cannot enqueue new owner
    /// work.
    pub fn quarantine_and_park(self) -> ! {
        self.core().lifecycle.quarantine();
        loop {
            // No publisher can make the predicate true after quarantine. A
            // spurious wake or scheduler diagnostic merely re-enters the same
            // generation-checked park without releasing owner identity.
            if self.core().park.try_wait_until(|| false).is_err() {
                let _ = crate::task::yield_current_cpu();
            }
        }
    }

    /// Converts a terminal session into proof for owner-local state reclamation.
    ///
    /// Failure retains the complete session, including its CPU lease, so the
    /// owner can finish IRQ teardown and retry without an anonymous half-close.
    pub fn try_into_closed(mut self) -> Result<MaintenanceClosed, MaintenanceCloseFailure<T>> {
        let core = self.core();
        if core.lifecycle.state() != MaintenanceState::Closed {
            let error = MaintenanceError::CloseIncomplete(core.lifecycle.state());
            return Err(MaintenanceCloseFailure {
                error,
                session: self,
            });
        }
        let lifecycle = Arc::clone(&core.lifecycle);
        self.wake.take();
        self.core.take();
        Ok(MaintenanceClosed {
            lifecycle,
            _not_send: PhantomData,
        })
    }

    pub(crate) fn validate_owner(&self) -> Result<(), MaintenanceError> {
        validate_owner_identity(self.core().owner_cpu, self.core().owner_thread)
    }

    fn validate_service_access(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        let state = self.core().lifecycle.state();
        if !self.core().lifecycle.permits_service_access() {
            return Err(MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Live,
                actual: state,
            }
            .into());
        }
        Ok(())
    }

    fn validate_wait_access(&self) -> Result<(), MaintenanceError> {
        self.validate_owner()?;
        let state = self.core().lifecycle.state();
        if state != MaintenanceState::Live {
            return Err(MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Live,
                actual: state,
            }
            .into());
        }
        Ok(())
    }

    pub(super) fn core(&self) -> &Arc<MaintenanceCore<T>> {
        self.core
            .as_ref()
            .expect("maintenance session core already converted to close proof")
    }

    pub(super) fn lifecycle(&self) -> &Arc<MaintenanceLifecycle> {
        &self.core().lifecycle
    }
}

pub(super) fn validate_owner_identity(
    owner_cpu: usize,
    owner_thread: ThreadId,
) -> Result<(), MaintenanceError> {
    let irq = ax_kspin::IrqGuard::new();
    let actual_cpu = ax_hal::percpu::this_cpu_id_pinned(irq.cpu_pin());
    if actual_cpu != owner_cpu {
        return Err(MaintenanceError::WrongOwnerCpu {
            expected: owner_cpu,
            actual: actual_cpu,
        });
    }
    let current = crate::task::current_thread_id()?;
    if current != owner_thread {
        return Err(MaintenanceError::WrongOwnerThread);
    }
    Ok(())
}

impl<T: Copy + Send + 'static> Drop for MaintenanceSession<T> {
    fn drop(&mut self) {
        if let Some(core) = &self.core {
            core.lifecycle.quarantine();
        }
    }
}

/// Failed close conversion retaining the owner session for a later retry.
pub struct MaintenanceCloseFailure<T: Copy + Send + 'static> {
    error: MaintenanceError,
    session: MaintenanceSession<T>,
}

impl<T: Copy + Send + 'static> MaintenanceCloseFailure<T> {
    /// Returns the close failure reason.
    pub const fn error(&self) -> MaintenanceError {
        self.error
    }

    /// Recovers the still-pinned owner session.
    pub fn into_session(self) -> MaintenanceSession<T> {
        self.session
    }
}

/// Send+Sync handle for ordinary cross-CPU request producers.
///
/// It never performs device access. Hard IRQ callers are rejected and must use
/// the separately registered [`LocalIrqWake`] capability.
pub struct DeviceMaintenanceHandle<T: Copy + Send + 'static> {
    core: Arc<MaintenanceCore<T>>,
    wake: ThreadWakeHandle,
}

/// Strong lifetime owner for one CPU-pinned maintenance thread.
///
/// Unlike a scheduler [`ThreadHandle`], this capability intentionally exposes
/// no affinity, policy, migration, or exit controls. Device runtimes retain it
/// only to make the owner thread's shutdown lifetime explicit; all usable
/// request and shutdown operations live on [`DeviceMaintenanceHandle`].
#[must_use = "retain the maintenance thread for the complete device lifetime"]
pub struct MaintenanceThread {
    thread: ThreadHandle,
}

impl MaintenanceThread {
    /// Returns the immutable scheduler identity used for diagnostics.
    pub fn owner_thread(&self) -> ThreadId {
        self.thread.id()
    }
}

impl<T: Copy + Send + 'static> DeviceMaintenanceHandle<T> {
    /// Returns the CPU on which the device owner is pinned.
    pub fn owner_cpu(&self) -> usize {
        self.core.owner_cpu
    }

    /// Returns the generation-bearing device owner thread identity.
    pub fn owner_thread(&self) -> ThreadId {
        self.core.owner_thread
    }

    /// Returns a read-only lifecycle snapshot.
    pub fn state(&self) -> MaintenanceState {
        self.core.lifecycle.state()
    }

    /// Creates another producer handle in ordinary task context.
    pub fn try_clone_task_context(&self) -> Result<Self, MaintenanceSubmitError> {
        if ax_hal::irq::in_irq_context() {
            return Err(MaintenanceSubmitError::HardIrqContext);
        }
        Ok(Self {
            core: Arc::clone(&self.core),
            wake: self.wake.clone(),
        })
    }

    /// Publishes one owned Copy request from any CPU and wakes the owner.
    pub fn submit_request(
        &self,
        causes: MaintenanceCauses,
        request: T,
    ) -> Result<MaintenancePublishResult, MaintenanceSubmitError> {
        if ax_hal::irq::in_irq_context() {
            return Err(MaintenanceSubmitError::HardIrqContext);
        }
        let publication = self
            .core
            .lifecycle
            .begin_publish()
            .map_err(|_| MaintenanceSubmitError::Closed)?;
        let result = self.core.mailbox.publish_task_event(causes, request);
        drop(publication);
        classify_task_wake(&self.core, result, self.wake.wake())
    }

    /// Coalesces a cause without consuming one mailbox slot.
    pub fn publish_cause(&self, cause: MaintenanceCauses) -> Result<(), MaintenanceSubmitError> {
        if ax_hal::irq::in_irq_context() {
            return Err(MaintenanceSubmitError::HardIrqContext);
        }
        let publication = self
            .core
            .lifecycle
            .begin_publish()
            .map_err(|_| MaintenanceSubmitError::Closed)?;
        self.core.mailbox.publish_task_cause(cause);
        drop(publication);
        classify_task_wake(
            &self.core,
            MaintenancePublishResult::Published,
            self.wake.wake(),
        )
        .map(|_| ())
    }

    /// Requests orderly owner-thread shutdown.
    pub fn request_shutdown(&self) -> Result<(), MaintenanceSubmitError> {
        self.publish_cause(MaintenanceCauses::SHUTDOWN)
    }
}

/// Move-only capability used by one registered local hard-IRQ callback.
pub struct LocalIrqWake<T: Copy + Send + 'static> {
    core: Arc<MaintenanceCore<T>>,
    wake: ThreadWakeHandle,
    _not_sync: PhantomData<Cell<()>>,
}

impl<T: Copy + Send + 'static> LocalIrqWake<T> {
    /// Publishes one acknowledged snapshot and wakes its CPU-local owner.
    ///
    /// The complete accepted path is allocation-free, lock-free, non-blocking,
    /// and invokes no user callback. This value must not be cloned or dropped
    /// from hard IRQ context.
    pub fn publish_from_irq(
        &self,
        causes: MaintenanceCauses,
        event: T,
    ) -> Result<MaintenancePublishResult, LocalIrqWakeError> {
        if !ax_hal::irq::in_irq_context() {
            return Err(LocalIrqWakeError::NotHardIrq);
        }
        let actual_cpu = ax_hal::percpu::this_cpu_id();
        if actual_cpu != self.core.owner_cpu {
            return Err(LocalIrqWakeError::WrongCpu {
                expected: self.core.owner_cpu,
                actual: actual_cpu,
            });
        }
        if self.wake.thread_id() != self.core.owner_thread {
            return Err(LocalIrqWakeError::OwnerIdentityMismatch);
        }
        let wake_target = self.wake.target_cpu().map(|cpu| cpu.as_u32() as usize);
        if wake_target != Some(self.core.owner_cpu) {
            return Err(LocalIrqWakeError::OwnerPlacementMismatch {
                expected: self.core.owner_cpu,
                actual: wake_target,
            });
        }
        if !self.core.lifecycle.permits_irq_publication() {
            return Err(LocalIrqWakeError::Closed);
        }
        // Every LocalIrqWake for a domain is registered on the fixed owner CPU,
        // and the checks above reject delivery elsewhere. A one-shot producer
        // gate contains unexpected nested delivery without retrying. The owner
        // session is the sole consumer. Its close protocol keeps this
        // capability counted until the action is disabled and synchronized,
        // so a publication racing Live -> Closing completes before Draining.
        let result = self
            .core
            .mailbox
            .publish_irq_event_serialized(causes, event);
        match self.wake.wake() {
            WakeResult::Notified | WakeResult::AlreadyPending => Ok(result),
            wake @ (WakeResult::Exited | WakeResult::Unavailable) => {
                self.core.lifecycle.quarantine();
                Err(LocalIrqWakeError::OwnerUnavailable {
                    publication: result,
                    wake,
                })
            }
        }
    }

    /// Returns the registered owner CPU.
    pub fn owner_cpu(&self) -> usize {
        self.core.owner_cpu
    }

    /// Returns the registered owner thread identity.
    pub fn owner_thread(&self) -> ThreadId {
        self.core.owner_thread
    }
}

impl<T: Copy + Send + 'static> Drop for LocalIrqWake<T> {
    fn drop(&mut self) {
        self.core.lifecycle.release_irq_capability();
    }
}

pub(super) struct MaintenanceCore<T: Copy + Send + 'static> {
    pub(super) lifecycle: Arc<MaintenanceLifecycle>,
    pub(super) mailbox: MaintenanceMailbox<T>,
    pub(super) park: WaitQueue,
    pub(super) owner_cpu: usize,
    pub(super) owner_thread: ThreadId,
}

impl<T: Copy + Send + 'static> MaintenanceCore<T> {
    fn pending_or_not_live(&self) -> bool {
        self.mailbox.has_pending() || self.lifecycle.state() != MaintenanceState::Live
    }
}

/// Runs a registration closure on the current CPU-pinned owner thread.
pub fn run_maintenance_current<T, F>(run: F) -> Result<(), MaintenanceError>
where
    T: Copy + Send + 'static,
    F: FnOnce(MaintenanceRegistrar<T>) -> Result<MaintenanceClosed, MaintenanceError>,
{
    let cpu_lease = pin_current_cpu()?;
    let thread = current_thread_handle()?;
    let owner_cpu = cpu_lease.cpu().as_u32() as usize;
    let core = Arc::new(MaintenanceCore {
        lifecycle: Arc::new(MaintenanceLifecycle::new()),
        mailbox: MaintenanceMailbox::new(),
        park: WaitQueue::new(),
        owner_cpu,
        owner_thread: thread.id(),
    });
    let registrar = MaintenanceRegistrar {
        core: Some(Arc::clone(&core)),
        wake: Some(thread.wake_handle()),
        _not_send: PhantomData,
    };
    let expected = Arc::clone(&registrar.core().lifecycle);
    match run(registrar) {
        Ok(closed) if Arc::ptr_eq(&expected, &closed.lifecycle) => Ok(()),
        Ok(_) => quarantine_owner_forever(core, cpu_lease, MaintenanceError::ForeignCloseProof),
        Err(error) => {
            if try_finish_safe_abort(&core) {
                Err(error)
            } else {
                quarantine_owner_forever(core, cpu_lease, error)
            }
        }
    }
}

fn try_finish_safe_abort<T: Copy + Send + 'static>(core: &MaintenanceCore<T>) -> bool {
    match core.lifecycle.state() {
        MaintenanceState::Registering => core.lifecycle.abort_registration(),
        MaintenanceState::Live | MaintenanceState::Quarantined => return false,
        MaintenanceState::Closing | MaintenanceState::Draining => {}
        MaintenanceState::Closed => return true,
    }

    match core.lifecycle.state() {
        MaintenanceState::Closing => {
            if core.lifecycle.try_begin_draining().is_err() {
                return false;
            }
        }
        MaintenanceState::Draining => {}
        MaintenanceState::Closed => return true,
        MaintenanceState::Registering | MaintenanceState::Live | MaintenanceState::Quarantined => {
            return false;
        }
    }

    // The transition into Draining proves that every capability and every
    // publisher which could still create mailbox evidence has returned. A
    // fresh observation after that transition is therefore a stable close
    // proof; accepted evidence is never discarded to make an abort succeed.
    if core.mailbox.has_pending() {
        return false;
    }
    core.lifecycle.finish_close(false).is_ok()
}

/// Retains the CPU lease and domain identity after an owner closure violates
/// its close protocol.
///
/// The caller may have registered a callback that could not be detached. It is
/// therefore unsound to return, unwind the owner thread, or permit migration.
/// Only a matching [`MaintenanceClosed`] proof may bypass this terminal path.
fn quarantine_owner_forever<T: Copy + Send + 'static>(
    core: Arc<MaintenanceCore<T>>,
    _cpu_lease: CurrentCpuLease,
    reason: MaintenanceError,
) -> ! {
    error!(
        "maintenance owner {:?} on CPU {} entered permanent quarantine: {reason}",
        core.owner_thread, core.owner_cpu
    );
    core.lifecycle.quarantine();
    loop {
        if core.park.try_wait_until(|| false).is_err() {
            let _ = crate::task::yield_current_cpu();
        }
    }
}

/// Creates one fair maintenance owner whose initial affinity contains only `cpu`.
///
/// The scheduler may receive this request from any task CPU, but it publishes
/// the new thread directly to the selected CPU before the entry point can run.
/// The entry then acquires its current-CPU lease before constructing any IRQ
/// callback or portable-driver owner capability.
pub fn spawn_maintenance_domain<T, F>(
    cpu: usize,
    name: String,
    run: F,
) -> Result<MaintenanceThread, MaintenanceError>
where
    T: Copy + Send + 'static,
    F: FnOnce(MaintenanceRegistrar<T>) -> Result<MaintenanceClosed, MaintenanceError>
        + Send
        + 'static,
{
    let topology = ax_hal::cpu_num();
    if cpu >= topology {
        return Err(MaintenanceError::InvalidCpu {
            cpu,
            cpu_count: topology,
        });
    }
    let cpu_id = u32::try_from(cpu).map_err(|_| MaintenanceError::InvalidCpu {
        cpu,
        cpu_count: topology,
    })?;
    let mut affinity = CpuSet::empty(topology);
    if !affinity.insert(CpuId::new(cpu_id)) {
        return Err(MaintenanceError::InvalidCpu {
            cpu,
            cpu_count: topology,
        });
    }
    let policy = SchedulePolicy::fair(Nice::new(-10)?, FairMode::Normal);
    crate::task::spawn_kernel_worker(
        move || {
            if let Err(error) = run_maintenance_current::<T, _>(run) {
                error!("maintenance owner exited after a safe registration abort: {error}");
            }
        },
        name,
        affinity,
        policy,
    )
    .map(|thread| MaintenanceThread { thread })
    .map_err(MaintenanceError::from)
}

fn classify_task_wake<T: Copy + Send + 'static>(
    core: &MaintenanceCore<T>,
    publication: MaintenancePublishResult,
    wake: WakeResult,
) -> Result<MaintenancePublishResult, MaintenanceSubmitError> {
    match wake {
        WakeResult::Notified | WakeResult::AlreadyPending => Ok(publication),
        WakeResult::Exited | WakeResult::Unavailable => {
            core.lifecycle.quarantine();
            Err(MaintenanceSubmitError::OwnerUnavailable { publication, wake })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn require_send<T: Send>() {}
    fn require_send_sync<T: Send + Sync>() {}

    #[test]
    fn remote_handle_is_cross_cpu_but_local_irq_wake_is_move_only() {
        require_send_sync::<DeviceMaintenanceHandle<u8>>();
        require_send::<LocalIrqWake<u8>>();
    }

    #[test]
    fn empty_registration_abort_closes_but_live_capability_aborts_quarantine() {
        let empty = test_core();
        assert!(try_finish_safe_abort(&empty));
        assert_eq!(empty.lifecycle.state(), MaintenanceState::Closed);

        let live = test_core();
        live.lifecycle.register_irq_capability().unwrap();
        live.lifecycle.activate().unwrap();
        assert!(!try_finish_safe_abort(&live));
        assert_eq!(live.lifecycle.state(), MaintenanceState::Live);

        live.lifecycle.begin_close().unwrap();
        assert!(!try_finish_safe_abort(&live));
        assert_eq!(live.lifecycle.state(), MaintenanceState::Closing);
        live.lifecycle.release_irq_capability();
        assert!(try_finish_safe_abort(&live));
        assert_eq!(live.lifecycle.state(), MaintenanceState::Closed);
    }

    #[test]
    fn accepted_event_with_unavailable_owner_closes_late_service() {
        let core = test_core();
        core.lifecycle.activate().unwrap();

        assert_eq!(
            classify_task_wake(
                &core,
                MaintenancePublishResult::Published,
                WakeResult::Unavailable,
            ),
            Err(MaintenanceSubmitError::OwnerUnavailable {
                publication: MaintenancePublishResult::Published,
                wake: WakeResult::Unavailable,
            })
        );
        assert_eq!(core.lifecycle.state(), MaintenanceState::Quarantined);
        assert!(!core.lifecycle.permits_service_access());
    }

    fn test_core() -> MaintenanceCore<u8> {
        MaintenanceCore {
            lifecycle: Arc::new(MaintenanceLifecycle::new()),
            mailbox: MaintenanceMailbox::new(),
            park: WaitQueue::new(),
            owner_cpu: 0,
            owner_thread: ThreadId::from_parts(1, 1),
        }
    }
}
