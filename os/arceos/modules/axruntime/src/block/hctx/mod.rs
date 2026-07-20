//! Runtime-owned blk-mq style hardware queue.

mod completion_quarantine;
mod irq_publication;
mod lifecycle;
mod ownership;
mod request_table;
mod service_loop;
mod staging;
mod submission;

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};

use ax_kspin::SpinNoPreempt;
use completion_quarantine::{
    CompletionPublicationError, CompletionQuarantineReservation, QuarantineRetention,
    RejectedCompletionQuarantine,
};
use irq_publication::{EpochEvent, MAX_EVENTS};
use lifecycle::shutdown_unpublished_queue;
use ownership::{DriverAccessGuard, DriverEndpointLease};
use rdif_block::{BlkError, OwnedRequest, QueueHandle, QueueInfo, QueueKind};
use request_table::RequestTable;
use staging::{
    DeferredCompletionSink, DispatchDisposition, DispatchResult, FixedTagQueue,
    QuarantineCompletionSink,
};
use thiserror::Error;

use super::{
    DispatchArbiter, DispatchSource, EventRing, HctxAccessGate, HctxAccessPermit, HctxCause,
    HctxControl, HctxTerminalGate, HctxTransition, HctxTransitionError, RequestTag, TagError,
    controller::{ControllerOwnerLink, source::BlockMaintenanceEvent},
};
use crate::{
    block::quarantine::QueueQuarantineReservation,
    maintenance::{DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceSubmitError},
    task::{TaskError, WaitQueue},
};

const MAX_REQUESTS: usize = 64;

/// Runtime activation or service failure for one hardware queue.
#[derive(Debug, Error)]
pub enum HardwareQueueError {
    /// Only interrupt-completed queues may be activated as an hctx.
    #[error("hardware queue {queue_id} is not an interrupt queue")]
    NotInterruptQueue { queue_id: usize },
    /// An interrupt queue omitted its required logical source set.
    #[error("hardware queue {queue_id} declares no interrupt source")]
    MissingInterruptSource { queue_id: usize },
    /// A CPU outside the fixed runtime topology was selected.
    #[error("hardware queue CPU {0} is outside the runtime topology")]
    InvalidCpu(usize),
    /// Request identity or state transition failed.
    #[error("block request identity or state transition is invalid")]
    RequestState,
    /// The fixed maintenance owner could not be activated.
    #[error(transparent)]
    Maintenance(#[from] MaintenanceSubmitError),
    /// The scheduler rejected a request-local wait.
    #[error(transparent)]
    Task(#[from] TaskError),
    /// Driver queue service failed after activation.
    #[error("block driver queue service failed: {0}")]
    Driver(BlkError),
    /// A fixed staging or completion structure exceeded its contract.
    #[error("block hardware queue fixed capacity was exhausted")]
    Capacity,
    /// The hard-IRQ event bridge filled before bounded service consumed it.
    #[error("block hardware queue {queue_id} IRQ event ring overflowed")]
    EventOverflow { queue_id: usize },
    /// A driver returned ownership under an unknown or stale request ID.
    #[error("block driver returned a stale request identity")]
    StaleCompletion,
    /// A source continuation crossed a controller recovery epoch.
    #[error("block IRQ event belongs to a stale controller epoch")]
    StaleIrqEvent,
    /// An interrupt-backed queue returned a completion from its submit call.
    #[error("interrupt block queue completed synchronously instead of through IRQ service")]
    SynchronousCompletion,
    /// Queue lifecycle no longer permits normal submission.
    #[error("block hardware queue is not running")]
    Offline,
    /// A task-only queue operation was attempted from hard IRQ context.
    #[error("block hardware queue operation requires task context")]
    UnsafeContext,
    /// Driver service was attempted outside the fixed maintenance owner.
    #[error("block hardware queue service requires its maintenance owner")]
    WrongOwner,
    /// The requested lifecycle transition did not own the current generation.
    #[error(transparent)]
    Lifecycle(#[from] HctxTransitionError),
}

impl From<BlkError> for HardwareQueueError {
    fn from(value: BlkError) -> Self {
        Self::Driver(value)
    }
}

impl From<TagError> for HardwareQueueError {
    fn from(_value: TagError) -> Self {
        Self::RequestState
    }
}

/// A submit failure before the runtime accepted request ownership.
#[derive(Debug, Error)]
#[error("block runtime did not accept the request: {error}")]
pub struct RuntimeSubmitError {
    error: HardwareQueueError,
    request: OwnedRequest,
}

impl RuntimeSubmitError {
    fn new(error: HardwareQueueError, request: OwnedRequest) -> Self {
        Self { error, request }
    }

    /// Returns the typed admission error and original CPU-owned request.
    pub fn into_parts(self) -> (HardwareQueueError, OwnedRequest) {
        (self.error, self.request)
    }
}

/// A request-local completion token; no global completion waitqueue is used.
#[must_use = "an accepted block request must be waited or cancelled"]
pub struct SubmittedRequest {
    queue: Arc<HardwareQueue>,
    tag: RequestTag,
}

/// Proof that admission is closed and every accepted request reached a
/// terminal completion while IRQ delivery was still available.
///
/// The controller must next mask and synchronize its IRQ routes and prove DMA
/// quiescence before reclaiming host requests or transferring ownership. The
/// driver queue remains retained for guest-return reinitialization.
pub struct QuiescedHardwareQueue {
    queue: Arc<HardwareQueue>,
    transition: HctxTransition,
}

/// Owner-local proof that admission is closed while accepted requests are
/// still allowed to reach terminal completion through IRQ service.
///
/// This permit is deliberately distinct from [`QuiescedHardwareQueue`]: the
/// sole maintenance owner must keep servicing the queue between these two
/// states and therefore must never block waiting for its own progress.
pub(in crate::block) struct DrainingHardwareQueue {
    queue: Arc<HardwareQueue>,
    transition: HctxTransition,
}

/// Proof that IRQ admission is detached and every earlier queue callback has
/// exited before controller-wide DMA quiescence begins.
pub(super) struct ServiceDrainedHardwareQueue {
    queue: Arc<HardwareQueue>,
    transition: HctxTransition,
}

/// One driver queue whose hardware state is advanced only by its controller's
/// fixed maintenance owner.
pub struct HardwareQueue {
    info: QueueInfo,
    queue: SpinNoPreempt<Option<QueueHandle>>,
    quarantine_reservation: SpinNoPreempt<Option<QueueQuarantineReservation>>,
    requests: RequestTable,
    rejected_completions: SpinNoPreempt<Option<Box<RejectedCompletionQuarantine>>>,
    completion_quarantine_reservation: SpinNoPreempt<Option<CompletionQuarantineReservation>>,
    software_contexts: [SpinNoPreempt<FixedTagQueue>; crate::CPU_CAPACITY],
    dispatch_list: SpinNoPreempt<FixedTagQueue>,
    dispatch_arbiter: SpinNoPreempt<DispatchArbiter<{ crate::CPU_CAPACITY }>>,
    events: EventRing<EpochEvent, MAX_EVENTS>,
    control: HctxControl,
    terminal_gate: HctxTerminalGate,
    access_gate: HctxAccessGate,
    fatal_completion_quarantine: AtomicBool,
    accepted_requests: AtomicUsize,
    inflight: AtomicUsize,
    drain_wait: WaitQueue,
    service_error: AtomicU8,
    maintenance: alloc::sync::Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
    controller_link: Arc<ControllerOwnerLink>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::block) enum OwnerServiceProgress {
    Complete,
    More,
}

impl HardwareQueue {
    /// Constructs a shutdown-lifetime hctx after its maintenance owner is live.
    pub(super) fn activate(
        queue: QueueHandle,
        quarantine_reservation: QueueQuarantineReservation,
        maintenance: Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
        controller_link: Arc<ControllerOwnerLink>,
        controller_cookie: usize,
    ) -> Result<Arc<Self>, HardwareQueueError> {
        let cpu = maintenance.owner_cpu();
        if cpu >= crate::CPU_CAPACITY {
            shutdown_unpublished_queue(queue, quarantine_reservation);
            return Err(HardwareQueueError::InvalidCpu(cpu));
        }
        let info = queue.info();
        let QueueKind::Interrupt { sources } = info.kind else {
            shutdown_unpublished_queue(queue, quarantine_reservation);
            return Err(HardwareQueueError::NotInterruptQueue { queue_id: info.id });
        };
        if sources.is_empty() {
            shutdown_unpublished_queue(queue, quarantine_reservation);
            return Err(HardwareQueueError::MissingInterruptSource { queue_id: info.id });
        }
        let requests = match RequestTable::new() {
            Ok(requests) => requests,
            Err(error) => {
                shutdown_unpublished_queue(queue, quarantine_reservation);
                return Err(error.into());
            }
        };
        let Some(completion_quarantine_reservation) =
            CompletionQuarantineReservation::reserve(info.id, controller_cookie)
        else {
            shutdown_unpublished_queue(queue, quarantine_reservation);
            return Err(HardwareQueueError::Capacity);
        };

        Ok(Arc::new(Self {
            info,
            queue: SpinNoPreempt::new(Some(queue)),
            quarantine_reservation: SpinNoPreempt::new(Some(quarantine_reservation)),
            requests,
            rejected_completions: SpinNoPreempt::new(Some(Box::new(
                RejectedCompletionQuarantine::new(controller_cookie),
            ))),
            completion_quarantine_reservation: SpinNoPreempt::new(Some(
                completion_quarantine_reservation,
            )),
            software_contexts: core::array::from_fn(|_| SpinNoPreempt::new(FixedTagQueue::new())),
            dispatch_list: SpinNoPreempt::new(FixedTagQueue::new()),
            dispatch_arbiter: SpinNoPreempt::new(DispatchArbiter::new()),
            events: EventRing::new(),
            control: HctxControl::new(),
            terminal_gate: HctxTerminalGate::new(),
            access_gate: HctxAccessGate::new(),
            fatal_completion_quarantine: AtomicBool::new(false),
            accepted_requests: AtomicUsize::new(0),
            inflight: AtomicUsize::new(0),
            drain_wait: WaitQueue::new(),
            service_error: AtomicU8::new(0),
            maintenance,
            controller_link,
        }))
    }

    /// Returns the driver-declared queue metadata.
    pub const fn info(&self) -> QueueInfo {
        self.info
    }

    /// CPU whose fixed controller maintenance owner services this queue.
    pub fn affinity_cpu(&self) -> usize {
        self.maintenance.owner_cpu()
    }

    fn claim_timeout(&self, tag: RequestTag) -> Result<bool, HardwareQueueError> {
        let claim = match self.requests.tags.claim_timeout(tag) {
            Ok(claim) => claim,
            Err(TagError::InvalidTransition | TagError::Stale) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        drop(claim);
        self.requests.clear_deadline(tag)?;
        if let Err(error) = self.queue_service(HctxCause::Timeout) {
            // The timeout claim already owns the terminal race. Preserve the
            // accepted request and converge on controller recovery instead of
            // reporting an admission failure that could invite buffer reuse.
            self.record_service_error(&error);
        }
        Ok(true)
    }

    fn request_cancel(&self, tag: RequestTag) -> Result<bool, HardwareQueueError> {
        if ax_hal::irq::in_irq_context() {
            return Err(HardwareQueueError::UnsafeContext);
        }
        let claim = match self.requests.tags.claim_cancel(tag) {
            Ok(claim) => claim,
            Err(TagError::InvalidTransition | TagError::Stale) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let _requires_dma_quiesce = claim.requires_dma_quiesce();
        drop(claim);

        if let Err(error) = self.queue_service(HctxCause::Cancel) {
            // Cancellation already owns the request-state race. Converge on
            // controller recovery instead of returning an admission failure
            // that could encourage the caller to reuse the buffer.
            self.record_service_error(&error);
        }
        Ok(true)
    }

    fn queue_service(&self, cause: HctxCause) -> Result<(), HardwareQueueError> {
        self.control.raise(cause);
        self.wake_owner()
    }

    fn wake_owner(&self) -> Result<(), HardwareQueueError> {
        self.maintenance.publish_cause(MaintenanceCauses::SUBMIT)?;
        Ok(())
    }

    pub(in crate::block) fn next_deadline_ns(&self) -> Option<u64> {
        self.requests.earliest_deadline()
    }

    pub(in crate::block) fn raise_owner_watchdog(&self) {
        self.control.raise(HctxCause::Watchdog);
    }

    fn try_driver_access(&self) -> Option<DriverAccessGuard<'_>> {
        self.access_gate
            .try_enter()
            .map(|permit| DriverAccessGuard {
                queue: self,
                permit: Some(permit),
            })
    }

    fn take_driver_on_owner(&self) -> Result<DriverEndpointLease<'_>, HardwareQueueError> {
        DriverEndpointLease::take(self).ok_or(HardwareQueueError::Offline)
    }
}
