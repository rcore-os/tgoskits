use alloc::boxed::Box;

use crate::{
    BlkError, CompletedRequest, ControllerEpoch, ControllerInitEndpoint, DmaQuiesced,
    DriverGeneric, IdList, IrqHandler, IrqSourceInfo, IrqSourceList, LifecycleEndpoint,
    LifecycleKind, MAX_CONTROLLER_QUEUES, OwnedRequest, QueueContractError, QueueEventBatch,
    QueueInfo, QueueKind, RequestId, SubmitError, SubmitOutcome, validate_lifecycle_identity,
};

pub type BInterface = Box<dyn Interface>;
pub type BQueue = Box<dyn IQueue>;
pub type BIrqHandler = Box<dyn IrqHandler>;

/// Portable control endpoint for one block device.
///
/// Every implementation must state its initialization capability explicitly:
///
/// ```compile_fail
/// use rdif_block::{
///     BIrqHandler, BlkError, DeviceInfo, DriverGeneric, Interface, IrqSourceList,
///     LifecycleEndpoint, QueueHandle, QueueLimits,
/// };
///
/// struct MissingInterruptContract;
///
/// impl DriverGeneric for MissingInterruptContract {
///     fn name(&self) -> &str { "invalid" }
/// }
///
/// impl Interface for MissingInterruptContract {
///     fn lifecycle(&mut self) -> LifecycleEndpoint<'_> { LifecycleEndpoint::Inline }
///     fn device_info(&self) -> DeviceInfo { DeviceInfo::new(1, 512) }
///     fn queue_limits(&self) -> QueueLimits { QueueLimits::simple(512, u64::MAX) }
///     fn create_queue(&mut self) -> Option<QueueHandle> { None }
///     fn enable_irq(&self) -> Result<(), BlkError> { Err(BlkError::NotSupported) }
///     fn disable_irq(&self) -> Result<(), BlkError> { Err(BlkError::NotSupported) }
///     fn is_irq_enabled(&self) -> bool { false }
///     fn irq_sources(&self) -> IrqSourceList { IrqSourceList::new() }
///     fn take_irq_handler(&mut self, _source_id: usize) -> Option<BIrqHandler> { None }
/// }
/// ```
pub trait Interface: DriverGeneric {
    /// Returns the discovery-to-ready initialization endpoint.
    ///
    /// Hardware implementations return `Pending` so their first command runs
    /// only after the OS has installed and enabled every declared
    /// initialization IRQ action. Inline devices and already-initialized
    /// objects must state `Ready` explicitly; omitting this method is never an
    /// implicit readiness claim.
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_>;

    /// Returns the controller-wide lifecycle capability retained by the
    /// runtime for recovery and ownership handoff.
    fn lifecycle(&mut self) -> LifecycleEndpoint<'_>;

    fn device_info(&self) -> crate::DeviceInfo;

    fn queue_limits(&self) -> crate::QueueLimits;

    fn create_queue(&mut self) -> Option<QueueHandle>;

    /// Unmasks device-side interrupt generation after OS IRQ actions are live.
    fn enable_irq(&self) -> Result<(), BlkError>;

    /// Masks device-side interrupt generation before the OS runtime closes or
    /// drains this controller's IRQ actions.
    ///
    /// The registered IRQ endpoint, its MMIO mapping, and every platform
    /// binding must remain live until this operation succeeds. That ordering
    /// lets an already-raised interrupt finish while preventing a new device
    /// assertion from racing action withdrawal. Implementations must leave DMA
    /// and queue ownership untouched; those require [`InterruptLifecycle`].
    fn disable_irq(&self) -> Result<(), BlkError>;

    fn is_irq_enabled(&self) -> bool;

    fn irq_sources(&self) -> IrqSourceList;

    fn take_irq_handler(&mut self, source_id: usize) -> Option<BIrqHandler>;
}

/// Preallocated task-side target for terminal request ownership.
///
/// `service_events` may call this method while the runtime holds its queue
/// borrow or a short queue lock. Implementations must only append to fixed or
/// preallocated storage. They must not block, allocate, wake arbitrary upper
/// layer tasks, or re-enter the queue. The runtime performs completion wakeups
/// after releasing the queue borrow/lock.
pub trait CompletionSink {
    fn complete(&mut self, completion: CompletedRequest);
}

/// Evidence retained by a bounded queue-service pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceContinuationReason {
    /// The driver has not yet consumed all immutable IRQ facts.
    RetainedFacts,
    /// A fixed completion cache still contains entries from this IRQ.
    CachedCompletions,
    /// The bounded completion budget ended before the acknowledged batch did.
    CompletionBudget,
}

/// Non-forgeable request to continue servicing one acknowledged source epoch.
#[derive(Debug, PartialEq, Eq)]
pub struct ServiceContinuation {
    source_id: usize,
    source_epoch: crate::IrqEventEpoch,
    reason: ServiceContinuationReason,
}

impl ServiceContinuation {
    pub(crate) const fn new(
        source_id: usize,
        source_epoch: crate::IrqEventEpoch,
        reason: ServiceContinuationReason,
    ) -> Self {
        Self {
            source_id,
            source_epoch,
            reason,
        }
    }

    pub const fn source_id(&self) -> usize {
        self.source_id
    }

    pub const fn source_epoch(&self) -> crate::IrqEventEpoch {
        self.source_epoch
    }

    pub const fn reason(&self) -> ServiceContinuationReason {
        self.reason
    }
}

/// Whether one bounded event-service pass drained its acknowledged facts.
#[derive(Debug, PartialEq, Eq)]
pub enum ServiceProgress {
    Idle,
    Continue(ServiceContinuation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueShutdownState {
    Live,
    Attempted,
    Closed,
}

/// One owned-request queue for a block device.
///
/// Interrupt request IDs are assigned by the runtime and remain stable across
/// direct and serialized dispatch; inline requests use [`RequestId::INLINE`].
/// On `Ok(SubmitOutcome::Queued)`, the queue owns the full request until exactly
/// one terminal [`CompletedRequest`] is emitted. Both
/// `SubmitOutcome::Completed` and [`SubmitError`] return the ID and complete
/// request ownership immediately.
///
/// Completion queries are deliberately absent. After acceptance, only an
/// acknowledged [`crate::Event`] may drive ownership into `CompletionSink`.
///
/// Every queue must also define how shutdown returns accepted ownership:
///
/// ```compile_fail
/// use rdif_block::{
///     BlkError, CompletionSink, DispatchMode, IQueue, OwnedRequest, QueueEventBatch,
///     QueueInfo, QueueKind, RequestId, ServiceProgress, SubmitError, SubmitOutcome,
/// };
///
/// struct MissingShutdown;
///
/// impl IQueue for MissingShutdown {
///     fn id(&self) -> usize { 0 }
///     fn info(&self) -> QueueInfo { unimplemented!() }
///     fn submit_owned(
///         &mut self,
///         id: RequestId,
///         request: OwnedRequest,
///     ) -> Result<SubmitOutcome, SubmitError> {
///         Err(SubmitError::new(id, BlkError::Retry, request))
///     }
///     fn service_events(
///         &mut self,
///         _events: &QueueEventBatch<'_>,
///         _sink: &mut dyn CompletionSink,
///     ) -> Result<ServiceProgress, BlkError> {
///         Ok(ServiceProgress::Idle)
///     }
/// }
/// ```
pub trait IQueue: Send + 'static {
    fn id(&self) -> usize;

    fn info(&self) -> QueueInfo;

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError>;

    /// Consumes task-side state synchronized by one IRQ event.
    ///
    /// The source endpoint has already acknowledged the relevant device source.
    /// Queue service may consume only the immutable facts in `events`; it must
    /// never read or clear controller-global IRQ status. Each call is bounded.
    /// Immediate requeue requires a [`ServiceContinuation`] minted from this
    /// exact acknowledged source epoch, so ordinary `Busy` cannot silently
    /// turn into completion polling.
    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError>;

    /// Returns every request still owned by this queue after controller-wide
    /// DMA quiescence has been proven.
    ///
    /// The queue remains allocated for the following reinitialization pass.
    /// Implementations may restore CPU ownership of DMA buffers only through
    /// this proof-gated method; ordinary shutdown is not a DMA stop primitive.
    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError>;

    /// Releases an idle queue after all accepted ownership was already
    /// completed or returned by [`Self::reclaim_after_quiesce`].
    ///
    /// Implementations must return an error rather than infer DMA quiescence or
    /// fabricate ownership for a request that can still be device-accessible.
    /// The generic [`QueueHandle`] invokes this endpoint at most once: a failed
    /// attempt permanently quarantines it instead of retrying an untrusted
    /// driver teardown transition.
    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError>;
}

/// Runtime-owned handle that requires explicit, one-shot ownership shutdown.
///
/// Dropping a live handle deliberately leaks its driver endpoint. A live
/// endpoint may still own DMA mappings or descriptors, so destroying it would
/// turn a control-plane error into use-after-free. The leak is the bounded,
/// fail-closed fallback; normal teardown must call [`Self::shutdown`] and
/// handle its error while retaining this value. Once shutdown starts, no
/// queue operation can re-enter the endpoint; a failed attempt leaves it in a
/// permanent quarantine and later attempts return [`BlkError::Offline`].
#[must_use = "a block queue must be explicitly shut down or retained in quarantine"]
pub struct QueueHandle {
    queue: Option<BQueue>,
    driver_id: usize,
    info: QueueInfo,
    controller_cookie: Option<usize>,
    reclaimed_epoch: Option<ControllerEpoch>,
    static_contract: Result<(), QueueContractError>,
    submit_contract_violated: bool,
    shutdown_state: QueueShutdownState,
}

impl QueueHandle {
    pub fn new(queue: BQueue) -> Self {
        let driver_id = queue.id();
        let info = queue.info();
        let static_contract = if driver_id == info.id {
            validate_queue_info(info)
        } else {
            Err(QueueContractError::QueueIdentityMismatch {
                advertised_id: driver_id,
                metadata_id: info.id,
            })
        };
        Self {
            queue: Some(queue),
            driver_id,
            info,
            controller_cookie: None,
            reclaimed_epoch: None,
            static_contract,
            submit_contract_violated: false,
            shutdown_state: QueueShutdownState::Live,
        }
    }

    /// Binds an interrupt queue to the retained controller that owns it.
    ///
    /// The runtime performs this one-way initialization before publishing the
    /// queue or enabling normal IRQ delivery. Keeping the identity and
    /// publication epoch in the generic handle prevents a permissive driver
    /// implementation from using a proof created by a sibling controller or
    /// one that predates the first recovery transition.
    pub fn bind_interrupt_controller(
        &mut self,
        controller_cookie: usize,
        publication_epoch: ControllerEpoch,
    ) -> Result<(), QueueContractError> {
        self.static_contract?;
        if !matches!(self.info.kind, QueueKind::Interrupt { .. }) {
            return Err(QueueContractError::LifecycleMismatch {
                expected: LifecycleKind::Interrupt,
                actual: LifecycleKind::Inline,
            });
        }
        validate_lifecycle_identity(LifecycleKind::Interrupt, controller_cookie)?;
        if self.controller_cookie.is_some() {
            return Err(QueueContractError::LifecycleIdentityAlreadyBound {
                queue_id: self.info.id,
            });
        }
        self.controller_cookie = Some(controller_cookie);
        self.reclaimed_epoch = Some(publication_epoch);
        Ok(())
    }

    pub fn id(&self) -> usize {
        self.driver_id
    }

    pub fn info(&self) -> QueueInfo {
        self.info
    }

    pub fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if self.shutdown_state != QueueShutdownState::Live
            || self.static_contract.is_err()
            || self.submit_contract_violated
            || (matches!(self.info.kind, QueueKind::Interrupt { .. })
                && self.controller_cookie.is_none())
        {
            return Err(SubmitError::new(id, BlkError::Offline, request));
        }
        if validate_request_identity(self.info, id).is_err() {
            return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
        }
        let Some(queue) = self.queue.as_mut() else {
            return Err(SubmitError::new(id, BlkError::Offline, request));
        };
        let outcome = queue.submit_owned(id, request);
        if validate_submit_contract(self.info, id, &outcome).is_err() {
            // The exact returned ownership remains in `outcome`, but this
            // endpoint can no longer be trusted with another request. The
            // runtime consumes or quarantines that ownership and explicitly
            // shuts down or recovers the retained queue.
            self.submit_contract_violated = true;
        }
        outcome
    }

    pub fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if events.queue_id() != self.info.id {
            return Err(BlkError::InvalidRequest);
        }
        if self.shutdown_state != QueueShutdownState::Live
            || self.static_contract.is_err()
            || self.submit_contract_violated
            || (matches!(self.info.kind, QueueKind::Interrupt { .. })
                && self.controller_cookie.is_none())
        {
            return Err(BlkError::Offline);
        }
        self.queue
            .as_mut()
            .ok_or(BlkError::Offline)?
            .service_events(events, sink)
    }

    pub fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if self.shutdown_state != QueueShutdownState::Live {
            return Err(BlkError::Offline);
        }
        if matches!(self.info.kind, QueueKind::Interrupt { .. }) {
            let Some(controller_cookie) = self.controller_cookie else {
                return Err(BlkError::InvalidDmaProof);
            };
            if proof.controller_cookie() != controller_cookie
                || self
                    .reclaimed_epoch
                    .is_some_and(|epoch| proof.epoch() <= epoch)
            {
                return Err(BlkError::InvalidDmaProof);
            }
        }
        self.queue
            .as_mut()
            .ok_or(BlkError::Offline)?
            .reclaim_after_quiesce(proof, sink)?;
        if matches!(self.info.kind, QueueKind::Interrupt { .. }) {
            self.reclaimed_epoch = Some(proof.epoch());
        }
        Ok(())
    }

    pub fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        match self.shutdown_state {
            QueueShutdownState::Closed => return Ok(()),
            QueueShutdownState::Attempted => return Err(BlkError::Offline),
            QueueShutdownState::Live => self.shutdown_state = QueueShutdownState::Attempted,
        }
        let Some(queue) = self.queue.as_mut() else {
            self.shutdown_state = QueueShutdownState::Closed;
            return Ok(());
        };
        queue.shutdown(sink)?;
        drop(
            self.queue
                .take()
                .expect("successful queue shutdown must retain its driver endpoint"),
        );
        self.shutdown_state = QueueShutdownState::Closed;
        Ok(())
    }
}

impl Drop for QueueHandle {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.take() {
            // A queue that did not acknowledge shutdown may still be accessed
            // by its controller or DMA engine. Retain that ownership forever
            // instead of running the driver's destructor on live hardware.
            core::mem::forget(queue);
        }
    }
}

/// Validates the logical interrupt sources required to activate one queue.
///
/// `declared` comes from [`Interface::irq_sources`], while `bound` names the
/// logical sources whose handler and OS interrupt binding are both live. This
/// API deliberately carries logical source IDs, not architecture IRQ numbers.
pub fn validate_queue_activation(
    info: QueueInfo,
    declared: &[IrqSourceInfo],
    bound: IdList,
) -> Result<(), QueueContractError> {
    validate_queue_info(info)?;
    let QueueKind::Interrupt { sources } = info.kind else {
        return Ok(());
    };

    for source_id in sources.iter() {
        let declared_for_queue = declared
            .iter()
            .any(|source| source.id == source_id && source.queues.contains(info.id));
        if !declared_for_queue {
            return Err(QueueContractError::UndeclaredInterruptSource {
                queue_id: info.id,
                source_id,
            });
        }
        if !bound.contains(source_id) {
            return Err(QueueContractError::UnboundInterruptSource {
                queue_id: info.id,
                source_id,
            });
        }
    }
    Ok(())
}

/// Validates queue metadata that does not depend on OS IRQ registration.
///
/// Bundle materialization calls this before publishing a logical device, so a
/// hardware queue with no completion source or watchdog never reaches IRQ
/// binding. [`validate_queue_activation`] subsequently checks that every named
/// source was declared by the controller and bound by the OS.
pub fn validate_queue_info(info: QueueInfo) -> Result<(), QueueContractError> {
    if info.id >= MAX_CONTROLLER_QUEUES {
        return Err(QueueContractError::InvalidControllerQueueId { queue_id: info.id });
    }
    if info.device.num_blocks == 0 || info.device.logical_block_size == 0 {
        return Err(QueueContractError::InvalidDeviceGeometry { queue_id: info.id });
    }
    let limits = info.limits;
    if !limits.dma_alignment.is_power_of_two()
        || limits.max_inflight == 0
        || limits.max_blocks_per_request == 0
        || limits.max_segments == 0
        || limits.max_segment_size == 0
        || limits.max_segment_size.saturating_mul(limits.max_segments)
            < info.device.logical_block_size
    {
        return Err(QueueContractError::InvalidQueueLimits { queue_id: info.id });
    }
    let QueueKind::Interrupt { sources } = info.kind else {
        return Ok(());
    };
    if sources.is_empty() {
        return Err(QueueContractError::MissingInterruptSources { queue_id: info.id });
    }
    if info.limits.request_timeout_ns == 0 {
        return Err(QueueContractError::MissingWatchdog { queue_id: info.id });
    }
    Ok(())
}

/// Validates the ownership transition returned by one queue submission.
///
/// The check borrows the result so either the runtime or a recovery path still
/// owns the exact [`OwnedRequest`] value. A violation involving a retained
/// request requires controller quiescence; callers must not retry the request
/// or free its backing merely because this function returned an error.
pub fn validate_submit_contract(
    info: QueueInfo,
    expected_id: RequestId,
    result: &Result<SubmitOutcome, SubmitError>,
) -> Result<(), QueueContractError> {
    validate_request_identity(info, expected_id)?;
    match (result, info.kind) {
        (Ok(SubmitOutcome::Completed(completion)), _) if completion.id != expected_id => {
            Err(QueueContractError::SubmitRequestIdMismatch {
                queue_id: info.id,
                expected: expected_id,
                returned: completion.id,
            })
        }
        (Err(error), _) if error.id() != expected_id => {
            Err(QueueContractError::SubmitRequestIdMismatch {
                queue_id: info.id,
                expected: expected_id,
                returned: error.id(),
            })
        }
        (Ok(SubmitOutcome::Completed(_)), QueueKind::Interrupt { .. }) => {
            Err(QueueContractError::SynchronousInterruptCompletion { queue_id: info.id })
        }
        (Ok(SubmitOutcome::Queued), QueueKind::Inline) => {
            Err(QueueContractError::QueuedInlineRequest { queue_id: info.id })
        }
        _ => Ok(()),
    }
}

/// Validates that a request identity belongs to the queue completion model.
///
/// Inline requests use one reserved sentinel because no completion lookup can
/// outlive the submission call. Interrupt-backed requests must instead carry a
/// generation-bearing identity suitable for a tag and completion table.
pub const fn validate_request_identity(
    info: QueueInfo,
    id: RequestId,
) -> Result<(), QueueContractError> {
    match (info.kind, id.is_inline()) {
        (QueueKind::Inline, false) => {
            Err(QueueContractError::InlineRequestIdentityRequired { queue_id: info.id })
        }
        (QueueKind::Interrupt { .. }, true) => {
            Err(QueueContractError::InterruptRequestIdentityRequired { queue_id: info.id })
        }
        _ => Ok(()),
    }
}

/// Validates that queue completion and controller recovery describe the same
/// hardware ownership model.
///
/// A mixed controller is interrupt-backed whenever any queue can retain an
/// accepted request asynchronously. Pure inline devices must not expose a
/// hardware recovery endpoint that the runtime could accidentally activate.
pub fn validate_lifecycle_activation(
    queue_kinds: &[QueueKind],
    actual: LifecycleKind,
) -> Result<(), QueueContractError> {
    let expected = if queue_kinds
        .iter()
        .any(|kind| matches!(kind, QueueKind::Interrupt { .. }))
    {
        LifecycleKind::Interrupt
    } else {
        LifecycleKind::Inline
    };
    if actual != expected {
        return Err(QueueContractError::LifecycleMismatch { expected, actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        CompletionHint, DeviceInfo, DispatchMode, Event, QueueLimits, RequestFlags, RequestOp,
    };

    fn source_list(source_id: usize) -> IdList {
        let mut sources = IdList::none();
        sources.insert(source_id);
        sources
    }

    fn interrupt_info() -> QueueInfo {
        QueueInfo {
            id: 2,
            device: DeviceInfo::new(8, 512),
            limits: QueueLimits::simple(512, u64::MAX),
            kind: QueueKind::Interrupt {
                sources: source_list(3),
            },
            dispatch_mode: DispatchMode::Serialized,
        }
    }

    fn flush_request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }

    struct NoopIrq {
        calls: usize,
    }

    impl IrqHandler for NoopIrq {
        fn handle_irq(&mut self) -> crate::IrqOutcome {
            self.calls += 1;
            crate::IrqOutcome::handled(Event::from_hint(CompletionHint::Queue { queue_id: 2 }))
        }
    }

    struct ContractQueue {
        info: QueueInfo,
        pending: Option<(RequestId, OwnedRequest)>,
    }

    impl ContractQueue {
        fn interrupt() -> Self {
            Self {
                info: interrupt_info(),
                pending: None,
            }
        }

        fn inline() -> Self {
            Self {
                info: QueueInfo {
                    kind: QueueKind::Inline,
                    dispatch_mode: DispatchMode::Direct,
                    ..interrupt_info()
                },
                pending: None,
            }
        }
    }

    static UNQUIESCED_QUEUE_DROPS: AtomicUsize = AtomicUsize::new(0);

    struct DropObservedQueue;

    struct ContractViolationQueue {
        calls: Arc<AtomicUsize>,
    }

    struct FailingShutdownQueue {
        calls: Arc<AtomicUsize>,
    }

    impl Drop for DropObservedQueue {
        fn drop(&mut self) {
            UNQUIESCED_QUEUE_DROPS.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl IQueue for DropObservedQueue {
        fn id(&self) -> usize {
            2
        }

        fn info(&self) -> QueueInfo {
            interrupt_info()
        }

        fn submit_owned(
            &mut self,
            id: RequestId,
            request: OwnedRequest,
        ) -> Result<SubmitOutcome, SubmitError> {
            Err(SubmitError::new(id, BlkError::Retry, request))
        }

        fn service_events(
            &mut self,
            _events: &QueueEventBatch<'_>,
            _sink: &mut dyn CompletionSink,
        ) -> Result<ServiceProgress, BlkError> {
            Ok(ServiceProgress::Idle)
        }

        fn reclaim_after_quiesce(
            &mut self,
            _proof: &DmaQuiesced,
            _sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            Ok(())
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }
    }

    impl IQueue for ContractViolationQueue {
        fn id(&self) -> usize {
            2
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                kind: QueueKind::Inline,
                dispatch_mode: DispatchMode::Direct,
                ..interrupt_info()
            }
        }

        fn submit_owned(
            &mut self,
            _id: RequestId,
            request: OwnedRequest,
        ) -> Result<SubmitOutcome, SubmitError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(SubmitError::new(
                RequestId::new(41),
                BlkError::Retry,
                request,
            ))
        }

        fn service_events(
            &mut self,
            _events: &QueueEventBatch<'_>,
            _sink: &mut dyn CompletionSink,
        ) -> Result<ServiceProgress, BlkError> {
            Err(BlkError::NotSupported)
        }

        fn reclaim_after_quiesce(
            &mut self,
            _proof: &DmaQuiesced,
            _sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            Ok(())
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }
    }

    impl IQueue for FailingShutdownQueue {
        fn id(&self) -> usize {
            2
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                kind: QueueKind::Inline,
                dispatch_mode: DispatchMode::Direct,
                ..interrupt_info()
            }
        }

        fn submit_owned(
            &mut self,
            id: RequestId,
            request: OwnedRequest,
        ) -> Result<SubmitOutcome, SubmitError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(SubmitOutcome::Completed(CompletedRequest::new(
                id,
                Ok(()),
                request,
            )))
        }

        fn service_events(
            &mut self,
            _events: &QueueEventBatch<'_>,
            _sink: &mut dyn CompletionSink,
        ) -> Result<ServiceProgress, BlkError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(BlkError::NotSupported)
        }

        fn reclaim_after_quiesce(
            &mut self,
            _proof: &DmaQuiesced,
            _sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(BlkError::Io)
        }
    }

    impl IQueue for ContractQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_owned(
            &mut self,
            id: RequestId,
            request: OwnedRequest,
        ) -> Result<SubmitOutcome, SubmitError> {
            self.pending = Some((id, request));
            Ok(SubmitOutcome::Queued)
        }

        fn service_events(
            &mut self,
            events: &QueueEventBatch<'_>,
            sink: &mut dyn CompletionSink,
        ) -> Result<ServiceProgress, BlkError> {
            assert_eq!(events.queue_id(), self.id());
            if let Some((id, request)) = self.pending.take() {
                sink.complete(CompletedRequest::new(id, Ok(()), request));
            }
            Ok(ServiceProgress::Idle)
        }

        fn reclaim_after_quiesce(
            &mut self,
            _proof: &DmaQuiesced,
            sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            if let Some((id, request)) = self.pending.take() {
                sink.complete(CompletedRequest::new(id, Err(BlkError::Cancelled), request));
            }
            Ok(())
        }

        fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            if self.pending.is_some() {
                return Err(BlkError::Busy);
            }
            let _ = sink;
            Ok(())
        }
    }

    #[derive(Default)]
    struct OwnedCompletionSink {
        completion: Option<CompletedRequest>,
    }

    impl CompletionSink for OwnedCompletionSink {
        fn complete(&mut self, completion: CompletedRequest) {
            self.completion = Some(completion);
        }
    }

    #[test]
    fn boxed_irq_handler_is_move_only_and_mutable() {
        let mut handler: BIrqHandler = Box::new(NoopIrq { calls: 0 });

        assert!(
            handler
                .handle_irq()
                .acknowledged_event()
                .is_some_and(|event| event.for_queue(2).is_some())
        );
        assert!(
            handler
                .handle_irq()
                .acknowledged_event()
                .is_some_and(|event| event.for_queue(2).is_some())
        );
    }

    #[test]
    fn runtime_allocated_request_id_round_trips_with_owned_request() {
        let mut queue = ContractQueue::interrupt();
        let request_id = RequestId::new(41);

        let outcome = queue.submit_owned(request_id, flush_request()).unwrap();

        assert!(matches!(outcome, SubmitOutcome::Queued));
        assert_eq!(
            queue.pending.as_ref().map(|pending| pending.0),
            Some(request_id)
        );

        let event = Event::from_hint(CompletionHint::Queue { queue_id: 2 });
        let events = event
            .for_queue(2)
            .expect("IRQ event must contain the target queue");
        let mut sink = OwnedCompletionSink::default();
        assert_eq!(
            queue.service_events(&events, &mut sink),
            Ok(ServiceProgress::Idle)
        );

        let completion = sink.completion.expect("completion must be returned");
        assert_eq!(completion.id, request_id);
        assert_eq!(completion.result, Ok(()));
        assert!(matches!(completion.request.op, RequestOp::Flush));
        assert_eq!(completion.request.lba, 0);
        assert_eq!(completion.request.block_count, 0);
        assert!(completion.request.data.is_none());
    }

    #[test]
    fn inline_submit_returns_request_ownership_without_polling() {
        let request_id = RequestId::INLINE;
        let outcome =
            SubmitOutcome::Completed(CompletedRequest::new(request_id, Ok(()), flush_request()));

        let SubmitOutcome::Completed(completion) = outcome else {
            panic!("inline request must complete during submission");
        };
        assert_eq!(completion.id, request_id);
        assert!(matches!(completion.request.op, RequestOp::Flush));
    }

    #[test]
    fn submit_error_returns_runtime_id_and_request_ownership() {
        let request_id = RequestId::new(19);
        let error = SubmitError::new(request_id, BlkError::Retry, flush_request());

        assert_eq!(error.id(), request_id);
        assert_eq!(error.error(), BlkError::Retry);
        let (returned_id, returned_error, request) = error.into_parts();
        assert_eq!(returned_id, request_id);
        assert_eq!(returned_error, BlkError::Retry);
        assert_eq!(request.op, RequestOp::Flush);
    }

    #[test]
    fn submit_contract_rejects_synchronous_hardware_completion() {
        let info = interrupt_info();
        let request_id = RequestId::new(23);
        let result = Ok(SubmitOutcome::Completed(CompletedRequest::new(
            request_id,
            Ok(()),
            flush_request(),
        )));

        assert_eq!(
            validate_submit_contract(info, request_id, &result),
            Err(QueueContractError::SynchronousInterruptCompletion { queue_id: 2 })
        );
        let Ok(SubmitOutcome::Completed(completion)) = result else {
            panic!("contract validation must preserve terminal ownership");
        };
        assert_eq!(completion.id, request_id);
        assert_eq!(completion.request.op, RequestOp::Flush);
    }

    #[test]
    fn submit_contract_rejects_an_inline_queue_that_retains_ownership() {
        let info = QueueInfo {
            kind: QueueKind::Inline,
            ..interrupt_info()
        };
        let request_id = RequestId::INLINE;
        let result = Ok(SubmitOutcome::Queued);

        assert_eq!(
            validate_submit_contract(info, request_id, &result),
            Err(QueueContractError::QueuedInlineRequest { queue_id: 2 })
        );
    }

    #[test]
    fn submit_contract_rejects_rewritten_request_ids_on_every_return_path() {
        let expected_id = RequestId::INLINE;
        let returned_id = RequestId::new(32);
        let inline_info = QueueInfo {
            kind: QueueKind::Inline,
            ..interrupt_info()
        };
        let completion = Ok(SubmitOutcome::Completed(CompletedRequest::new(
            returned_id,
            Ok(()),
            flush_request(),
        )));
        let rejection = Err(SubmitError::new(
            returned_id,
            BlkError::Retry,
            flush_request(),
        ));
        let expected_error = Err(QueueContractError::SubmitRequestIdMismatch {
            queue_id: 2,
            expected: expected_id,
            returned: returned_id,
        });

        assert_eq!(
            validate_submit_contract(inline_info, expected_id, &completion),
            expected_error
        );
        assert_eq!(
            validate_submit_contract(inline_info, expected_id, &rejection),
            expected_error
        );
    }

    #[test]
    fn queue_handle_closes_admission_after_a_submit_contract_violation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handle = QueueHandle::new(Box::new(ContractViolationQueue {
            calls: Arc::clone(&calls),
        }));
        let first_id = RequestId::INLINE;
        let second_id = RequestId::INLINE;

        let first = handle
            .submit_owned(first_id, flush_request())
            .expect_err("the fake driver intentionally rewrites the request identity");
        assert_ne!(first.id(), first_id);

        let second = handle
            .submit_owned(second_id, flush_request())
            .expect_err("a poisoned queue must reject later admission before driver entry");
        assert_eq!(second.id(), second_id);
        assert_eq!(second.error(), BlkError::Offline);
        assert_eq!(calls.load(Ordering::Acquire), 1);

        handle
            .shutdown(&mut OwnedCompletionSink::default())
            .unwrap();
    }

    #[test]
    fn inline_queue_rejects_a_generation_identity_before_driver_entry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handle = QueueHandle::new(Box::new(ContractViolationQueue {
            calls: Arc::clone(&calls),
        }));
        let generated_id = RequestId::new(42);

        let rejection = handle
            .submit_owned(generated_id, flush_request())
            .expect_err("inline submission must use its reserved identity");

        assert_eq!(rejection.id(), generated_id);
        assert_eq!(rejection.error(), BlkError::InvalidRequest);
        assert_eq!(calls.load(Ordering::Acquire), 0);
        handle
            .shutdown(&mut OwnedCompletionSink::default())
            .unwrap();
    }

    #[test]
    fn interrupt_queue_rejects_the_inline_identity_before_driver_entry() {
        let mut handle = QueueHandle::new(Box::new(ContractQueue::interrupt()));
        handle
            .bind_interrupt_controller(1, ControllerEpoch::INITIAL)
            .unwrap();

        let rejection = handle
            .submit_owned(RequestId::INLINE, flush_request())
            .expect_err("interrupt submission requires a generation identity");

        assert_eq!(rejection.id(), RequestId::INLINE);
        assert_eq!(rejection.error(), BlkError::InvalidRequest);
        handle
            .shutdown(&mut OwnedCompletionSink::default())
            .unwrap();
    }

    #[test]
    fn request_identity_validation_reports_the_queue_mode() {
        let inline = QueueInfo {
            kind: QueueKind::Inline,
            ..interrupt_info()
        };
        let interrupt = interrupt_info();

        assert_eq!(validate_request_identity(inline, RequestId::INLINE), Ok(()));
        assert_eq!(
            validate_request_identity(inline, RequestId::new(1)),
            Err(QueueContractError::InlineRequestIdentityRequired { queue_id: 2 })
        );
        assert_eq!(
            validate_request_identity(interrupt, RequestId::INLINE),
            Err(QueueContractError::InterruptRequestIdentityRequired { queue_id: 2 })
        );
        assert_eq!(
            validate_request_identity(interrupt, RequestId::new(1)),
            Ok(())
        );
    }

    #[test]
    fn interrupt_queue_activation_requires_declared_and_bound_sources() {
        let info = interrupt_info();
        let declared = [IrqSourceInfo::new(3, source_list(info.id))];

        assert_eq!(
            validate_queue_activation(info, &declared, IdList::none()),
            Err(QueueContractError::UnboundInterruptSource {
                queue_id: 2,
                source_id: 3,
            })
        );
        assert_eq!(
            validate_queue_activation(info, &declared, source_list(3)),
            Ok(())
        );
    }

    #[test]
    fn interrupt_queue_rejects_empty_source_contract() {
        let info = QueueInfo {
            kind: QueueKind::Interrupt {
                sources: IdList::none(),
            },
            ..interrupt_info()
        };

        assert_eq!(
            validate_queue_activation(info, &[], IdList::none()),
            Err(QueueContractError::MissingInterruptSources { queue_id: 2 })
        );
    }

    #[test]
    fn queue_handle_cannot_bind_an_interrupt_queue_without_a_source() {
        let info = QueueInfo {
            kind: QueueKind::Interrupt {
                sources: IdList::none(),
            },
            ..interrupt_info()
        };
        let mut handle = QueueHandle::new(Box::new(ContractQueue {
            info,
            pending: None,
        }));

        assert_eq!(
            handle.bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL),
            Err(QueueContractError::MissingInterruptSources { queue_id: 2 })
        );
        handle
            .shutdown(&mut OwnedCompletionSink::default())
            .unwrap();
    }

    #[test]
    fn interrupt_queue_rejects_a_zero_watchdog_budget() {
        let mut info = interrupt_info();
        info.limits.request_timeout_ns = 0;
        let declared = [IrqSourceInfo::new(3, source_list(info.id))];

        assert_eq!(
            validate_queue_activation(info, &declared, source_list(3)),
            Err(QueueContractError::MissingWatchdog { queue_id: 2 })
        );
    }

    #[test]
    fn activation_rejects_a_queue_identity_outside_the_routing_bitmap() {
        let mut info = interrupt_info();
        info.id = MAX_CONTROLLER_QUEUES;

        assert_eq!(
            validate_queue_activation(info, &[], IdList::none()),
            Err(QueueContractError::InvalidControllerQueueId {
                queue_id: MAX_CONTROLLER_QUEUES,
            })
        );
    }

    #[test]
    fn activation_rejects_zero_capacity_or_block_size() {
        let mut info = interrupt_info();
        info.device.num_blocks = 0;
        assert_eq!(
            validate_queue_info(info),
            Err(QueueContractError::InvalidDeviceGeometry { queue_id: 2 })
        );

        info.device = DeviceInfo::new(8, 0);
        assert_eq!(
            validate_queue_info(info),
            Err(QueueContractError::InvalidDeviceGeometry { queue_id: 2 })
        );
    }

    #[test]
    fn activation_rejects_unusable_queue_limits() {
        let invalid_limits = [
            QueueLimits {
                max_inflight: 0,
                ..interrupt_info().limits
            },
            QueueLimits {
                max_blocks_per_request: 0,
                ..interrupt_info().limits
            },
            QueueLimits {
                max_segments: 0,
                ..interrupt_info().limits
            },
            QueueLimits {
                max_segment_size: 0,
                ..interrupt_info().limits
            },
            QueueLimits {
                dma_alignment: 3,
                ..interrupt_info().limits
            },
        ];

        for limits in invalid_limits {
            let info = QueueInfo {
                limits,
                ..interrupt_info()
            };
            assert_eq!(
                validate_queue_info(info),
                Err(QueueContractError::InvalidQueueLimits { queue_id: 2 })
            );
        }
    }

    #[test]
    fn binding_an_inline_queue_reports_the_actual_lifecycle() {
        let mut handle = QueueHandle::new(Box::new(ContractQueue::inline()));

        assert_eq!(
            handle.bind_interrupt_controller(1, ControllerEpoch::INITIAL),
            Err(QueueContractError::LifecycleMismatch {
                expected: LifecycleKind::Interrupt,
                actual: LifecycleKind::Inline,
            })
        );

        handle
            .shutdown(&mut OwnedCompletionSink::default())
            .unwrap();
    }

    #[test]
    fn explicit_shutdown_returns_every_accepted_request() {
        let mut handle = QueueHandle::new(Box::new(ContractQueue::interrupt()));
        handle
            .bind_interrupt_controller(1, ControllerEpoch::INITIAL)
            .unwrap();
        let request_id = RequestId::new(53);
        assert!(matches!(
            handle.submit_owned(request_id, flush_request()).unwrap(),
            SubmitOutcome::Queued
        ));
        let mut sink = OwnedCompletionSink::default();

        let proof = unsafe {
            // SAFETY: the contract queue owns no hardware or DMA.
            DmaQuiesced::new(crate::ControllerEpoch::new(2), 1)
        };
        handle.reclaim_after_quiesce(&proof, &mut sink).unwrap();

        handle.shutdown(&mut sink).unwrap();

        let completion = sink.completion.expect("shutdown must return ownership");
        assert_eq!(completion.id, request_id);
        assert_eq!(completion.result, Err(BlkError::Cancelled));
        assert_eq!(completion.request.op, RequestOp::Flush);
    }

    #[test]
    fn dropping_an_unquiesced_queue_leaks_it_fail_closed_without_panicking() {
        UNQUIESCED_QUEUE_DROPS.store(0, Ordering::Release);

        drop(QueueHandle::new(Box::new(DropObservedQueue)));

        assert_eq!(
            UNQUIESCED_QUEUE_DROPS.load(Ordering::Acquire),
            0,
            "an unquiesced driver object may still own DMA and must not be destroyed"
        );
    }

    #[test]
    fn failed_shutdown_is_a_one_shot_driver_transaction() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handle = QueueHandle::new(Box::new(FailingShutdownQueue {
            calls: Arc::clone(&calls),
        }));
        let mut sink = OwnedCompletionSink::default();

        assert_eq!(handle.shutdown(&mut sink), Err(BlkError::Io));
        assert_eq!(handle.shutdown(&mut sink), Err(BlkError::Offline));

        let rejection = handle
            .submit_owned(RequestId::INLINE, flush_request())
            .expect_err("shutdown attempt must permanently close admission");
        assert_eq!(rejection.error(), BlkError::Offline);

        let event = Event::from_hint(CompletionHint::Queue { queue_id: 2 });
        let batch = event
            .for_queue(2)
            .expect("test event must address the retained queue");
        assert_eq!(
            handle.service_events(&batch, &mut sink),
            Err(BlkError::Offline)
        );

        let proof = unsafe {
            // SAFETY: the inline fixture owns no hardware or DMA.
            DmaQuiesced::new(ControllerEpoch::new(2), 1)
        };
        assert_eq!(
            handle.reclaim_after_quiesce(&proof, &mut sink),
            Err(BlkError::Offline)
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn queue_handle_rejects_irq_evidence_for_a_sibling_queue() {
        let mut handle = QueueHandle::new(Box::new(ContractQueue::interrupt()));
        handle
            .bind_interrupt_controller(1, ControllerEpoch::INITIAL)
            .unwrap();
        let request_id = RequestId::new(61);
        assert!(matches!(
            handle.submit_owned(request_id, flush_request()),
            Ok(SubmitOutcome::Queued)
        ));
        let sibling_event = Event::from_hint(CompletionHint::Queue { queue_id: 7 });
        let sibling_batch = sibling_event
            .for_queue(7)
            .expect("sibling event must carry its own queue evidence");
        let mut sink = OwnedCompletionSink::default();

        assert_eq!(
            handle.service_events(&sibling_batch, &mut sink),
            Err(BlkError::InvalidRequest)
        );
        assert!(sink.completion.is_none());

        let proof = unsafe {
            // SAFETY: the contract queue owns no hardware or DMA.
            DmaQuiesced::new(crate::ControllerEpoch::new(2), 1)
        };
        handle.reclaim_after_quiesce(&proof, &mut sink).unwrap();
        handle.shutdown(&mut sink).unwrap();
    }
}
