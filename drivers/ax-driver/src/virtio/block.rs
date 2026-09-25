//! Interrupt-driven VirtIO MMIO block registration for the shared block runtime.

extern crate alloc;

use alloc::{boxed::Box, format, vec};
use core::{mem, slice};

use bitflags::bitflags;
use dma_api::{DmaConstraints, DmaDeviceInfo, DmaDomainId, InFlightDma};
use mmio_api::MmioRaw;
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, BlockController, CompletedRequest,
    CompletionSink, ControlEvent, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo,
    DriverGeneric, HardIrqHandler, HardwareQueue, IrqAck, IrqEndpoint, IrqQueueMask,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestId, RequestOp, SubmissionSink,
    validate_owned_request,
};
use rdrive::{PlatformDevice, probe::OnProbeError};
use virtio_drivers::{Error as VirtIoError, queue::VirtQueue, transport::Transport};

use crate::{binding_info_from_fdt, block::PlatformDeviceBlock, virtio::VirtIoHalImpl};

const INTERRUPT_STATUS: usize = 0x60;
const INTERRUPT_ACK: usize = 0x64;
const QUEUE_INTERRUPT: u32 = 1;
const SECTOR_SIZE: usize = 512;
const QUEUE_SIZE: usize = 16;

bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct BlockFeatures: u64 {
        const READ_ONLY = 1 << 5;
        const FLUSH = 1 << 9;
        const VERSION_1 = 1 << 32;
    }
}

struct RawBlock<T: Transport> {
    transport: T,
    queue: VirtQueue<VirtIoHalImpl, QUEUE_SIZE>,
    device: DeviceInfo,
    flush_supported: bool,
}

impl<T: Transport> RawBlock<T> {
    fn new(mut transport: T) -> Result<Self, VirtIoError> {
        let features = transport
            .begin_init(BlockFeatures::READ_ONLY | BlockFeatures::FLUSH | BlockFeatures::VERSION_1);
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
            transport,
            queue,
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
        self.transport.queue_unset(0);
    }
}

pub fn register_fdt_transport<T: Transport + 'static>(
    info: &rdrive::register::FdtInfo<'_>,
    platform: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let binding = binding_info_from_fdt(info)?;
    if binding.irq().is_none() {
        return Err(OnProbeError::other("virtio-blk requires a wired IRQ"));
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
        crate::binding_resolver::dma_coherency_from_fdt(info),
        DmaConstraints::new(u64::MAX),
    );
    let controller = VirtioBlockController {
        raw: Some(raw),
        irq_regs: Some(irq_regs),
        device,
        dma,
        started: false,
    };
    platform.register_block_with_info(controller, binding);
    Ok(())
}

struct VirtioBlockController<T: Transport + 'static> {
    raw: Option<RawBlock<T>>,
    irq_regs: Option<MmioRaw>,
    device: DeviceInfo,
    dma: DmaDeviceInfo,
    started: bool,
}

// SAFETY: The controller is moved to one block-runtime maintenance task. Its
// transport and queue are never accessed from the separately mapped IRQ registers.
unsafe impl<T: Transport + 'static> Send for VirtioBlockController<T> {}

impl<T: Transport + 'static> DriverGeneric for VirtioBlockController<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> BlockController for VirtioBlockController<T> {
    fn device_info(&self) -> DeviceInfo {
        self.device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } if target_queues > 0 && !self.started => {
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
            ControllerEvent::OnlineSmp { .. }
            | ControllerEvent::Irq(_)
            | ControllerEvent::Rearm { source_id: 0 }
            | ControllerEvent::QuiesceIrqs
                if self.started =>
            {
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown if self.started => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            ControllerEvent::Watchdog { .. } => Err(BlkError::TimedOut),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

struct VirtioBlockIrq {
    regs: MmioRaw,
}

// SAFETY: The fixed MMIO mapping remains valid for the board's lifetime and
// this endpoint is moved into one IRQ registration. Only it reads and clears
// the device's interrupt status; the queue task accesses other registers.
unsafe impl Send for VirtioBlockIrq {}

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
            ControlEvent::new(0, 0),
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

struct VirtioBlockQueue<T: Transport + 'static> {
    raw: Option<RawBlock<T>>,
    pending: Option<PendingRequest>,
    next_id: usize,
    info: QueueInfo,
}

// SAFETY: The queue, its transport, and each in-flight DMA backing move to
// exactly one runtime task. The IRQ endpoint owns a separate MMIO mapping and
// never touches the queue or its request memory.
unsafe impl<T: Transport + 'static> Send for VirtioBlockQueue<T> {}

impl<T: Transport + 'static> HardwareQueue for VirtioBlockQueue<T> {
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
        if self.pending.is_some() {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull);
        }
        let Some(request) = requests.front() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        };
        if let Err(error) = validate_owned_request(self.info, request) {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(error));
        }
        let mut header = Box::new([0_u8; 16]);
        let response = Box::new([3_u8; 1]);
        let request_type: u32 = match request.op {
            RequestOp::Read => 0,
            RequestOp::Write => 1,
            RequestOp::Flush => 4,
        };
        header[..4].copy_from_slice(&request_type.to_le_bytes());
        header[8..].copy_from_slice(&request.lba.to_le_bytes());
        let Some(raw) = self.raw.as_mut() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Fatal(BlkError::Io));
        };
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
        sink.accepted(id);
        BatchSubmitResult::new(1, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        let raw = self.raw.as_mut().ok_or(BlkError::Io)?;
        if raw.queue.should_notify() {
            raw.transport.notify(0);
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
        let result = raw.complete(pending);
        if !matches!(
            result,
            Ok(()) | Err(VirtIoError::IoError | VirtIoError::Unsupported | VirtIoError::NotReady)
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
        sink.complete(CompletedRequest::new(
            pending.id,
            result.map_err(|_| BlkError::Io),
            data,
        ));
        Ok(())
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.pending.is_some() {
            // The pinned VirtIOBlk API cannot reset its transport. Keep the
            // queue and its DMA backing quarantined on an incomplete shutdown.
            return Err(BlkError::TimedOut);
        }
        if let Some(raw) = self.raw.as_mut() {
            raw.queue.set_dev_notify(false);
        }
        self.raw.take();
        Ok(())
    }
}

impl<T: Transport + 'static> Drop for VirtioBlockQueue<T> {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            // Without a transport reset, the device can still read the request
            // and queue descriptors. Quarantine both allocations together.
            mem::forget(pending);
            if let Some(raw) = self.raw.take() {
                mem::forget(raw);
            }
        }
    }
}
