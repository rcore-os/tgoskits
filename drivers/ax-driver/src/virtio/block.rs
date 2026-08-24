extern crate alloc;

use alloc::{boxed::Box, format, sync::Arc, vec};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::PreemptIrqSaveGuard;
use dma_api::{DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection, InFlightDma};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, BlockController, CompletedRequest,
    CompletionSink, ControlEvent, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo,
    DriverGeneric, HardIrqHandler, HardwareQueue, IrqAck, IrqEndpoint, IrqQueueMask, OwnedRequest,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestFlags, RequestId, RequestOp, SubmissionSink,
    SubmitError, validate_owned_request,
};
use rdrive::{PlatformDevice, probe::OnProbeError, register::ProbeKind};
use virtio_drivers::{
    Error as VirtIoError, PAGE_SIZE,
    device::blk::SECTOR_SIZE,
    queue::VirtQueue,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport, mmio::MmioTransport},
};

use crate::{
    BindingInfo, binding_info_from_fdt,
    block::PlatformDeviceBlock,
    virtio::{self, VirtIoHalImpl},
};

const DEVICE_NAME: &str = "virtio-blk";
const QUEUE_ID: usize = 0;
const IRQ_SOURCE_ID: usize = 0;
const VIRTIO_BLOCK_QUEUE_SIZE: usize = 16;
const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VIRTIO_F_RING_INDIRECT_DESC: u64 = 1 << 28;
const VIRTIO_F_RING_EVENT_IDX: u64 = 1 << 29;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_BLOCK_SUPPORTED_FEATURES: u64 = VIRTIO_BLK_F_RO
    | VIRTIO_BLK_F_FLUSH
    | VIRTIO_F_RING_INDIRECT_DESC
    | VIRTIO_F_RING_EVENT_IDX
    | VIRTIO_F_VERSION_1;
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;
const VIRTIO_BLK_S_NOT_READY: u8 = 3;
// The virtio-drivers block queue currently exposes one request at a time to
// this adapter. Keep requests large enough to amortize one IRQ wake per I/O.
const MAX_TRANSFER_SIZE: usize = 4 * 1024 * 1024;

type RawVirtioQueue = VirtQueue<VirtIoHalImpl, VIRTIO_BLOCK_QUEUE_SIZE>;

crate::model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

fn probe_fdt(probe: rdrive::register::ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let binding = binding_info_from_fdt(&info)?;
    let dma = virtio_block_dma_info(crate::binding_resolver::dma_coherency_from_fdt(&info));
    let (device_type, transport) = virtio::probe_fdt_mmio_device(&info)?;
    if device_type != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }

    // The IRQ endpoint owns a second mapping of the fixed VirtIO MMIO
    // interrupt registers. Queue notification and IRQ acknowledgement then
    // have separate mutation owners and never share a task/IRQ lock.
    let (irq_device_type, irq_transport) = virtio::probe_fdt_mmio_device(&info)?;
    if irq_device_type != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    register_mmio_transport_with_info(platform, transport, irq_transport, binding, dma)
}

pub(super) fn register_mmio_transport(
    platform: PlatformDevice,
    transport: MmioTransport<'static>,
    irq_transport: MmioTransport<'static>,
) -> Result<(), OnProbeError> {
    register_mmio_transport_with_info(
        platform,
        transport,
        irq_transport,
        BindingInfo::empty(),
        virtio_block_dma_info(DmaCoherency::NonCoherent),
    )
}

fn register_mmio_transport_with_info(
    platform: PlatformDevice,
    transport: MmioTransport<'static>,
    irq_transport: MmioTransport<'static>,
    binding: BindingInfo,
    dma: DmaDeviceInfo,
) -> Result<(), OnProbeError> {
    let controller =
        VirtioBlockController::new(transport, irq_transport, dma).map_err(|error| {
            OnProbeError::other(format!("failed to initialize virtio-blk: {error:?}"))
        })?;
    platform.register_block_with_info(controller, binding);
    log::info!("registered IRQ-driven VirtIO MMIO block device");
    Ok(())
}

struct VirtioBlockController {
    shared: Arc<VirtioBlockShared>,
    info: DeviceInfo,
    dma: DmaDeviceInfo,
    supports_flush: bool,
    irq_handler: Option<Box<dyn HardIrqHandler>>,
    irq_enabled: Arc<AtomicBool>,
    started: bool,
    stopped: bool,
}

impl VirtioBlockController {
    fn new(
        transport: MmioTransport<'static>,
        irq_transport: MmioTransport<'static>,
        dma: DmaDeviceInfo,
    ) -> Result<Self, VirtIoError> {
        let mut raw = RawVirtioBlock::new(transport)?;
        raw.disable_interrupts();
        let supports_flush = raw.supports_flush();
        let info = DeviceInfo {
            read_only: raw.readonly(),
            name: Some(DEVICE_NAME),
            model: Some("virtio-blk-mmio"),
            ..DeviceInfo::new(raw.capacity(), SECTOR_SIZE)
        };
        let irq_enabled = Arc::new(AtomicBool::new(false));
        Ok(Self {
            shared: Arc::new(VirtioBlockShared::new(raw)),
            info,
            dma,
            supports_flush,
            irq_handler: Some(Box::new(VirtioMmioBlockIrq {
                transport: irq_transport,
                enabled: Arc::clone(&irq_enabled),
            })),
            irq_enabled,
            started: false,
            stopped: false,
        })
    }

    fn start(&mut self, target_queues: usize) -> Result<ControllerUpdate, BlkError> {
        if self.started || self.stopped || target_queues == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let handler = self.irq_handler.take().ok_or(BlkError::InvalidRequest)?;
        self.shared.enable_interrupts()?;
        self.irq_enabled.store(true, Ordering::Release);
        self.started = true;

        let queue: Box<dyn HardwareQueue> = Box::new(VirtioBlockQueue {
            shared: Arc::clone(&self.shared),
            info: self.info,
            dma: self.dma,
            supports_flush: self.supports_flush,
        });
        let endpoint = IrqEndpoint::new(IRQ_SOURCE_ID, 1 << QUEUE_ID, handler);
        Ok(
            ControllerUpdate::with_resources(ControllerState::Ready, vec![queue], vec![endpoint])
                .with_device_info(self.info),
        )
    }

    fn quiesce_interrupts(&mut self) -> Result<ControllerUpdate, BlkError> {
        if self.stopped {
            return Ok(ControllerUpdate::state(ControllerState::Shutdown));
        }
        if self.started {
            self.shared.disable_interrupts()?;
        }
        self.irq_enabled.store(false, Ordering::Release);
        Ok(ControllerUpdate::state(if self.started {
            ControllerState::Ready
        } else {
            ControllerState::WaitingForIrq
        }))
    }

    fn shutdown(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.stopped {
            self.irq_enabled.store(false, Ordering::Release);
            self.shared.shutdown()?;
            self.stopped = true;
        }
        Ok(ControllerUpdate::state(ControllerState::Shutdown))
    }
}

impl DriverGeneric for VirtioBlockController {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl BlockController for VirtioBlockController {
    fn device_info(&self) -> DeviceInfo {
        self.info
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } => self.start(target_queues),
            ControllerEvent::OnlineSmp { target_queues } => {
                if !self.started || self.stopped || target_queues == 0 {
                    return Err(BlkError::InvalidRequest);
                }
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Irq(control) => {
                if control.source_id() != IRQ_SOURCE_ID || !self.started || self.stopped {
                    return Err(BlkError::InvalidRequest);
                }
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Rearm { source_id } => {
                if source_id != IRQ_SOURCE_ID || !self.started || self.stopped {
                    return Err(BlkError::InvalidRequest);
                }
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::RegisterRetry => Err(BlkError::InvalidRequest),
            ControllerEvent::QuiesceIrqs => self.quiesce_interrupts(),
            ControllerEvent::Watchdog { .. } | ControllerEvent::Shutdown => self.shutdown(),
        }
    }
}

struct VirtioBlockShared {
    state: UnsafeCell<VirtioBlockState>,
    task_access_active: AtomicBool,
}

// SAFETY: every task-context access to the transport, queue, and in-flight
// request is serialized by `task_access_active`. Local IRQs and preemption are
// disabled while that gate is held. The hard IRQ endpoint owns a separate MMIO
// transport and never dereferences this state.
unsafe impl Send for VirtioBlockShared {}
// SAFETY: see the `Send` contract above; shared references expose mutation only
// through `with_task`, which enforces the same exclusion protocol.
unsafe impl Sync for VirtioBlockShared {}

impl VirtioBlockShared {
    fn new(raw: RawVirtioBlock) -> Self {
        Self {
            state: UnsafeCell::new(VirtioBlockState {
                raw: Some(raw),
                inflight: None,
                next_request_id: 1,
            }),
            task_access_active: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, operation: impl FnOnce(&mut VirtioBlockState) -> R) -> R {
        let _irq_guard = PreemptIrqSaveGuard::new();
        let _access = TaskAccessGuard::enter(&self.task_access_active);
        // SAFETY: `TaskAccessGuard` provides exclusive task-context access and
        // the IRQ endpoint uses a distinct transport mapping.
        operation(unsafe { &mut *self.state.get() })
    }

    fn enable_interrupts(&self) -> Result<(), BlkError> {
        self.with_task(|state| {
            let raw = state.raw.as_mut().ok_or(BlkError::Io)?;
            raw.enable_interrupts();
            Ok(())
        })
    }

    fn disable_interrupts(&self) -> Result<(), BlkError> {
        self.with_task(|state| {
            let raw = state.raw.as_mut().ok_or(BlkError::Io)?;
            raw.disable_interrupts();
            let _ = raw.ack_interrupt();
            Ok(())
        })
    }

    fn shutdown(&self) -> Result<(), BlkError> {
        self.with_task(|state| {
            let Some(mut raw) = state.raw.take() else {
                return Ok(());
            };
            raw.disable_interrupts();
            let _ = raw.ack_interrupt();
            // Dropping VirtIOBlk unsets queue zero and waits for the transport
            // to confirm it, terminating DMA before queue shutdown returns
            // request ownership to the runtime.
            drop(raw);
            Ok(())
        })
    }
}

struct TaskAccessGuard<'a>(&'a AtomicBool);

impl<'a> TaskAccessGuard<'a> {
    fn enter(active: &'a AtomicBool) -> Self {
        while active
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self(active)
    }
}

impl Drop for TaskAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct VirtioBlockState {
    // Keep the transport before descriptor backing so its Drop implementation
    // unsets the queue before any fallback field teardown can release pointers.
    raw: Option<RawVirtioBlock>,
    inflight: Option<InflightRequest>,
    next_request_id: usize,
}

impl VirtioBlockState {
    fn submit_one(
        &mut self,
        info: QueueInfo,
        mut request: OwnedRequest,
    ) -> Result<RequestId, SubmitError> {
        if let Err(error) = validate_owned_request(info, &request) {
            return Err(SubmitError::new(error, request));
        }
        if self.inflight.is_some() {
            return Err(SubmitError::new(BlkError::Retry, request));
        }

        let op = match request.op {
            RequestOp::Read => InflightOp::Read,
            RequestOp::Write => InflightOp::Write,
            RequestOp::Flush => InflightOp::Flush,
        };
        let mut data = request.data.take();
        match (op, data.as_ref()) {
            (InflightOp::Read | InflightOp::Write, Some(data))
                if dma_direction_matches(op, data.direction()) => {}
            (InflightOp::Flush, None) => {}
            _ => {
                request.data = data;
                return Err(SubmitError::new(BlkError::InvalidRequest, request));
            }
        }

        let mut storage = Box::<InflightStorage>::default();
        let lba = match op {
            InflightOp::Read | InflightOp::Write => match usize::try_from(request.lba) {
                Ok(lba) => lba,
                Err(_) => {
                    request.data = data;
                    return Err(SubmitError::new(
                        BlkError::InvalidBlockIndex(request.lba),
                        request,
                    ));
                }
            },
            InflightOp::Flush => 0,
        };
        let Some(raw) = self.raw.as_mut() else {
            request.data = data;
            return Err(SubmitError::new(BlkError::Io, request));
        };
        let token = match op {
            InflightOp::Read => {
                let Some(data) = data.as_mut() else {
                    request.data = data;
                    return Err(SubmitError::new(BlkError::InvalidRequest, request));
                };
                // SAFETY: the prepared DMA allocation, request, and response
                // storage remain owned by `InflightRequest` until the used-ring
                // entry is consumed or the queue is reset during shutdown.
                unsafe {
                    raw.read_blocks_nb(
                        lba,
                        &mut storage.request,
                        core::slice::from_raw_parts_mut(data.cpu_ptr().as_ptr(), data.len().get()),
                        &mut storage.response,
                    )
                }
            }
            InflightOp::Write => {
                let Some(data) = data.as_ref() else {
                    request.data = data;
                    return Err(SubmitError::new(BlkError::InvalidRequest, request));
                };
                // SAFETY: identical lifetime contract to the read path; the
                // device receives only immutable access to the write payload.
                unsafe {
                    raw.write_blocks_nb(
                        lba,
                        &mut storage.request,
                        core::slice::from_raw_parts(data.cpu_ptr().as_ptr(), data.len().get()),
                        &mut storage.response,
                    )
                }
            }
            InflightOp::Flush => {
                // SAFETY: request and response storage remain owned by the
                // in-flight request until completion or queue reset.
                unsafe { raw.flush_nb(&mut storage.request, &mut storage.response) }
            }
        };
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                request.data = data;
                return Err(SubmitError::new(map_virtio_error(error), request));
            }
        };

        let id = RequestId::new(self.next_request_id);
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        // SAFETY: any data request has been published to VirtIO and the
        // resulting in-flight owner is retained until a terminal used entry or
        // queue reset proves that the device can no longer access the buffer.
        let dma = data.map(|data| unsafe { data.into_in_flight() });
        self.inflight = Some(InflightRequest {
            id,
            token,
            op,
            storage,
            dma,
        });
        Ok(id)
    }

    fn complete_after_irq(&mut self) -> Result<Option<CompletedRequest>, BlkError> {
        let raw = self.raw.as_mut().ok_or(BlkError::Io)?;
        let Some(inflight) = self.inflight.as_ref() else {
            return Ok(None);
        };
        let Some(used_token) = raw.peek_used() else {
            return Ok(None);
        };
        if used_token != inflight.token {
            return Err(BlkError::Io);
        }

        let mut inflight = self.inflight.take().ok_or(BlkError::Io)?;
        let completion = match (inflight.op, inflight.dma.as_mut()) {
            (InflightOp::Read, Some(dma)) => {
                // SAFETY: the used token matches this exact descriptor chain
                // and all three buffers retain their submitted addresses.
                unsafe {
                    raw.complete_read_blocks(
                        inflight.token,
                        &inflight.storage.request,
                        core::slice::from_raw_parts_mut(dma.cpu_ptr().as_ptr(), dma.len().get()),
                        &mut inflight.storage.response,
                    )
                }
            }
            (InflightOp::Write, Some(dma)) => {
                // SAFETY: the matching used token terminates device access to
                // the immutable write payload and descriptor metadata.
                unsafe {
                    raw.complete_write_blocks(
                        inflight.token,
                        &inflight.storage.request,
                        core::slice::from_raw_parts(dma.cpu_ptr().as_ptr(), dma.len().get()),
                        &mut inflight.storage.response,
                    )
                }
            }
            (InflightOp::Flush, None) => {
                // SAFETY: the used token matches the request/response-only
                // descriptor chain retained by this in-flight request.
                unsafe {
                    raw.complete_flush(
                        inflight.token,
                        &inflight.storage.request,
                        &mut inflight.storage.response,
                    )
                }
            }
            _ => Err(VirtIoError::InvalidParam),
        };

        let dma_is_terminal = matches!(
            &completion,
            Ok(()) | Err(VirtIoError::IoError | VirtIoError::Unsupported | VirtIoError::NotReady)
        );
        let result = completion.map_err(map_virtio_completion_error);
        let data = if dma_is_terminal {
            // SAFETY: pop_used consumed the matching used-ring entry before
            // returning success or a device-reported terminal status.
            inflight
                .dma
                .take()
                .map(|dma| unsafe { dma.complete_after_quiesce() })
        } else {
            // An unexpected transport invariant failure may leave descriptors
            // reachable. Quarantine both DMA and metadata rather than making
            // either allocation reusable.
            if let Some(dma) = inflight.dma.take() {
                let _quarantined = dma.quarantine();
            }
            core::mem::forget(inflight.storage);
            None
        };
        Ok(Some(CompletedRequest::new(inflight.id, result, data)))
    }

    fn fail_after_shutdown(&mut self) -> Result<Option<CompletedRequest>, BlkError> {
        if self.raw.is_some() {
            return Err(BlkError::Io);
        }
        let Some(inflight) = self.inflight.take() else {
            return Ok(None);
        };
        // SAFETY: controller shutdown dropped RawVirtioBlock only after
        // queue_unset confirmed that the device can no longer access backing.
        let data = inflight
            .dma
            .map(|dma| unsafe { dma.complete_after_quiesce() });
        Ok(Some(CompletedRequest::new(
            inflight.id,
            Err(BlkError::Io),
            data,
        )))
    }
}

struct VirtioBlockQueue {
    shared: Arc<VirtioBlockShared>,
    info: DeviceInfo,
    dma: DmaDeviceInfo,
    supports_flush: bool,
}

impl VirtioBlockQueue {
    fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: QUEUE_ID,
            device: self.info,
            limits: virtio_block_limits(self.dma, self.supports_flush),
        }
    }
}

impl HardwareQueue for VirtioBlockQueue {
    fn id(&self) -> usize {
        QUEUE_ID
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let Some(request) = requests.pop_front() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        };
        match self
            .shared
            .with_task(|state| state.submit_one(self.queue_info(), request))
        {
            Ok(id) => {
                sink.accepted(id);
                BatchSubmitResult::new(1, BatchSubmitDisposition::Continue)
            }
            Err(error) => {
                let disposition = if error.error == BlkError::Retry {
                    BatchSubmitDisposition::QueueFull
                } else {
                    BatchSubmitDisposition::Fatal(error.error)
                };
                requests.push_front(error.into_request());
                BatchSubmitResult::new(0, disposition)
            }
        }
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        // virtio-drivers publishes and conditionally notifies from the
        // nonblocking submit primitive; this depth-one adapter has no separate
        // doorbell operation to batch.
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let completion = self
            .shared
            .with_task(VirtioBlockState::complete_after_irq)?;
        if let Some(completion) = completion {
            sink.complete(completion);
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let completion = self
            .shared
            .with_task(VirtioBlockState::fail_after_shutdown)?;
        if let Some(completion) = completion {
            sink.complete(completion);
        }
        Ok(())
    }
}

struct InflightRequest {
    id: RequestId,
    token: u16,
    op: InflightOp,
    storage: Box<InflightStorage>,
    dma: Option<InFlightDma>,
}

#[derive(Clone, Copy)]
enum InflightOp {
    Read,
    Write,
    Flush,
}

#[derive(Default)]
struct InflightStorage {
    request: VirtioBlockRequest,
    response: VirtioBlockResponse,
}

struct RawVirtioBlock {
    transport: MmioTransport<'static>,
    queue: RawVirtioQueue,
    capacity: u64,
    negotiated_features: u64,
}

impl RawVirtioBlock {
    fn new(mut transport: MmioTransport<'static>) -> Result<Self, VirtIoError> {
        let negotiated_features = negotiate_block_features(&mut transport);
        let capacity = transport.read_consistent(|| {
            let low = transport.read_config_space::<u32>(0)?;
            let high = transport.read_config_space::<u32>(core::mem::size_of::<u32>())?;
            Ok(u64::from(low) | (u64::from(high) << 32))
        })?;
        log::info!("found a block device of size {}KB", capacity / 2);

        let queue = VirtQueue::new(
            &mut transport,
            QUEUE_ID as u16,
            negotiated_features & VIRTIO_F_RING_INDIRECT_DESC != 0,
            negotiated_features & VIRTIO_F_RING_EVENT_IDX != 0,
        )?;
        transport.finish_init();
        Ok(Self {
            transport,
            queue,
            capacity,
            negotiated_features,
        })
    }

    const fn capacity(&self) -> u64 {
        self.capacity
    }

    const fn readonly(&self) -> bool {
        self.negotiated_features & VIRTIO_BLK_F_RO != 0
    }

    const fn supports_flush(&self) -> bool {
        self.negotiated_features & VIRTIO_BLK_F_FLUSH != 0
    }

    fn enable_interrupts(&mut self) {
        self.queue.set_dev_notify(true);
    }

    fn disable_interrupts(&mut self) {
        self.queue.set_dev_notify(false);
    }

    fn ack_interrupt(&mut self) -> InterruptStatus {
        self.transport.ack_interrupt()
    }

    fn peek_used(&self) -> Option<u16> {
        self.queue.peek_used()
    }

    /// Submits one read while retaining the caller-owned buffers in flight.
    ///
    /// # Safety
    ///
    /// `request`, `data`, and `response` must remain at the same addresses and
    /// must not be accessed until the matching completion or queue reset.
    unsafe fn read_blocks_nb(
        &mut self,
        lba: usize,
        request: &mut VirtioBlockRequest,
        data: &mut [u8],
        response: &mut VirtioBlockResponse,
    ) -> Result<u16, VirtIoError> {
        if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
            return Err(VirtIoError::InvalidParam);
        }
        request.prepare(VIRTIO_BLK_T_IN, lba as u64);
        response.prepare();
        // SAFETY: the caller retains every descriptor buffer until completion.
        let token = unsafe {
            self.queue
                .add(&[request.as_bytes()], &mut [data, response.as_bytes_mut()])?
        };
        self.notify_queue_if_needed();
        Ok(token)
    }

    /// Submits one write while retaining the caller-owned buffers in flight.
    ///
    /// # Safety
    ///
    /// `request`, `data`, and `response` must remain at the same addresses and
    /// must not be accessed until the matching completion or queue reset.
    unsafe fn write_blocks_nb(
        &mut self,
        lba: usize,
        request: &mut VirtioBlockRequest,
        data: &[u8],
        response: &mut VirtioBlockResponse,
    ) -> Result<u16, VirtIoError> {
        if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
            return Err(VirtIoError::InvalidParam);
        }
        request.prepare(VIRTIO_BLK_T_OUT, lba as u64);
        response.prepare();
        // SAFETY: the caller retains every descriptor buffer until completion.
        let token = unsafe {
            self.queue
                .add(&[request.as_bytes(), data], &mut [response.as_bytes_mut()])?
        };
        self.notify_queue_if_needed();
        Ok(token)
    }

    /// Submits one cache flush without polling for its completion.
    ///
    /// # Safety
    ///
    /// `request` and `response` must remain at the same addresses and must not
    /// be accessed until the matching completion or queue reset.
    unsafe fn flush_nb(
        &mut self,
        request: &mut VirtioBlockRequest,
        response: &mut VirtioBlockResponse,
    ) -> Result<u16, VirtIoError> {
        if !self.supports_flush() {
            return Err(VirtIoError::Unsupported);
        }
        request.prepare(VIRTIO_BLK_T_FLUSH, 0);
        response.prepare();
        // SAFETY: the caller retains both descriptor buffers until completion.
        let token = unsafe {
            self.queue
                .add(&[request.as_bytes()], &mut [response.as_bytes_mut()])?
        };
        self.notify_queue_if_needed();
        Ok(token)
    }

    /// Reclaims the buffers used by the matching read request.
    ///
    /// # Safety
    ///
    /// All buffers must be the exact buffers passed to [`Self::read_blocks_nb`]
    /// for `token`, and the token must be next in the used ring.
    unsafe fn complete_read_blocks(
        &mut self,
        token: u16,
        request: &VirtioBlockRequest,
        data: &mut [u8],
        response: &mut VirtioBlockResponse,
    ) -> Result<(), VirtIoError> {
        // SAFETY: the caller provides the exact buffers submitted for `token`.
        unsafe {
            self.queue.pop_used(
                token,
                &[request.as_bytes()],
                &mut [data, response.as_bytes_mut()],
            )?;
        }
        response.result()
    }

    /// Reclaims the buffers used by the matching write request.
    ///
    /// # Safety
    ///
    /// All buffers must be the exact buffers passed to
    /// [`Self::write_blocks_nb`] for `token`, and the token must be next in the
    /// used ring.
    unsafe fn complete_write_blocks(
        &mut self,
        token: u16,
        request: &VirtioBlockRequest,
        data: &[u8],
        response: &mut VirtioBlockResponse,
    ) -> Result<(), VirtIoError> {
        // SAFETY: the caller provides the exact buffers submitted for `token`.
        unsafe {
            self.queue.pop_used(
                token,
                &[request.as_bytes(), data],
                &mut [response.as_bytes_mut()],
            )?;
        }
        response.result()
    }

    /// Reclaims the buffers used by the matching flush request.
    ///
    /// # Safety
    ///
    /// Both buffers must be the exact buffers passed to [`Self::flush_nb`] for
    /// `token`, and the token must be next in the used ring.
    unsafe fn complete_flush(
        &mut self,
        token: u16,
        request: &VirtioBlockRequest,
        response: &mut VirtioBlockResponse,
    ) -> Result<(), VirtIoError> {
        // SAFETY: the caller provides the exact buffers submitted for `token`.
        unsafe {
            self.queue
                .pop_used(token, &[request.as_bytes()], &mut [response.as_bytes_mut()])?;
        }
        response.result()
    }

    fn notify_queue_if_needed(&mut self) {
        if self.queue.should_notify() {
            self.transport.notify(QUEUE_ID as u16);
        }
    }
}

impl Drop for RawVirtioBlock {
    fn drop(&mut self) {
        self.transport.queue_unset(QUEUE_ID as u16);
    }
}

#[repr(align(8))]
#[derive(Default)]
struct VirtioBlockRequest([u8; 16]);

impl VirtioBlockRequest {
    fn prepare(&mut self, request_type: u32, sector: u64) {
        self.0[..4].copy_from_slice(&request_type.to_le_bytes());
        self.0[4..8].fill(0);
        self.0[8..].copy_from_slice(&sector.to_le_bytes());
    }

    const fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

struct VirtioBlockResponse([u8; 1]);

impl VirtioBlockResponse {
    fn prepare(&mut self) {
        self.0[0] = VIRTIO_BLK_S_NOT_READY;
    }

    const fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }

    const fn result(&self) -> Result<(), VirtIoError> {
        match self.0[0] {
            VIRTIO_BLK_S_OK => Ok(()),
            VIRTIO_BLK_S_IOERR => Err(VirtIoError::IoError),
            VIRTIO_BLK_S_UNSUPP => Err(VirtIoError::Unsupported),
            VIRTIO_BLK_S_NOT_READY => Err(VirtIoError::NotReady),
            _ => Err(VirtIoError::IoError),
        }
    }
}

impl Default for VirtioBlockResponse {
    fn default() -> Self {
        Self([VIRTIO_BLK_S_NOT_READY])
    }
}

fn negotiate_block_features(transport: &mut MmioTransport<'static>) -> u64 {
    transport.set_status(DeviceStatus::empty());
    transport.set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);
    let negotiated = transport.read_device_features() & VIRTIO_BLOCK_SUPPORTED_FEATURES;
    transport.write_driver_features(negotiated);
    transport
        .set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK);
    transport.set_guest_page_size(PAGE_SIZE as u32);
    negotiated
}

struct VirtioMmioBlockIrq {
    transport: MmioTransport<'static>,
    enabled: Arc<AtomicBool>,
}

// SAFETY: the mapping has static lifetime and this endpoint is moved into one
// non-reentrant IRQ registration. It accesses only VirtIO interrupt status and
// acknowledgement registers; queue registers belong to the task endpoint.
unsafe impl Send for VirtioMmioBlockIrq {}

impl HardIrqHandler for VirtioMmioBlockIrq {
    fn ack(&mut self) -> IrqAck {
        if !self.enabled.load(Ordering::Acquire) {
            return IrqAck::spurious(IRQ_SOURCE_ID);
        }
        irq_ack_from_status(self.transport.ack_interrupt())
    }
}

const fn irq_ack_from_status(status: InterruptStatus) -> IrqAck {
    if status.is_empty() {
        return IrqAck::spurious(IRQ_SOURCE_ID);
    }
    let queues = if status.contains(InterruptStatus::QUEUE_INTERRUPT) {
        IrqQueueMask::from_queue(QUEUE_ID)
    } else {
        IrqQueueMask::none()
    };
    IrqAck::cleared(queues, ControlEvent::new(IRQ_SOURCE_ID, 0))
}

const fn virtio_block_dma_info(coherency: DmaCoherency) -> DmaDeviceInfo {
    DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        coherency,
        DmaConstraints::new(u64::MAX)
            .with_align(PAGE_SIZE)
            .with_max_segment_size(MAX_TRANSFER_SIZE),
    )
}

const fn virtio_block_limits(dma: DmaDeviceInfo, supports_flush: bool) -> QueueLimits {
    QueueLimits {
        dma,
        dma_length_alignment: SECTOR_SIZE,
        max_inflight: 1,
        max_submit_batch: 1,
        max_blocks_per_request: (MAX_TRANSFER_SIZE / SECTOR_SIZE) as u32,
        max_segments: 1,
        supported_flags: RequestFlags::NONE,
        supports_flush,
    }
}

const fn dma_direction_matches(op: InflightOp, direction: DmaDirection) -> bool {
    matches!(
        (op, direction),
        (
            InflightOp::Read,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) | (
            InflightOp::Write,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        )
    )
}

const fn map_virtio_error(error: VirtIoError) -> BlkError {
    match error {
        VirtIoError::QueueFull | VirtIoError::NotReady => BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => BlkError::Other("invalid VirtIO block state"),
        VirtIoError::AlreadyUsed => BlkError::Other("VirtIO block resource already used"),
        VirtIoError::InvalidParam => BlkError::InvalidRequest,
        VirtIoError::DmaError => BlkError::NoMemory,
        VirtIoError::IoError => BlkError::Io,
        VirtIoError::Unsupported => BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => BlkError::Other("unexpected VirtIO socket error"),
    }
}

const fn map_virtio_completion_error(error: VirtIoError) -> BlkError {
    match map_virtio_error(error) {
        BlkError::Retry => BlkError::Io,
        error => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_interrupt_activates_only_the_block_queue() {
        let queue = irq_ack_from_status(InterruptStatus::QUEUE_INTERRUPT);
        assert!(queue.queues().contains(QUEUE_ID));
        assert!(!queue.is_spurious());

        let config = irq_ack_from_status(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT);
        assert!(config.queues().is_empty());
        assert!(!config.is_spurious());
        assert!(irq_ack_from_status(InterruptStatus::empty()).is_spurious());
    }

    #[test]
    fn queue_limits_preserve_owned_dma_single_queue_contract() {
        let dma = virtio_block_dma_info(dma_api::DmaCoherency::Coherent);
        let limits = virtio_block_limits(dma, false);
        let constraints = limits.dma.constraints();

        assert_eq!(limits.dma.domain(), dma_api::DmaDomainId::Direct);
        assert_eq!(limits.dma.coherency(), dma_api::DmaCoherency::Coherent);
        assert_eq!(constraints.addr_mask, u64::MAX);
        assert_eq!(constraints.align, 0x1000);
        assert_eq!(constraints.boundary, None);
        assert_eq!(constraints.max_segment_size, Some(MAX_TRANSFER_SIZE));
        assert_eq!(limits.max_inflight, 1);
        assert_eq!(limits.max_submit_batch, 1);
        assert_eq!(limits.max_segments, 1);
        assert_eq!(limits.dma_length_alignment, SECTOR_SIZE);
        assert!(!limits.supports_flush);
    }

    #[test]
    fn negotiated_flush_is_exposed_to_the_block_runtime() {
        const DMA: dma_api::DmaDeviceInfo =
            virtio_block_dma_info(dma_api::DmaCoherency::NonCoherent);
        const LIMITS: QueueLimits = virtio_block_limits(DMA, true);
        const _: () = assert!(LIMITS.supports_flush);

        assert!(LIMITS.supports_flush);
    }

    #[test]
    fn request_direction_must_match_device_dma_ownership() {
        assert!(dma_direction_matches(
            InflightOp::Read,
            DmaDirection::FromDevice
        ));
        assert!(dma_direction_matches(
            InflightOp::Write,
            DmaDirection::ToDevice
        ));
        assert!(!dma_direction_matches(
            InflightOp::Read,
            DmaDirection::ToDevice
        ));
        assert!(!dma_direction_matches(
            InflightOp::Write,
            DmaDirection::FromDevice
        ));
    }
}
