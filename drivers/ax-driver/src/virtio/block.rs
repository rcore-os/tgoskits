//! Interrupt-driven VirtIO MMIO block registration for the shared block runtime.

extern crate alloc;

use alloc::{boxed::Box, format, sync::Arc, vec};
use core::{
    mem::{self, ManuallyDrop},
    slice,
    time::Duration,
};

use ax_sync::RawSpinLock;
use bitflags::bitflags;
use dma_api::{
    DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection, DmaDomainId, InFlightDma,
};
use mmio_api::MmioRaw;
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, BlockController, CompletedRequest,
    CompletionSink, ControlEvent, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo,
    DriverGeneric, HardIrqHandler, HardwareQueue, IrqAck, IrqEndpoint, IrqQueueMask,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestId, RequestOp, SubmissionSink,
    validate_owned_request,
};
use rdrive::{PlatformDevice, probe::OnProbeError};
use virtio_drivers::{
    Error as VirtIoError,
    queue::VirtQueue,
    transport::{DeviceStatus, Transport},
};

use crate::{binding_info_from_fdt, block::PlatformDeviceBlock, virtio::VirtIoHalImpl};

const INTERRUPT_STATUS: usize = 0x60;
const INTERRUPT_ACK: usize = 0x64;
const QUEUE_INTERRUPT: u32 = 1;
const CONFIGURATION_CHANGE_INTERRUPT: u32 = 2;
const SECTOR_SIZE: usize = 512;
// An unwritten response must not be mistaken for a successful completion.
const RESPONSE_UNWRITTEN: u8 = 3;
const QUEUE_SIZE: usize = 16;
const RESET_RETRY_DELAY: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetState {
    Running,
    Resetting,
    Stopped,
}

// Only task-context control and queue endpoints acquire this lock. The hard
// IRQ endpoint owns separate interrupt registers and never takes this lock.
struct TransportControl<T: Transport> {
    transport: T,
    state: ResetState,
}

impl<T: Transport> TransportControl<T> {
    fn ensure_running(&self) -> Result<(), BlkError> {
        if self.state == ResetState::Running {
            Ok(())
        } else {
            Err(BlkError::Io)
        }
    }

    fn stop(&mut self) -> ControllerState {
        if self.state == ResetState::Running {
            // Serialize descriptor publication and notification against reset.
            self.state = ResetState::Resetting;
            self.transport.set_status(DeviceStatus::empty());
        }
        if self.state == ResetState::Resetting && self.transport.get_status().is_empty() {
            // VirtIO 1.2 section 2.4: status zero confirms that the device will
            // no longer notify or interact with queues until reinitialized.
            self.state = ResetState::Stopped;
        }
        if self.state == ResetState::Stopped {
            ControllerState::Shutdown
        } else {
            ControllerState::RegisterPending {
                retry_after: RESET_RETRY_DELAY,
            }
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct BlockFeatures: u64 {
        const READ_ONLY = 1 << 5;
        const FLUSH = 1 << 9;
        const VERSION_1 = 1 << 32;
    }
}

struct RawBlock<T: Transport> {
    control: Arc<RawSpinLock<TransportControl<T>>>,
    queue: ManuallyDrop<VirtQueue<VirtIoHalImpl, QUEUE_SIZE>>,
    device: DeviceInfo,
    flush_supported: bool,
}

impl<T: Transport> RawBlock<T> {
    fn new(mut transport: T) -> Result<Self, VirtIoError> {
        let features = transport
            .begin_init(BlockFeatures::READ_ONLY | BlockFeatures::FLUSH | BlockFeatures::VERSION_1);
        // Legacy MMIO has no FEATURES_OK handshake. Modern devices may reject
        // the proposed subset; do not configure queues or publish a controller
        // until the device has accepted it (VirtIO 1.2 section 3.1).
        if !transport.requires_legacy_layout() {
            let status = transport.get_status();
            if !status.contains(DeviceStatus::FEATURES_OK) {
                transport.set_status(status | DeviceStatus::FAILED);
                return Err(VirtIoError::Unsupported);
            }
        }
        let capacity = transport.read_consistent(|| {
            let low: u32 = transport.read_config_space(0)?;
            let high: u32 = transport.read_config_space(4)?;
            Ok(u64::from(low) | (u64::from(high) << 32))
        })?;
        let queue = VirtQueue::new(&mut transport, 0, false, false)?;
        transport.finish_init();
        let mut device = DeviceInfo::new(capacity, SECTOR_SIZE);
        device.read_only = features.contains(BlockFeatures::READ_ONLY);
        device.name = Some("virtio-blk");
        Ok(Self {
            control: Arc::new(RawSpinLock::new(TransportControl {
                transport,
                state: ResetState::Running,
            })),
            queue: ManuallyDrop::new(queue),
            device,
            flush_supported: features.contains(BlockFeatures::FLUSH),
        })
    }

    fn submit(
        &mut self,
        op: RequestOp,
        header: &[u8; 16],
        response: &mut [u8; 1],
        data: Option<&InFlightDma>,
    ) -> Result<u16, VirtIoError> {
        // SAFETY: The pending request owns this in-flight backing before add()
        // can publish its descriptor. The queue task does not access its bytes.
        let buffer = data.map(|data| unsafe {
            slice::from_raw_parts_mut(data.cpu_ptr().as_ptr(), data.len().get())
        });
        let token = match (op, buffer) {
            (RequestOp::Read, Some(buffer)) => {
                // SAFETY: The pending request keeps all three buffers at their
                // original addresses until this token is consumed.
                unsafe { self.queue.add(&[header], &mut [buffer, response])? }
            }
            (RequestOp::Write, Some(buffer)) => {
                // SAFETY: The pending request keeps all three buffers at their
                // original addresses until this token is consumed.
                unsafe { self.queue.add(&[header, buffer], &mut [response])? }
            }
            (RequestOp::Flush, None) => {
                // SAFETY: The pending request retains both boxed buffers until
                // the matching used-ring entry is reclaimed.
                unsafe { self.queue.add(&[header], &mut [response])? }
            }
            _ => return Err(VirtIoError::InvalidParam),
        };
        Ok(token)
    }

    fn complete(&mut self, pending: &mut PendingRequest) -> Result<(), VirtIoError> {
        // SAFETY: The sole queue owner has observed this exact used-ring token;
        // each slice uses the same boxed storage or DMA backing as at add().
        unsafe {
            match pending.op {
                RequestOp::Read => {
                    let data = pending.data.as_ref().ok_or(VirtIoError::InvalidParam)?;
                    let buffer =
                        slice::from_raw_parts_mut(data.cpu_ptr().as_ptr(), data.len().get());
                    self.queue.pop_used(
                        pending.token,
                        &[&pending.header[..]],
                        &mut [buffer, &mut pending.response[..]],
                    )?;
                }
                RequestOp::Write => {
                    let data = pending.data.as_ref().ok_or(VirtIoError::InvalidParam)?;
                    let buffer = slice::from_raw_parts(data.cpu_ptr().as_ptr(), data.len().get());
                    self.queue.pop_used(
                        pending.token,
                        &[&pending.header[..], buffer],
                        &mut [&mut pending.response[..]],
                    )?;
                }
                RequestOp::Flush => {
                    self.queue.pop_used(
                        pending.token,
                        &[&pending.header[..]],
                        &mut [&mut pending.response[..]],
                    )?;
                }
            }
        }
        match pending.response[0] {
            0 => Ok(()),
            2 => Err(VirtIoError::Unsupported),
            _ => Err(VirtIoError::IoError),
        }
    }
}

impl<T: Transport> Drop for RawBlock<T> {
    fn drop(&mut self) {
        // Drop must never spin waiting for hardware. If the single reset
        // observation is inconclusive, ManuallyDrop retains the DMA rings.
        if self.control.lock().stop() == ControllerState::Shutdown {
            // SAFETY: Reset completion was observed under the same lock that
            // excludes publication. No endpoint can restart this transport.
            unsafe { ManuallyDrop::drop(&mut self.queue) };
        }
    }
}

pub fn register_fdt_transport<T: Transport + Send + 'static>(
    info: &rdrive::register::FdtInfo<'_>,
    platform: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let binding = binding_info_from_fdt(info)?;
    if binding.irq().is_none() {
        return Err(OnProbeError::other("virtio-blk requires a wired IRQ"));
    }
    let dma_coherency = crate::binding_resolver::dma_coherency_from_fdt(info);
    if dma_coherency != DmaCoherency::Coherent {
        return Err(OnProbeError::other(
            "virtio-blk requires coherent DMA until its HAL provides cache maintenance",
        ));
    }
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("virtio-blk MMIO registers missing"))?;
    let irq_regs =
        axklib::mmio::ioremap_raw(reg.address.into(), reg.size.unwrap_or(0x1000) as usize)
            .map_err(|err| OnProbeError::other(format!("virtio-blk IRQ MMIO mapping: {err:?}")))?;
    if irq_regs.size() < INTERRUPT_ACK + size_of::<u32>() {
        return Err(OnProbeError::other("virtio-blk MMIO registers too short"));
    }
    let raw = RawBlock::<T>::new(transport)
        .map_err(|err| OnProbeError::other(format!("virtio-blk initialization: {err:?}")))?;
    let device = raw.device;
    let dma = DmaDeviceInfo::new(
        DmaDomainId::Direct,
        dma_coherency,
        DmaConstraints::new(u64::MAX),
    );
    let controller = VirtioBlockController {
        control: Arc::clone(&raw.control),
        raw: Some(raw),
        irq_regs: Some(irq_regs),
        device,
        dma,
        started: false,
    };
    platform.register_block_with_info(controller, binding);
    Ok(())
}

// Start transfers the bootstrap queue and IRQ mapping to their sole owners.
struct VirtioBlockController<T: Transport + Send + 'static> {
    raw: Option<RawBlock<T>>,
    irq_regs: Option<MmioRaw>,
    device: DeviceInfo,
    dma: DmaDeviceInfo,
    started: bool,
    control: Arc<RawSpinLock<TransportControl<T>>>,
}

impl<T: Transport + Send + 'static> DriverGeneric for VirtioBlockController<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + Send + 'static> BlockController for VirtioBlockController<T> {
    fn device_info(&self) -> DeviceInfo {
        self.device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } if target_queues > 0 && !self.started => {
                self.control.lock().ensure_running()?;
                let raw = self.raw.take().ok_or(BlkError::InvalidRequest)?;
                let regs = self.irq_regs.take().ok_or(BlkError::InvalidRequest)?;
                self.started = true;
                let mut limits = QueueLimits::simple(SECTOR_SIZE, self.dma);
                limits.supports_flush = raw.flush_supported;
                let queue = VirtioBlockQueue {
                    raw: Some(raw),
                    pending: None,
                    next_id: 0,
                    info: QueueInfo {
                        id: 0,
                        device: self.device,
                        limits,
                    },
                };
                let irq = IrqEndpoint::new(
                    0,
                    IrqQueueMask::from_queue(0),
                    Box::new(VirtioBlockIrq { regs }),
                );
                Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    vec![Box::new(queue)],
                    vec![irq],
                )
                .with_device_info(self.device))
            }
            ControllerEvent::OnlineSmp { .. } | ControllerEvent::Rearm { source_id: 0 }
                if self.started =>
            {
                self.control.lock().ensure_running()?;
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Irq(event) if self.started => {
                if event.bits() & u64::from(CONFIGURATION_CHANGE_INTERRUPT) == 0 {
                    self.control.lock().ensure_running()?;
                    return Ok(ControllerUpdate::state(ControllerState::Ready));
                }
                // The shared block runtime freezes geometry once queues become
                // ready. Stop the transport instead of continuing with stale
                // capacity after an unsupported live configuration change.
                self.started = false;
                Ok(ControllerUpdate::state(self.control.lock().stop()))
            }
            ControllerEvent::QuiesceIrqs
            | ControllerEvent::Watchdog { .. }
            | ControllerEvent::Shutdown => {
                // Quiescing IRQs starts the reset as well: VirtIO has no
                // transport-wide interrupt mask. The runtime waits for this
                // transition before asking queues to return their DMA backing.
                self.started = false;
                Ok(ControllerUpdate::state(self.control.lock().stop()))
            }
            ControllerEvent::RegisterRetry
                if self.control.lock().state == ResetState::Resetting =>
            {
                Ok(ControllerUpdate::state(self.control.lock().stop()))
            }
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

// One IRQ registration owns this endpoint. Only it reads and clears the
// interrupt status; the queue task accesses other registers.
struct VirtioBlockIrq {
    regs: MmioRaw,
}

impl HardIrqHandler for VirtioBlockIrq {
    fn ack(&mut self) -> IrqAck {
        let status: u32 = self.regs.read(INTERRUPT_STATUS);
        if status == 0 {
            return IrqAck::spurious(0);
        }
        self.regs.write(INTERRUPT_ACK, status);
        IrqAck::cleared(
            if status & QUEUE_INTERRUPT != 0 {
                IrqQueueMask::from_queue(0)
            } else {
                IrqQueueMask::none()
            },
            ControlEvent::new(0, u64::from(status & CONFIGURATION_CHANGE_INTERRUPT)),
        )
    }
}

struct PendingRequest {
    id: RequestId,
    token: u16,
    op: RequestOp,
    header: Box<[u8; 16]>,
    response: Box<[u8; 1]>,
    data: Option<InFlightDma>,
}

fn validate_request_direction(request: &rdif_block::OwnedRequest) -> Result<(), BlkError> {
    let Some(data) = request.data.as_ref() else {
        return Ok(());
    };
    let matches_operation = match request.op {
        RequestOp::Read => matches!(
            data.direction(),
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Write => matches!(
            data.direction(),
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Flush => false,
    };
    if matches_operation {
        Ok(())
    } else {
        Err(BlkError::InvalidRequest)
    }
}

// One task owns the queue and in-flight backing. The IRQ endpoint never
// accesses request memory; the transport is shared under the control lock.
struct VirtioBlockQueue<T: Transport + Send + 'static> {
    raw: Option<RawBlock<T>>,
    pending: Option<PendingRequest>,
    next_id: usize,
    info: QueueInfo,
}

impl<T: Transport + Send + 'static> HardwareQueue for VirtioBlockQueue<T> {
    fn id(&self) -> usize {
        self.info.id
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let Some(raw) = self.raw.as_ref() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(BlkError::Io));
        };
        if let Err(error) = raw.control.lock().ensure_running() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
        }
        if self.pending.is_some() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull);
        }
        let Some(request) = requests.front() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        };
        if let Err(error) = validate_owned_request(self.info, request) {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
        }
        if let Err(error) = validate_request_direction(request) {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
        }
        let mut header = Box::new([0_u8; 16]);
        let response = Box::new([RESPONSE_UNWRITTEN; 1]);
        let request_type: u32 = match request.op {
            RequestOp::Read => 0,
            RequestOp::Write => 1,
            RequestOp::Flush => 4,
        };
        header[..4].copy_from_slice(&request_type.to_le_bytes());
        // Flush addresses the whole device and requires sector zero.
        if request.is_data_op() {
            header[8..].copy_from_slice(&request.lba.to_le_bytes());
        }
        let Some(raw) = self.raw.as_mut() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(BlkError::Io));
        };
        let control = Arc::clone(&raw.control);
        let control = control.lock();
        if let Err(error) = control.ensure_running() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
        }
        let descriptors_needed = if request.is_data_op() { 3 } else { 2 };
        if raw.queue.available_desc() < descriptors_needed {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull);
        }
        let mut request = requests
            .pop_front()
            .expect("validated front request remains present");
        // SAFETY: This queue exclusively owns the prepared backing and retains
        // the in-flight value before publishing its descriptor.
        let data = request
            .data
            .take()
            .map(|data| unsafe { data.into_in_flight() });
        let mut pending = PendingRequest {
            id: RequestId::new(self.next_id),
            token: 0,
            op: request.op,
            header,
            response,
            data,
        };
        let token = match raw.submit(
            pending.op,
            &pending.header,
            &mut pending.response,
            pending.data.as_ref(),
        ) {
            Ok(token) => token,
            Err(error) => {
                // VirtQueue::add returns errors before advancing the available
                // ring, so this backing was never exposed to the device.
                request.data = pending.data.take().map(|data| {
                    // SAFETY: add() failed before publishing a descriptor;
                    // hardware has no path to this backing.
                    unsafe { data.complete_after_quiesce() }
                        .into_cpu_buffer()
                        .prepare_for_device()
                });
                requests.push_front(request);
                let disposition = if error == VirtIoError::QueueFull {
                    BatchSubmitDisposition::QueueFull
                } else {
                    BatchSubmitDisposition::Fatal(BlkError::Io)
                };
                return BatchSubmitResult::new(0, disposition);
            }
        };
        pending.token = token;
        let id = pending.id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending = Some(pending);
        drop(control);
        sink.accepted(id);
        BatchSubmitResult::new(1, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        let raw = self.raw.as_mut().ok_or(BlkError::Io)?;
        let mut control = raw.control.lock();
        control.ensure_running()?;
        if raw.queue.should_notify() {
            control.transport.notify(0);
        }
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let Some(raw) = self.raw.as_mut() else {
            return Err(BlkError::Io);
        };
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        match raw.queue.peek_used() {
            None => return Ok(()),
            Some(token) if token != pending.token => return Err(BlkError::Io),
            Some(_) => {}
        }
        let control = Arc::clone(&raw.control);
        let control = control.lock();
        control.ensure_running()?;
        let result = raw.complete(pending);
        if !matches!(
            result,
            Ok(()) | Err(VirtIoError::IoError | VirtIoError::Unsupported)
        ) {
            return Err(BlkError::Io);
        }
        let pending = self
            .pending
            .take()
            .expect("completion still owns pending request");
        // SAFETY: pop_used consumed the matching token; hardware can no longer
        // access this request's data, including when its response reported I/O failure.
        let data = pending
            .data
            .map(|data| unsafe { data.complete_after_quiesce() });
        drop(control);
        sink.complete(CompletedRequest::new(
            pending.id,
            result.map_err(|_| BlkError::Io),
            data,
        ));
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if let Some(raw) = self.raw.as_ref()
            && raw.control.lock().state != ResetState::Stopped
        {
            return Err(BlkError::TimedOut);
        }
        // A reset invalidates the used ring. Do not fabricate a used token or
        // call pop_used for an aborted request. VirtIoHalImpl uses direct
        // mappings (unshare is a no-op), so no bounce mappings need unwinding.
        self.raw.take();
        if let Some(pending) = self.pending.take() {
            // SAFETY: The controller confirmed reset before queue teardown;
            // publication is permanently disabled by the shared reset state.
            let data = pending
                .data
                .map(|data| unsafe { data.complete_after_quiesce() });
            sink.complete(CompletedRequest::new(pending.id, Err(BlkError::Io), data));
        }
        Ok(())
    }
}

impl<T: Transport + Send + 'static> Drop for VirtioBlockQueue<T> {
    fn drop(&mut self) {
        let stopped = self
            .raw
            .as_ref()
            .is_none_or(|raw| raw.control.lock().stop() == ControllerState::Shutdown);
        if let Some(pending) = self.pending.take() {
            if stopped {
                // SAFETY: The reset read-back above proves that no DMA can
                // reach this backing, and no endpoint can restart the device.
                drop(
                    pending
                        .data
                        .map(|data| unsafe { data.complete_after_quiesce() }),
                );
            } else {
                // A failed reset retains request storage as well as rings.
                mem::forget(pending);
            }
        }
        // RawBlock retains the rings unless reset completion is confirmed.
    }
}
