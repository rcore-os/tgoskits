//! VirtIO block device controller for the owned-DMA, interrupt-driven block
//! runtime.
//!
//! One virtio-mmio block device is exposed as one [`BlockController`] with a
//! single hardware queue and one IRQ source. The transport and virtqueue live
//! in [`VirtioBlkShared`]: task-side queue operations run with local IRQs and
//! preemption disabled, while the hard-IRQ endpoint only acknowledges the
//! transport and latches a deferred acknowledgement when the task side
//! currently owns the device. Interrupts synchronize state; tasks advance
//! flow.

use alloc::{boxed::Box, format, sync::Arc, vec, vec::Vec};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    slice,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::PreemptIrqSaveGuard;
use dma_api::{CoherentArray, InFlightDma};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, BlockController, CompletedRequest,
    CompletionSink, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo, DriverGeneric,
    HardIrqHandler, HardwareQueue, IrqAck, IrqEndpoint, IrqQueueMask, OwnedRequest,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestFlags, RequestId, RequestOp, SubmissionSink,
    SubmitError,
};
use rdrive::probe::{OnProbeError, fdt::ProbeFdt};
use virtio_drivers::transport::{Transport, mmio::MmioTransport};

use crate::{block::ProbeFdtBlock, virtio::VirtIoHalImpl};

/// Descriptor count of the request virtqueue. Must be a power of two and no
/// larger than the queue size any supported device offers (QEMU's
/// `virtio-blk-device` offers 128).
const QUEUE_DESCRIPTORS: usize = 64;
/// Descriptors occupied by one request: header, data, response.
const DESCRIPTORS_PER_REQUEST: usize = 3;
/// Requests that can be in flight before the virtqueue is full.
const MAX_INFLIGHT: usize = QUEUE_DESCRIPTORS / DESCRIPTORS_PER_REQUEST;
/// The virtio-blk logical sector size is fixed by the specification.
const LOGICAL_BLOCK_SIZE: usize = 512;
/// Cap for one data segment; keeps descriptor lengths well below u32 limits.
const MAX_SEGMENT_SIZE: usize = 1024 * 1024;
/// The single request virtqueue of a non-mq virtio-blk device.
const QUEUE_INDEX: u16 = 0;
/// The controller-local IRQ source shared by the queue.
const IRQ_SOURCE_ID: usize = 0;
/// Bytes of one virtio-blk request header.
const HEADER_BYTES: usize = 16;

bitflags::bitflags! {
    /// Driver-side virtio-blk feature words offered during negotiation.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct DriverFeatures: u64 {
        /// Request the cache-flush command.
        const FLUSH = 1 << 9;
        /// Suppress used-ring interrupts unless the driver must act.
        const RING_EVENT_IDX = 1 << 29;
    }
}

bitflags::bitflags! {
    /// Device-side virtio-blk feature bits inspected before negotiation.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct DeviceFeatureBits: u64 {
        /// The backing device rejects writes.
        const READ_ONLY = 1 << 5;
    }
}

/// Virtio-blk request header as laid out by VIRTIO 1.1 5.2.6.
#[derive(Clone, Copy)]
struct BlkRequestHeader {
    kind: u32,
    reserved: u32,
    sector: u64,
}

impl BlkRequestHeader {
    const KIND_IN: u32 = 0;
    const KIND_OUT: u32 = 1;
    const KIND_FLUSH: u32 = 4;

    const fn read(sector: u64) -> Self {
        Self {
            kind: Self::KIND_IN,
            reserved: 0,
            sector,
        }
    }

    const fn write(sector: u64) -> Self {
        Self {
            kind: Self::KIND_OUT,
            reserved: 0,
            sector,
        }
    }

    const fn flush() -> Self {
        Self {
            kind: Self::KIND_FLUSH,
            reserved: 0,
            sector: 0,
        }
    }

    fn to_le_bytes(self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0u8; HEADER_BYTES];
        bytes[0..4].copy_from_slice(&self.kind.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.reserved.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.sector.to_le_bytes());
        bytes
    }
}

/// Status byte written by the device at the end of one request buffer.
const BLK_STATUS_OK: u8 = 0;

/// Transport and virtqueue shared between the task-side queue owner and the
/// hard-IRQ endpoint.
struct VirtioBlkInner<T: Transport> {
    transport: T,
    queue: virtio_drivers::queue::VirtQueue<VirtIoHalImpl, QUEUE_DESCRIPTORS>,
}

/// Cell that serializes task-side and IRQ-side access to one virtio device.
///
/// Task-side callers keep local IRQs and preemption disabled, so with
/// IRQ affinity on one CPU the hard IRQ can never interleave. The atomic
/// guard additionally covers SMP configurations; when the IRQ side loses the
/// race it only latches a deferred acknowledgement that the next task-side
/// access flushes.
struct VirtioBlkShared<T: Transport> {
    inner: UnsafeCell<VirtioBlkInner<T>>,
    access_active: AtomicBool,
    irq_ack_pending: AtomicBool,
}

// SAFETY: all mutable access to `inner` is serialized through `access_active`;
// task-side callers additionally hold `PreemptIrqSaveGuard`, and the hard IRQ
// path never blocks on the guard.
unsafe impl<T: Transport> Send for VirtioBlkShared<T> {}
// SAFETY: same serialization contract as `Send`.
unsafe impl<T: Transport> Sync for VirtioBlkShared<T> {}

struct SharedAccessGuard<'a>(&'a AtomicBool);

impl<'a> SharedAccessGuard<'a> {
    fn enter_task(active: &'a AtomicBool) -> Self {
        Self::enter(active)
    }

    fn try_enter_irq(active: &'a AtomicBool) -> Option<Self> {
        if active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(Self(active))
        } else {
            None
        }
    }

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

impl Drop for SharedAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<T: Transport> VirtioBlkShared<T> {
    fn new(inner: VirtioBlkInner<T>) -> Self {
        Self {
            inner: UnsafeCell::new(inner),
            access_active: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, f: impl FnOnce(&mut VirtioBlkInner<T>) -> R) -> R {
        let _irqs = PreemptIrqSaveGuard::new();
        let _active = SharedAccessGuard::enter_task(&self.access_active);
        // SAFETY: `access_active` serializes all mutable access to the shared
        // transport and queue; task-side callers also keep local IRQs and
        // preemption disabled across the guarded region.
        let inner = unsafe { &mut *self.inner.get() };
        self.flush_pending_irq_ack(inner);
        let result = f(inner);
        self.flush_pending_irq_ack(inner);
        result
    }

    fn try_with_irq<R>(&self, f: impl FnOnce(&mut VirtioBlkInner<T>) -> R) -> Option<R> {
        let _active = SharedAccessGuard::try_enter_irq(&self.access_active)?;
        // SAFETY: the guard is held; task-side callers cannot enter while it
        // is taken. IRQ context never waits for task-side access.
        Some(f(unsafe { &mut *self.inner.get() }))
    }

    fn flush_pending_irq_ack(&self, inner: &mut VirtioBlkInner<T>) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let _ = inner.transport.ack_interrupt();
        }
    }

    fn handle_irq(&self) -> IrqAck {
        let acked = self
            .try_with_irq(|inner| {
                self.irq_ack_pending.store(false, Ordering::Release);
                inner
                    .transport
                    .ack_interrupt()
                    .contains(virtio_drivers::transport::InterruptStatus::QUEUE_INTERRUPT)
            })
            .unwrap_or_else(|| {
                self.irq_ack_pending.store(true, Ordering::Release);
                false
            });

        let queues = IrqQueueMask::from_queue(0);
        if acked {
            IrqAck::cleared(queues, rdif_block::ControlEvent::new(IRQ_SOURCE_ID, 1))
        } else if self.irq_ack_pending.load(Ordering::Acquire) {
            IrqAck::masked_needs_rearm(queues, rdif_block::ControlEvent::new(IRQ_SOURCE_ID, 1))
        } else {
            IrqAck::spurious(IRQ_SOURCE_ID)
        }
    }
}

/// One in-flight request slot indexed by its virtqueue head descriptor.
struct BlkSlot {
    dma: Option<InFlightDma>,
    kind: RequestOp,
    header_index: usize,
    data_len: usize,
}

/// Hardware queue that owns every submitted request until completion.
pub struct VirtioBlkQueue<T: Transport> {
    shared: Arc<VirtioBlkShared<T>>,
    info: QueueInfo,
    /// Request state per virtqueue head descriptor; `None` when free.
    slots: Vec<Option<BlkSlot>>,
    /// Per-slot DMA-coherent request header and status byte buffers.
    headers: Vec<CoherentArray<u8>>,
    statuses: Vec<CoherentArray<u8>>,
    /// Head descriptor to header buffer mapping; `None` when free.
    header_of: Vec<Option<usize>>,
    /// Reverse of `header_of` for free-buffer search.
    header_used: Vec<bool>,
}

impl<T: Transport + 'static> VirtioBlkQueue<T> {
    fn new(shared: Arc<VirtioBlkShared<T>>, info: QueueInfo) -> Result<Self, OnProbeError> {
        // virtio-mmio on QEMU is a direct-mapped, coherent device.
        let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::Coherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ));
        let mut slots = Vec::with_capacity(QUEUE_DESCRIPTORS);
        slots.resize_with(QUEUE_DESCRIPTORS, || None);
        let mut headers = Vec::with_capacity(MAX_INFLIGHT);
        let mut statuses = Vec::with_capacity(MAX_INFLIGHT);
        for _ in 0..MAX_INFLIGHT {
            headers.push(
                dma.coherent_array_zero::<u8>(HEADER_BYTES)
                    .map_err(|error| {
                        OnProbeError::other(format!("virtio-blk header DMA: {error:?}"))
                    })?,
            );
            statuses.push(dma.coherent_array_zero::<u8>(1).map_err(|error| {
                OnProbeError::other(format!("virtio-blk status DMA: {error:?}"))
            })?);
        }
        let mut header_of = Vec::with_capacity(QUEUE_DESCRIPTORS);
        header_of.resize_with(QUEUE_DESCRIPTORS, || None);
        let mut header_used = Vec::with_capacity(MAX_INFLIGHT);
        header_used.resize(MAX_INFLIGHT, false);
        Ok(Self {
            shared,
            info,
            slots,
            headers,
            statuses,
            header_of,
            header_used,
        })
    }

    /// Stage one request on the virtqueue. Returns the head descriptor used
    /// as the request id, or the request back to the caller on failure.
    fn stage_request(&mut self, request: OwnedRequest) -> Result<u16, SubmitError> {
        let Some(free) = self.header_used.iter().position(|used| !*used) else {
            return Err(SubmitError::new(BlkError::Retry, request));
        };

        let data_len = request.data.as_ref().map_or(0, |data| data.len().get());
        if data_len > MAX_SEGMENT_SIZE {
            return Err(SubmitError::new(BlkError::InvalidRequest, request));
        }
        let header = match request.op {
            RequestOp::Flush => BlkRequestHeader::flush(),
            RequestOp::Read => BlkRequestHeader::read(request.lba),
            RequestOp::Write => BlkRequestHeader::write(request.lba),
        };
        let has_data = request.op == RequestOp::Read || request.op == RequestOp::Write;
        if has_data && data_len == 0 {
            return Err(SubmitError::new(BlkError::InvalidRequest, request));
        }

        self.headers[free].write_with_cpu(HEADER_BYTES, |bytes| {
            bytes.copy_from_slice(&header.to_le_bytes())
        });
        self.statuses[free].set_cpu(0, 0xff);

        let header_bytes = self.headers[free].as_slice_cpu();
        // SAFETY: this slot is free, so the descriptor chain referencing its
        // status byte has not been published yet and the device cannot access
        // the buffer concurrently.
        let status_bytes = unsafe { self.statuses[free].as_mut_slice_cpu() };

        let token = self.shared.with_task(|inner| {
            if !has_data {
                let mut outputs = [status_bytes];
                // SAFETY: every buffer stays owned by this queue (slot arrays
                // or the request's DMA backing) until the matching used-ring
                // entry is popped.
                unsafe { inner.queue.add(&[header_bytes], &mut outputs) }
            } else if request.op == RequestOp::Read {
                // SAFETY: the DMA backing stays owned by this queue from the
                // moment the descriptor chain is published until the matching
                // used-ring entry is popped.
                let data = unsafe {
                    data_slice(request.data.as_ref().expect("checked").cpu_ptr(), data_len)
                };
                let mut outputs = [data, status_bytes];
                unsafe { inner.queue.add(&[header_bytes], &mut outputs) }
            } else {
                // SAFETY: same ownership contract as the read path.
                let data = unsafe {
                    data_slice(request.data.as_ref().expect("checked").cpu_ptr(), data_len)
                };
                let inputs = [header_bytes, data];
                let mut outputs = [status_bytes];
                unsafe { inner.queue.add(&inputs, &mut outputs) }
            }
        });

        let token = match token {
            Ok(token) => token,
            Err(error) => return Err(SubmitError::new(map_stage_error(error), request)),
        };

        // SAFETY: the descriptor chain referencing the request's DMA backing
        // is published; hardware now owns the buffer until the used ring
        // hands it back.
        let dma = request
            .data
            .map(|prepared| unsafe { prepared.into_in_flight() });
        self.slots[usize::from(token)] = Some(BlkSlot {
            dma,
            kind: request.op,
            header_index: free,
            data_len,
        });
        self.header_of[usize::from(token)] = Some(free);
        self.header_used[free] = true;
        Ok(token)
    }

    /// Pops the next used-ring entry and completes its request.
    ///
    /// # Safety
    ///
    /// The caller must only pass the token returned by `peek_used`; its
    /// descriptor chain is complete and owned by this queue.
    unsafe fn complete_token(&mut self, token: u16, sink: &mut dyn CompletionSink) {
        let Some(slot) = self.slots[usize::from(token)].take() else {
            return;
        };
        let header_index = slot.header_index;

        let status = self.shared.with_task(|inner| {
            let header_bytes = self.headers[header_index].as_slice_cpu();
            // SAFETY: the descriptor chain completed, so the device no
            // longer accesses this slot's status byte.
            let status_bytes = unsafe { self.statuses[header_index].as_mut_slice_cpu() };
            let pop = match slot.kind {
                RequestOp::Flush => {
                    let mut outputs = [status_bytes];
                    // SAFETY: the buffers match the ones published by
                    // `stage_request` for this token and are still owned by
                    // this queue.
                    unsafe { inner.queue.pop_used(token, &[header_bytes], &mut outputs) }
                }
                RequestOp::Read => {
                    // SAFETY: same ownership contract as the flush path; the
                    // data pointer is only read for slice reconstruction and
                    // is not dereferenced when the slot carries no DMA.
                    let data = unsafe {
                        data_slice(
                            slot.dma
                                .as_ref()
                                .map_or(core::ptr::NonNull::dangling(), InFlightDma::cpu_ptr),
                            slot.data_len,
                        )
                    };
                    let mut outputs = [data, status_bytes];
                    unsafe { inner.queue.pop_used(token, &[header_bytes], &mut outputs) }
                }
                RequestOp::Write => {
                    // SAFETY: same ownership contract as the flush path.
                    let data = unsafe {
                        data_slice(
                            slot.dma
                                .as_ref()
                                .map_or(core::ptr::NonNull::dangling(), InFlightDma::cpu_ptr),
                            slot.data_len,
                        )
                    };
                    let inputs = [header_bytes, data];
                    let mut outputs = [status_bytes];
                    unsafe { inner.queue.pop_used(token, &inputs, &mut outputs) }
                }
            };
            match pop {
                Ok(_) => self.statuses[header_index].read_cpu(0),
                Err(error) => {
                    log::warn!("virtio-blk failed to pop used entry {token}: {error:?}");
                    None
                }
            }
        });

        self.header_of[usize::from(token)] = None;
        self.header_used[header_index] = false;
        let result = if status == Some(BLK_STATUS_OK) {
            Ok(())
        } else {
            log::warn!(
                "virtio-blk request {token} failed: status={:#04x}",
                status.unwrap_or(u8::MAX)
            );
            Err(BlkError::Io)
        };

        let dma = slot.dma.map(|dma| {
            // SAFETY: consuming the used-ring entry is the device's terminal
            // ownership handoff for this descriptor chain.
            unsafe { dma.complete_after_quiesce() }
        });
        sink.complete(CompletedRequest::new(
            RequestId::new(usize::from(token)),
            result,
            dma,
        ));
    }
}

/// Builds the data view of one request for descriptor publication.
///
/// # Safety
///
/// The caller must keep the request's DMA backing alive until the matching
/// used-ring entry is popped, and must not alias the returned slice with any
/// other access to the buffer. The unbounded lifetime is bounded in practice
/// by the queue's slot ownership rules.
unsafe fn data_slice(data: core::ptr::NonNull<u8>, len: usize) -> &'static mut [u8] {
    // SAFETY: contract of this helper; the buffer is DMA-coherent memory
    // owned by the request and outlives the descriptor chain.
    unsafe { slice::from_raw_parts_mut(data.as_ptr(), len) }
}

impl<T: Transport + 'static> HardwareQueue for VirtioBlkQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let mut accepted = 0;
        while accepted < self.info.limits.max_submit_batch && !requests.is_empty() {
            match self.stage_request(requests.pop_front().expect("batch is not empty")) {
                Ok(token) => {
                    sink.accepted(RequestId::new(usize::from(token)));
                    accepted += 1;
                }
                Err(failed) if failed.error == BlkError::Retry => {
                    requests.push_front(failed.into_request());
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::QueueFull);
                }
                Err(failed) => {
                    let error = failed.error;
                    requests.push_front(failed.into_request());
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::Fatal(error));
                }
            }
        }
        BatchSubmitResult::new(accepted, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.shared.with_task(|inner| {
            if inner.queue.should_notify() {
                inner.transport.notify(QUEUE_INDEX);
            }
        });
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        while let Some(token) = self.shared.with_task(|inner| inner.queue.peek_used()) {
            // SAFETY: the token comes from the used ring, so its descriptor
            // chain is complete and owned by this queue.
            unsafe { self.complete_token(token, sink) };
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for (token, slot) in self.slots.iter_mut().enumerate() {
            let Some(slot) = slot.take() else {
                continue;
            };
            if let Some(index) = self.header_of[token].take() {
                self.header_used[index] = false;
            }
            let dma = slot.dma.map(|dma| {
                // SAFETY: the runtime calls queue shutdown only after the
                // controller confirmed quiesce; the device no longer touches
                // the buffers.
                unsafe { dma.complete_after_quiesce() }
            });
            sink.complete(CompletedRequest::new(
                RequestId::new(token),
                Err(BlkError::Io),
                dma,
            ));
        }
        Ok(())
    }
}

/// Hard-IRQ endpoint: only acknowledges the transport and publishes the queue
/// mask. It never drains queues or touches DMA ownership.
struct VirtioBlkIrqEndpoint<T: Transport> {
    shared: Arc<VirtioBlkShared<T>>,
}

impl<T: Transport + 'static> HardIrqHandler for VirtioBlkIrqEndpoint<T> {
    fn ack(&mut self) -> IrqAck {
        self.shared.handle_irq()
    }
}

/// Controller facade owned by the block runtime.
pub struct VirtioBlkDevice<T: Transport> {
    shared: Arc<VirtioBlkShared<T>>,
    info: DeviceInfo,
    limits: QueueLimits,
}

impl<T: Transport + 'static> DriverGeneric for VirtioBlkDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> BlockController for VirtioBlkDevice<T> {
    fn device_info(&self) -> DeviceInfo {
        self.info
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => {
                self.shared
                    .with_task(|inner| inner.queue.set_dev_notify(true));
                let queue = VirtioBlkQueue::new(Arc::clone(&self.shared), self.queue_info())
                    .map_err(|_| BlkError::Other("virtio-blk queue resource allocation failed"))?;
                let endpoint = IrqEndpoint::new(
                    IRQ_SOURCE_ID,
                    IrqQueueMask::from_queue(0).bits(),
                    Box::new(VirtioBlkIrqEndpoint {
                        shared: Arc::clone(&self.shared),
                    }),
                );
                Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    vec![Box::new(queue)],
                    vec![endpoint],
                ))
            }
            ControllerEvent::RegisterRetry
            | ControllerEvent::OnlineSmp { .. }
            | ControllerEvent::Irq(_)
            | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Rearm { .. } => {
                self.shared.with_task(|_| {});
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::QuiesceIrqs => {
                self.shared
                    .with_task(|inner| inner.queue.set_dev_notify(false));
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown => Ok(ControllerUpdate::state(ControllerState::Shutdown)),
        }
    }
}

impl<T: Transport + 'static> VirtioBlkDevice<T> {
    fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: 0,
            device: self.info,
            limits: self.limits,
        }
    }
}

fn map_stage_error(error: virtio_drivers::Error) -> BlkError {
    log::warn!("virtio-blk failed to stage request: {error:?}");
    BlkError::Io
}

/// Static (non-FDT) registration is not wired to an IRQ binding yet; the
/// interrupt-driven block runtime requires FDT interrupt metadata.
pub fn register_static<T: Transport>(
    _plat_dev: rdrive::PlatformDevice,
    _transport: T,
) -> Result<(), OnProbeError> {
    Err(OnProbeError::Unsupported(
        "virtio-blk registration requires an FDT interrupt binding",
    ))
}

/// Negotiates the device, builds the request virtqueue, and registers the
/// controller with the block runtime.
pub fn register_fdt_transport(
    probe: ProbeFdt<'_>,
    transport: MmioTransport<'static>,
) -> Result<(), OnProbeError> {
    let mut transport = transport;
    let device_features = transport.read_device_features();
    let read_only = device_features & DeviceFeatureBits::READ_ONLY.bits() != 0;
    let negotiated = transport.begin_init(DriverFeatures::FLUSH | DriverFeatures::RING_EVENT_IDX);
    let supports_flush = negotiated.contains(DriverFeatures::FLUSH);
    let event_idx = negotiated.contains(DriverFeatures::RING_EVENT_IDX);

    let capacity_bytes = transport
        .read_config_space::<[u8; 8]>(0)
        .map_err(|_| OnProbeError::other("virtio-blk capacity config read failed"))?;
    let capacity = u64::from(u32::from_le_bytes([
        capacity_bytes[0],
        capacity_bytes[1],
        capacity_bytes[2],
        capacity_bytes[3],
    ])) | (u64::from(u32::from_le_bytes([
        capacity_bytes[4],
        capacity_bytes[5],
        capacity_bytes[6],
        capacity_bytes[7],
    ])) << 32);

    let queue =
        virtio_drivers::queue::VirtQueue::new(&mut transport, QUEUE_INDEX, false, event_idx)
            .map_err(|error| {
                OnProbeError::other(format!("virtio-blk queue setup failed: {error:?}"))
            })?;
    transport.finish_init();

    log::info!(
        "virtio-blk ready: capacity={capacity} sectors, read_only={read_only}, \
         flush={supports_flush}"
    );

    let shared = Arc::new(VirtioBlkShared::new(VirtioBlkInner { transport, queue }));
    let info = DeviceInfo {
        num_blocks: capacity,
        logical_block_size: LOGICAL_BLOCK_SIZE,
        read_only,
        name: Some("virtio-blk"),
        vendor: Some("virtio"),
        model: Some("blk"),
    };
    // Same DMA identity the queue used for its descriptor/header allocations:
    // direct-mapped, coherent, full bus width, with the virtio segment size
    // expressed through the DMA constraints.
    let constraints = dma_api::DmaConstraints {
        align: LOGICAL_BLOCK_SIZE,
        max_segment_size: Some(MAX_SEGMENT_SIZE),
        ..dma_api::DmaConstraints::new(u64::MAX)
    };
    let dma_info = dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        dma_api::DmaCoherency::Coherent,
        constraints,
    );
    let limits = QueueLimits {
        dma: dma_info,
        dma_length_alignment: LOGICAL_BLOCK_SIZE,
        max_inflight: MAX_INFLIGHT,
        max_submit_batch: MAX_INFLIGHT,
        max_blocks_per_request: (MAX_SEGMENT_SIZE / LOGICAL_BLOCK_SIZE) as u32,
        max_segments: 1,
        supported_flags: RequestFlags::NONE,
        supports_flush,
    };
    let controller = VirtioBlkDevice {
        shared,
        info,
        limits,
    };
    probe.register_block(controller).map_err(|error| {
        OnProbeError::other(format!("virtio-blk registration failed: {error:?}"))
    })?;
    Ok(())
}
