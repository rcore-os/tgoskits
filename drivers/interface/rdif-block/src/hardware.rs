use alloc::{boxed::Box, vec::Vec};
use core::time::Duration;

use crate::{
    BlkError, CompletedRequest, DeviceInfo, HardIrqHandler, OwnedRequestBatch, QueueInfo, RequestId,
};

/// Heap-owned hardware queue transferred to one runtime maintenance task.
pub type BHardwareQueue = Box<dyn HardwareQueue>;

/// Heap-owned block controller state machine.
pub type BBlockController = Box<dyn BlockController>;

/// Receives terminal requests after hardware has relinquished DMA ownership.
pub trait CompletionSink {
    /// Accepts one terminal request and its completed DMA backing.
    fn complete(&mut self, request: CompletedRequest);
}

/// Receives driver-assigned identifiers for requests accepted from one batch.
///
/// Calls must follow the same order in which requests were removed from the
/// front of [`OwnedRequestBatch`].
pub trait SubmissionSink {
    /// Records one request whose ownership has moved to the hardware queue.
    fn accepted(&mut self, id: RequestId);
}

/// Reason a queue stopped consuming the current submission batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchSubmitDisposition {
    /// Every request offered within the queue's batch limit was accepted.
    Continue,
    /// Queue resources are exhausted; remaining requests stay runtime-owned.
    QueueFull,
    /// The queue can no longer submit requests safely.
    Fatal(BlkError),
}

/// Result of one native queue batch operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchSubmitResult {
    accepted: usize,
    disposition: BatchSubmitDisposition,
}

impl BatchSubmitResult {
    /// Creates a batch result.
    pub const fn new(accepted: usize, disposition: BatchSubmitDisposition) -> Self {
        Self {
            accepted,
            disposition,
        }
    }

    /// Returns how many requests were removed from the batch.
    pub const fn accepted(self) -> usize {
        self.accepted
    }

    /// Returns why submission stopped.
    pub const fn disposition(self) -> BatchSubmitDisposition {
        self.disposition
    }
}

/// A hardware submission/completion queue with one task-context owner.
///
/// The runtime must move a queue to exactly one maintenance task. Hard IRQ
/// handlers never hold or call this object.
pub trait HardwareQueue: Send + 'static {
    /// Returns the stable driver-local queue identifier.
    fn id(&self) -> usize;

    /// Returns immutable device and queue constraints.
    fn info(&self) -> QueueInfo;

    /// Stages an ordered prefix of validated requests for hardware submission.
    ///
    /// For each removed request, the driver must synchronously report its
    /// request identifier to `sink`. Every request not accepted must remain in
    /// `requests` in its original order. This method does not require staged
    /// descriptors to be visible to hardware until [`Self::commit_submissions`].
    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult;

    /// Publishes every descriptor staged by the preceding batch operation.
    ///
    /// The runtime calls this exactly once when that operation accepted at
    /// least one request, including partial and fatal results.
    ///
    /// # Errors
    ///
    /// Returns an error if staged ownership cannot be published safely.
    fn commit_submissions(&mut self) -> Result<(), BlkError>;

    /// Drains completions after the runtime receives an acknowledged IRQ event.
    ///
    /// This method must not be called as a periodic or submit-side poll. Every
    /// request delivered to `sink` is terminal and includes returned DMA
    /// ownership.
    ///
    /// # Errors
    ///
    /// Returns an error when the completion queue cannot be consumed safely.
    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError>;

    /// Returns the delay requested for register-only queue progress.
    ///
    /// The runtime owns the timer and the shared transition deadline. This is
    /// distinct from completion drain: expiry may advance only register and
    /// protocol bookkeeping state and must never inspect a hardware
    /// completion source.
    fn register_retry_after(&self) -> Option<Duration> {
        None
    }

    /// Advances register-only queue state after a runtime-owned timer expires.
    ///
    /// `sink` receives requests whose hardware completion was acknowledged by
    /// an earlier IRQ but whose protocol state could only become terminal
    /// after this register transition. Implementations must not inspect a
    /// hardware completion source from this method.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue cannot safely continue initialization
    /// or recovery. Implementations may request another retry through
    /// [`Self::register_retry_after`].
    fn advance_register_retry(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        Err(BlkError::NotSupported)
    }

    /// Quiesces the queue and returns every request whose DMA is safe to reuse.
    ///
    /// Backing still reachable by hardware must not be reported as completed.
    /// If this method returns an error, the queue may still own DMA-visible
    /// backing, so the caller must keep the entire queue alive.
    ///
    /// # Errors
    ///
    /// Returns an error when hardware cannot be quiesced completely.
    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError>;
}

/// Driver-private controller event published by a hard IRQ handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEvent {
    source_id: usize,
    bits: u64,
}

impl ControlEvent {
    /// Creates an event for one controller IRQ source.
    pub const fn new(source_id: usize, bits: u64) -> Self {
        Self { source_id, bits }
    }

    /// Returns the controller-local IRQ source identifier.
    pub const fn source_id(self) -> usize {
        self.source_id
    }

    /// Returns the opaque driver-private event bits.
    pub const fn bits(self) -> u64 {
        self.bits
    }

    /// Returns whether the event carries no driver-private state.
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

/// Input that advances a [`BlockController`] lifecycle state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerEvent {
    /// Starts the bootstrap controller and requests at least one I/O queue.
    Start { target_queues: usize },
    /// Retries a register-only transition before the shared deadline.
    RegisterRetry,
    /// Delivers state acknowledged by a hard IRQ handler.
    Irq(ControlEvent),
    /// Requests additional hardware queues after SMP becomes fully online.
    OnlineSmp { target_queues: usize },
    /// Rearms a source previously returned as masked.
    Rearm { source_id: usize },
    /// Masks device interrupt generation before registrations are disabled.
    QuiesceIrqs,
    /// Reports a queue whose request deadline expired without an IRQ.
    Watchdog { queue_id: usize },
    /// Stops DMA after IRQs and queue mutation are quiesced.
    ///
    /// The controller may return [`ControllerState::RegisterPending`] until
    /// hardware confirms its terminal register state. Queue memory must remain
    /// alive until the transition reaches [`ControllerState::Shutdown`].
    Shutdown,
}

/// Observable controller progress after one state-machine transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerState {
    /// A register-only transition should be retried after the requested delay.
    ///
    /// The runtime owns the shared transition deadline and sleeps on its
    /// notification object until this delay expires. Acknowledged IRQ and
    /// shutdown events take priority over the retry.
    RegisterPending { retry_after: Duration },
    /// Further progress requires a matching acknowledged IRQ event.
    WaitingForIrq,
    /// The requested bootstrap or SMP queue target is operational.
    Ready,
    /// The controller has stopped and owns no active hardware queue.
    Shutdown,
}

/// IRQ endpoint emitted by a controller transition.
pub struct IrqEndpoint {
    source_id: usize,
    queue_bits: u64,
    handler: Box<dyn HardIrqHandler>,
}

impl IrqEndpoint {
    /// Creates an endpoint whose boxed handler is owned by one IRQ token.
    pub fn new(source_id: usize, queue_bits: u64, handler: Box<dyn HardIrqHandler>) -> Self {
        Self {
            source_id,
            queue_bits,
            handler,
        }
    }

    /// Returns the controller-local IRQ source identifier.
    pub const fn source_id(&self) -> usize {
        self.source_id
    }

    /// Returns the hardware queues activated by this fixed endpoint.
    pub const fn queue_bits(&self) -> u64 {
        self.queue_bits
    }

    /// Transfers the handler into the runtime IRQ registration token.
    pub fn into_handler(self) -> Box<dyn HardIrqHandler> {
        self.handler
    }
}

/// Resources and state emitted by one controller transition.
pub struct ControllerUpdate {
    state: ControllerState,
    queues: Vec<BHardwareQueue>,
    irq_endpoints: Vec<IrqEndpoint>,
    device_info: Option<DeviceInfo>,
}

impl ControllerUpdate {
    /// Creates an update without newly emitted resources.
    pub const fn state(state: ControllerState) -> Self {
        Self {
            state,
            queues: Vec::new(),
            irq_endpoints: Vec::new(),
            device_info: None,
        }
    }

    /// Creates an update containing newly owned queues and IRQ endpoints.
    pub fn with_resources(
        state: ControllerState,
        queues: Vec<BHardwareQueue>,
        irq_endpoints: Vec<IrqEndpoint>,
    ) -> Self {
        Self {
            state,
            queues,
            irq_endpoints,
            device_info: None,
        }
    }

    /// Attaches device geometry discovered during controller initialization.
    pub const fn with_device_info(mut self, info: DeviceInfo) -> Self {
        self.device_info = Some(info);
        self
    }

    /// Returns the controller state after the transition.
    pub const fn controller_state(&self) -> ControllerState {
        self.state
    }

    /// Transfers newly created hardware queues to the runtime.
    pub fn take_queues(&mut self) -> Vec<BHardwareQueue> {
        core::mem::take(&mut self.queues)
    }

    /// Transfers newly created IRQ endpoints to registration tokens.
    pub fn take_irq_endpoints(&mut self) -> Vec<IrqEndpoint> {
        core::mem::take(&mut self.irq_endpoints)
    }

    /// Takes newly discovered device geometry, if this transition produced it.
    pub fn take_device_info(&mut self) -> Option<DeviceInfo> {
        self.device_info.take()
    }
}

/// Portable block-controller lifecycle and queue factory boundary.
pub trait BlockController: crate::DriverGeneric {
    /// Returns immutable namespace information for the exposed block device.
    fn device_info(&self) -> DeviceInfo;

    /// Returns the maximum number of I/O queues this configured controller can
    /// expose. Runtime CPU and IRQ-vector limits may reduce the requested count.
    fn max_io_queues(&self) -> usize;

    /// Advances controller initialization, scaling, rearm, or shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if the transition is invalid, the requested resources
    /// cannot be created, or hardware reports a terminal failure. Callers must
    /// unwind every resource emitted by earlier successful transitions.
    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError>;
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec};

    use super::*;
    use crate::{
        BatchSubmitDisposition, HardIrqHandler, IrqAck, IrqQueueMask, OwnedRequest,
        OwnedRequestBatch, QueueLimits, RequestFlags, RequestOp, SubmissionSink,
    };

    fn test_dma() -> dma_api::DmaDeviceInfo {
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        )
    }

    #[derive(Default)]
    struct AcceptedIds(Vec<RequestId>);

    impl SubmissionSink for AcceptedIds {
        fn accepted(&mut self, id: RequestId) {
            self.0.push(id);
        }
    }

    struct NoopQueue;

    impl HardwareQueue for NoopQueue {
        fn id(&self) -> usize {
            3
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: self.id(),
                device: DeviceInfo::new(8, 512),
                limits: QueueLimits::simple(512, test_dma()),
            }
        }

        fn submit_batch_owned(
            &mut self,
            _requests: &mut OwnedRequestBatch,
            _sink: &mut dyn SubmissionSink,
        ) -> BatchSubmitResult {
            BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull)
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            Ok(())
        }

        fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }
    }

    struct QueueIrq;

    impl HardIrqHandler for QueueIrq {
        fn ack(&mut self) -> IrqAck {
            IrqAck::cleared(IrqQueueMask::from_queue(3), ControlEvent::new(7, 0x20))
        }
    }

    #[test]
    fn controller_update_transfers_move_only_queue_and_handler_ownership() {
        let queue: BHardwareQueue = Box::new(NoopQueue);
        let endpoint = IrqEndpoint::new(7, 1 << 3, Box::new(QueueIrq));
        let mut update =
            ControllerUpdate::with_resources(ControllerState::Ready, vec![queue], vec![endpoint]);

        let mut queues = update.take_queues();
        let mut endpoints = update.take_irq_endpoints();
        assert_eq!(queues[0].id(), 3);
        assert_eq!(endpoints[0].source_id(), 7);

        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let mut batch = OwnedRequestBatch::from_iter([request]);
        let mut accepted = AcceptedIds::default();
        let result = queues[0].submit_batch_owned(&mut batch, &mut accepted);
        assert_eq!(result.disposition(), BatchSubmitDisposition::QueueFull);
        assert_eq!(batch.len(), 1);

        let mut handler = endpoints.remove(0).into_handler();
        let ack = handler.ack();
        assert!(ack.queues().contains(3));
        assert_eq!(ack.control_event().bits(), 0x20);
    }

    #[test]
    fn batch_queue_full_preserves_every_unaccepted_request() {
        let mut queue = NoopQueue;
        let mut batch = OwnedRequestBatch::from_iter([
            OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
            OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
        ]);
        let mut accepted = AcceptedIds::default();

        let result = queue.submit_batch_owned(&mut batch, &mut accepted);

        assert_eq!(result.accepted(), 0);
        assert_eq!(result.disposition(), BatchSubmitDisposition::QueueFull);
        assert!(accepted.0.is_empty());
        assert_eq!(batch.len(), 2);
        assert_eq!(QueueLimits::simple(512, test_dma()).max_submit_batch, 1);
    }
}
