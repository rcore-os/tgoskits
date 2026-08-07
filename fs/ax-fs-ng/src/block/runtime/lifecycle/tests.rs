use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    any::Any,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    time::Duration,
};
use std::{
    alloc::{alloc_zeroed, dealloc},
    sync::{Mutex as StdMutex, mpsc},
    thread,
    time::Instant,
};

use dma_api::{
    DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp, InFlightDma,
};
use irq_framework::{HwIrq, IrqDomainId, IrqId};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, CompletedRequest, CompletionSink, ControlEvent,
    DriverGeneric, GroupIrqSink, HardIrqHandler, HardwareQueue, IrqAck, IrqDisposition,
    IrqQueueMask, OwnedRequestBatch, QueueLimits, RequestId, SharedHardIrqHandler, SubmissionSink,
};

use super::{device::create_cpu_channels, *};
use crate::os::{BlockIrqOutcome, BlockIrqRegistrar, install_dma_op, set_irq_registrar};

struct TestDmaOp;

impl DmaOp for TestDmaOp {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let cpu_addr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe { DmaAllocHandle::new(cpu_addr, (cpu_addr.as_ptr() as u64).into(), layout) })
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

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { self.dealloc_contiguous(handle) };
        Ok(())
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

static TEST_DMA_OP: TestDmaOp = TestDmaOp;

struct LifecycleQueue {
    log: Arc<StdMutex<Vec<&'static str>>>,
}

impl HardwareQueue for LifecycleQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        test_queue_info()
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
        self.log.lock().unwrap().push("queue_shutdown");
        Ok(())
    }
}

struct SpuriousHandler;

impl HardIrqHandler for SpuriousHandler {
    fn ack(&mut self) -> IrqAck {
        IrqAck::spurious(0)
    }
}

struct QueueZeroHandler;

impl HardIrqHandler for QueueZeroHandler {
    fn ack(&mut self) -> IrqAck {
        IrqAck::cleared(IrqQueueMask::from_queue(0), ControlEvent::new(0, 0))
    }
}

struct SharedSpuriousHandler;

impl SharedHardIrqHandler for SharedSpuriousHandler {
    fn ack(&mut self, _sink: &mut dyn GroupIrqSink) -> IrqDisposition {
        IrqDisposition::Spurious
    }
}

#[derive(Default)]
struct BatchingQueueCounters {
    submitted: AtomicUsize,
    commits: AtomicUsize,
    largest_batch: AtomicUsize,
}

struct BatchingReadQueue {
    counters: Arc<BatchingQueueCounters>,
    next_id: usize,
    pending: Vec<(RequestId, Option<InFlightDma>)>,
}

impl DriverGeneric for BatchingReadQueue {
    fn name(&self) -> &str {
        "batching-read"
    }
}

impl HardwareQueue for BatchingReadQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        batching_queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        self.counters
            .largest_batch
            .fetch_max(requests.len(), Ordering::AcqRel);
        let mut accepted = 0;
        while self.pending.len() < 4 {
            let Some(request) = requests.pop_front() else {
                break;
            };
            self.next_id += 1;
            let id = RequestId::new(self.next_id);
            let data = request
                .data
                .map(|prepared| unsafe { prepared.into_in_flight() });
            self.pending.push((id, data));
            sink.accepted(id);
            accepted += 1;
        }
        self.counters
            .submitted
            .fetch_add(accepted, Ordering::AcqRel);
        let disposition = if requests.is_empty() {
            BatchSubmitDisposition::Continue
        } else {
            BatchSubmitDisposition::QueueFull
        };
        BatchSubmitResult::new(accepted, disposition)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.counters.commits.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for (id, data) in self.pending.drain(..) {
            let data = data.map(|in_flight| unsafe { in_flight.complete_after_quiesce() });
            sink.complete(CompletedRequest::new(id, Ok(()), data));
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for (id, data) in self.pending.drain(..) {
            let data = data.map(|in_flight| unsafe { in_flight.complete_after_quiesce() });
            sink.complete(CompletedRequest::new(id, Err(BlkError::Io), data));
        }
        Ok(())
    }
}

struct BatchingReadController {
    queue: Option<BatchingReadQueue>,
}

impl DriverGeneric for BatchingReadController {
    fn name(&self) -> &str {
        "batching-read-controller"
    }
}

impl BlockController for BatchingReadController {
    fn device_info(&self) -> DeviceInfo {
        batching_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().unwrap())],
                vec![IrqEndpoint::new(0, 1, Box::new(QueueZeroHandler))],
            )),
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct LifecycleController {
    queue: Option<LifecycleQueue>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

struct TerminalBeforeShutdownController {
    queue: Option<LifecycleQueue>,
    terminal: bool,
}

impl DriverGeneric for TerminalBeforeShutdownController {
    fn name(&self) -> &str {
        "terminal-before-shutdown-controller"
    }
}

impl BlockController for TerminalBeforeShutdownController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().unwrap())],
                vec![IrqEndpoint::new(0, 1, Box::new(SpuriousHandler))],
            )),
            ControllerEvent::Watchdog { .. } => {
                self.terminal = true;
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            ControllerEvent::QuiesceIrqs if self.terminal => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            ControllerEvent::Shutdown => Err(BlkError::Io),
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for LifecycleController {
    fn name(&self) -> &str {
        "lifecycle-controller"
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockController for LifecycleController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().unwrap())],
                vec![IrqEndpoint::new(0, 1, Box::new(SpuriousHandler))],
            )),
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("controller_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                self.log.lock().unwrap().push("controller_shutdown");
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct GroupMemberController {
    name: &'static str,
    queue: Option<LifecycleQueue>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

impl DriverGeneric for GroupMemberController {
    fn name(&self) -> &str {
        self.name
    }
}

impl BlockController for GroupMemberController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::Rearm { .. } => {
                self.log.lock().unwrap().push("member_rearm");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("member_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                self.log.lock().unwrap().push("member_shutdown");
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct TestControllerGroup {
    members: Option<Vec<BlockGroupMember>>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

impl DriverGeneric for TestControllerGroup {
    fn name(&self) -> &str {
        "test-controller-group"
    }
}

impl BlockControllerGroup for TestControllerGroup {
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError> {
        match event {
            GroupControllerEvent::Start => Ok(GroupControllerUpdate::with_resources(
                ControllerState::Ready,
                self.members.take().ok_or(BlkError::Io)?,
                vec![SharedIrqEndpoint::new(0, Box::new(SharedSpuriousHandler))],
            )),
            GroupControllerEvent::Rearm { .. } => {
                self.log.lock().unwrap().push("group_rearm");
                Ok(GroupControllerUpdate::state(ControllerState::Ready))
            }
            GroupControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("group_quiesce");
                Ok(GroupControllerUpdate::state(ControllerState::Ready))
            }
            GroupControllerEvent::Shutdown => {
                self.log.lock().unwrap().push("group_shutdown");
                Ok(GroupControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(GroupControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct EndpointFirstController {
    queue: Option<LifecycleQueue>,
    register_retries: Arc<AtomicUsize>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

struct WaitingForIrqController;

impl DriverGeneric for WaitingForIrqController {
    fn name(&self) -> &str {
        "waiting-for-irq-controller"
    }
}

impl BlockController for WaitingForIrqController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::WaitingForIrq,
                Vec::new(),
                vec![IrqEndpoint::new(0, 0, Box::new(SpuriousHandler))],
            )),
            ControllerEvent::Rearm { .. } => {
                Ok(ControllerUpdate::state(ControllerState::WaitingForIrq))
            }
            ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
            _ => Ok(ControllerUpdate::state(ControllerState::WaitingForIrq)),
        }
    }
}

impl DriverGeneric for EndpointFirstController {
    fn name(&self) -> &str {
        "endpoint-first-controller"
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockController for EndpointFirstController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::RegisterPending {
                    retry_after: Duration::from_millis(30),
                },
                Vec::new(),
                vec![IrqEndpoint::new(0, 0, Box::new(SpuriousHandler))],
            )),
            ControllerEvent::RegisterRetry => {
                self.register_retries.fetch_add(1, Ordering::Relaxed);
                Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    vec![Box::new(self.queue.take().unwrap())],
                    Vec::new(),
                ))
            }
            ControllerEvent::Rearm { .. } => {
                Ok(ControllerUpdate::state(ControllerState::RegisterPending {
                    retry_after: Duration::from_millis(1),
                }))
            }
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("controller_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                self.log.lock().unwrap().push("controller_shutdown");
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

struct TestIrqRegistrar {
    log: StdMutex<Option<Arc<StdMutex<Vec<&'static str>>>>>,
    action: StdMutex<Option<Arc<StdMutex<Option<BlockIrqAction>>>>>,
    fail_registration: AtomicBool,
}

static TEST_IRQ_REGISTRAR: TestIrqRegistrar = TestIrqRegistrar {
    log: StdMutex::new(None),
    action: StdMutex::new(None),
    fail_registration: AtomicBool::new(false),
};
static TEST_IRQ_REGISTRAR_SERIAL: StdMutex<()> = StdMutex::new(());

fn lock_test_irq_registrar() -> std::sync::MutexGuard<'static, ()> {
    TEST_IRQ_REGISTRAR_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TestIrqRegistration {
    log: Arc<StdMutex<Vec<&'static str>>>,
    action: Arc<StdMutex<Option<BlockIrqAction>>>,
}

impl BlockIrqRegistration for TestIrqRegistration {
    fn enable(&self) -> AxResult {
        self.log.lock().unwrap().push("irq_enable");
        Ok(())
    }

    fn disable_and_synchronize(&self) -> AxResult {
        self.log.lock().unwrap().push("irq_disable_sync");
        Ok(())
    }
}

impl Drop for TestIrqRegistration {
    fn drop(&mut self) {
        self.action.lock().unwrap().take();
        self.log.lock().unwrap().push("irq_free");
    }
}

impl TestIrqRegistrar {
    fn run_registered_action(&self) -> BlockIrqOutcome {
        let action = self
            .action
            .lock()
            .unwrap()
            .clone()
            .expect("test IRQ action was not registered");
        action
            .lock()
            .unwrap()
            .as_mut()
            .expect("test IRQ action was already freed")
            .run()
    }
}

impl BlockIrqRegistrar for TestIrqRegistrar {
    fn register(
        &self,
        _name: String,
        _irq: IrqId,
        _cpu: usize,
        action: BlockIrqAction,
    ) -> AxResult<Box<dyn BlockIrqRegistration>> {
        let log = self.log.lock().unwrap().clone().ok_or(AxError::BadState)?;
        if self.fail_registration.load(Ordering::Acquire) {
            log.lock().unwrap().push("irq_register_failed");
            return Err(AxError::Io);
        }
        log.lock().unwrap().push("irq_register_disabled");
        let action = Arc::new(StdMutex::new(Some(action)));
        *self.action.lock().unwrap() = Some(Arc::clone(&action));
        Ok(Box::new(TestIrqRegistration { log, action }))
    }
}

fn test_queue_info() -> QueueInfo {
    let mut limits = QueueLimits::simple(512, u64::MAX);
    limits.max_inflight = 1;
    limits.supports_flush = true;
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(32, 512),
        limits,
    }
}

fn batching_queue_info() -> QueueInfo {
    let mut limits = QueueLimits::simple(512, u64::MAX);
    limits.max_blocks_per_request = 1;
    limits.max_inflight = 4;
    limits.max_submit_batch = 4;
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(32, 512),
        limits,
    }
}

fn log_position(log: &[&str], item: &str) -> usize {
    log.iter()
        .position(|entry| *entry == item)
        .unwrap_or_else(|| panic!("missing lifecycle event {item}: {log:?}"))
}

#[test]
fn read_blocks_queues_the_next_bounded_window_before_waiting() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let counters = Arc::new(BatchingQueueCounters::default());
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
        }),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "batching-read",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let reader = Arc::clone(&handle);
    let (result_tx, result_rx) = mpsc::channel();
    let read_thread = thread::spawn(move || {
        let mut buffer = vec![0; 8 * 512];
        result_tx.send(reader.read_blocks(0, &mut buffer)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while counters.submitted.load(Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < deadline,
            "synchronous wrapper waited before submitting the full I/O window"
        );
        thread::yield_now();
    }
    while counters.commits.load(Ordering::Acquire) < 1 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not commit the submitted I/O window"
        );
        thread::yield_now();
    }

    assert_eq!(counters.largest_batch.load(Ordering::Acquire), 4);
    assert_eq!(counters.commits.load(Ordering::Acquire), 1);
    while handle
        .inner
        .cpu_channels
        .lock()
        .iter()
        .map(|channel| channel.channel.queued_len())
        .sum::<usize>()
        < 4
    {
        assert!(
            Instant::now() < deadline,
            "the requester did not queue the second window before the first IRQ"
        );
        thread::yield_now();
    }
    assert_eq!(counters.submitted.load(Ordering::Acquire), 4);
    assert!(result_rx.try_recv().is_err());
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    while counters.submitted.load(Ordering::Acquire) < 8 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not refill from the queued second window"
        );
        thread::yield_now();
    }
    while counters.commits.load(Ordering::Acquire) < 2 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not commit the refilled second window"
        );
        thread::yield_now();
    }
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    read_thread.join().unwrap();
    assert_eq!(handle.shutdown(), 1);
}

#[test]
fn failed_irq_registration_stops_controller_before_dropping_emitted_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(true, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(11));
    let result = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "failed-registration",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);

    assert_eq!(result.err(), Some(BlkError::Io));
    let log = log.lock().unwrap();
    let failed_registration = log_position(&log, "irq_register_failed");
    let controller = log_position(&log, "controller_shutdown");
    let queue = log_position(&log, "queue_shutdown");
    assert!(failed_registration < controller);
    assert!(controller < queue);
}

#[test]
fn teardown_disables_controller_before_queue_memory_is_released() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(9));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "lifecycle",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let hctxs = handle.inner.hctxs.lock().clone();
    let cpu_channels = create_cpu_channels(&hctxs, 8).unwrap();
    assert_eq!(cpu_channels.len(), 8);
    assert!(cpu_channels.iter().all(|channel| channel.hctx.id() == 0));
    for channel in cpu_channels {
        channel.channel.close();
    }

    assert_eq!(handle.shutdown(), 1);
    let log = log.lock().unwrap();
    let quiesce = log_position(&log, "controller_quiesce");
    let disable = log_position(&log, "irq_disable_sync");
    let free = log_position(&log, "irq_free");
    let queue = log_position(&log, "queue_shutdown");
    let controller = log_position(&log, "controller_shutdown");
    assert!(quiesce < disable);
    assert!(disable < free);
    assert!(free < controller);
    assert!(controller < queue);
}

#[test]
fn controller_group_enables_shared_irq_before_unmasking_sources_and_tears_down_once() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let member = |member_name| {
        Box::new(GroupMemberController {
            name: member_name,
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
        }) as Box<dyn BlockController>
    };
    let group = TestControllerGroup {
        members: Some(vec![
            BlockGroupMember::new(0, member("group-member-0")),
            BlockGroupMember::new(1, member("group-member-1")),
        ]),
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(14));
    let runtime = BlockRuntime::from_rdif_sources(
        Vec::new(),
        [RdifBlockGroup::new_with_irqs(
            "shared-group",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(group),
        )],
    );

    assert_eq!(runtime.devices().len(), 2);
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|entry| **entry == "irq_register_disabled")
            .count(),
        1
    );
    {
        let log = log.lock().unwrap();
        let irq_enable = log_position(&log, "irq_enable");
        assert!(irq_enable < log_position(&log, "member_rearm"));
        assert!(irq_enable < log_position(&log, "group_rearm"));
    }
    assert_eq!(runtime.release_irqs_for_passthrough(), 1);
    let log = log.lock().unwrap();
    assert_eq!(
        log.iter()
            .filter(|entry| **entry == "irq_disable_sync")
            .count(),
        1
    );
    assert_eq!(log.iter().filter(|entry| **entry == "irq_free").count(), 1);
    assert_eq!(
        log.iter()
            .filter(|entry| **entry == "member_shutdown")
            .count(),
        2
    );
    assert!(log_position(&log, "irq_disable_sync") < log_position(&log, "member_shutdown"));
    assert!(log_position(&log, "member_shutdown") < log_position(&log, "group_shutdown"));
}

#[test]
fn late_hctx_failure_cannot_resurrect_a_stopped_device() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = LifecycleController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        log,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "late-failure-after-stop",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(handle.shutdown(), 1);
    handle.inner.hctx_failed(0, BlkError::Io);
    assert_eq!(
        handle.inner.state.load(Ordering::Acquire),
        DEVICE_STOPPED,
        "a stale failure notification must not regress terminal device state"
    );
    assert_eq!(
        handle.shutdown(),
        0,
        "terminal teardown must remain idempotent after a stale failure"
    );
}

#[test]
fn teardown_releases_queue_when_quiesce_confirms_prior_watchdog_shutdown() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let controller = TerminalBeforeShutdownController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        terminal: false,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(13));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "terminal-before-shutdown",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(
        handle
            .inner
            .controller
            .call(ControllerEvent::Watchdog { queue_id: 0 }),
        Ok(ControllerState::Shutdown)
    );
    assert_eq!(handle.shutdown(), 1);
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|event| *event == "queue_shutdown"),
        "a prior terminal acknowledgement must permit queue teardown"
    );
}

#[test]
fn controller_can_register_control_irq_before_creating_an_io_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    crate::os::task::reset_test_wait_timeout_count();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let register_retries = Arc::new(AtomicUsize::new(0));
    let controller = EndpointFirstController {
        queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        register_retries: Arc::clone(&register_retries),
        log,
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(10));

    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "endpoint-first",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(register_retries.load(Ordering::Relaxed), 1);
    assert!(
        crate::os::task::test_wait_timeout_count() >= 1,
        "register retry must sleep on the runtime notification"
    );
    assert_eq!(handle.inner.hctxs.lock().len(), 1);
    assert_eq!(handle.inner.cpu_channels.lock().len(), 1);
    assert_eq!(handle.shutdown(), 1);
}

#[test]
fn bootstrap_preserves_waiting_for_irq_controller_without_io_queue() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let irq = IrqId::new(IrqDomainId(1), HwIrq(15));

    let handle = BlockDeviceHandle::bootstrap(
        String::from("waiting-for-irq"),
        vec![BlockIrqSource { source_id: 0, irq }],
        Box::new(WaitingForIrqController),
    )
    .expect("a control IRQ may precede creation of the first I/O queue");

    assert_eq!(handle.inner.state.load(Ordering::Acquire), DEVICE_STARTING);
    assert!(handle.inner.hctxs.lock().is_empty());
    assert_eq!(handle.shutdown(), 1);
}

fn barrier_test_inner() -> Arc<DeviceInner> {
    let ops = runtime_ops().unwrap();
    let controller_notification = ops.notification();
    Arc::new(DeviceInner {
        name: String::from("barrier-test"),
        info: IrqMutex::new(test_queue_info().device),
        max_io_queues: 1,
        irq_sources: Vec::new(),
        hctxs: IrqMutex::new(Vec::new()),
        detached_queues: IrqMutex::new(Vec::new()),
        cpu_channels: IrqMutex::new(Vec::new()),
        irq_registrations: IrqMutex::new(Vec::new()),
        controller: Arc::new(ControllerPort {
            commands: BoundedChannel::with_item_notification(
                1,
                Arc::clone(&controller_notification),
            )
            .unwrap(),
            notification: controller_notification,
            irq_latches: IrqMutex::new(Vec::new()),
        }),
        controller_thread: IrqMutex::new(None),
        state: AtomicU8::new(DEVICE_READY),
        accepting: AtomicBool::new(true),
        active_data: AtomicUsize::new(0),
        flush_active: AtomicBool::new(false),
        data_gate_waiters: TaskWaiters::new(),
        flush_gate_waiters: TaskWaiters::new(),
        data_drain_waiters: TaskWaiters::new(),
        state_notification: ops.notification(),
    })
}

#[test]
fn flush_barrier_waits_for_prior_data_and_holds_later_data() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner
        .enter_data_submissions(1, SubmissionAdmission::Blocking)
        .unwrap();

    let flush_inner = Arc::clone(&inner);
    let (flush_tx, flush_rx) = mpsc::channel();
    let flush_thread = thread::spawn(move || {
        flush_inner
            .begin_flush_barrier(SubmissionAdmission::Blocking)
            .unwrap();
        flush_tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !inner.flush_active.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "flush gate was not acquired");
        thread::yield_now();
    }

    let later_inner = Arc::clone(&inner);
    let (later_tx, later_rx) = mpsc::channel();
    let later_thread = thread::spawn(move || {
        later_inner
            .enter_data_submissions(1, SubmissionAdmission::Blocking)
            .unwrap();
        later_tx.send(()).unwrap();
    });
    assert!(flush_rx.recv_timeout(Duration::from_millis(20)).is_err());
    assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

    inner.request_completed(RequestOp::Write, 1, Ok(()));
    flush_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

    inner.request_completed(RequestOp::Flush, 0, Ok(()));
    later_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    inner.request_completed(RequestOp::Read, 1, Ok(()));
    flush_thread.join().unwrap();
    later_thread.join().unwrap();
}

#[test]
fn nowait_admission_never_sleeps_behind_flush_barrier() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.flush_active.store(true, Ordering::Release);

    assert_eq!(
        inner.enter_data_submissions(1, SubmissionAdmission::Nowait),
        Err(BlkError::Retry)
    );
    assert_eq!(inner.active_data.load(Ordering::Acquire), 0);

    inner.flush_active.store(false, Ordering::Release);
    inner.active_data.store(1, Ordering::Release);
    assert_eq!(
        inner.begin_flush_barrier(SubmissionAdmission::Nowait),
        Err(BlkError::Retry)
    );
    assert!(!inner.flush_active.load(Ordering::Acquire));
}

#[test]
fn flush_completion_wakes_every_blocked_data_submitter() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.flush_active.store(true, Ordering::Release);

    let (done_tx, done_rx) = mpsc::channel();
    let mut joins = Vec::new();
    for _ in 0..3 {
        let waiter = Arc::clone(&inner);
        let done_tx = done_tx.clone();
        joins.push(thread::spawn(move || {
            waiter
                .enter_data_submissions(1, SubmissionAdmission::Blocking)
                .unwrap();
            done_tx.send(()).unwrap();
        }));
    }
    drop(done_tx);
    let deadline = Instant::now() + Duration::from_secs(1);
    while inner.data_gate_waiters.len() != 3 {
        assert!(
            Instant::now() < deadline,
            "data submitters did not enter the barrier wait set"
        );
        thread::yield_now();
    }

    inner.request_completed(RequestOp::Flush, 0, Ok(()));
    for _ in 0..3 {
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    for _ in 0..3 {
        inner.request_completed(RequestOp::Read, 1, Ok(()));
    }
    for join in joins {
        join.join().unwrap();
    }
}
