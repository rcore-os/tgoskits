//! Runtime-owned blk-mq style hardware queue.

mod completion_quarantine;
mod irq_publication;
mod lifecycle;
mod ownership;
mod request_table;
mod service_loop;
mod staging;
mod submission;

use alloc::boxed::Box;
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use ax_kspin::SpinNoPreempt;
use completion_quarantine::{
    CompletionPublicationError, QuarantineRetention, RejectedCompletionQuarantine,
};
use irq_publication::{EpochEvent, MAX_EVENTS};
use lifecycle::shutdown_unpublished_queue;
use ownership::{DriverAccessGuard, WorkOwnerLink};
use rdif_block::{BlkError, OwnedRequest, QueueHandle, QueueInfo, QueueKind};
use request_table::RequestTable;
use staging::{CompletionBatch, DispatchDisposition, DispatchResult, FixedTagQueue};
use thiserror::Error;

use super::{
    DispatchArbiter, DispatchSource, EventRing, HctxAccessGate, HctxAccessPermit, HctxCause,
    HctxControl, HctxTerminalGate, HctxTransition, HctxTransitionError, RequestTag, TagError,
    controller::ControllerOwnerLink,
};
use crate::{
    task::{TaskError, WaitQueue},
    workqueue::{DelayedWork, WorkItem, WorkOutcome, WorkPriority, WorkQueue, WorkQueueError},
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
    /// Shared worker admission or ownership failed.
    #[error(transparent)]
    WorkQueue(#[from] WorkQueueError),
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
    queue: &'static HardwareQueue,
    tag: RequestTag,
}

/// Proof that admission is closed and every accepted request reached a
/// terminal completion while IRQ delivery was still available.
///
/// The controller must next mask and synchronize its IRQ routes and prove DMA
/// quiescence before reclaiming host requests or transferring ownership. The
/// driver queue remains retained for guest-return reinitialization.
pub struct QuiescedHardwareQueue {
    queue: &'static HardwareQueue,
    transition: HctxTransition,
}

/// Proof that IRQ admission is detached and every earlier queue callback has
/// exited before controller-wide DMA quiescence begins.
pub(super) struct ServiceDrainedHardwareQueue {
    queue: &'static HardwareQueue,
    transition: HctxTransition,
}

/// One serial driver queue and one coalescing work item, executed by a shared
/// per-CPU high-priority worker rather than a dedicated thread.
pub struct HardwareQueue {
    info: QueueInfo,
    queue: SpinNoPreempt<QueueHandle>,
    requests: RequestTable,
    rejected_completions: SpinNoPreempt<RejectedCompletionQuarantine>,
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
    work_domain: Pin<&'static WorkQueue>,
    service_work: WorkItem,
    watchdog_work: DelayedWork,
    controller_link: &'static ControllerOwnerLink,
    _work_link: &'static WorkOwnerLink,
}

impl HardwareQueue {
    /// Constructs a shutdown-lifetime hctx after its shared worker is live.
    pub(super) fn activate(
        queue: QueueHandle,
        cpu: usize,
        controller_link: &'static ControllerOwnerLink,
        controller_cookie: usize,
    ) -> Result<Pin<&'static Self>, HardwareQueueError> {
        if cpu >= crate::CPU_CAPACITY {
            shutdown_unpublished_queue(queue);
            return Err(HardwareQueueError::InvalidCpu(cpu));
        }
        let info = queue.info();
        let QueueKind::Interrupt { sources } = info.kind else {
            shutdown_unpublished_queue(queue);
            return Err(HardwareQueueError::NotInterruptQueue { queue_id: info.id });
        };
        if sources.is_empty() {
            shutdown_unpublished_queue(queue);
            return Err(HardwareQueueError::MissingInterruptSource { queue_id: info.id });
        }
        let requests = match RequestTable::new() {
            Ok(requests) => requests,
            Err(error) => {
                shutdown_unpublished_queue(queue);
                return Err(error.into());
            }
        };

        let work_domain = Box::leak(Box::new(WorkQueue::new(cpu, WorkPriority::High)));
        let work_domain = unsafe {
            // SAFETY: the logical domain is deliberately retained for the
            // shutdown lifetime and can never move after publication.
            Pin::new_unchecked(&*work_domain)
        };
        let work_link = Box::leak(Box::new(WorkOwnerLink::new()));
        let link_address = ptr::from_ref(work_link).expose_provenance();
        let queue = Box::leak(Box::new(Self {
            info,
            queue: SpinNoPreempt::new(queue),
            requests,
            rejected_completions: SpinNoPreempt::new(RejectedCompletionQuarantine::new(
                controller_cookie,
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
            work_domain,
            service_work: WorkItem::new(service_work_entry, link_address),
            watchdog_work: DelayedWork::new(watchdog_work_entry, link_address),
            controller_link,
            _work_link: work_link,
        }));
        work_link
            .owner
            .store(ptr::from_mut(queue), Ordering::Release);
        Ok(unsafe {
            // SAFETY: HardwareQueue contains an intrusive WorkItem and is
            // intentionally retained at this stable address until shutdown.
            Pin::new_unchecked(&*queue)
        })
    }

    /// Returns the driver-declared queue metadata.
    pub const fn info(&self) -> QueueInfo {
        self.info
    }

    /// CPU whose shared high-priority worker services this queue.
    pub fn affinity_cpu(&self) -> usize {
        self.work_domain.cpu()
    }

    fn claim_timeout(&'static self, tag: RequestTag) -> Result<bool, HardwareQueueError> {
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

    fn request_cancel(&'static self, tag: RequestTag) -> Result<bool, HardwareQueueError> {
        if ax_hal::irq::in_irq_context() {
            return Err(HardwareQueueError::UnsafeContext);
        }
        let driver = self.queue.lock();
        let claim = match self.requests.tags.claim_cancel(tag) {
            Ok(claim) => claim,
            Err(TagError::InvalidTransition | TagError::Stale) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let _requires_dma_quiesce = claim.requires_dma_quiesce();
        drop(claim);
        drop(driver);

        if let Err(error) = self.queue_service(HctxCause::Cancel) {
            // Cancellation already owns the request-state race. Converge on
            // controller recovery instead of returning an admission failure
            // that could encourage the caller to reuse the buffer.
            self.record_service_error(&error);
        }
        Ok(true)
    }

    fn queue_service(&'static self, cause: HctxCause) -> Result<(), HardwareQueueError> {
        self.control.raise(cause);
        self.queue_service_work()
    }

    fn queue_service_work(&'static self) -> Result<(), HardwareQueueError> {
        let _queue_result = self.work_domain.queue_work_on(self.service_work())?;
        Ok(())
    }

    fn service_work(&'static self) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: the containing HardwareQueue is leaked and pinned for the
            // shutdown lifetime, so its intrusive WorkItem cannot move.
            Pin::new_unchecked(&self.service_work)
        }
    }

    fn watchdog_work(&'static self) -> Pin<&'static DelayedWork> {
        unsafe {
            // SAFETY: the containing HardwareQueue is retained at a stable
            // address for the complete timer and worker shutdown lifetime.
            Pin::new_unchecked(&self.watchdog_work)
        }
    }

    fn try_driver_access(&'static self) -> Option<DriverAccessGuard> {
        self.access_gate
            .try_enter()
            .map(|permit| DriverAccessGuard {
                queue: self,
                permit: Some(permit),
            })
    }
}

fn service_work_entry(data: usize) -> WorkOutcome {
    let link = unsafe {
        // SAFETY: activation leaks WorkOwnerLink before publishing this callback
        // data and retains it for the work item's shutdown lifetime.
        &*ptr::with_exposed_provenance::<WorkOwnerLink>(data)
    };
    let owner = link.owner.load(Ordering::Acquire);
    assert!(
        !owner.is_null(),
        "block service work ran before owner publication"
    );
    let queue = unsafe {
        // SAFETY: owner publication occurs only after the leaked HardwareQueue
        // is fully initialized; it is never moved or freed afterwards.
        &*owner
    };
    let Some(_access) = queue.try_driver_access() else {
        return WorkOutcome::Complete;
    };
    match queue.service_bounded() {
        Ok(outcome) => outcome,
        Err(error) => {
            queue.record_service_error(&error);
            WorkOutcome::Complete
        }
    }
}

fn watchdog_work_entry(data: usize) -> WorkOutcome {
    let link = unsafe {
        // SAFETY: activation retains this link until every timer and work
        // reference has been cancelled and drained.
        &*ptr::with_exposed_provenance::<WorkOwnerLink>(data)
    };
    let owner = link.owner.load(Ordering::Acquire);
    assert!(
        !owner.is_null(),
        "block watchdog ran before owner publication"
    );
    let queue = unsafe {
        // SAFETY: the owner is a leaked, fully initialized HardwareQueue and
        // is never replaced after Release publication.
        &*owner
    };
    let cause = if queue.watchdog_work.take_failure().is_some() {
        HctxCause::Timeout
    } else {
        HctxCause::Watchdog
    };
    if let Err(error) = queue.queue_service(cause) {
        queue.record_service_error(&error);
    }
    WorkOutcome::Complete
}
