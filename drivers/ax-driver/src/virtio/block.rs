extern crate alloc;

use alloc::{boxed::Box, format, sync::Arc, vec};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::PreemptIrqGuard;
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{BlkReq, BlkResp, SECTOR_SIZE, VirtIOBlk},
    transport::{InterruptStatus, Transport},
};

use crate::{
    BindingInfo, binding_info_from_fdt, block::PlatformDeviceBlock, virtio::VirtIoHalImpl,
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

// Keep one IRQ-driven completion large enough to amortize the interrupt/drain
// wake cost while preserving the intentionally conservative max_inflight=1.
const VIRTIO_BLK_DMA_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const VIRTIO_BLK_QUEUE_ID: usize = 0;
const VIRTIO_BLK_IRQ_SOURCE_ID: usize = 0;

#[cfg(feature = "pci")]
model_register!(
    name: "VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::Block)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

fn probe_fdt(probe: rdrive::register::ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let binding_info = binding_info_from_fdt(&info)?;
    let (ty, transport) = crate::virtio::probe_fdt_mmio_device(&info)?;
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    register_transport_with_info(plat_dev, transport, binding_info)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = VirtIoBlkDevice::new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-blk: {err:?}")))?;
    plat_dev.register_block_with_info(
        BlockDevice {
            dev: Some(Arc::new(dev)),
            queue_created: false,
            irq_handler_taken: false,
        },
        info,
    );
    log::info!("registered virtio block device");
    Ok(())
}

struct VirtIoBlkDevice<T: Transport + 'static> {
    inner: UnsafeCell<VirtIoBlkInner<T>>,
    access_active: AtomicBool,
    irq_ack_pending: AtomicBool,
    irq_enabled: AtomicBool,
}

struct VirtIoBlkInner<T: Transport + 'static> {
    raw: VirtIOBlk<VirtIoHalImpl, T>,
    inflight: Option<InflightRequest>,
    next_request_id: usize,
}

unsafe impl<T: Transport + 'static> Send for VirtIoBlkDevice<T> {}
unsafe impl<T: Transport + 'static> Sync for VirtIoBlkDevice<T> {}

impl<T: Transport + 'static> VirtIoBlkDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOBlk::new(transport)?;
        raw.disable_interrupts();
        Ok(Self {
            inner: UnsafeCell::new(VirtIoBlkInner {
                raw,
                inflight: None,
                next_request_id: 1,
            }),
            access_active: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
            irq_enabled: AtomicBool::new(false),
        })
    }

    fn with_task<R>(&self, f: impl FnOnce(&mut VirtIoBlkInner<T>) -> R) -> R {
        let _irq_guard = PreemptIrqGuard::new();
        let _active = VirtioBlkAccessGuard::enter_task(&self.access_active);
        // SAFETY: `access_active` serializes all mutable access to the raw
        // transport, and task-side callers keep local IRQ/preemption disabled.
        let inner = unsafe { &mut *self.inner.get() };
        self.flush_pending_irq_ack(inner);
        let ret = f(inner);
        self.flush_pending_irq_ack(inner);
        ret
    }

    fn enable_irq(&self) {
        self.irq_enabled.store(true, Ordering::Release);
        self.with_task(|inner| inner.raw.enable_interrupts());
    }

    fn disable_irq(&self) {
        self.irq_enabled.store(false, Ordering::Release);
        self.with_task(|inner| inner.raw.disable_interrupts());
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn handle_irq(&self) -> rdif_block::Event {
        virtio_blk_irq_event(
            &self.access_active,
            &self.irq_ack_pending,
            self.is_irq_enabled(),
            || {
                // SAFETY: `virtio_blk_irq_event` calls this closure only while
                // holding the IRQ-side access guard.
                let inner = unsafe { &mut *self.inner.get() };
                inner.raw.ack_interrupt()
            },
        )
    }

    fn flush_pending_irq_ack(&self, inner: &mut VirtIoBlkInner<T>) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let _ = inner.raw.ack_interrupt();
        }
    }
}

struct BlockDevice<T: Transport + 'static> {
    dev: Option<Arc<VirtIoBlkDevice<T>>>,
    queue_created: bool,
    irq_handler_taken: bool,
}

impl<T: Transport + 'static> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> rdif_block::Interface for BlockDevice<T> {
    fn device_info(&self) -> rdif_block::DeviceInfo {
        let blocks = self
            .dev
            .as_ref()
            .map(|dev| dev.with_task(|inner| inner.raw.capacity()))
            .unwrap_or(0);
        rdif_block::DeviceInfo {
            name: Some("virtio-blk"),
            ..rdif_block::DeviceInfo::new(blocks, SECTOR_SIZE)
        }
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        rdif_block::QueueLimits {
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_mask: u64::MAX,
            dma_alignment: 0x1000,
            max_inflight: 1,
            max_blocks_per_request: (VIRTIO_BLK_DMA_BUFFER_SIZE / SECTOR_SIZE) as u32,
            max_segments: 1,
            max_segment_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
            supported_flags: rdif_block::RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }

    fn create_queue(&mut self) -> Option<Box<dyn rdif_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.dev.as_ref().map(|dev| {
            self.queue_created = true;
            Box::new(BlockQueue {
                id: 0,
                raw: Arc::clone(dev),
            }) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.enable_irq();
        }
    }

    fn disable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.disable_irq();
        }
    }

    fn is_irq_enabled(&self) -> bool {
        self.dev.as_ref().is_some_and(|dev| dev.is_irq_enabled())
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        if self.dev.is_none() {
            return vec![];
        }
        let mut queues = rdif_block::IdList::none();
        queues.insert(VIRTIO_BLK_QUEUE_ID);
        vec![rdif_block::IrqSourceInfo::new(
            VIRTIO_BLK_IRQ_SOURCE_ID,
            queues,
        )]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<rdif_block::BIrqHandler> {
        if source_id != VIRTIO_BLK_IRQ_SOURCE_ID || self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        self.dev.as_ref().map(|dev| {
            Box::new(VirtioBlkIrqHandler {
                inner: Arc::clone(dev),
            }) as _
        })
    }
}

struct BlockQueue<T: Transport + 'static> {
    id: usize,
    raw: Arc<VirtIoBlkDevice<T>>,
}

// SAFETY: Submitted request segments are retained only in the single in-flight
// slot and are released when the matching `poll_request` reports a terminal
// result.
unsafe impl<T: Transport + 'static> rdif_block::IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        let blocks = self.raw.with_task(|inner| inner.raw.capacity());
        rdif_block::QueueInfo {
            id: self.id,
            device: rdif_block::DeviceInfo {
                name: Some("virtio-blk"),
                ..rdif_block::DeviceInfo::new(blocks, SECTOR_SIZE)
            },
            limits: rdif_block::QueueLimits {
                dma_domain: dma_api::DmaDomainId::legacy_global(),
                dma_mask: u64::MAX,
                dma_alignment: 0x1000,
                max_inflight: 1,
                max_blocks_per_request: (VIRTIO_BLK_DMA_BUFFER_SIZE / SECTOR_SIZE) as u32,
                max_segments: 1,
                max_segment_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
                supported_flags: rdif_block::RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        }
    }

    fn submit_request(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        rdif_block::validate_request(self.info(), &request)?;
        self.raw.with_task(|inner| inner.submit_request(request))
    }

    fn poll_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        self.raw.with_task(|inner| inner.poll_request(request))
    }
}

impl<T: Transport + 'static> VirtIoBlkInner<T> {
    fn submit_request(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        if self.inflight.is_some() {
            return Err(rdif_block::BlkError::Retry);
        }

        let op = match request.op {
            rdif_block::RequestOp::Read => InflightOp::Read,
            rdif_block::RequestOp::Write => InflightOp::Write,
            rdif_block::RequestOp::Flush
            | rdif_block::RequestOp::Discard
            | rdif_block::RequestOp::WriteZeroes => return Err(rdif_block::BlkError::NotSupported),
        };
        let segment = request
            .segments
            .first()
            .copied()
            .ok_or(rdif_block::BlkError::InvalidRequest)?;
        let mut buffer = InflightBuffer::from_segment(segment);
        let mut storage = Box::<InflightStorage>::default();
        let token = match op {
            InflightOp::Read => {
                // SAFETY: RDIF requires request segments to remain valid and
                // exclusively owned until the matching `poll_request` returns a
                // terminal result.
                unsafe {
                    self.raw.read_blocks_nb(
                        request.lba as usize,
                        &mut storage.req,
                        buffer.as_mut_slice(),
                        &mut storage.resp,
                    )
                }
            }
            InflightOp::Write => {
                // SAFETY: See the read path above; virtio retains the segment
                // until `complete_write_blocks` consumes the descriptor.
                unsafe {
                    self.raw.write_blocks_nb(
                        request.lba as usize,
                        &mut storage.req,
                        buffer.as_slice(),
                        &mut storage.resp,
                    )
                }
            }
        }
        .map_err(map_virtio_err_to_blk_err)?;

        let request_id = self.alloc_request_id();
        self.inflight = Some(InflightRequest {
            id: request_id,
            token,
            op,
            storage,
            buffer,
        });
        Ok(request_id)
    }

    fn poll_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        let inflight_key = self
            .inflight
            .as_ref()
            .map(|inflight| (inflight.id, inflight.token));
        if classify_completion_poll(inflight_key, request, self.raw.peek_used())?
            == rdif_block::RequestStatus::Pending
        {
            return Ok(rdif_block::RequestStatus::Pending);
        }

        let mut inflight = self
            .inflight
            .take()
            .ok_or(rdif_block::BlkError::InvalidRequest)?;
        let result = match inflight.op {
            InflightOp::Read => {
                // SAFETY: `inflight` owns the same request metadata and buffer
                // raw parts that were passed to `read_blocks_nb`.
                unsafe {
                    self.raw.complete_read_blocks(
                        inflight.token,
                        &inflight.storage.req,
                        inflight.buffer.as_mut_slice(),
                        &mut inflight.storage.resp,
                    )
                }
            }
            InflightOp::Write => {
                // SAFETY: `inflight` owns the same request metadata and buffer
                // raw parts that were passed to `write_blocks_nb`.
                unsafe {
                    self.raw.complete_write_blocks(
                        inflight.token,
                        &inflight.storage.req,
                        inflight.buffer.as_slice(),
                        &mut inflight.storage.resp,
                    )
                }
            }
        };
        result.map_err(map_virtio_completion_err_to_blk_err)?;
        Ok(rdif_block::RequestStatus::Complete)
    }

    fn alloc_request_id(&mut self) -> rdif_block::RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        rdif_block::RequestId::new(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InflightOp {
    Read,
    Write,
}

struct InflightRequest {
    id: rdif_block::RequestId,
    token: u16,
    op: InflightOp,
    storage: Box<InflightStorage>,
    buffer: InflightBuffer,
}

#[derive(Default)]
struct InflightStorage {
    req: BlkReq,
    resp: BlkResp,
}

impl InflightRequest {
    #[cfg(test)]
    fn req_addr(&self) -> usize {
        core::ptr::addr_of!(self.storage.req) as usize
    }

    #[cfg(test)]
    fn resp_addr(&self) -> usize {
        core::ptr::addr_of!(self.storage.resp) as usize
    }
}

impl InflightStorage {
    #[cfg(test)]
    fn req_addr(&self) -> usize {
        core::ptr::addr_of!(self.req) as usize
    }

    #[cfg(test)]
    fn resp_addr(&self) -> usize {
        core::ptr::addr_of!(self.resp) as usize
    }
}

#[derive(Clone, Copy)]
struct InflightBuffer {
    virt: *mut u8,
    len: usize,
}

impl InflightBuffer {
    fn from_segment(segment: rdif_block::Segment<'_>) -> Self {
        Self {
            virt: segment.virt,
            len: segment.len,
        }
    }

    unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: Callers uphold the RDIF request lifetime until completion.
        unsafe { core::slice::from_raw_parts(self.virt.cast_const(), self.len) }
    }

    unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: Callers uphold exclusive RDIF request ownership until
        // completion.
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.len) }
    }
}

struct VirtioBlkIrqHandler<T: Transport + 'static> {
    inner: Arc<VirtIoBlkDevice<T>>,
}

impl<T: Transport + 'static> rdif_block::IrqHandler for VirtioBlkIrqHandler<T> {
    fn handle_irq(&mut self) -> rdif_block::Event {
        self.inner.handle_irq()
    }
}

struct VirtioBlkAccessGuard<'a>(&'a AtomicBool);

impl<'a> VirtioBlkAccessGuard<'a> {
    fn enter_task(active: &'a AtomicBool) -> Self {
        Self::enter(active)
    }

    fn try_enter_irq(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Self(active))
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

impl Drop for VirtioBlkAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn virtio_blk_irq_event(
    access_active: &AtomicBool,
    irq_ack_pending: &AtomicBool,
    irq_enabled: bool,
    ack_status: impl FnOnce() -> InterruptStatus,
) -> rdif_block::Event {
    if !irq_enabled {
        return rdif_block::Event::none();
    }
    let Some(_active) = VirtioBlkAccessGuard::try_enter_irq(access_active) else {
        irq_ack_pending.store(true, Ordering::Release);
        return rdif_block::Event::none();
    };
    irq_ack_pending.store(false, Ordering::Release);
    virtio_blk_event_from_irq_status(true, ack_status())
}

fn virtio_blk_event_from_irq_status(
    irq_enabled: bool,
    status: InterruptStatus,
) -> rdif_block::Event {
    if !irq_enabled || !status.contains(InterruptStatus::QUEUE_INTERRUPT) {
        return rdif_block::Event::none();
    }
    rdif_block::Event::from_queue_bits(1 << VIRTIO_BLK_QUEUE_ID)
}

fn classify_completion_poll(
    inflight: Option<(rdif_block::RequestId, u16)>,
    request: rdif_block::RequestId,
    used_token: Option<u16>,
) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
    let Some((inflight_request, inflight_token)) = inflight else {
        return Err(rdif_block::BlkError::InvalidRequest);
    };
    if inflight_request != request {
        return Err(rdif_block::BlkError::InvalidRequest);
    }
    if used_token == Some(inflight_token) {
        Ok(rdif_block::RequestStatus::Complete)
    } else {
        Ok(rdif_block::RequestStatus::Pending)
    }
}

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rdif_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => rdif_block::BlkError::Other("bad internal state"),
        VirtIoError::AlreadyUsed => rdif_block::BlkError::Other("already exists"),
        VirtIoError::InvalidParam => rdif_block::BlkError::InvalidRequest,
        VirtIoError::DmaError => rdif_block::BlkError::NoMemory,
        VirtIoError::IoError => rdif_block::BlkError::Io,
        VirtIoError::Unsupported => rdif_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rdif_block::BlkError::Other("socket error"),
    }
}

fn map_virtio_completion_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match map_virtio_err_to_blk_err(err) {
        rdif_block::BlkError::Retry => rdif_block::BlkError::Io,
        err => err,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicBool, Ordering};

    use rdif_block::{BlkError, RequestId, RequestStatus};
    use virtio_drivers::transport::InterruptStatus;

    use super::{
        InflightBuffer, InflightOp, InflightRequest, SECTOR_SIZE, VIRTIO_BLK_DMA_BUFFER_SIZE,
        VirtioBlkAccessGuard, classify_completion_poll, virtio_blk_event_from_irq_status,
        virtio_blk_irq_event,
    };

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
    fn busy_task_access_defers_irq_ack_without_ready_event() {
        let access_active = AtomicBool::new(false);
        let irq_ack_pending = AtomicBool::new(false);
        let _task_guard = VirtioBlkAccessGuard::enter_task(&access_active);

        let event = virtio_blk_irq_event(&access_active, &irq_ack_pending, true, || {
            InterruptStatus::QUEUE_INTERRUPT
        });

        assert!(event.is_empty());
        assert!(irq_ack_pending.load(Ordering::Acquire));
    }

    #[test]
    fn poll_stays_pending_until_matching_token_is_used() {
        let request_id = RequestId::new(7);
        let token = 3;

        assert_eq!(
            classify_completion_poll(Some((request_id, token)), request_id, None),
            Ok(RequestStatus::Pending)
        );
        assert_eq!(
            classify_completion_poll(Some((request_id, token)), request_id, Some(token + 1)),
            Ok(RequestStatus::Pending)
        );
        assert_eq!(
            classify_completion_poll(Some((request_id, token)), request_id, Some(token)),
            Ok(RequestStatus::Complete)
        );
        assert_eq!(
            classify_completion_poll(Some((request_id, token)), RequestId::new(8), Some(token)),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn submitted_descriptor_storage_must_not_move_into_inflight_slot() {
        let storage = Box::<super::InflightStorage>::default();
        let submitted_req_addr = storage.req_addr();
        let submitted_resp_addr = storage.resp_addr();
        let inflight = InflightRequest {
            id: RequestId::new(1),
            token: 0,
            op: InflightOp::Read,
            storage,
            buffer: InflightBuffer {
                virt: core::ptr::NonNull::<u8>::dangling().as_ptr(),
                len: SECTOR_SIZE,
            },
        };

        assert_eq!(
            inflight.req_addr(),
            submitted_req_addr,
            "virtio descriptors must keep pointing at the same BlkReq storage until completion"
        );
        assert_eq!(
            inflight.resp_addr(),
            submitted_resp_addr,
            "virtio descriptors must keep pointing at the same BlkResp storage until completion"
        );
    }

    #[test]
    fn irq_driven_path_uses_large_requests_to_amortize_completion_wakes() {
        assert!(
            VIRTIO_BLK_DMA_BUFFER_SIZE >= 4 * 1024 * 1024,
            "max_inflight=1 IRQ-driven completion needs large request chunks"
        );
    }
}
