use alloc::{
    alloc::{alloc_zeroed, dealloc},
    sync::Arc,
};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull, sync::atomic::AtomicBool};

use rdif_block::{
    CompletionHint, Event, InitIrqProgress, InterruptLifecycle,
    dma_api::{CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection},
};
use sdio_host2::{DataBuffer, RequestPoll, SdioHost as PhysicalSdioHost, Transaction};

use super::*;
use crate::{
    BlockPoll, BlockRequestId, CommandResponsePoll, DataCommandPoll, Error, OperationPoll,
    cmd::Command,
    sdio::{
        card::SdioSdmmc,
        host::{ClockSpeed, HostEvent, HostEventKind, SdioHost, SdioIrqHandle, SdioIrqHost},
        host2::{SdioHost2Adapter, SdioHost2Irq},
    },
};

fn test_dma() -> DeviceDma {
    DeviceDma::new_legacy(u64::MAX, &TEST_DMA)
}

fn dma_config() -> BlockConfig {
    BlockConfig::dma("mock-sd", 32, test_dma())
        .with_max_blocks_per_request(8)
        .with_max_segment_size(8 * BLOCK_SIZE)
}

fn interrupt_pio_config() -> BlockConfig {
    BlockConfig::interrupt_pio("mock-sd-pio", 32)
}

fn dma_quiesced(control: &BlockControl<MockHost>) -> rdif_block::DmaQuiesced {
    unsafe {
        // SAFETY: MockHost performs no real DMA and each test serializes all
        // request access before constructing this proof.
        rdif_block::DmaQuiesced::new(
            rdif_block::ControllerEpoch::new(1),
            control.controller_cookie(),
        )
    }
}

fn block_control(config: BlockConfig) -> Arc<BlockControl<MockHost>> {
    let raw = SharedCore::new(SdioSdmmc::new(MockHost::default()));
    Arc::new(BlockControl {
        raw,
        config,
        irq_enabled: AtomicBool::new(true),
        queue_taken: AtomicBool::new(false),
    })
}

fn cpu_dma(direction: DmaDirection) -> CpuDmaBuffer {
    CpuDmaBuffer::new_zero(
        &test_dma(),
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        direction,
    )
    .expect("test DMA allocation must succeed")
}

fn request(op: rdif_block::RequestOp, direction: DmaDirection) -> OwnedRequest {
    OwnedRequest {
        op,
        lba: 0,
        block_count: 1,
        data: Some(cpu_dma(direction)),
        flags: rdif_block::RequestFlags::NONE,
    }
}

fn request_event(id: RequestId) -> Event {
    Event::from_hint(CompletionHint::Request {
        queue_id: 0,
        request_id: id,
    })
}

#[derive(Default)]
struct OneCompletionSink {
    completion: Option<CompletedRequest>,
    calls: usize,
}

impl CompletionSink for OneCompletionSink {
    fn complete(&mut self, completion: CompletedRequest) {
        assert!(
            self.completion.is_none(),
            "fixed test sink has capacity for one completion"
        );
        self.calls += 1;
        self.completion = Some(completion);
    }
}

impl OneCompletionSink {
    fn take(&mut self) -> CompletedRequest {
        self.completion.take().expect("completion must be present")
    }
}

#[test]
fn fifo_config_is_initialization_only_and_cannot_create_runtime_queue() {
    let config = BlockConfig::fifo("mock-sd", 8);
    let limits = queue_limits(&config, DEFAULT_DMA_MASK);
    let mut device = BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), config);

    assert_eq!(limits.max_inflight, 1);
    assert_eq!(limits.max_blocks_per_request, 1);
    assert_eq!(limits.max_segment_size, BLOCK_SIZE);
    assert!(Interface::create_queue(&mut device).is_none());
    assert!(Interface::irq_sources(&device).is_empty());
    assert_eq!(Interface::enable_irq(&device), Err(BlkError::NotSupported));
}

#[test]
fn interrupt_pio_ready_device_publishes_only_an_interrupt_queue() {
    let config = interrupt_pio_config();
    let mut device = BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), config);

    assert_eq!(Interface::enable_irq(&device), Ok(()));
    let queue = Interface::create_queue(&mut device).expect("PIO Ready must publish a queue");
    let QueueKind::Interrupt { sources } = queue.info().kind else {
        panic!("normal PIO must never publish an inline or polling queue");
    };
    assert_eq!(sources.bits(), 1);
    assert_eq!(Interface::irq_sources(&device).len(), 1);
}

#[test]
fn lifecycle_irq_is_acknowledged_only_after_the_register_gate_is_acquired() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let raw = device.control.raw.clone();
    raw.with_mut(|card| card.host_mut().deferred_ack_contended = true);

    assert_eq!(device.service_deferred_irq(0), InitIrqProgress::Deferred);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 1);
        card.host_mut().deferred_ack_contended = false;
        card.host_mut().deferred_ack_unhandled = true;
    });

    assert_eq!(device.service_deferred_irq(0), InitIrqProgress::Unhandled);
    raw.with_mut(|card| card.host_mut().deferred_ack_unhandled = false);
    assert_eq!(
        device.service_deferred_irq(0),
        InitIrqProgress::Acknowledged
    );
    assert_eq!(device.service_deferred_irq(1), InitIrqProgress::Unhandled);
    raw.with_mut(|card| assert_eq!(card.host().deferred_ack_calls, 3));
}

#[test]
fn lifecycle_irq_ack_error_is_a_typed_failure_not_an_unhandled_source() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    device
        .control
        .raw
        .with_mut(|card| card.host_mut().fail_deferred_ack = true);

    assert_eq!(
        device.service_deferred_irq(0),
        InitIrqProgress::Failed(rdif_block::InitError::Hardware(
            "SD/MMC controller IRQ acknowledgement failed",
        ))
    );
}

#[test]
fn shared_core_contention_defers_lifecycle_irq_without_touching_hardware() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let raw = device.control.raw.clone();
    let held = raw
        .try_borrow_mut()
        .expect("test setup must own the shared core");

    assert_eq!(device.service_deferred_irq(0), InitIrqProgress::Deferred);

    drop(held);
    raw.with_mut(|card| assert_eq!(card.host().deferred_ack_calls, 0));
    assert_eq!(
        device.service_deferred_irq(0),
        InitIrqProgress::Acknowledged
    );
}

#[test]
fn interrupt_pio_completion_returns_the_exact_owned_buffer() {
    let control = block_control(interrupt_pio_config());
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(41);
    let request = request(rdif_block::RequestOp::Read, DmaDirection::FromDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();

    assert!(matches!(
        IQueue::submit_owned(&mut queue, runtime_id, request),
        Ok(SubmitOutcome::Queued)
    ));
    let mut sink = OneCompletionSink::default();
    IQueue::service_events(
        &mut queue,
        &request_event(runtime_id).for_queue(0).unwrap(),
        &mut sink,
    )
    .unwrap();

    let completion = sink.take();
    assert_eq!(completion.result, Ok(()));
    assert_eq!(completion.request.data.unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn interrupt_pio_submit_failure_returns_the_exact_owned_buffer() {
    let control = block_control(interrupt_pio_config());
    control
        .raw
        .with_mut(|card| card.host_mut().fail_submit = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(42);
    let request = request(rdif_block::RequestOp::Write, DmaDirection::ToDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();

    let error = IQueue::submit_owned(&mut queue, runtime_id, request)
        .expect_err("mock host must reject this request before activation");
    let (_, error_kind, returned) = error.into_parts();

    assert_eq!(error_kind, BlkError::Retry);
    assert_eq!(returned.data.unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn shared_core_contention_rejects_submit_with_retry_and_original_buffer() {
    let control = block_control(interrupt_pio_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(420);
    let request = request(rdif_block::RequestOp::Write, DmaDirection::ToDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();
    let held = raw
        .try_borrow_mut()
        .expect("test setup must own the shared core");

    let error = IQueue::submit_owned(&mut queue, runtime_id, request)
        .expect_err("a contended shared core cannot accept request ownership");
    let (returned_id, error, request) = error.into_parts();
    assert_eq!(returned_id, runtime_id);
    assert_eq!(error, BlkError::Retry);
    assert_eq!(request.data.unwrap().cpu_ptr(), original_ptr);
    assert!(queue.pending.is_none());

    drop(held);
}

#[test]
fn interrupt_pio_error_returns_buffer_only_after_proof_gated_reclaim() {
    let control = block_control(interrupt_pio_config());
    control
        .raw
        .with_mut(|card| card.host_mut().fail_service = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(43);
    let request = request(rdif_block::RequestOp::Read, DmaDirection::FromDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();
    IQueue::submit_owned(&mut queue, runtime_id, request).unwrap();
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(
            &mut queue,
            &request_event(runtime_id).for_queue(0).unwrap(),
            &mut sink,
        ),
        Err(BlkError::Io)
    );
    assert_eq!(sink.calls, 0);

    let proof = dma_quiesced(&queue.control);
    IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink).unwrap();
    let completion = sink.take();
    assert_eq!(completion.result, Err(BlkError::Io));
    assert_eq!(completion.request.data.unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn foreign_controller_proof_cannot_reclaim_an_owned_request() {
    let control = block_control(interrupt_pio_config());
    let controller_cookie = Arc::as_ptr(&control).expose_provenance();
    let foreign_cookie = controller_cookie.wrapping_add(1).max(1);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(44);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();
    let foreign = unsafe {
        // SAFETY: the mock performs no DMA. The deliberately foreign cookie
        // exercises queue-side proof identity validation.
        rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(1), foreign_cookie)
    };
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::reclaim_after_quiesce(&mut queue, &foreign, &mut sink),
        Err(BlkError::InvalidDmaProof)
    );
    assert_eq!(sink.calls, 0);

    let local = unsafe {
        // SAFETY: the mock performs no DMA and request access is serialized.
        rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(1), controller_cookie)
    };
    IQueue::reclaim_after_quiesce(&mut queue, &local, &mut sink).unwrap();
    assert_eq!(sink.take().id, runtime_id);
}

#[test]
fn runtime_queue_requires_dma_and_is_exclusive() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let mut queue = Interface::create_queue(&mut device).expect("DMA IRQ queue must exist");
    assert!(Interface::create_queue(&mut device).is_none());

    let mut sink = OneCompletionSink::default();
    queue.shutdown(&mut sink).unwrap();
    drop(queue);

    let mut replacement =
        Interface::create_queue(&mut device).expect("shutdown queue must release its claim");
    replacement.shutdown(&mut sink).unwrap();
}

#[test]
fn queue_contract_is_interrupt_source_zero_and_serialized() {
    let control = block_control(dma_config());
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let info = IQueue::info(&queue);

    assert_eq!(info.id, 0);
    assert_eq!(info.dispatch_mode, DispatchMode::Serialized);
    let QueueKind::Interrupt { sources } = info.kind else {
        panic!("SD/MMC queue must be interrupt backed");
    };
    assert_eq!(sources.bits(), 1);

    IQueue::shutdown(&mut queue, &mut OneCompletionSink::default()).unwrap();
}

#[test]
fn interrupt_queue_rejects_submission_until_completion_irq_is_enabled() {
    let control = block_control(dma_config());
    control
        .irq_enabled
        .store(false, core::sync::atomic::Ordering::Release);
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(31);

    let error = IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .expect_err("hardware request cannot be accepted before IRQ activation");

    assert_eq!(error.id(), runtime_id);
    assert_eq!(error.error(), BlkError::Offline);
    raw.with_mut(|card| assert_eq!(card.host().last_submitted_host_id, None));
    let event = Event::from_queue_bits(1);
    let event = event.for_queue(0).unwrap();
    assert_eq!(
        IQueue::service_events(&mut queue, &event, &mut OneCompletionSink::default(),),
        Err(BlkError::Offline)
    );
    IQueue::shutdown(&mut queue, &mut OneCompletionSink::default()).unwrap();
}

#[test]
fn interface_irq_contract_is_explicit_and_handler_is_one_shot() {
    let mut disabled = BlockDevice::from_card_for_test(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::fifo("mock-sd", 8),
    );
    assert_eq!(
        Interface::enable_irq(&disabled),
        Err(BlkError::NotSupported)
    );
    assert_eq!(
        Interface::disable_irq(&disabled),
        Err(BlkError::NotSupported)
    );
    assert!(Interface::irq_sources(&disabled).is_empty());
    assert!(Interface::take_irq_handler(&mut disabled, 0).is_none());

    let mut enabled =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    assert_eq!(Interface::enable_irq(&enabled), Ok(()));
    assert!(Interface::is_irq_enabled(&enabled));
    let sources = Interface::irq_sources(&enabled);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id, 0);
    assert!(sources[0].queues.contains(0));
    assert!(Interface::take_irq_handler(&mut enabled, 1).is_none());
    let mut handler = Interface::take_irq_handler(&mut enabled, 0).unwrap();
    assert!(Interface::take_irq_handler(&mut enabled, 0).is_none());
    assert!(handler.handle_irq().event().for_queue(0).is_some());
    assert_eq!(Interface::disable_irq(&enabled), Ok(()));
    assert!(!Interface::is_irq_enabled(&enabled));
}

#[test]
fn register_contention_is_reported_as_deferred_ack_not_as_completion() {
    let mut host = MockHost::default();
    host.defer_irq_ack = true;
    let mut device = BlockDevice::from_card_for_test(SdioSdmmc::new(host), dma_config());
    let mut handler = Interface::take_irq_handler(&mut device, 0).unwrap();

    let outcome = handler.handle_irq();

    assert!(outcome.is_handled());
    assert!(outcome.is_deferred());
    let event = outcome.event();
    assert!(event.for_queue(0).unwrap().requires_irq_ack());
}

#[test]
fn controller_lifecycle_quiesces_and_reinitializes_without_busy_waiting() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let epoch = rdif_block::ControllerEpoch::new(9);
    let cookie = Arc::as_ptr(&device.control).expose_provenance();
    let lifecycle = match Interface::lifecycle(&mut device) {
        rdif_block::LifecycleEndpoint::Interrupt(lifecycle) => lifecycle,
        rdif_block::LifecycleEndpoint::Inline => panic!("SD/MMC must expose a hardware lifecycle"),
    };

    assert_eq!(
        lifecycle.begin_dma_quiesce(epoch, rdif_block::RecoveryCause::QueueFault { queue_id: 0 },),
        Ok(())
    );
    let first_wake = match lifecycle.poll_dma_quiesce(rdif_block::InitInput::at(1_000)) {
        rdif_block::InitPoll::Pending(schedule) => schedule.wake_at_ns(),
        _ => panic!("the first bounded pass must only arm controller quiescence"),
    };
    assert!(first_wake.is_some_and(|deadline| deadline > 1_000));
    let proof = match lifecycle.poll_dma_quiesce(rdif_block::InitInput::at(
        first_wake.expect("quiesce must publish an absolute wake time"),
    )) {
        rdif_block::InitPoll::Ready(proof) => proof,
        _ => panic!("mock controller must finish quiescence at its absolute deadline"),
    };
    assert_eq!(proof.epoch(), epoch);
    assert_eq!(proof.controller_cookie(), cookie);

    lifecycle.begin_reinitialize(proof).unwrap();
    let ready = match lifecycle.poll_reinitialize(rdif_block::InitInput::at(2_000)) {
        rdif_block::InitPoll::Ready(ready) => ready,
        _ => panic!("mock controller reconstruction must finish in one bounded pass"),
    };
    assert_eq!(ready.epoch(), epoch);
    assert_eq!(ready.controller_cookie(), cookie);
}

#[test]
fn controller_lifecycle_starts_fresh_quiescence_after_guest_ownership() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let lifecycle = match Interface::lifecycle(&mut device) {
        rdif_block::LifecycleEndpoint::Interrupt(lifecycle) => lifecycle,
        rdif_block::LifecycleEndpoint::Inline => panic!("SD/MMC must expose a hardware lifecycle"),
    };
    let first_epoch = rdif_block::ControllerEpoch::new(10);
    lifecycle
        .begin_dma_quiesce(first_epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    let first_wake = match lifecycle.poll_dma_quiesce(rdif_block::InitInput::at(1_000)) {
        rdif_block::InitPoll::Pending(schedule) => schedule
            .wake_at_ns()
            .expect("quiescence must publish an absolute deadline"),
        _ => panic!("first bounded pass must arm controller quiescence"),
    };
    let proof = match lifecycle.poll_dma_quiesce(rdif_block::InitInput::at(first_wake)) {
        rdif_block::InitPoll::Ready(proof) => proof,
        _ => panic!("mock controller must finish handoff quiescence"),
    };
    lifecycle.enter_guest_owned(proof).unwrap();

    let return_epoch = rdif_block::ControllerEpoch::new(11);
    lifecycle
        .begin_dma_quiesce(return_epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    assert!(matches!(
        lifecycle.poll_dma_quiesce(rdif_block::InitInput::at(first_wake + 1)),
        rdif_block::InitPoll::Pending(_)
    ));
}

#[test]
fn irq_handler_does_not_enter_shared_card_core() {
    let mut device =
        BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    let mut handler = Interface::take_irq_handler(&mut device, 0).unwrap();
    let _guard = device
        .control
        .raw
        .try_borrow_mut()
        .expect("test setup must own the shared card core");

    let event = handler.handle_irq().event();

    assert!(event.for_queue(0).is_some());
}

#[test]
fn irq_control_errors_are_propagated_without_fallback() {
    let device = BlockDevice::from_card_for_test(SdioSdmmc::new(MockHost::default()), dma_config());
    device.control.raw.with_mut(|card| {
        card.host_mut().fail_irq_enable = true;
    });
    assert_eq!(Interface::enable_irq(&device), Err(BlkError::Io));
    assert!(!Interface::is_irq_enabled(&device));

    device.control.raw.with_mut(|card| {
        card.host_mut().fail_irq_enable = false;
    });
    Interface::enable_irq(&device).unwrap();
    device.control.raw.with_mut(|card| {
        card.host_mut().fail_irq_disable = true;
    });
    assert_eq!(Interface::disable_irq(&device), Err(BlkError::Io));
    assert!(Interface::is_irq_enabled(&device));
}

#[test]
fn submit_failure_returns_runtime_id_and_original_cpu_buffer() {
    let control = block_control(dma_config());
    control
        .raw
        .with_mut(|card| card.host_mut().fail_submit = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(77);
    let request = request(rdif_block::RequestOp::Write, DmaDirection::ToDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();

    let error = IQueue::submit_owned(&mut queue, runtime_id, request).unwrap_err();
    let (returned_id, error_kind, returned) = error.into_parts();

    assert_eq!(returned_id, runtime_id);
    assert_eq!(error_kind, BlkError::Retry);
    assert_eq!(returned.data.as_ref().unwrap().cpu_ptr(), original_ptr);
    IQueue::shutdown(&mut queue, &mut OneCompletionSink::default()).unwrap();
}

#[test]
fn submit_only_arms_hardware_and_keeps_runtime_identity() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(7);

    assert!(matches!(
        IQueue::submit_owned(
            &mut queue,
            runtime_id,
            request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
        ),
        Ok(SubmitOutcome::Queued)
    ));
    raw.with_mut(|card| {
        assert_eq!(card.host().service_calls, 0);
        assert_eq!(
            card.host().last_submitted_host_id,
            Some(BlockRequestId::new(100))
        );
    });

    let mut sink = OneCompletionSink::default();
    let event = request_event(runtime_id);
    IQueue::service_events(&mut queue, &event.for_queue(0).unwrap(), &mut sink).unwrap();
    assert_eq!(sink.take().id, runtime_id);
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn wrong_request_hint_does_not_advance_host_fsm() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(9);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();

    let wrong = request_event(RequestId::new(100));
    let mut sink = OneCompletionSink::default();
    assert_eq!(
        IQueue::service_events(&mut queue, &wrong.for_queue(0).unwrap(), &mut sink),
        Ok(ServiceProgress::Idle)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| assert_eq!(card.host().service_calls, 0));

    let right = request_event(runtime_id);
    IQueue::service_events(&mut queue, &right.for_queue(0).unwrap(), &mut sink).unwrap();
    assert_eq!(sink.take().id, runtime_id);
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn deferred_irq_without_an_active_request_is_acknowledged_then_fails_typed() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    raw.with_mut(|card| card.host_mut().deferred_ack_contended = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let deferred = rdif_block::IrqOutcome::deferred(Event::from_queue_bits(1)).event();
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(&mut queue, &deferred.for_queue(0).unwrap(), &mut sink),
        Ok(ServiceProgress::More)
    );
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 1);
        card.host_mut().deferred_ack_contended = false;
    });

    assert_eq!(
        IQueue::service_events(&mut queue, &deferred.for_queue(0).unwrap(), &mut sink),
        Err(BlkError::Io)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 2);
        assert_eq!(card.host().service_calls, 0);
    });
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn deferred_irq_with_a_mismatched_hint_is_acknowledged_then_enters_recovery() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(10);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();
    let deferred = rdif_block::IrqOutcome::deferred(request_event(RequestId::new(11))).event();
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(&mut queue, &deferred.for_queue(0).unwrap(), &mut sink),
        Err(BlkError::Io)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 1);
        assert_eq!(card.host().service_calls, 0);
    });

    let proof = dma_quiesced(&queue.control);
    IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink).unwrap();
    let completion = sink.take();
    assert_eq!(completion.id, runtime_id);
    assert_eq!(completion.result, Err(BlkError::Io));
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn late_deferred_irq_after_completion_is_acknowledged_then_fails_typed() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(12);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();
    let mut sink = OneCompletionSink::default();
    let completed = request_event(runtime_id);
    IQueue::service_events(&mut queue, &completed.for_queue(0).unwrap(), &mut sink).unwrap();
    assert_eq!(sink.take().id, runtime_id);

    let deferred = rdif_block::IrqOutcome::deferred(Event::from_queue_bits(1)).event();
    assert_eq!(
        IQueue::service_events(&mut queue, &deferred.for_queue(0).unwrap(), &mut sink),
        Err(BlkError::Io)
    );
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 1);
        assert_eq!(card.host().service_calls, 1);
    });
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn acknowledged_event_advances_once_and_returns_cpu_dma_ownership() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    raw.with_mut(|card| card.host_mut().pending_once = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(11);
    let request = request(rdif_block::RequestOp::Read, DmaDirection::FromDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();
    IQueue::submit_owned(&mut queue, runtime_id, request).unwrap();
    let event = Event::from_queue_bits(1);
    let batch = event.for_queue(0).unwrap();
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(&mut queue, &batch, &mut sink),
        Ok(ServiceProgress::Idle)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| assert_eq!(card.host().service_calls, 1));

    IQueue::service_events(&mut queue, &batch, &mut sink).unwrap();
    let completed = sink.take();
    assert_eq!(completed.id, runtime_id);
    assert_eq!(completed.result, Ok(()));
    assert_eq!(completed.request.data.unwrap().cpu_ptr(), original_ptr);
    raw.with_mut(|card| assert_eq!(card.host().service_calls, 2));
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn shared_core_contention_retains_event_for_the_next_service_pass() {
    let control = block_control(interrupt_pio_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(421);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();
    let event = request_event(runtime_id);
    let batch = event.for_queue(0).unwrap();
    let mut sink = OneCompletionSink::default();
    let held = raw
        .try_borrow_mut()
        .expect("test setup must own the shared core");

    assert_eq!(
        IQueue::service_events(&mut queue, &batch, &mut sink),
        Ok(ServiceProgress::More)
    );
    assert_eq!(sink.calls, 0);

    drop(held);
    assert_eq!(
        IQueue::service_events(&mut queue, &batch, &mut sink),
        Ok(ServiceProgress::Idle)
    );
    assert_eq!(sink.take().id, runtime_id);
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn deferred_event_is_acknowledged_before_request_state_is_inspected() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    raw.with_mut(|card| card.host_mut().deferred_ack_contended = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(12);
    IQueue::submit_owned(
        &mut queue,
        runtime_id,
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();
    let mut deferred = Event::none();
    deferred.push_queue(0);
    let deferred = rdif_block::IrqOutcome::deferred(deferred).event();
    let batch = deferred.for_queue(0).unwrap();
    let mut sink = OneCompletionSink::default();

    for _ in 0..64 {
        assert_eq!(
            IQueue::service_events(&mut queue, &batch, &mut sink),
            Ok(ServiceProgress::More)
        );
    }
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 64);
        assert_eq!(card.host().service_calls, 0);
        card.host_mut().deferred_ack_contended = false;
        card.host_mut().deferred_ack_unhandled = true;
    });

    assert_eq!(
        IQueue::service_events(&mut queue, &batch, &mut sink),
        Ok(ServiceProgress::Idle)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 65);
        assert_eq!(card.host().service_calls, 0);
        card.host_mut().deferred_ack_unhandled = false;
    });

    assert_eq!(
        IQueue::service_events(&mut queue, &batch, &mut sink),
        Ok(ServiceProgress::Idle)
    );
    assert_eq!(sink.take().id, runtime_id);
    raw.with_mut(|card| {
        assert_eq!(card.host().deferred_ack_calls, 66);
        assert_eq!(card.host().service_calls, 1);
    });
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn device_error_waits_for_controller_quiescence_before_returning_dma() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    raw.with_mut(|card| card.host_mut().fail_service = true);
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(13);
    let request = request(rdif_block::RequestOp::Write, DmaDirection::ToDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();
    IQueue::submit_owned(&mut queue, runtime_id, request).unwrap();
    let event = Event::from_queue_bits(1);
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(&mut queue, &event.for_queue(0).unwrap(), &mut sink),
        Err(BlkError::Io)
    );
    assert_eq!(sink.calls, 0);
    raw.with_mut(|card| {
        assert_eq!(card.host().service_calls, 1);
        assert_eq!(card.host().aborts, 0);
    });

    let proof = dma_quiesced(&queue.control);
    IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink).unwrap();

    let completed = sink.take();
    assert_eq!(completed.id, runtime_id);
    assert_eq!(completed.result, Err(BlkError::Io));
    assert_eq!(completed.request.data.unwrap().cpu_ptr(), original_ptr);
    raw.with_mut(|card| {
        assert_eq!(card.host().service_calls, 1);
        assert_eq!(card.host().aborts, 1);
    });
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
}

#[test]
fn dropping_an_active_queue_quarantines_without_synchronous_abort() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    IQueue::submit_owned(
        &mut queue,
        RequestId::new(19),
        request(rdif_block::RequestOp::Read, DmaDirection::FromDevice),
    )
    .unwrap();

    drop(queue);

    raw.with_mut(|card| {
        assert_eq!(
            card.host().aborts,
            0,
            "Drop cannot busy-wait or claim controller quiescence"
        );
    });
}

#[test]
fn failed_dma_abort_never_fabricates_a_terminal_completion() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    raw.with_mut(|card| {
        card.host_mut().fail_service = true;
        card.host_mut().fail_abort = true;
    });
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(14);
    let request = request(rdif_block::RequestOp::Read, DmaDirection::FromDevice);
    IQueue::submit_owned(&mut queue, runtime_id, request).unwrap();
    let event = Event::from_queue_bits(1);
    let mut sink = OneCompletionSink::default();

    assert_eq!(
        IQueue::service_events(&mut queue, &event.for_queue(0).unwrap(), &mut sink),
        Err(BlkError::Io)
    );
    assert_eq!(sink.calls, 0);

    let proof = dma_quiesced(&queue.control);
    assert_eq!(
        IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink),
        Err(BlkError::Quarantined)
    );
    assert_eq!(sink.calls, 0);

    raw.with_mut(|card| card.host_mut().fail_abort = false);
    IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink).unwrap();
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
    assert_eq!(sink.calls, 1);
    assert_eq!(sink.take().result, Err(BlkError::Io));
}

#[test]
fn proof_gated_reclaim_returns_each_accepted_request_exactly_once() {
    let control = block_control(dma_config());
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let runtime_id = RequestId::new(17);
    let request = request(rdif_block::RequestOp::Read, DmaDirection::FromDevice);
    let original_ptr = request.data.as_ref().unwrap().cpu_ptr();
    IQueue::submit_owned(&mut queue, runtime_id, request).unwrap();
    let mut sink = OneCompletionSink::default();

    let proof = dma_quiesced(&queue.control);
    IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut sink).unwrap();
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
    let completed = sink.completion.as_ref().unwrap();
    assert_eq!(completed.id, runtime_id);
    assert_eq!(completed.result, Err(BlkError::Cancelled));
    assert_eq!(
        completed.request.data.as_ref().unwrap().cpu_ptr(),
        original_ptr
    );
    assert_eq!(sink.calls, 1);
    IQueue::shutdown(&mut queue, &mut sink).unwrap();
    assert_eq!(sink.calls, 1);
    raw.with_mut(|card| assert_eq!(card.host().aborts, 1));
}

#[test]
fn empty_event_cannot_produce_a_queue_service_batch() {
    assert!(Event::none().for_queue(0).is_none());
}

#[test]
fn block_error_mapping_preserves_timeout() {
    assert_eq!(
        map_dev_err_to_blk_err(Error::Timeout(Default::default())),
        BlkError::TimedOut
    );
}

#[test]
fn host2_terminal_error_retains_dma_until_proof_gated_reclaim() {
    let mut host = SdioHost2Adapter::new(Host2Mock {
        fail_poll: true,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    let mut slot = ProtocolBlockSlot::default();
    let mut pending = None;
    let prepared = cpu_dma(DmaDirection::FromDevice).prepare_for_device();
    let original_ptr = prepared.cpu_ptr();
    let id = <SdioHost2Adapter<Host2Mock> as BlockHost>::submit_owned_read_request(
        &mut host,
        0,
        HostRequestBuffer::Dma(prepared),
        &mut slot,
        &mut pending,
    )
    .unwrap();

    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::service_request(
            &mut host,
            &mut pending,
            id,
            &mut slot,
        ),
        Err(Error::Timeout(_))
    ));
    assert!(pending.is_some());
    assert_eq!(slot.active_id, Some(id));
    assert!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).is_none(),
        "event-service failure must not fabricate DMA quiescence"
    );

    <SdioHost2Adapter<Host2Mock> as BlockHost>::abort_request(&mut host, &mut pending, &mut slot)
        .unwrap();
    let completed =
        <SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).unwrap();
    assert_eq!(completed.into_cpu_buffer().cpu_ptr(), original_ptr);
}

#[test]
fn host2_terminal_error_retains_owned_pio_until_proof_gated_reclaim() {
    let mut host = SdioHost2Adapter::new(Host2Mock {
        fail_poll: true,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    let mut slot = ProtocolBlockSlot::default();
    let mut pending = None;
    let buffer = cpu_dma(DmaDirection::FromDevice);
    let original_ptr = buffer.cpu_ptr();
    let id = <SdioHost2Adapter<Host2Mock> as BlockHost>::submit_owned_read_request(
        &mut host,
        0,
        HostRequestBuffer::InterruptPio(buffer),
        &mut slot,
        &mut pending,
    )
    .unwrap();

    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::service_request(
            &mut host,
            &mut pending,
            id,
            &mut slot,
        ),
        Err(Error::Timeout(_))
    ));
    assert!(pending.is_some());
    assert!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).is_none(),
        "terminal status alone must not release an owned PIO buffer"
    );

    <SdioHost2Adapter<Host2Mock> as BlockHost>::abort_request(&mut host, &mut pending, &mut slot)
        .unwrap();
    let completed =
        <SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).unwrap();
    assert_eq!(completed.into_cpu_buffer().cpu_ptr(), original_ptr);
}

#[test]
fn host2_failed_abort_retains_owned_pio_request_for_later_recovery() {
    let mut host = SdioHost2Adapter::new(Host2Mock {
        fail_poll: false,
        fail_abort: true,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    let mut slot = ProtocolBlockSlot::default();
    let mut pending = None;
    let buffer = cpu_dma(DmaDirection::FromDevice);
    let original_ptr = buffer.cpu_ptr();
    let id = <SdioHost2Adapter<Host2Mock> as BlockHost>::submit_owned_read_request(
        &mut host,
        0,
        HostRequestBuffer::InterruptPio(buffer),
        &mut slot,
        &mut pending,
    )
    .unwrap();

    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::abort_request(
            &mut host,
            &mut pending,
            &mut slot,
        ),
        Err(Error::Busy)
    ));
    assert!(pending.is_some(), "a failed abort must retain the request");
    assert_eq!(slot.active_id, Some(id));
    assert!(<SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).is_none());

    host.with_host_mut(|physical| physical.fail_abort = false);
    <SdioHost2Adapter<Host2Mock> as BlockHost>::abort_request(&mut host, &mut pending, &mut slot)
        .unwrap();
    let completed =
        <SdioHost2Adapter<Host2Mock> as BlockHost>::take_completed_buffer(&mut slot).unwrap();
    assert_eq!(completed.into_cpu_buffer().cpu_ptr(), original_ptr);
    assert!(pending.is_none());
}

#[test]
fn host2_block_lifecycle_is_fail_closed_until_hardware_opts_in() {
    let mut host = SdioHost2Adapter::new(Host2Mock {
        fail_poll: false,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::begin_recovery(
            &mut host,
            rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
        ),
        Err(Error::UnsupportedCommand)
    ));

    host.enable_block_lifecycle();
    let mut recovery = <SdioHost2Adapter<Host2Mock> as BlockHost>::begin_recovery(
        &mut host,
        rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
    )
    .expect("an explicitly installed hardware lifecycle must start");
    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(10),
        ),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(11),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    <SdioHost2Adapter<Host2Mock> as BlockHost>::begin_reinitialize(&mut host, &mut recovery)
        .unwrap();
    assert!(matches!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(12),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    assert!(
        <SdioHost2Adapter<Host2Mock> as BlockHost>::begin_recovery(
            &mut host,
            rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
        )
        .is_ok(),
        "the activation-time recovery slot must be reusable without reallocating"
    );
}

#[test]
fn staged_initialization_rejects_progress_before_irq_is_bound_and_enabled() {
    let card = SdioSdmmc::new_host2(Host2Mock {
        fail_poll: false,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    let init = crate::sdio::OwnedSdioInit::new(card, crate::sdio::CardInitPreference::SdFirst);
    let mut staged = StagedBlockDevice::new(init, dma_config(), ready_host2_device);

    assert!(matches!(
        rdif_block::InitialController::poll_init(&mut staged, rdif_block::InitInput::at(0)),
        rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
    ));

    let handler = rdif_block::InitialController::take_irq_handler(&mut staged, 0)
        .expect("initialization must publish one owned IRQ endpoint");
    assert!(matches!(
        rdif_block::InitialController::poll_init(&mut staged, rdif_block::InitInput::at(1)),
        rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
    ));

    Interface::enable_irq(&staged).expect("bound initialization IRQ must be enabled explicitly");
    assert!(matches!(
        rdif_block::InitialController::poll_init(&mut staged, rdif_block::InitInput::at(2)),
        rdif_block::InitPoll::Pending(_)
    ));
    drop(handler);
}

#[test]
fn staged_initialization_preserves_deferred_irq_acknowledgement_state() {
    let card = SdioSdmmc::new_host2(Host2Mock {
        fail_poll: false,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: true,
    });
    let init = crate::sdio::OwnedSdioInit::new(card, crate::sdio::CardInitPreference::SdFirst);
    let mut staged = StagedBlockDevice::new(init, dma_config(), ready_host2_device);

    assert_eq!(
        rdif_block::InitialController::service_deferred_irq(&mut staged, 1),
        rdif_block::InitIrqProgress::Unhandled
    );
    assert_eq!(
        rdif_block::InitialController::service_deferred_irq(&mut staged, 0),
        rdif_block::InitIrqProgress::Deferred
    );
}

#[test]
fn staged_initialization_reports_successful_deferred_irq_acknowledgement() {
    let card = SdioSdmmc::new_host2(Host2Mock {
        fail_poll: false,
        fail_abort: false,
        irq_enabled: false,
        defer_irq_ack: false,
    });
    let init = crate::sdio::OwnedSdioInit::new(card, crate::sdio::CardInitPreference::SdFirst);
    let mut staged = StagedBlockDevice::new(init, dma_config(), ready_host2_device);

    assert_eq!(
        rdif_block::InitialController::service_deferred_irq(&mut staged, 0),
        rdif_block::InitIrqProgress::Acknowledged
    );
}

fn ready_host2_device(
    card: crate::sdio::InitializedSdioCard<SdioHost2Adapter<Host2Mock>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<Host2Mock>> {
    BlockDevice::from_initialized(card, config)
}

#[derive(Clone, Copy, Default)]
struct MockEvent {
    kind: HostEventKind,
    ack_deferred: bool,
    request_service: bool,
}

impl HostEvent for MockEvent {
    fn kind(&self) -> HostEventKind {
        self.kind
    }

    fn ack_deferred(&self) -> bool {
        self.ack_deferred
    }

    fn requests_block_queue_service(&self) -> bool {
        self.request_service
    }
}

#[derive(Default)]
struct MockIrqEndpoint {
    defer_ack: bool,
    control_only: bool,
}

impl SdioIrqHandle for MockIrqEndpoint {
    type Event = MockEvent;

    fn handle_irq(&mut self) -> Self::Event {
        MockEvent {
            kind: if self.control_only {
                HostEventKind::Other
            } else {
                HostEventKind::TransferComplete
            },
            ack_deferred: self.defer_ack,
            request_service: !self.control_only,
        }
    }
}

#[test]
fn acknowledged_control_irq_does_not_activate_the_block_queue() {
    let mut handler = crate::rdif::irq::BlockIrqHandler::<SdioHost2Adapter<Host2Mock>> {
        irq: MockIrqEndpoint {
            defer_ack: false,
            control_only: true,
        },
    };

    let outcome = rdif_block::IrqHandler::handle_irq(&mut handler);
    assert!(outcome.is_handled());
    assert!(!outcome.is_deferred());
    assert!(outcome.event().is_empty());
}

struct Host2Mock {
    fail_poll: bool,
    fail_abort: bool,
    irq_enabled: bool,
    defer_irq_ack: bool,
}

struct Host2Request {
    in_flight: Option<dma_api::InFlightDma>,
    owned_cpu: Option<CpuDmaBuffer>,
    completed_dma: Option<CompletedDma>,
    completed_cpu: Option<CpuDmaBuffer>,
}

impl PhysicalSdioHost for Host2Mock {
    type TransactionRequest<'a>
        = Host2Request
    where
        Self: 'a;
    type BusRequest = ();

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        let (in_flight, owned_cpu) = match transaction.data.map(|data| data.buffer) {
            Some(DataBuffer::Dma(buffer)) => (Some(unsafe { buffer.into_in_flight() }), None),
            Some(DataBuffer::OwnedCpu(buffer)) => (None, Some(buffer)),
            Some(DataBuffer::Read(_) | DataBuffer::Write(_)) | None => (None, None),
        };
        Ok(Host2Request {
            in_flight,
            owned_cpu,
            completed_dma: None,
            completed_cpu: None,
        })
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        if core::mem::take(&mut self.fail_poll) {
            return Ok(RequestPoll::Ready(Err(sdio_host2::Error::Timeout)));
        }
        complete_host2_dma(request);
        Ok(RequestPoll::Ready(Ok(sdio_host2::RawResponse::empty())))
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        if self.fail_abort {
            return Err(sdio_host2::Error::Busy);
        }
        complete_host2_dma(request);
        Ok(())
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CompletedDma>
    where
        Self: 'a,
    {
        request.completed_dma.take()
    }

    fn take_completed_cpu<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CpuDmaBuffer>
    where
        Self: 'a,
    {
        request.completed_cpu.take()
    }

    unsafe fn submit_bus_op(
        &mut self,
        _op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        Ok(())
    }

    fn poll_bus_op(
        &mut self,
        _request: &mut Self::BusRequest,
    ) -> Result<RequestPoll<()>, sdio_host2::PollRequestError> {
        Ok(RequestPoll::Ready(Ok(())))
    }

    fn abort_bus_op(&mut self, _request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        Ok(())
    }
}

impl SdioHost2Irq for Host2Mock {
    type Event = MockEvent;
    type IrqHandle = MockIrqEndpoint;

    fn completion_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.irq_enabled = true;
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.irq_enabled = false;
        Ok(())
    }

    fn irq_handle(&mut self) -> Self::IrqHandle {
        MockIrqEndpoint {
            defer_ack: self.defer_irq_ack,
            control_only: false,
        }
    }
}

impl crate::sdio::SdioHost2Lifecycle for Host2Mock {
    type RecoveryState = MockRecovery;

    fn begin_recovery(
        &mut self,
        _cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, Error> {
        Ok(MockRecovery::QuiesceStart)
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state {
            MockRecovery::QuiesceStart => {
                *state = MockRecovery::QuiesceWait {
                    ready_at_ns: input.now_ns,
                };
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
            }
            MockRecovery::QuiesceWait { .. } => rdif_block::InitPoll::Ready(()),
            MockRecovery::Reinitialize => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        *state = MockRecovery::Reinitialize;
        Ok(())
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        _input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state {
            MockRecovery::Reinitialize => rdif_block::InitPoll::Ready(()),
            MockRecovery::QuiesceStart | MockRecovery::QuiesceWait { .. } => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }
}

fn complete_host2_dma(request: &mut Host2Request) {
    if let Some(in_flight) = request.in_flight.take() {
        request.completed_dma = Some(unsafe { in_flight.complete_after_quiesce() });
    }
    request.completed_cpu = request.owned_cpu.take();
}

struct MockHost {
    irq_enabled: bool,
    fail_irq_enable: bool,
    fail_irq_disable: bool,
    fail_submit: bool,
    fail_service: bool,
    fail_abort: bool,
    defer_irq_ack: bool,
    fail_deferred_ack: bool,
    deferred_ack_contended: bool,
    deferred_ack_unhandled: bool,
    deferred_ack_calls: usize,
    pending_once: bool,
    next_host_id: usize,
    last_submitted_host_id: Option<BlockRequestId>,
    service_calls: usize,
    aborts: usize,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            irq_enabled: false,
            fail_irq_enable: false,
            fail_irq_disable: false,
            fail_submit: false,
            fail_service: false,
            fail_abort: false,
            defer_irq_ack: false,
            fail_deferred_ack: false,
            deferred_ack_contended: false,
            deferred_ack_unhandled: false,
            deferred_ack_calls: 0,
            pending_once: false,
            next_host_id: 100,
            last_submitted_host_id: None,
            service_calls: 0,
            aborts: 0,
        }
    }
}

#[derive(Default)]
struct MockSlot {
    in_flight: Option<MockHostBacking>,
    completed: Option<CompletedHostBuffer>,
}

enum MockHostBacking {
    Dma(dma_api::InFlightDma),
    InterruptPio(CpuDmaBuffer),
}

struct MockRequest {
    id: BlockRequestId,
}

#[derive(Default)]
enum MockRecovery {
    #[default]
    QuiesceStart,
    QuiesceWait {
        ready_at_ns: u64,
    },
    Reinitialize,
}

impl SdioHost for MockHost {
    type Event = MockEvent;
    type DataRequest<'a> = ();
    type BusRequest = crate::sdio::ReadyBusRequest;

    fn submit_command(&mut self, _cmd: &Command) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
        Ok(CommandResponsePoll::Pending)
    }

    fn submit_read_data<'a>(
        &mut self,
        _cmd: &Command,
        _buf: &'a mut [u8],
        _block_size: u32,
        _block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        Err(Error::UnsupportedCommand)
    }

    fn submit_write_data<'a>(
        &mut self,
        _cmd: &Command,
        _buf: &'a [u8],
        _block_size: u32,
        _block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        Err(Error::UnsupportedCommand)
    }

    fn poll_data_request<'a>(
        &mut self,
        _request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        Err(Error::UnsupportedCommand)
    }

    fn set_bus_width(&mut self, _width: crate::sdio::BusWidth) -> Result<(), Error> {
        Ok(())
    }

    fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
        Ok(())
    }

    fn submit_bus_op(&mut self, op: crate::sdio::SdioBusOp) -> Result<Self::BusRequest, Error> {
        crate::sdio::submit_ready_bus_op(self, op)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        crate::sdio::poll_ready_bus_op(request)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        if self.fail_irq_enable {
            return Err(Error::BusError(Default::default()));
        }
        self.irq_enabled = true;
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        if self.fail_irq_disable {
            return Err(Error::BusError(Default::default()));
        }
        self.irq_enabled = false;
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        self.irq_enabled
    }
}

impl SdioIrqHost for MockHost {
    type IrqHandle = MockIrqEndpoint;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        MockIrqEndpoint {
            defer_ack: self.defer_irq_ack,
            control_only: false,
        }
    }
}

impl BlockHost for MockHost {
    type Request = MockRequest;
    type Slot = MockSlot;
    type RecoveryState = MockRecovery;

    fn prepare_block_runtime(&mut self) {}

    fn acknowledge_deferred_irq(&mut self) -> Result<crate::sdio::DeferredIrqAck, Error> {
        self.deferred_ack_calls += 1;
        if self.fail_deferred_ack {
            return Err(Error::BusError(Default::default()));
        }
        Ok(if self.deferred_ack_contended {
            crate::sdio::DeferredIrqAck::Contended
        } else if self.deferred_ack_unhandled {
            crate::sdio::DeferredIrqAck::Unhandled
        } else {
            crate::sdio::DeferredIrqAck::Acknowledged
        })
    }

    fn begin_recovery(
        &mut self,
        _cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, Error> {
        Ok(MockRecovery::QuiesceStart)
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match *state {
            MockRecovery::QuiesceStart => {
                let ready_at_ns = input.now_ns.saturating_add(100);
                *state = MockRecovery::QuiesceWait { ready_at_ns };
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(ready_at_ns))
            }
            MockRecovery::QuiesceWait { ready_at_ns } if input.now_ns >= ready_at_ns => {
                rdif_block::InitPoll::Ready(())
            }
            MockRecovery::QuiesceWait { ready_at_ns } => {
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(ready_at_ns))
            }
            MockRecovery::Reinitialize => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        *state = MockRecovery::Reinitialize;
        Ok(())
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        _input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        match state {
            MockRecovery::Reinitialize => rdif_block::InitPoll::Ready(()),
            MockRecovery::QuiesceStart | MockRecovery::QuiesceWait { .. } => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
        }
    }

    fn service_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        self.service_calls += 1;
        let Some(active) = pending.as_ref() else {
            return Err(Error::InvalidArgument);
        };
        if active.id != request {
            return Ok(BlockPoll::Pending);
        }
        if self.fail_service {
            return Err(Error::BusError(Default::default()));
        }
        if core::mem::take(&mut self.pending_once) {
            return Ok(BlockPoll::Pending);
        }
        *pending = None;
        complete_mock_dma(slot);
        Ok(BlockPoll::Complete)
    }

    fn abort_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        slot: &mut Self::Slot,
    ) -> Result<(), Error> {
        if self.fail_abort {
            self.aborts += 1;
            return Err(Error::BusError(Default::default()));
        }
        if pending.take().is_some() {
            self.aborts += 1;
        }
        complete_mock_dma(slot);
        Ok(())
    }

    fn submit_owned_read_request(
        &mut self,
        _start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        self.submit_mock(buffer, slot, pending)
    }

    fn submit_owned_write_request(
        &mut self,
        _start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        self.submit_mock(buffer, slot, pending)
    }

    fn take_completed_buffer(slot: &mut Self::Slot) -> Option<CompletedHostBuffer> {
        slot.completed.take()
    }
}

impl MockHost {
    fn submit_mock(
        &mut self,
        buffer: HostRequestBuffer,
        slot: &mut MockSlot,
        pending: &mut Option<MockRequest>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        if self.fail_submit {
            self.fail_submit = false;
            return Err(OwnedBlockSubmitError::new(BlkError::Retry, buffer));
        }
        if pending.is_some() || slot.in_flight.is_some() {
            return Err(OwnedBlockSubmitError::new(BlkError::Retry, buffer));
        }
        let id = BlockRequestId::new(self.next_host_id);
        self.next_host_id = self.next_host_id.wrapping_add(1);
        self.last_submitted_host_id = Some(id);
        slot.in_flight = Some(match buffer {
            HostRequestBuffer::Dma(buffer) => {
                MockHostBacking::Dma(unsafe { buffer.into_in_flight() })
            }
            HostRequestBuffer::InterruptPio(buffer) => MockHostBacking::InterruptPio(buffer),
        });
        *pending = Some(MockRequest { id });
        Ok(id)
    }
}

fn complete_mock_dma(slot: &mut MockSlot) {
    if let Some(in_flight) = slot.in_flight.take() {
        slot.completed = Some(match in_flight {
            MockHostBacking::Dma(in_flight) => {
                CompletedHostBuffer::Dma(unsafe { in_flight.complete_after_quiesce() })
            }
            MockHostBacking::InterruptPio(buffer) => CompletedHostBuffer::InterruptPio(buffer),
        });
    }
}

struct TestDma;
static TEST_DMA: TestDma = TestDma;

impl dma_api::DmaOp for TestDma {
    fn page_size(&self) -> usize {
        BLOCK_SIZE
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = unsafe { alloc_zeroed(layout) };
        NonNull::new(ptr).map(|ptr| unsafe {
            dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        Err(dma_api::DmaError::NoMemory)
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}
