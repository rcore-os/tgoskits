extern crate std;

use alloc::{boxed::Box, sync::Arc};
use core::{
    alloc::Layout,
    cell::Cell,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::alloc::{alloc_zeroed, dealloc};

use dma_api::{
    CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle,
    DmaOp,
};
use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, ControllerInitEndpoint, DispatchMode, Interface,
    OwnedRequest, QueueKind, RequestFlags, RequestOp,
};
use virtio_drivers::{
    PhysAddr,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};

use super::{
    VIRTIO_BLK_IRQ_SOURCE_ID, VIRTIO_BLK_QUEUE_ID,
    controller::BlockDevice,
    device::{VirtIoBlkDevice, mask_and_publish_irq_disabled},
    initialization::{VIRTIO_BLK_F_RO, VirtioBlockInitPhase},
    irq::{
        VirtioBlkAccessGuard, initialization_irq_outcome, service_deferred_initialization_irq,
        take_deferred_virtio_queue_irq, virtio_blk_event_from_irq_status, virtio_blk_irq_outcome,
    },
    lifecycle::{VirtioBlockLifecycle, VirtioLifecycleHardware},
    queue::{
        BlockQueue, DmaDropFacts, InflightOp, InflightRequest, InflightStorage,
        ReclaimProofTracker, VIRTIO_BLK_DMA_BUFFER_SIZE, prepare_virtio_dma,
        take_inflight_after_used_descriptor, virtio_queue_ids, virtio_queue_info,
    },
};

mod lifecycle;

#[test]
fn reclaim_proof_is_bound_to_owner_and_advances_monotonically() {
    let mut tracker = ReclaimProofTracker::for_test(0x51a7);
    let wrong_owner = unsafe {
        // SAFETY: this value-only test never returns real DMA ownership.
        rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(2), 0xdead)
    };
    assert_eq!(
        tracker.validate(&wrong_owner),
        Err(BlkError::InvalidDmaProof)
    );

    let current = unsafe {
        // SAFETY: this value-only test never returns real DMA ownership.
        rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(2), 0x51a7)
    };
    assert_eq!(tracker.validate(&current), Ok(()));
    tracker.commit(&current);
    assert_eq!(tracker.validate(&current), Err(BlkError::InvalidDmaProof));

    let stale = unsafe {
        // SAFETY: this value-only test never returns real DMA ownership.
        rdif_block::DmaQuiesced::new(rdif_block::ControllerEpoch::new(1), 0x51a7)
    };
    assert_eq!(tracker.validate(&stale), Err(BlkError::InvalidDmaProof));
}

struct RecordingTransport {
    commands: Arc<AtomicUsize>,
    status: Cell<DeviceStatus>,
    sticky_reset: bool,
}

impl RecordingTransport {
    fn new(commands: Arc<AtomicUsize>) -> Self {
        Self {
            commands,
            status: Cell::new(DeviceStatus::empty()),
            sticky_reset: false,
        }
    }

    fn with_sticky_reset(mut self) -> Self {
        self.sticky_reset = true;
        self
    }

    fn record(&self) {
        self.commands.fetch_add(1, Ordering::Relaxed);
    }
}

impl Transport for RecordingTransport {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }

    fn read_device_features(&mut self) -> u64 {
        self.record();
        1 << 32
    }

    fn write_driver_features(&mut self, _driver_features: u64) {
        self.record();
    }

    fn max_queue_size(&mut self, _queue: u16) -> u32 {
        16
    }

    fn notify(&mut self, _queue: u16) {
        self.record();
    }

    fn get_status(&self) -> DeviceStatus {
        self.status.get()
    }

    fn set_status(&mut self, status: DeviceStatus) {
        self.record();
        if self.sticky_reset && status.is_empty() && !self.status.get().is_empty() {
            return;
        }
        self.status.set(status);
    }

    fn set_guest_page_size(&mut self, _guest_page_size: u32) {
        self.record();
    }

    fn requires_legacy_layout(&self) -> bool {
        false
    }

    fn queue_set(
        &mut self,
        _queue: u16,
        _size: u32,
        _descriptors: PhysAddr,
        _driver_area: PhysAddr,
        _device_area: PhysAddr,
    ) {
        self.record();
    }

    fn queue_unset(&mut self, _queue: u16) {
        self.record();
    }

    fn queue_used(&mut self, _queue: u16) -> bool {
        false
    }

    fn ack_interrupt(&mut self) -> InterruptStatus {
        self.record();
        InterruptStatus::empty()
    }

    fn read_config_generation(&self) -> u32 {
        0
    }

    fn read_config_space<T>(&self, _offset: usize) -> virtio_drivers::Result<T> {
        Err(virtio_drivers::Error::ConfigSpaceMissing)
    }

    fn write_config_space<T>(&mut self, _offset: usize, _value: T) -> virtio_drivers::Result<()> {
        Err(virtio_drivers::Error::Unsupported)
    }
}

struct TestDma;

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe { DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

static TEST_DMA: TestDma = TestDma;

fn dma_buffer(direction: DmaDirection) -> CpuDmaBuffer {
    dma_buffer_with_alignment(direction, 512)
}

fn dma_buffer_with_alignment(direction: DmaDirection, alignment: usize) -> CpuDmaBuffer {
    CpuDmaBuffer::new_zero(
        &DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
        NonZeroUsize::new(512).expect("test DMA length must be non-zero"),
        alignment,
        direction,
    )
    .expect("test DMA allocation must succeed")
}

fn owned_request(op: RequestOp, data: CpuDmaBuffer) -> OwnedRequest {
    OwnedRequest {
        op,
        lba: 0,
        block_count: 1,
        data: Some(data),
        flags: RequestFlags::NONE,
    }
}

#[test]
fn discovery_does_not_touch_transport_before_first_init_poll() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::clone(&commands)));

    assert_eq!(commands.load(Ordering::Relaxed), 0);
    assert!(!device.is_ready());
    assert_eq!(device.capacity_if_ready(), None);

    device.enable_irq();

    let progress = device.poll_init(rdif_block::InitInput::at(17));
    let rdif_block::InitPoll::Pending(schedule) = progress else {
        panic!("the first bounded pass must only issue reset");
    };
    assert!(!schedule.run_again());
    assert!(schedule.wake_at_ns().is_some());
    assert!(commands.load(Ordering::Relaxed) > 0);
    assert_eq!(
        device.with_task(|inner| inner.init_phase),
        VirtioBlockInitPhase::ResetWait
    );

    let progress = device.poll_init(rdif_block::InitInput::at(50_017));
    assert!(matches!(progress, rdif_block::InitPoll::Pending(_)));
    assert_eq!(
        device.with_task(|inner| inner.init_phase),
        VirtioBlockInitPhase::FeatureNegotiation
    );
}

#[test]
fn controller_cannot_enable_initialization_before_taking_its_irq_endpoint() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = Arc::new(VirtIoBlkDevice::discovered(RecordingTransport::new(
        Arc::clone(&commands),
    )));
    let mut controller = BlockDevice::discovered(device);

    assert_eq!(
        controller.enable_irq(),
        Err(BlkError::Other("virtio block IRQ endpoint is not bound"))
    );
    assert_eq!(commands.load(Ordering::Relaxed), 0);

    let ControllerInitEndpoint::Pending(initializer) = controller.controller_init() else {
        panic!("a discovered VirtIO controller must expose staged initialization")
    };
    assert!(matches!(
        initializer.poll_init(rdif_block::InitInput::at(0)),
        rdif_block::InitPoll::Failed(rdif_block::InitError::MissingInterrupt)
    ));
    assert_eq!(
        commands.load(Ordering::Relaxed),
        0,
        "initialization must not issue reset before the IRQ endpoint is active"
    );

    let ControllerInitEndpoint::Pending(initializer) = controller.controller_init() else {
        panic!("a discovered VirtIO controller must expose staged initialization")
    };
    assert!(
        initializer
            .take_irq_handler(VIRTIO_BLK_IRQ_SOURCE_ID)
            .is_some()
    );
    assert_eq!(controller.enable_irq(), Ok(()));

    let ControllerInitEndpoint::Pending(initializer) = controller.controller_init() else {
        panic!("a bound VirtIO controller must remain staged until reset completes")
    };
    assert!(matches!(
        initializer.poll_init(rdif_block::InitInput::at(17)),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(commands.load(Ordering::Relaxed) > 0);
}

#[test]
fn ready_controller_requires_the_normal_io_irq_endpoint() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = Arc::new(VirtIoBlkDevice::discovered(RecordingTransport::new(
        commands,
    )));
    let mut controller = BlockDevice::discovered(Arc::clone(&device));
    let ControllerInitEndpoint::Pending(initializer) = controller.controller_init() else {
        panic!("a discovered VirtIO controller must expose staged initialization")
    };
    assert!(
        initializer
            .take_irq_handler(VIRTIO_BLK_IRQ_SOURCE_ID)
            .is_some()
    );
    device.with_task(|inner| inner.init_phase = VirtioBlockInitPhase::Ready);

    assert_eq!(
        controller.enable_irq(),
        Err(BlkError::Other("virtio block IRQ endpoint is not bound"))
    );
    assert!(
        controller
            .take_irq_handler(VIRTIO_BLK_IRQ_SOURCE_ID)
            .is_some()
    );
    assert_eq!(controller.enable_irq(), Ok(()));
}

#[test]
fn registered_recovery_initializer_can_run_while_device_irq_is_masked() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::clone(&commands)));
    device.enable_irq();
    device.disable_irq();

    let progress = device.poll_init(rdif_block::InitInput::at(17));

    assert!(matches!(progress, rdif_block::InitPoll::Pending(_)));
    assert!(
        commands.load(Ordering::Relaxed) > 0,
        "recovery must rebuild a registered controller before IRQ delivery is re-enabled"
    );
}

#[test]
fn initialization_failure_resets_transport_before_returning_the_original_error() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(commands));
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::DriverReady;
        inner.capacity = 2;
        inner.retained_capacity = Some(1);
        inner.retained_read_only = Some(false);
        inner.transport.set_status(DeviceStatus::DRIVER);
    });

    let progress = device.poll_init(rdif_block::InitInput::at(100));

    assert!(matches!(
        progress,
        rdif_block::InitPoll::Failed(rdif_block::InitError::Hardware(
            "virtio block geometry changed across controller reset"
        ))
    ));
    assert_eq!(
        device.with_task(|inner| inner.init_phase),
        VirtioBlockInitPhase::Failed
    );
    assert!(device.with_task(|inner| inner.transport.get_status().is_empty()));
}

#[test]
fn terminal_initialization_failure_does_not_restart_device_reset() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::clone(&commands)));
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::DriverReady;
        inner.capacity = 2;
        inner.retained_capacity = Some(1);
        inner.retained_read_only = Some(false);
        inner.transport.set_status(DeviceStatus::DRIVER);
    });
    assert!(matches!(
        device.poll_init(rdif_block::InitInput::at(100)),
        rdif_block::InitPoll::Failed(_)
    ));
    let terminal_commands = commands.load(Ordering::Relaxed);

    assert!(matches!(
        device.poll_init(rdif_block::InitInput::at(200)),
        rdif_block::InitPoll::Failed(_)
    ));
    assert_eq!(commands.load(Ordering::Relaxed), terminal_commands);
}

#[test]
fn initialization_reset_timeout_enters_dma_quarantine_before_failure() {
    let commands = Arc::new(AtomicUsize::new(0));
    let transport = RecordingTransport::new(commands).with_sticky_reset();
    let device = VirtIoBlkDevice::discovered(transport);
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::DriverReady;
        inner.capacity = 2;
        inner.retained_capacity = Some(1);
        inner.retained_read_only = Some(false);
        inner.transport.set_status(DeviceStatus::DRIVER);
    });

    let rdif_block::InitPoll::Pending(schedule) = device.poll_init(rdif_block::InitInput::at(100))
    else {
        panic!("an unacknowledged failure reset must remain pending")
    };
    assert_eq!(schedule.wake_at_ns(), Some(50_100));
    assert_eq!(
        device.with_task(|inner| inner.init_phase),
        VirtioBlockInitPhase::FailureReset
    );

    assert!(matches!(
        device.poll_init(rdif_block::InitInput::at(1_000_000_100)),
        rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut)
    ));
    assert_eq!(
        device.with_task(|inner| inner.init_phase),
        VirtioBlockInitPhase::Failed
    );
}

#[test]
fn device_irq_source_is_masked_before_disabled_state_is_published() {
    let enabled = AtomicBool::new(true);

    mask_and_publish_irq_disabled(&enabled, || {
        assert!(
            enabled.load(Ordering::Acquire),
            "IRQ acknowledgement must remain active until the device source is masked"
        );
    });

    assert!(!enabled.load(Ordering::Acquire));
}

#[test]
fn contended_initialization_irq_uses_the_runtime_deferred_contract() {
    let active = AtomicBool::new(false);
    let pending = AtomicBool::new(false);
    let owner = VirtioBlkAccessGuard::enter_task(&active);

    let outcome = initialization_irq_outcome(&active, &pending, true, || {
        panic!("contended init IRQ must not access transport state")
    });

    assert!(outcome.is_handled());
    assert!(outcome.is_deferred());
    assert!(pending.load(Ordering::Acquire));

    drop(owner);
    assert_eq!(
        service_deferred_initialization_irq(&active, &pending, true, || {
            InterruptStatus::QUEUE_INTERRUPT
        }),
        rdif_block::InitIrqProgress::Acknowledged
    );
    assert!(!pending.load(Ordering::Acquire));
}

#[test]
fn request_wire_header_uses_little_endian_sector_and_operation() {
    let mut storage = InflightStorage::default();
    storage.prepare(InflightOp::Write, 0x0102_0304_0506_0708);

    assert_eq!(&storage.req[..4], &1_u32.to_le_bytes());
    assert_eq!(&storage.req[4..8], &[0; 4]);
    assert_eq!(&storage.req[8..], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(storage.resp, [3]);
}

#[test]
fn queue_interrupt_is_required_for_irq_event() {
    assert!(
        virtio_blk_event_from_irq_status(true, InterruptStatus::empty()).is_empty(),
        "shared IRQ callbacks without a virtio queue interrupt must not wake block queues"
    );
    assert!(
        virtio_blk_event_from_irq_status(true, InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT)
            .is_empty(),
        "config-only interrupts must not be reported as block completions"
    );
    assert!(
        virtio_blk_event_from_irq_status(false, InterruptStatus::QUEUE_INTERRUPT).is_empty(),
        "disabled completion IRQs must not report queue readiness"
    );

    let event = virtio_blk_event_from_irq_status(true, InterruptStatus::QUEUE_INTERRUPT);
    assert!(event.queues.contains(0));
    assert!(!event.is_empty());
}

#[test]
fn empty_shared_irq_is_unhandled_without_queue_activation() {
    let access_active = AtomicBool::new(false);
    let irq_ack_pending = AtomicBool::new(false);

    let outcome = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        InterruptStatus::empty()
    });

    assert!(!outcome.is_handled());
    assert!(!outcome.is_deferred());
    assert!(outcome.event().is_empty());
    assert!(!irq_ack_pending.load(Ordering::Acquire));
}

#[test]
fn config_only_shared_irq_is_acknowledged_without_queue_activation() {
    let access_active = AtomicBool::new(false);
    let irq_ack_pending = AtomicBool::new(false);

    let outcome = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT
    });

    assert!(outcome.is_handled());
    assert!(!outcome.is_deferred());
    assert!(outcome.event().is_empty());
    assert!(!irq_ack_pending.load(Ordering::Acquire));
}

#[test]
fn busy_task_access_defers_irq_ack_without_faking_acknowledgement() {
    let access_active = AtomicBool::new(false);
    let irq_ack_pending = AtomicBool::new(false);
    let _task_guard = VirtioBlkAccessGuard::enter_task(&access_active);

    let outcome = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        InterruptStatus::QUEUE_INTERRUPT
    });

    assert!(outcome.is_handled());
    let event = outcome.event();
    let queue = event
        .for_queue(VIRTIO_BLK_QUEUE_ID)
        .expect("deferred virtio IRQ must activate queue service");
    assert!(queue.requires_irq_ack());
    assert!(irq_ack_pending.load(Ordering::Acquire));
}

#[test]
fn repeated_contended_irqs_coalesce_behind_one_deferred_activation() {
    let access_active = AtomicBool::new(false);
    let irq_ack_pending = AtomicBool::new(false);
    let _task_guard = VirtioBlkAccessGuard::enter_task(&access_active);

    let first = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        panic!("contended IRQ must not access the transport")
    });
    let repeated = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        panic!("coalesced IRQ must not access the transport")
    });

    assert!(first.is_deferred());
    assert!(
        first
            .event()
            .for_queue(VIRTIO_BLK_QUEUE_ID)
            .is_some_and(|event| event.requires_irq_ack())
    );
    assert!(repeated.is_deferred());
    assert!(repeated.event().is_empty());
    assert!(irq_ack_pending.load(Ordering::Acquire));
}

#[test]
fn deferred_ack_survives_failed_worker_claim_and_reactivates_after_release() {
    let access_active = AtomicBool::new(false);
    let irq_ack_pending = AtomicBool::new(false);
    let task_owner = VirtioBlkAccessGuard::enter_task(&access_active);
    let outcome = virtio_blk_irq_outcome(&access_active, &irq_ack_pending, true, || {
        panic!("contended hard IRQ must not access the transport")
    });
    let irq_event = outcome.event();
    let event = irq_event
        .for_queue(VIRTIO_BLK_QUEUE_ID)
        .expect("contention must activate the queue continuation");

    assert!(event.requires_irq_ack());
    assert!(VirtioBlkAccessGuard::try_enter_task(&access_active).is_none());
    assert!(
        irq_ack_pending.load(Ordering::Acquire),
        "a failed worker claim must leave the destructive acknowledgement pending"
    );

    drop(task_owner);
    let _worker_owner = VirtioBlkAccessGuard::try_enter_task(&access_active)
        .expect("the requeued worker must acquire transport ownership after release");
    assert!(take_deferred_virtio_queue_irq(&irq_ack_pending, || {
        InterruptStatus::QUEUE_INTERRUPT
    }));
    assert!(!irq_ack_pending.load(Ordering::Acquire));
}

#[test]
fn deferred_irq_ack_classifies_source_before_queue_service() {
    let pending = AtomicBool::new(true);
    assert!(!take_deferred_virtio_queue_irq(&pending, || {
        InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT
    }));
    assert!(!pending.load(Ordering::Acquire));

    pending.store(true, Ordering::Release);
    assert!(take_deferred_virtio_queue_irq(&pending, || {
        InterruptStatus::QUEUE_INTERRUPT
    }));
    assert!(!pending.load(Ordering::Acquire));

    assert!(!take_deferred_virtio_queue_irq(&pending, || {
        panic!("a coalesced deferred IRQ must not acknowledge twice")
    }));
}

#[test]
fn task_side_irq_continuation_never_spins_on_transport_ownership() {
    let active = AtomicBool::new(false);
    let _owner = VirtioBlkAccessGuard::enter_task(&active);

    assert!(VirtioBlkAccessGuard::try_enter_task(&active).is_none());
    assert!(
        active.load(Ordering::Acquire),
        "failed try-enter must not release the current transport owner"
    );
}

#[test]
fn queue_metadata_uses_the_declared_logical_irq_source() {
    let info = virtio_queue_info(16);
    let QueueKind::Interrupt { sources } = info.kind else {
        panic!("virtio block must be interrupt-backed");
    };

    assert_eq!(info.dispatch_mode, DispatchMode::Direct);
    assert_eq!(sources.bits(), 1 << VIRTIO_BLK_IRQ_SOURCE_ID);
    assert_eq!(virtio_queue_ids().bits(), 1 << VIRTIO_BLK_QUEUE_ID);
}

#[test]
fn submitted_descriptor_storage_must_not_move_into_inflight_slot() {
    let device =
        VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::new(AtomicUsize::new(0))));
    let (submitted_req_addr, submitted_resp_addr) = device.with_task(|inner| {
        let storage = inner
            .descriptor_storage
            .as_deref()
            .expect("descriptor storage must be preallocated");
        (storage.req_addr(), storage.resp_addr())
    });
    let moved_owner = Some(device);
    let (stored_req_addr, stored_resp_addr) = moved_owner
        .as_ref()
        .expect("device owner must remain live")
        .with_task(|inner| {
            let storage = inner
                .descriptor_storage
                .as_deref()
                .expect("descriptor storage must remain owned");
            (storage.req_addr(), storage.resp_addr())
        });

    assert_eq!(
        stored_req_addr, submitted_req_addr,
        "virtio descriptors must keep pointing at the same BlkReq storage until completion"
    );
    assert_eq!(
        stored_resp_addr, submitted_resp_addr,
        "virtio descriptors must keep pointing at the same BlkResp storage until completion"
    );
}

#[test]
fn request_submission_does_not_allocate_or_free_in_worker_context() {
    let device = Arc::new(VirtIoBlkDevice::discovered(RecordingTransport::new(
        Arc::new(AtomicUsize::new(0)),
    )));
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::Ready;
        inner.capacity = 1;
    });
    let mut queue = BlockQueue::new(Arc::clone(&device));
    let request = owned_request(
        RequestOp::Read,
        dma_buffer_with_alignment(DmaDirection::FromDevice, 0x1000),
    );

    let (result, activity) = crate::test_klib::audit_allocations(|| {
        rdif_block::IQueue::submit_owned(&mut queue, rdif_block::RequestId::new(53), request)
    });

    let error = result.expect_err("an unconfigured queue must reject the request");
    assert_eq!(error.error(), BlkError::Retry);
    assert_eq!(
        activity,
        crate::test_klib::AllocationActivity {
            allocations: 0,
            deallocations: 0,
        },
        "staged high-priority work must reuse preallocated descriptor storage"
    );
}

#[test]
fn dropping_an_idle_live_queue_requires_dma_quarantine() {
    let live_idle_queue = DmaDropFacts {
        failure_reset_in_progress: false,
        queue_configured: true,
        request_inflight: false,
        reset_acknowledged: false,
    };

    assert!(
        live_idle_queue.requires_quarantine(),
        "PCI queue_unset is a no-op, so DRIVER_OK cannot prove virtqueue DMA is idle"
    );
}

#[test]
fn dropping_live_device_quarantines_descriptor_storage() {
    let id = rdif_block::RequestId::new(41);
    let (op, request, prepared) = prepare_virtio_dma(
        id,
        owned_request(RequestOp::Read, dma_buffer(DmaDirection::FromDevice)),
    )
    .expect("test read request must prepare DMA");
    let drop_counter = Arc::new(AtomicUsize::new(0));
    let mut storage = Box::new(InflightStorage::with_drop_counter(Arc::clone(
        &drop_counter,
    )));
    storage.prepare(op, request.lba);
    // SAFETY: the test models a live descriptor whose device ownership is not
    // proven quiesced before the controller object is unexpectedly dropped.
    let dma = unsafe { prepared.into_in_flight() };
    let inflight = InflightRequest::for_test(id, 7, op, request, dma);
    let device =
        VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::new(AtomicUsize::new(0))));
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::Ready;
        inner.descriptor_storage = Some(storage);
        inner.inflight = Some(inflight);
    });

    drop(device);

    assert_eq!(
        drop_counter.load(Ordering::Relaxed),
        0,
        "live descriptor storage must be leaked with its DMA quarantine"
    );
}

#[test]
fn descriptor_pop_failure_retains_request_for_quiesced_recovery() {
    let id = rdif_block::RequestId::new(43);
    let (op, request, prepared) = prepare_virtio_dma(
        id,
        owned_request(RequestOp::Read, dma_buffer(DmaDirection::FromDevice)),
    )
    .expect("test read request must prepare DMA");
    let drop_counter = Arc::new(AtomicUsize::new(0));
    let storage = Box::new(InflightStorage::with_drop_counter(Arc::clone(
        &drop_counter,
    )));
    // SAFETY: no hardware exists in this unit test. The model deliberately
    // reports a failed used-ring consume before any quiescence proof exists.
    let dma = unsafe { prepared.into_in_flight() };
    let inflight = InflightRequest::for_test(id, 9, op, request, dma);
    let device =
        VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::new(AtomicUsize::new(0))));
    device.with_task(|inner| {
        inner.descriptor_storage = Some(storage);
        inner.inflight = Some(inflight);
        assert_eq!(
            take_inflight_after_used_descriptor(&mut inner.inflight, |_| {
                Err(virtio_drivers::Error::IoError)
            })
            .map(|_| ()),
            Err(BlkError::Io)
        );
        assert!(
            inner.inflight.is_some(),
            "a failed pop_used must remain reclaimable after controller quiescence"
        );
    });
    assert_eq!(drop_counter.load(Ordering::Relaxed), 0);

    drop(device);
    assert_eq!(drop_counter.load(Ordering::Relaxed), 0);
}

#[test]
fn quiesced_recovery_completes_an_accepted_request_exactly_once() {
    let id = rdif_block::RequestId::new(47);
    let (op, request, prepared) = prepare_virtio_dma(
        id,
        owned_request(RequestOp::Read, dma_buffer(DmaDirection::FromDevice)),
    )
    .expect("test read request must prepare DMA");
    let original_ptr = prepared.cpu_ptr();
    // SAFETY: this test has no hardware transport or configured queue, so the
    // synthetic accepted request cannot be accessed by a bus master.
    let dma = unsafe { prepared.into_in_flight() };
    let inflight = InflightRequest::for_test(id, 11, op, request, dma);
    let device = Arc::new(VirtIoBlkDevice::discovered(RecordingTransport::new(
        Arc::new(AtomicUsize::new(0)),
    )));
    device.with_task(|inner| {
        inner.init_phase = VirtioBlockInitPhase::Ready;
        inner.inflight = Some(inflight);
    });
    let mut queue = BlockQueue::new(Arc::clone(&device));
    // SAFETY: no queue was configured and no hardware exists in this test, so
    // DMA is already quiesced for the synthetic controller epoch.
    let proof = unsafe {
        rdif_block::DmaQuiesced::new(
            rdif_block::ControllerEpoch::new(3),
            core::ptr::from_ref(&*device).expose_provenance(),
        )
    };
    let mut completions = CompletionRecorder::default();

    rdif_block::IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut completions)
        .expect("the accepted request must be reclaimable after quiescence");
    assert_eq!(
        rdif_block::IQueue::reclaim_after_quiesce(&mut queue, &proof, &mut completions),
        Err(BlkError::InvalidDmaProof),
        "one queue cannot consume the same controller epoch twice"
    );

    assert_eq!(completions.calls, 1);
    let completion = completions
        .completion
        .take()
        .expect("the accepted request must have one terminal completion");
    assert_eq!(completion.id, id);
    assert_eq!(completion.result, Err(BlkError::Cancelled));
    assert_eq!(
        completion
            .request
            .data
            .as_ref()
            .expect("terminal completion must return DMA ownership")
            .cpu_ptr(),
        original_ptr
    );
}

#[test]
fn dma_prepare_and_completion_restore_the_exact_runtime_buffer() {
    let buffer = dma_buffer(DmaDirection::FromDevice);
    let original_ptr = buffer.cpu_ptr();
    let (op, request, prepared) = prepare_virtio_dma(
        rdif_block::RequestId::new(3),
        owned_request(RequestOp::Read, buffer),
    )
    .expect("matching read DMA must be accepted");

    assert_eq!(op, InflightOp::Read);
    assert!(request.data.is_none());
    assert_eq!(prepared.cpu_ptr(), original_ptr);

    // SAFETY: this test models a command accepted and then immediately
    // quiesced without a device ever observing the prepared allocation.
    let inflight = unsafe { prepared.into_in_flight() };
    // SAFETY: no hardware exists in this unit test, so bus-master access is
    // impossible and the modelled request is already quiesced.
    let restored = unsafe { inflight.complete_after_quiesce() }.into_cpu_buffer();
    assert_eq!(restored.cpu_ptr(), original_ptr);
}

#[test]
fn rejected_dma_direction_returns_the_exact_runtime_buffer() {
    let buffer = dma_buffer(DmaDirection::ToDevice);
    let original_ptr = buffer.cpu_ptr();
    let rejected = match prepare_virtio_dma(
        rdif_block::RequestId::new(7),
        owned_request(RequestOp::Read, buffer),
    ) {
        Ok(_) => panic!("read must reject a ToDevice buffer"),
        Err(rejected) => rejected,
    };
    let (id, error, request) = rejected.into_parts();

    assert_eq!(id, rdif_block::RequestId::new(7));
    assert_eq!(error, rdif_block::BlkError::InvalidRequest);
    assert_eq!(
        request
            .data
            .as_ref()
            .expect("rejected request must retain DMA ownership")
            .cpu_ptr(),
        original_ptr
    );
}

#[test]
fn bidirectional_dma_is_accepted_for_read_and_write_requests() {
    for op in [RequestOp::Read, RequestOp::Write] {
        let request = owned_request(op, dma_buffer(DmaDirection::Bidirectional));

        let (_, _, prepared) = prepare_virtio_dma(rdif_block::RequestId::new(9), request)
            .expect("RDIF-valid bidirectional DMA must remain usable by VirtIO block");
        assert_eq!(prepared.direction(), DmaDirection::Bidirectional);
    }
}

#[test]
fn interrupt_path_uses_large_requests_to_amortize_completion_wakes() {
    let max_segment_size = virtio_queue_info(0).limits.max_segment_size;
    assert_eq!(max_segment_size, VIRTIO_BLK_DMA_BUFFER_SIZE);
    assert!(
        max_segment_size >= 4 * 1024 * 1024,
        "max_inflight=1 IRQ-driven completion needs large request chunks"
    );
}

#[derive(Default)]
struct CompletionRecorder {
    calls: usize,
    completion: Option<CompletedRequest>,
}

impl CompletionSink for CompletionRecorder {
    fn complete(&mut self, completion: CompletedRequest) {
        self.calls += 1;
        assert!(
            self.completion.replace(completion).is_none(),
            "one accepted request must not emit more than one terminal completion"
        );
    }
}
