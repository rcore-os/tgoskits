use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, ControllerEpoch, ControllerReady, DmaQuiesced,
    IQueue, InitInput, InitPoll, InterruptLifecycle, LifecycleEndpoint, LifecycleKind,
    OwnedRequest, QueueContractError, QueueEventBatch, QueueExecution, QueueHandle, QueueInfo,
    QueueKind, RecoveryCause, RequestId, ServiceProgress, SubmitError, SubmitOutcome,
    validate_lifecycle_activation, validate_lifecycle_identity,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Running,
    Quiescing,
    Quiesced,
    GuestOwned,
    Reinitializing,
    Ready,
}

struct FakeLifecycle {
    state: State,
    epoch: ControllerEpoch,
    cookie: usize,
}

impl InterruptLifecycle for FakeLifecycle {
    fn controller_cookie(&self) -> usize {
        self.cookie
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), rdif_block::InitError> {
        if !matches!(self.state, State::Running | State::GuestOwned) {
            return Err(rdif_block::InitError::InvalidState);
        }
        self.epoch = epoch;
        self.state = State::Quiescing;
        Ok(())
    }

    fn poll_dma_quiesce(&mut self, _input: InitInput) -> InitPoll<DmaQuiesced> {
        self.state = State::Quiesced;
        InitPoll::Ready(unsafe {
            // SAFETY: the fake has no hardware or DMA and therefore reaches a
            // quiesced state immediately.
            DmaQuiesced::new(self.epoch, self.cookie)
        })
    }

    fn enter_guest_owned(&mut self, quiesced: DmaQuiesced) -> Result<(), rdif_block::InitError> {
        assert_eq!(quiesced.epoch(), self.epoch);
        assert_eq!(quiesced.controller_cookie(), self.cookie);
        assert_eq!(self.state, State::Quiesced);
        self.state = State::GuestOwned;
        Ok(())
    }

    fn begin_reinitialize(&mut self, quiesced: DmaQuiesced) -> Result<(), rdif_block::InitError> {
        assert_eq!(quiesced.epoch(), self.epoch);
        assert_eq!(quiesced.controller_cookie(), self.cookie);
        self.state = State::Reinitializing;
        Ok(())
    }

    fn poll_reinitialize(&mut self, _input: InitInput) -> InitPoll<ControllerReady> {
        self.state = State::Ready;
        InitPoll::Ready(unsafe {
            // SAFETY: the fake has reconstructed all of its empty state.
            ControllerReady::new(self.epoch, self.cookie)
        })
    }
}

#[test]
fn lifecycle_proofs_are_generation_and_controller_bound() {
    let epoch = ControllerEpoch::new(7);
    let mut lifecycle = FakeLifecycle {
        state: State::Running,
        epoch,
        cookie: 0x51a7,
    };
    assert_eq!(
        LifecycleEndpoint::Interrupt(&mut lifecycle).kind(),
        LifecycleKind::Interrupt
    );
    assert_eq!(lifecycle.controller_cookie(), 0x51a7);

    lifecycle
        .begin_dma_quiesce(epoch, RecoveryCause::QueueFault { queue_id: 3 })
        .unwrap();
    let InitPoll::Ready(proof) = lifecycle.poll_dma_quiesce(InitInput::at(100)) else {
        panic!("fake lifecycle must quiesce immediately");
    };
    lifecycle.begin_reinitialize(proof).unwrap();
    let InitPoll::Ready(ready) = lifecycle.poll_reinitialize(InitInput::at(101)) else {
        panic!("fake lifecycle must reinitialize immediately");
    };
    assert_eq!(ready.epoch(), epoch);
    assert_eq!(ready.controller_cookie(), 0x51a7);
}

#[test]
fn guest_ownership_consumes_the_old_proof_before_return_quiescence_starts() {
    let first_epoch = ControllerEpoch::new(7);
    let second_epoch = ControllerEpoch::new(8);
    let mut lifecycle = FakeLifecycle {
        state: State::Running,
        epoch: first_epoch,
        cookie: 0x51a7,
    };

    lifecycle
        .begin_dma_quiesce(first_epoch, RecoveryCause::Handoff)
        .unwrap();
    let InitPoll::Ready(proof) = lifecycle.poll_dma_quiesce(InitInput::at(100)) else {
        panic!("fake lifecycle must quiesce before guest ownership")
    };
    lifecycle.enter_guest_owned(proof).unwrap();
    assert_eq!(lifecycle.state, State::GuestOwned);

    lifecycle
        .begin_dma_quiesce(second_epoch, RecoveryCause::Handoff)
        .unwrap();
    assert_eq!(lifecycle.epoch, second_epoch);
    assert_eq!(lifecycle.state, State::Quiescing);
}

#[test]
fn inline_endpoint_cannot_be_misread_as_interrupt_lifecycle() {
    assert_eq!(LifecycleEndpoint::Inline.kind(), LifecycleKind::Inline);
}

#[test]
fn queue_completion_kind_must_match_controller_lifecycle() {
    let inline = [QueueKind::Inline];
    assert_eq!(
        validate_lifecycle_activation(&inline, LifecycleKind::Inline),
        Ok(())
    );
    assert_eq!(
        validate_lifecycle_activation(&inline, LifecycleKind::Interrupt),
        Err(QueueContractError::LifecycleMismatch {
            expected: LifecycleKind::Inline,
            actual: LifecycleKind::Interrupt,
        })
    );

    let interrupt = [QueueKind::Interrupt {
        sources: rdif_block::IdList::from_bits(1),
    }];
    assert_eq!(
        validate_lifecycle_activation(&interrupt, LifecycleKind::Inline),
        Err(QueueContractError::LifecycleMismatch {
            expected: LifecycleKind::Interrupt,
            actual: LifecycleKind::Inline,
        })
    );
}

#[test]
fn interrupt_lifecycle_requires_a_stable_nonzero_identity() {
    assert_eq!(
        validate_lifecycle_identity(LifecycleKind::Interrupt, 0),
        Err(QueueContractError::InvalidLifecycleIdentity)
    );
    assert_eq!(
        validate_lifecycle_identity(LifecycleKind::Interrupt, 0x51a7),
        Ok(())
    );
    assert_eq!(
        validate_lifecycle_identity(LifecycleKind::Inline, 0),
        Ok(())
    );
}

struct ReclaimQueue {
    pending: Option<(RequestId, OwnedRequest)>,
}

impl IQueue for ReclaimQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: 0,
            device: rdif_block::DeviceInfo::new(1, 512),
            limits: rdif_block::QueueLimits::simple(512, u64::MAX),
            kind: QueueKind::Interrupt {
                sources: rdif_block::IdList::from_bits(1),
            },
            execution: QueueExecution::Serialized,
        }
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
        _events: &QueueEventBatch<'_>,
        _sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
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

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.pending.is_none().then_some(()).ok_or(BlkError::Busy)
    }
}

#[derive(Default)]
struct ReclaimSink {
    completion: Option<CompletedRequest>,
}

impl CompletionSink for ReclaimSink {
    fn complete(&mut self, completion: CompletedRequest) {
        self.completion = Some(completion);
    }
}

#[test]
fn accepted_ownership_is_reclaimed_only_with_dma_proof() {
    let mut queue = QueueHandle::new(Box::new(ReclaimQueue { pending: None }));
    queue
        .bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL)
        .expect("the runtime must bind the retained controller before publication");
    let request = OwnedRequest {
        op: rdif_block::RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: rdif_block::RequestFlags::NONE,
    };
    queue.submit_owned(RequestId::new(1), request).unwrap();
    let proof = unsafe {
        // SAFETY: this fake queue owns no hardware or DMA.
        DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
    };
    let mut sink = ReclaimSink::default();

    queue.reclaim_after_quiesce(&proof, &mut sink).unwrap();
    queue.close().unwrap();

    assert!(sink.completion.is_some());
}

#[test]
fn interrupt_queue_cannot_publish_before_its_controller_identity_is_bound() {
    let mut queue = QueueHandle::new(Box::new(ReclaimQueue { pending: None }));
    let request = OwnedRequest {
        op: rdif_block::RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: rdif_block::RequestFlags::NONE,
    };
    let rejection = queue
        .submit_owned(RequestId::new(2), request)
        .expect_err("an unbound interrupt queue must remain unpublished");
    let (id, error, request) = rejection.into_parts();
    assert_eq!(id, RequestId::new(2));
    assert_eq!(error, BlkError::Offline);
    assert!(matches!(request.op, rdif_block::RequestOp::Flush));

    queue
        .bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL)
        .unwrap();
    assert_eq!(
        queue.bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL),
        Err(QueueContractError::LifecycleIdentityAlreadyBound { queue_id: 0 })
    );
    let proof = unsafe {
        // SAFETY: this fake queue owns no hardware or DMA.
        DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
    };
    queue
        .reclaim_after_quiesce(&proof, &mut ReclaimSink::default())
        .unwrap();
    queue.close().unwrap();
}

#[test]
fn queue_handle_rejects_foreign_and_replayed_dma_proofs_before_driver_code() {
    let mut queue = QueueHandle::new(Box::new(ReclaimQueue { pending: None }));
    queue
        .bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL)
        .expect("the runtime must bind the retained controller before publication");
    let request = OwnedRequest {
        op: rdif_block::RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: rdif_block::RequestFlags::NONE,
    };
    queue.submit_owned(RequestId::new(2), request).unwrap();
    let foreign = unsafe {
        // SAFETY: both fake controllers own no hardware or DMA. The foreign
        // cookie intentionally does not identify this queue's controller.
        DmaQuiesced::new(ControllerEpoch::new(2), 0x600d)
    };
    let publication_epoch = unsafe {
        // SAFETY: the fake queue owns no hardware or DMA. This proof is stale
        // because its epoch is the queue publication epoch, not a later
        // controller quiescence transition.
        DmaQuiesced::new(ControllerEpoch::new(1), 0x51a7)
    };
    let matching = unsafe {
        // SAFETY: the fake queue owns no hardware or DMA.
        DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
    };
    let mut sink = ReclaimSink::default();

    assert_eq!(
        queue.reclaim_after_quiesce(&foreign, &mut sink),
        Err(BlkError::InvalidDmaProof)
    );
    assert_eq!(
        queue.reclaim_after_quiesce(&publication_epoch, &mut sink),
        Err(BlkError::InvalidDmaProof)
    );
    assert!(
        sink.completion.is_none(),
        "a foreign proof must not reach a permissive driver implementation"
    );

    queue
        .reclaim_after_quiesce(&matching, &mut sink)
        .expect("the bound controller proof must reclaim accepted ownership");
    assert!(sink.completion.is_some());
    assert_eq!(
        queue.reclaim_after_quiesce(&matching, &mut ReclaimSink::default()),
        Err(BlkError::InvalidDmaProof),
        "one queue may consume a controller epoch at most once"
    );
    queue.close().unwrap();
}

struct FailingCloseQueue {
    drops: Arc<AtomicUsize>,
}

struct IncompleteReclaimQueue {
    pending: Option<(RequestId, OwnedRequest)>,
    shutdown_calls: Arc<AtomicUsize>,
}

impl Drop for FailingCloseQueue {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

impl IQueue for FailingCloseQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            kind: QueueKind::Inline,
            execution: QueueExecution::Inline,
            ..ReclaimQueue { pending: None }.info()
        }
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        Err(SubmitError::new(id, BlkError::Offline, request))
    }

    fn service_events(
        &mut self,
        _events: &QueueEventBatch<'_>,
        _sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        Err(BlkError::Offline)
    }

    fn reclaim_after_quiesce(
        &mut self,
        _proof: &DmaQuiesced,
        _sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        Err(BlkError::Io)
    }
}

impl IQueue for IncompleteReclaimQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        ReclaimQueue { pending: None }.info()
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

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.shutdown_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[test]
fn failed_close_returns_a_named_quarantine_owner_without_dropping_the_endpoint() {
    let drops = Arc::new(AtomicUsize::new(0));
    let queue = QueueHandle::new(Box::new(FailingCloseQueue {
        drops: Arc::clone(&drops),
    }));

    let failure = queue
        .close()
        .expect_err("the fixture must fail its one-shot close transaction");
    assert_eq!(failure.error(), BlkError::Io);
    assert_eq!(drops.load(Ordering::Acquire), 0);

    let quarantine = failure.into_quarantine();
    assert_eq!(quarantine.reason(), BlkError::Io);
    drop(quarantine);
    assert_eq!(
        drops.load(Ordering::Acquire),
        0,
        "dropping a named quarantine must retain a possibly DMA-visible endpoint"
    );
}

#[test]
fn accepted_ledger_blocks_destroy_when_dma_reclaim_omits_an_owner() {
    let shutdown_calls = Arc::new(AtomicUsize::new(0));
    let mut queue = QueueHandle::new(Box::new(IncompleteReclaimQueue {
        pending: None,
        shutdown_calls: Arc::clone(&shutdown_calls),
    }));
    queue
        .bind_interrupt_controller(0x51a7, ControllerEpoch::INITIAL)
        .unwrap();
    queue
        .submit_owned(
            RequestId::new(9),
            OwnedRequest {
                op: rdif_block::RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: rdif_block::RequestFlags::NONE,
            },
        )
        .unwrap();
    let proof = unsafe {
        // SAFETY: the fake queue owns no hardware or DMA.
        DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
    };

    assert_eq!(
        queue.reclaim_after_quiesce(&proof, &mut ReclaimSink::default()),
        Err(BlkError::Busy)
    );
    let failure = queue
        .close()
        .expect_err("an accepted owner missing after reclaim must prevent destruction");
    assert_eq!(failure.error(), BlkError::Busy);
    assert_eq!(shutdown_calls.load(Ordering::Acquire), 0);
    drop(failure.into_quarantine());
}
