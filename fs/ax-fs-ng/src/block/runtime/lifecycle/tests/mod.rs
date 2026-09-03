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
    IrqQueueMask, OwnedRequestBatch, QueueLimits, RequestFlags, RequestId, SharedHardIrqHandler,
    SubmissionSink,
};

use super::{device::create_cpu_channels, *};
use crate::os::{BlockIrqOutcome, BlockIrqRegistrar, install_dma_op, set_irq_registrar};

mod batching;
mod flush_barrier;
mod publication;
mod resource_rollback;
mod teardown;

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
        Some(unsafe {
            DmaAllocHandle::new(
                cpu_addr,
                cpu_addr,
                (cpu_addr.as_ptr() as u64).into(),
                layout,
            )
        })
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

struct IndexedLifecycleQueue {
    id: usize,
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

impl HardwareQueue for IndexedLifecycleQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            ..test_queue_info()
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
    fua_submitted: AtomicUsize,
    commits: AtomicUsize,
    largest_batch: AtomicUsize,
}

struct BatchingReadQueue {
    counters: Arc<BatchingQueueCounters>,
    next_id: usize,
    pending: Vec<(RequestId, Option<InFlightDma>)>,
    fail_next_drain: bool,
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
            if request.flags.contains(RequestFlags::FUA) {
                self.counters.fua_submitted.fetch_add(1, Ordering::AcqRel);
            }
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
        let result = if core::mem::take(&mut self.fail_next_drain) {
            Err(BlkError::Io)
        } else {
            Ok(())
        };
        for (id, data) in self.pending.drain(..) {
            let data = data.map(|in_flight| unsafe { in_flight.complete_after_quiesce() });
            sink.complete(CompletedRequest::new(id, result, data));
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
    repeat_device_info_on_quiesce: bool,
}

struct TerminalBeforeShutdownController {
    queue: Option<LifecycleQueue>,
    terminal: bool,
}

struct ProvisionalGroupTerminalController {
    queue: Option<LifecycleQueue>,
}

struct QuiesceFailureController {
    queue: Option<LifecycleQueue>,
}

struct DropTrackedShutdownFailureGroup {
    log: Arc<StdMutex<Vec<&'static str>>>,
}

struct GroupMemberShutdownFailureController {
    queue: Option<LifecycleQueue>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

struct DropTrackedMemberFailureGroup {
    members: Option<Vec<BlockGroupMember>>,
    log: Arc<StdMutex<Vec<&'static str>>>,
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

impl DriverGeneric for ProvisionalGroupTerminalController {
    fn name(&self) -> &str {
        "provisional-group-terminal-controller"
    }
}

impl BlockController for ProvisionalGroupTerminalController {
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
            ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for QuiesceFailureController {
    fn name(&self) -> &str {
        "quiesce-failure-controller"
    }
}

impl BlockController for QuiesceFailureController {
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
                vec![IrqEndpoint::new(0, 1, Box::new(SpuriousHandler))],
            )),
            ControllerEvent::QuiesceIrqs => Err(BlkError::Io),
            ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl Drop for DropTrackedShutdownFailureGroup {
    fn drop(&mut self) {
        self.log.lock().unwrap().push("group_controller_drop");
    }
}

impl DriverGeneric for DropTrackedShutdownFailureGroup {
    fn name(&self) -> &str {
        "drop-tracked-shutdown-failure-group"
    }
}

impl BlockControllerGroup for DropTrackedShutdownFailureGroup {
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError> {
        match event {
            GroupControllerEvent::Shutdown => Err(BlkError::Io),
            _ => Ok(GroupControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for GroupMemberShutdownFailureController {
    fn name(&self) -> &str {
        "group-member-shutdown-failure-controller"
    }
}

impl BlockController for GroupMemberShutdownFailureController {
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
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("member_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown => Err(BlkError::Io),
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl Drop for DropTrackedMemberFailureGroup {
    fn drop(&mut self) {
        self.log.lock().unwrap().push("group_controller_drop");
    }
}

impl DriverGeneric for DropTrackedMemberFailureGroup {
    fn name(&self) -> &str {
        "drop-tracked-member-failure-group"
    }
}

impl BlockControllerGroup for DropTrackedMemberFailureGroup {
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError> {
        match event {
            GroupControllerEvent::Start => Ok(GroupControllerUpdate::with_resources(
                ControllerState::Ready,
                self.members.take().ok_or(BlkError::Io)?,
                vec![SharedIrqEndpoint::new(0, Box::new(SharedSpuriousHandler))],
            )),
            GroupControllerEvent::Rearm { .. } | GroupControllerEvent::QuiesceIrqs => {
                Ok(GroupControllerUpdate::state(ControllerState::Ready))
            }
            GroupControllerEvent::Shutdown => {
                Ok(GroupControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(GroupControllerUpdate::state(ControllerState::Ready)),
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
                let update = ControllerUpdate::state(ControllerState::Ready);
                if self.repeat_device_info_on_quiesce {
                    Ok(update.with_device_info(self.device_info()))
                } else {
                    Ok(update)
                }
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
    terminal_on_rearm: bool,
    rearm_count: usize,
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
                self.rearm_count += 1;
                if self.terminal_on_rearm && self.rearm_count > 1 {
                    Ok(ControllerUpdate::state(ControllerState::Shutdown))
                } else {
                    Ok(ControllerUpdate::state(ControllerState::Ready))
                }
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

struct TwoIrqControllerGroup {
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

impl DriverGeneric for TwoIrqControllerGroup {
    fn name(&self) -> &str {
        "two-irq-controller-group"
    }
}

impl BlockControllerGroup for TwoIrqControllerGroup {
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError> {
        match event {
            GroupControllerEvent::Start => Ok(GroupControllerUpdate::with_resources(
                ControllerState::Ready,
                self.members.take().ok_or(BlkError::Io)?,
                vec![
                    SharedIrqEndpoint::new(0, Box::new(SharedSpuriousHandler)),
                    SharedIrqEndpoint::new(1, Box::new(SharedSpuriousHandler)),
                ],
            )),
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
    next_registration: AtomicUsize,
    fail_enable_at: AtomicUsize,
}

static TEST_IRQ_REGISTRAR: TestIrqRegistrar = TestIrqRegistrar {
    log: StdMutex::new(None),
    action: StdMutex::new(None),
    fail_registration: AtomicBool::new(false),
    next_registration: AtomicUsize::new(0),
    fail_enable_at: AtomicUsize::new(usize::MAX),
};
static TEST_IRQ_FAIL_SYNCHRONIZE: AtomicBool = AtomicBool::new(false);
static TEST_IRQ_REGISTRAR_SERIAL: StdMutex<()> = StdMutex::new(());

fn lock_test_irq_registrar() -> std::sync::MutexGuard<'static, ()> {
    TEST_IRQ_REGISTRAR_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_device_teardown(device: &DeviceInner) {
    loop {
        if device.lifecycle_gate.lock().phase == DevicePhase::Stopped {
            return;
        }
        device
            .shutdown_waiters
            .wait_while(|| device.lifecycle_gate.lock().phase != DevicePhase::Stopped)
            .unwrap();
    }
}

struct TestIrqRegistration {
    log: Arc<StdMutex<Vec<&'static str>>>,
    action: Arc<StdMutex<Option<BlockIrqAction>>>,
    fail_enable: bool,
}

impl BlockIrqRegistration for TestIrqRegistration {
    fn enable(&self) -> BlockResult {
        self.log.lock().unwrap().push("irq_enable");
        if self.fail_enable {
            self.log.lock().unwrap().push("irq_enable_failed");
            Err(BlockError::Io)
        } else {
            Ok(())
        }
    }

    fn disable_and_synchronize(&self) -> BlockResult {
        self.log.lock().unwrap().push("irq_disable_sync");
        if TEST_IRQ_FAIL_SYNCHRONIZE.load(Ordering::Acquire) {
            Err(BlockError::Io)
        } else {
            Ok(())
        }
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
    ) -> BlockResult<Box<dyn BlockIrqRegistration>> {
        let log = self
            .log
            .lock()
            .unwrap()
            .clone()
            .ok_or(BlockError::InvalidState)?;
        if self.fail_registration.load(Ordering::Acquire) {
            log.lock().unwrap().push("irq_register_failed");
            return Err(BlockError::Io);
        }
        let registration_index = self.next_registration.fetch_add(1, Ordering::AcqRel);
        log.lock().unwrap().push("irq_register_disabled");
        let action = Arc::new(StdMutex::new(Some(action)));
        *self.action.lock().unwrap() = Some(Arc::clone(&action));
        Ok(Box::new(TestIrqRegistration {
            log,
            action,
            fail_enable: registration_index == self.fail_enable_at.load(Ordering::Acquire),
        }))
    }
}

fn test_queue_info() -> QueueInfo {
    let mut limits = QueueLimits::simple(
        512,
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ),
    );
    limits.max_inflight = 1;
    limits.supports_flush = true;
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(32, 512),
        limits,
    }
}

fn batching_queue_info() -> QueueInfo {
    let mut limits = QueueLimits::simple(
        512,
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ),
    );
    limits.max_blocks_per_request = 1;
    limits.max_inflight = 4;
    limits.max_submit_batch = 4;
    limits.supported_flags = RequestFlags::FUA;
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
