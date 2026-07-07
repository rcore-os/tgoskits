#[cfg(feature = "irq")]
use alloc::sync::Arc;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::alloc::Layout;
#[cfg(feature = "irq")]
use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
#[cfg(not(test))]
use ax_kspin::SpinNoIrq as BlockQueueLock;
#[cfg(test)]
use ax_kspin::SpinRaw as BlockQueueLock;
use dma_api::{ContiguousArray, CpuDmaBuffer, DeviceDma, DmaDirection};
use log::{error, warn};
use rdif_block::{
    BlkError, Buffer, CompletedRequest, IQueue, Interface, OwnedRequest, QueueHandle, QueueInfo,
    Request, RequestFlags, RequestId, RequestOp, RequestPoll, RequestStatus, TransferChunk,
    TransferPlan, TransferPlanner, TransferRuntimeCaps,
};
use rdrive::{Device, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

pub struct Block {
    name: String,
    info: BindingInfo,
    irq_enabled: bool,
    #[cfg(feature = "irq")]
    irq_handler: Option<BlockIrqHandler>,
    interface: Box<dyn Interface>,
    queues: BlockQueueLock<BlockQueues>,
}

struct BlockQueues {
    queue: RuntimeQueue,
    pool: BlockBufferPool,
    #[cfg(feature = "irq")]
    irq_events: Arc<BlockIrqEvents>,
}

enum RuntimeQueue {
    Legacy(Box<dyn IQueue>),
    Owned(QueueHandle),
}

struct BlockBufferPool {
    dma: DeviceDma,
    planner: TransferPlanner,
    size: usize,
    align: usize,
}

pub struct PlatformBlockDevice {
    name: String,
    interface: Option<Box<dyn Interface>>,
    info: BindingInfo,
}

/// A probed block device exposed through the portable `rdif-block` interface.
///
/// Runtime code should use this form and create/poll `rdif_block::IQueue`
/// objects directly, installing IRQ handlers according to the OS policy.
pub struct RdifBlockDevice {
    name: String,
    irqs: Vec<crate::BindingIrqBinding>,
    interface: Box<dyn Interface>,
}

const MAX_BLOCK_BUFFER_SIZE: usize = 16 * 1024;

impl PlatformBlockDevice {
    fn new(name: String, interface: Box<dyn Interface>, info: BindingInfo) -> Self {
        Self {
            name,
            interface: Some(interface),
            info,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformBlockDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

#[cfg(feature = "irq")]
pub struct BlockIrqHandler {
    handler: Box<dyn rdif_block::IrqHandler>,
    events: Option<Arc<BlockIrqEvents>>,
}

#[cfg(feature = "irq")]
impl BlockIrqHandler {
    fn new(handler: Box<dyn rdif_block::IrqHandler>, events: Arc<BlockIrqEvents>) -> Self {
        Self {
            handler,
            events: Some(events),
        }
    }

    fn new_raw(handler: Box<dyn rdif_block::IrqHandler>) -> Self {
        Self {
            handler,
            events: None,
        }
    }

    pub fn handle(&mut self) -> rdif_block::Event {
        let event = self.handler.handle_irq();
        if let Some(events) = &self.events {
            events.record(event);
        }
        event
    }
}

#[cfg(not(feature = "irq"))]
pub struct BlockIrqHandler;

#[cfg(feature = "irq")]
#[derive(Default)]
struct BlockIrqEvents {
    queues: AtomicU64,
}

#[cfg(feature = "irq")]
impl BlockIrqEvents {
    fn record(&self, event: rdif_block::Event) {
        let queues = event.queues.bits();
        if queues != 0 {
            self.queues.fetch_or(queues, Ordering::Release);
        }
    }

    fn take_queue(&self, id: usize) -> bool {
        if id >= u64::BITS as usize {
            return false;
        }
        let mask = 1_u64 << id;
        let mut current = self.queues.load(Ordering::Acquire);
        while current & mask != 0 {
            match self.queues.compare_exchange_weak(
                current,
                current & !mask,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
        false
    }
}

impl Block {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn enable_irq(&mut self) {
        self.interface.enable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub fn disable_irq(&mut self) {
        self.interface.disable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub const fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    pub fn irq(&self) -> Option<&BindingIrq> {
        self.info.irq()
    }

    #[cfg(feature = "irq")]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let irq = self.info.irq_num()?;
        let handler = self.irq_handler.take()?;
        Some((irq, handler))
    }

    #[cfg(not(feature = "irq"))]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let _ = self;
        None
    }

    pub fn num_blocks(&self) -> u64 {
        self.queues.lock().queue.info().device.num_blocks
    }

    pub fn block_size(&self) -> usize {
        self.queues.lock().queue.info().device.logical_block_size
    }

    pub fn flush(&mut self) -> AxResult {
        let mut queues = self.queues.lock();
        if queues.queue.is_owned() {
            let request_id = match queues
                .queue
                .owned_mut()
                .ok_or(AxError::BadState)?
                .submit_request(OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                }) {
                Ok(request_id) => request_id,
                Err(err) if err.error == BlkError::NotSupported => return Ok(()),
                Err(err) => return Err(map_blk_err_to_ax_err(err.error)),
            };
            return queues.poll_owned_until_complete(request_id).map(|_| ());
        }

        let mut segments = [];
        let request = Request {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };
        let request_id = match queues
            .queue
            .legacy_mut()
            .ok_or(AxError::BadState)?
            .submit_request(request)
        {
            Ok(request_id) => request_id,
            Err(BlkError::NotSupported) => return Ok(()),
            Err(err) => return Err(map_blk_err_to_ax_err(err)),
        };
        queues.poll_until_complete(request_id)
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let mut queues = self.queues.lock();
        let info = queues.queue.info();
        validate_io(info, block_id, buf.len())?;

        let plan = transfer_plan(queues.pool.planner, block_id, buf.len())?;
        for chunk in plan {
            let block_buf =
                &mut buf[chunk.byte_offset..chunk.byte_offset.saturating_add(chunk.byte_len)];
            if queues.queue.is_owned() {
                let dma_buffer = queues
                    .pool
                    .alloc_owned(DmaDirection::FromDevice, block_buf.len())?;
                let request_id = queues.submit_owned_request(OwnedRequest {
                    op: RequestOp::Read,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    data: Some(dma_buffer.prepare_for_device()),
                    flags: RequestFlags::NONE,
                })?;
                let completed = queues.poll_owned_until_complete(request_id)?;
                let completed_dma = completed.data.ok_or(AxError::BadState)?;
                completed_dma.copy_from_device_to_slice(block_buf);
            } else {
                let mut dma_buffer = queues.pool.alloc(DmaDirection::FromDevice)?;
                dma_buffer.prepare_for_device(0, block_buf.len());
                let mut segments = segments_from_dma(&mut dma_buffer, chunk)?;
                let request_id = queues.submit_legacy_request(Request {
                    op: RequestOp::Read,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    segments: &mut segments,
                    flags: RequestFlags::NONE,
                })?;
                queues.poll_until_complete(request_id)?;
                dma_buffer.copy_from_device_to_slice(block_buf);
            }
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut queues = self.queues.lock();
        let info = queues.queue.info();
        validate_io(info, block_id, buf.len())?;

        let plan = transfer_plan(queues.pool.planner, block_id, buf.len())?;
        for chunk in plan {
            let block_buf =
                &buf[chunk.byte_offset..chunk.byte_offset.saturating_add(chunk.byte_len)];
            if queues.queue.is_owned() {
                let mut dma_buffer = queues
                    .pool
                    .alloc_owned(DmaDirection::ToDevice, block_buf.len())?;
                dma_buffer.copy_to_device_from_slice(block_buf);
                let request_id = queues.submit_owned_request(OwnedRequest {
                    op: RequestOp::Write,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    data: Some(dma_buffer.prepare_for_device()),
                    flags: RequestFlags::NONE,
                })?;
                let completed = queues.poll_owned_until_complete(request_id)?;
                drop(completed.data);
            } else {
                let mut dma_buffer = queues.pool.alloc(DmaDirection::ToDevice)?;
                dma_buffer.copy_to_device_from_slice(block_buf);
                let mut segments = segments_from_dma(&mut dma_buffer, chunk)?;
                let request_id = queues.submit_legacy_request(Request {
                    op: RequestOp::Write,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    segments: &mut segments,
                    flags: RequestFlags::NONE,
                })?;
                queues.poll_until_complete(request_id)?;
            }
        }
        Ok(())
    }
}

impl RdifBlockDevice {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn irq(&self) -> Option<&BindingIrq> {
        self.irq_for_source(0)
            .or_else(|| self.irqs.first().map(|binding| &binding.irq))
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.irq().cloned()
    }

    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.irqs
            .iter()
            .find(|binding| binding.source_id == source_id)
            .map(|binding| &binding.irq)
    }

    pub fn irq_for_source_cloned(&self, source_id: usize) -> Option<BindingIrq> {
        self.irq_for_source(source_id).cloned()
    }

    pub fn irq_sources(&self) -> &[crate::BindingIrqBinding] {
        &self.irqs
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.irq().and_then(BindingIrq::legacy_num)
    }

    pub fn irq_num_for_source(&self, source_id: usize) -> Option<usize> {
        self.irq_for_source(source_id)
            .and_then(BindingIrq::legacy_num)
    }

    pub fn interface(&self) -> &dyn Interface {
        &*self.interface
    }

    pub fn interface_mut(&mut self) -> &mut dyn Interface {
        &mut *self.interface
    }

    pub fn into_interface(self) -> Box<dyn Interface> {
        self.interface
    }

    pub fn enable_irq(&mut self) {
        self.interface.enable_irq();
    }

    pub fn disable_irq(&mut self) {
        self.interface.disable_irq();
    }

    #[cfg(feature = "irq")]
    pub fn take_irq_handler(&mut self, source_id: usize) -> Option<(usize, BlockIrqHandler)> {
        let irq_num = self.irq_num_for_source(source_id)?;
        self.interface
            .take_irq_handler(source_id)
            .map(BlockIrqHandler::new_raw)
            .map(|handler| (irq_num, handler))
    }

    #[cfg(not(feature = "irq"))]
    pub fn take_irq_handler(&mut self, _source_id: usize) -> Option<(usize, BlockIrqHandler)> {
        None
    }
}

impl BlockQueues {
    fn new(
        queue: RuntimeQueue,
        #[cfg(feature = "irq")] irq_events: Arc<BlockIrqEvents>,
    ) -> AxResult<Self> {
        let info = queue.info();
        let block_size = info.device.logical_block_size;
        if block_size == 0 {
            return Err(AxError::BadState);
        }
        let planner = block_transfer_planner(info)?;
        let layout = block_buffer_layout(info, planner.chunk_size())?;
        Ok(Self {
            queue,
            pool: BlockBufferPool {
                dma: DeviceDma::new(
                    info.limits.dma_domain,
                    info.limits.dma_mask,
                    axklib::dma::op(),
                ),
                planner,
                size: layout.size(),
                align: layout.align(),
            },
            #[cfg(feature = "irq")]
            irq_events,
        })
    }

    fn submit_legacy_request(&mut self, request: Request<'_>) -> AxResult<RequestId> {
        self.queue
            .legacy_mut()
            .ok_or(AxError::BadState)?
            .submit_request(request)
            .map_err(map_blk_err_to_ax_err)
    }

    fn submit_owned_request(&mut self, request: OwnedRequest) -> AxResult<RequestId> {
        self.queue
            .owned_mut()
            .ok_or(AxError::BadState)?
            .submit_request(request)
            .map_err(|err| map_blk_err_to_ax_err(err.error))
    }

    fn poll_until_complete(&mut self, request: RequestId) -> AxResult {
        loop {
            let status = self
                .queue
                .legacy_mut()
                .ok_or(AxError::BadState)?
                .poll_request(request)
                .map_err(map_blk_err_to_ax_err)?;
            match status {
                RequestStatus::Complete => {
                    #[cfg(feature = "irq")]
                    let _ = self.irq_events.take_queue(self.queue.id());
                    return Ok(());
                }
                RequestStatus::Pending => {
                    #[cfg(feature = "irq")]
                    if self.irq_events.take_queue(self.queue.id()) {
                        continue;
                    }
                    core::hint::spin_loop();
                }
            }
        }
    }

    fn poll_owned_until_complete(&mut self, request: RequestId) -> AxResult<CompletedRequest> {
        loop {
            let status = self
                .queue
                .owned_mut()
                .ok_or(AxError::BadState)?
                .poll_request(request)
                .map_err(|err| map_blk_err_to_ax_err(err.into()))?;
            match status {
                RequestPoll::Ready(completed) => {
                    #[cfg(feature = "irq")]
                    let _ = self.irq_events.take_queue(self.queue.id());
                    completed.result.map_err(map_blk_err_to_ax_err)?;
                    return Ok(completed);
                }
                RequestPoll::Pending => {
                    #[cfg(feature = "irq")]
                    if self.irq_events.take_queue(self.queue.id()) {
                        continue;
                    }
                    core::hint::spin_loop();
                }
            }
        }
    }
}

impl BlockBufferPool {
    fn alloc(&self, direction: DmaDirection) -> AxResult<ContiguousArray<u8>> {
        self.dma
            .contiguous_array_zero_with_align(self.size, self.align, direction)
            .map_err(BlkError::from)
            .map_err(map_blk_err_to_ax_err)
    }

    fn alloc_owned(&self, direction: DmaDirection, len: usize) -> AxResult<CpuDmaBuffer> {
        CpuDmaBuffer::new_zero(
            &self.dma,
            core::num::NonZeroUsize::new(len).ok_or(AxError::BadState)?,
            self.align,
            direction,
        )
        .map_err(BlkError::from)
        .map_err(map_blk_err_to_ax_err)
    }
}

impl RuntimeQueue {
    #[cfg(feature = "irq")]
    fn id(&self) -> usize {
        match self {
            Self::Legacy(queue) => queue.id(),
            Self::Owned(queue) => queue.id(),
        }
    }

    fn info(&self) -> QueueInfo {
        match self {
            Self::Legacy(queue) => queue.info(),
            Self::Owned(queue) => queue.info(),
        }
    }

    fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    fn legacy_mut(&mut self) -> Option<&mut dyn IQueue> {
        match self {
            Self::Legacy(queue) => Some(&mut **queue),
            Self::Owned(_) => None,
        }
    }

    fn owned_mut(&mut self) -> Option<&mut QueueHandle> {
        match self {
            Self::Legacy(_) => None,
            Self::Owned(queue) => Some(queue),
        }
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for Block {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let info = dev.info.clone();
        let irq = info.irq_num();
        let mut interface = dev.interface.take().ok_or(AxError::BadState)?;
        let queue = interface
            .create_owned_queue()
            .map(RuntimeQueue::Owned)
            .or_else(|| interface.create_queue().map(RuntimeQueue::Legacy))
            .ok_or(AxError::BadState)?;
        #[cfg(feature = "irq")]
        let irq_events = Arc::new(BlockIrqEvents::default());
        let queues = BlockQueues::new(
            queue,
            #[cfg(feature = "irq")]
            Arc::clone(&irq_events),
        )?;

        #[cfg(feature = "irq")]
        let irq_handler = irq
            .as_ref()
            .and_then(|_| take_legacy_irq_handler(interface.as_mut()))
            .map(|handler| BlockIrqHandler::new(handler, irq_events));
        drop(dev);

        #[cfg(feature = "irq")]
        let info = if irq_handler.is_some() {
            info
        } else {
            BindingInfo::empty()
        };
        #[cfg(feature = "irq")]
        let irq_handler = irq_handler;
        #[cfg(not(feature = "irq"))]
        let info = {
            let _ = irq;
            BindingInfo::empty()
        };

        Ok(Self {
            name,
            info,
            irq_enabled: interface.is_irq_enabled(),
            #[cfg(feature = "irq")]
            irq_handler,
            interface,
            queues: BlockQueueLock::new(queues),
        })
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for RdifBlockDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let irqs = dev.info.irq_sources().to_vec();
        let interface = dev.interface.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            irqs,
            interface,
        })
    }
}

pub trait PlatformDeviceBlock {
    fn register_block<T: Interface>(self, dev: T) -> Option<usize>;
    fn register_block_with_info<T: Interface>(self, dev: T, info: BindingInfo) -> Option<usize>;
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: Interface>(self, dev: T) -> Option<usize> {
        self.register_block_with_info(dev, BindingInfo::empty())
    }

    fn register_block_with_info<T: Interface>(self, dev: T, info: BindingInfo) -> Option<usize> {
        register_block_with_info(self, dev, info)
    }
}

pub trait ProbeFdtBlock {
    fn register_block<T: Interface>(self, dev: T) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtBlock for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_block<T: Interface>(self, dev: T) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

pub trait ProbeAcpiBlock {
    fn register_block<T: Interface>(self, dev: T) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeAcpiBlock for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_block<T: Interface>(self, dev: T) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciBlock {
    fn register_block<T: Interface>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;
}

#[cfg(feature = "pci")]
impl ProbePciBlock for rdrive::probe::pci::ProbePci<'_> {
    fn register_block<T: Interface>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

fn register_block_with_info<T: Interface>(
    plat_dev: rdrive::PlatformDevice,
    dev: T,
    info: BindingInfo,
) -> Option<usize> {
    let name = dev.name().to_string();
    register_bound_device(
        plat_dev,
        PlatformBlockDevice::new(name, Box::new(dev), info),
    )
}

pub fn take_block_devices() -> Vec<Block> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|dev| match Block::try_from(dev) {
            Ok(block) => Some(block),
            Err(err) => {
                warn!("failed to take block device: {err:?}");
                None
            }
        })
        .collect()
}

pub fn take_rdif_block_devices() -> Vec<RdifBlockDevice> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|dev| match RdifBlockDevice::try_from(dev) {
            Ok(block) => Some(block),
            Err(err) => {
                warn!("failed to take rdif block device: {err:?}");
                None
            }
        })
        .collect()
}

#[deprecated(note = "use take_rdif_block_devices")]
pub fn take_raw_block_devices() -> Vec<RdifBlockDevice> {
    take_rdif_block_devices()
}

#[deprecated(note = "use RdifBlockDevice")]
pub type RawBlockDevice = RdifBlockDevice;

#[cfg(feature = "irq")]
fn take_legacy_irq_handler(
    interface: &mut dyn Interface,
) -> Option<Box<dyn rdif_block::IrqHandler>> {
    let has_legacy_source = interface.irq_sources().iter().any(|source| source.id == 0);
    has_legacy_source
        .then(|| interface.take_irq_handler(0))
        .flatten()
}

fn validate_io(info: QueueInfo, block_id: u64, len: usize) -> AxResult {
    let block_size = info.device.logical_block_size;
    if block_size == 0 || !len.is_multiple_of(block_size) {
        return Err(AxError::InvalidInput);
    }
    let block_count = len / block_size;
    let end = block_id
        .checked_add(block_count as u64)
        .ok_or(AxError::InvalidInput)?;
    if end > info.device.num_blocks {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

fn block_buffer_layout(info: QueueInfo, size: usize) -> AxResult<Layout> {
    let block_size = info.device.logical_block_size;
    if size < block_size {
        return Err(AxError::BadState);
    }
    Layout::from_size_align(size, info.limits.dma_alignment.max(1)).map_err(|_| AxError::BadState)
}

fn block_transfer_planner(info: QueueInfo) -> AxResult<TransferPlanner> {
    TransferPlanner::new(
        info.device,
        info.limits,
        TransferRuntimeCaps {
            max_transfer_bytes: MAX_BLOCK_BUFFER_SIZE,
            max_segments: usize::MAX,
        },
    )
    .map_err(map_blk_err_to_ax_err)
}

fn transfer_plan(planner: TransferPlanner, block_id: u64, len: usize) -> AxResult<TransferPlan> {
    planner.plan(block_id, len).map_err(map_blk_err_to_ax_err)
}

fn segments_from_dma(
    buffer: &mut ContiguousArray<u8>,
    chunk: TransferChunk,
) -> AxResult<Vec<Buffer<'_>>> {
    let base_virt = buffer.as_ptr().as_ptr();
    let base_bus = buffer.dma_addr().as_u64();
    let planned_segments = chunk.segments();
    let mut segments = Vec::with_capacity(planned_segments.len());
    for segment in planned_segments {
        let virt = unsafe { base_virt.add(segment.byte_offset) };
        let bus = base_bus
            .checked_add(segment.byte_offset as u64)
            .ok_or(AxError::InvalidInput)?;
        segments.push(unsafe { Buffer::from_raw_parts(virt, bus, segment.byte_len) });
    }
    Ok(segments)
}

fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => AxError::InvalidInput,
        BlkError::Io => AxError::Io,
        BlkError::Other(error) => {
            error!("Block device error: {error}");
            AxError::Io
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{boxed::Box, string::String, vec::Vec};
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::{
        alloc::{alloc_zeroed, dealloc},
        sync::Mutex,
    };

    use dma_api::{
        DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
    };
    use rdif_block::{
        DeviceInfo, DriverGeneric, IQueueOwned, PollError, QueueLimits, SubmitError,
        validate_request,
    };

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SyncOp {
        ForDevice {
            size: usize,
            direction: DmaDirection,
        },
        ForCpu {
            size: usize,
            direction: DmaDirection,
        },
    }

    #[derive(Default)]
    struct TrackingDma {
        ops: Mutex<Vec<SyncOp>>,
    }

    impl TrackingDma {
        fn has_read_device_sync(&self) -> bool {
            self.ops.lock().unwrap().iter().any(|op| {
                matches!(
                    op,
                    SyncOp::ForDevice {
                        size: 512,
                        direction: DmaDirection::FromDevice
                    }
                )
            })
        }

        fn sync_count(&self, expected: SyncOp) -> usize {
            self.ops
                .lock()
                .unwrap()
                .iter()
                .filter(|op| **op == expected)
                .count()
        }
    }

    impl DmaOp for TrackingDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let ptr = unsafe { alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
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

        fn sync_alloc_for_device(
            &self,
            _handle: &DmaAllocHandle,
            _offset: usize,
            size: usize,
            direction: DmaDirection,
        ) {
            self.ops
                .lock()
                .unwrap()
                .push(SyncOp::ForDevice { size, direction });
        }

        fn sync_alloc_for_cpu(
            &self,
            _handle: &DmaAllocHandle,
            _offset: usize,
            size: usize,
            direction: DmaDirection,
        ) {
            self.ops
                .lock()
                .unwrap()
                .push(SyncOp::ForCpu { size, direction });
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SubmittedRequest {
        op: RequestOp,
        lba: u64,
        block_count: u32,
        data_len: usize,
        segment_lengths: Vec<usize>,
    }

    #[derive(Default)]
    struct RequestLog {
        requests: Mutex<Vec<SubmittedRequest>>,
    }

    impl RequestLog {
        fn push(&self, request: SubmittedRequest) {
            self.requests.lock().unwrap().push(request);
        }

        fn snapshot(&self) -> Vec<SubmittedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    struct TestInterface;

    impl DriverGeneric for TestInterface {
        fn name(&self) -> &str {
            "test-block"
        }
    }

    impl Interface for TestInterface {
        fn device_info(&self) -> DeviceInfo {
            DeviceInfo {
                name: Some("test-block"),
                ..DeviceInfo::new(8, 512)
            }
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
            None
        }
    }

    #[test]
    fn platform_block_device_exposes_binding_info_irq_num() {
        let irq = 47;
        let device = PlatformBlockDevice::new(
            "test-block".into(),
            Box::new(TestInterface),
            BindingInfo::with_irq(Some(irq)).unwrap(),
        );

        assert_eq!(BoundDevice::binding_info(&device).irq_num(), Some(irq));
        assert_eq!(BoundDevice::irq_num(&device), Some(irq));
    }

    struct TestQueue {
        dma: &'static TrackingDma,
    }

    // SAFETY: The queue copies data synchronously during `submit_request` and
    // never stores segment pointers after the call returns.
    unsafe impl IQueue for TestQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 0,
                device: DeviceInfo {
                    name: Some("test-block"),
                    ..DeviceInfo::new(8, 512)
                },
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            assert!(
                self.dma.has_read_device_sync(),
                "read DMA buffers must be synchronized for device ownership before submit"
            );
            validate_request(self.info(), &request)?;
            request.segments[0].fill(0x5a);
            Ok(RequestId::new(0))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    struct RecordingQueue {
        info: QueueInfo,
        log: &'static RequestLog,
    }

    impl RecordingQueue {
        fn new(log: &'static RequestLog, limits: QueueLimits) -> Self {
            Self {
                info: QueueInfo {
                    id: 0,
                    device: DeviceInfo {
                        name: Some("recording-block"),
                        ..DeviceInfo::new(64, 512)
                    },
                    limits,
                },
                log,
            }
        }
    }

    struct OwnedRecordingQueue {
        info: QueueInfo,
        log: &'static RequestLog,
        pending: Option<CompletedRequest>,
    }

    impl OwnedRecordingQueue {
        fn new(log: &'static RequestLog, limits: QueueLimits) -> Self {
            Self {
                info: QueueInfo {
                    id: 0,
                    device: DeviceInfo {
                        name: Some("owned-recording-block"),
                        ..DeviceInfo::new(64, 512)
                    },
                    limits,
                },
                log,
                pending: None,
            }
        }
    }

    impl IQueueOwned for OwnedRecordingQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
            if let Err(err) = rdif_block::validate_owned_request(self.info, &request) {
                return Err(SubmitError::new(err, request));
            }
            let id = RequestId::new(self.log.snapshot().len());
            let op = request.op;
            let lba = request.lba;
            let block_count = request.block_count;
            let flags = request.flags;
            let data_len = request.data_len();
            let mut completed_data = request.data.map(|data| data.into_cpu_buffer());
            if op == RequestOp::Read {
                let Some(buffer) = completed_data.as_mut() else {
                    return Err(SubmitError::new(
                        BlkError::InvalidRequest,
                        OwnedRequest {
                            op,
                            lba,
                            block_count,
                            data: None,
                            flags,
                        },
                    ));
                };
                unsafe {
                    buffer.as_mut_slice_cpu().fill(block_count as u8);
                }
            }
            self.log.push(SubmittedRequest {
                op,
                lba,
                block_count,
                data_len,
                segment_lengths: alloc::vec![data_len],
            });
            let completed_data = completed_data.map(|buffer| {
                let in_flight = unsafe { buffer.prepare_for_device().into_in_flight() };
                unsafe { in_flight.complete_after_quiesce() }
            });
            self.pending = Some(CompletedRequest::new(id, Ok(()), completed_data));
            Ok(id)
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestPoll, PollError> {
            Ok(self
                .pending
                .take()
                .map(RequestPoll::Ready)
                .unwrap_or(RequestPoll::Pending))
        }

        fn cancel_request(&mut self, _request: RequestId) -> Result<RequestPoll, PollError> {
            self.pending
                .take()
                .map(RequestPoll::Ready)
                .ok_or(PollError::UnknownRequest)
        }
    }

    // SAFETY: The queue records request metadata synchronously and never
    // accesses request segments after `submit_request` returns.
    unsafe impl IQueue for RecordingQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            validate_request(self.info, &request)?;
            if request.op == RequestOp::Read {
                request.segments[0].fill(request.block_count as u8);
            }
            self.log.push(SubmittedRequest {
                op: request.op,
                lba: request.lba,
                block_count: request.block_count,
                data_len: request.data_len(),
                segment_lengths: request.segments.iter().map(|segment| segment.len).collect(),
            });
            Ok(RequestId::new(self.log.snapshot().len()))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    fn block_with_queue(queue: Box<dyn IQueue>, dma: &'static TrackingDma) -> Block {
        let info = queue.info();
        let planner = block_transfer_planner(info).unwrap();
        let layout = block_buffer_layout(info, planner.chunk_size()).unwrap();
        Block {
            name: String::from("test-block"),
            info: BindingInfo::empty(),
            irq_enabled: false,
            #[cfg(feature = "irq")]
            irq_handler: None,
            interface: Box::new(TestInterface),
            queues: BlockQueueLock::new(BlockQueues {
                queue: RuntimeQueue::Legacy(queue),
                pool: BlockBufferPool {
                    dma: DeviceDma::new_legacy(u64::MAX, dma),
                    planner,
                    size: layout.size(),
                    align: layout.align(),
                },
                #[cfg(feature = "irq")]
                irq_events: Arc::new(BlockIrqEvents::default()),
            }),
        }
    }

    #[test]
    fn read_block_syncs_dma_buffer_for_device_before_submit() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let mut block = block_with_queue(Box::new(TestQueue { dma }), dma);
        let mut buf = [0_u8; 512];

        block.read_block(0, &mut buf).unwrap();

        assert_eq!(buf, [0x5a; 512]);
    }

    #[test]
    fn write_block_batches_requests_to_queue_limits() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_alignment: 4096,
            max_inflight: 1,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let buf = [0x42_u8; 8192];

        block.write_block(4, &buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [
                SubmittedRequest {
                    op: RequestOp::Write,
                    lba: 4,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
                SubmittedRequest {
                    op: RequestOp::Write,
                    lba: 12,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
            ]
        );
        assert_eq!(
            dma.sync_count(SyncOp::ForDevice {
                size: 4096,
                direction: DmaDirection::ToDevice,
            }),
            2
        );
    }

    #[test]
    fn read_block_batches_requests_to_queue_limits() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_alignment: 4096,
            max_inflight: 1,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let mut buf = [0_u8; 8192];

        block.read_block(4, &mut buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [
                SubmittedRequest {
                    op: RequestOp::Read,
                    lba: 4,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
                SubmittedRequest {
                    op: RequestOp::Read,
                    lba: 12,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
            ]
        );
        assert_eq!(buf, [8; 8192]);
        assert_eq!(
            dma.sync_count(SyncOp::ForDevice {
                size: 4096,
                direction: DmaDirection::FromDevice,
            }),
            2
        );
        assert_eq!(
            dma.sync_count(SyncOp::ForCpu {
                size: 4096,
                direction: DmaDirection::FromDevice,
            }),
            2
        );
    }

    #[test]
    fn owned_queue_read_returns_completed_dma_to_caller_buffer() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_alignment: 4096,
            max_inflight: 1,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let queue = QueueHandle::new(Box::new(OwnedRecordingQueue::new(log, limits)));
        let info = queue.info();
        let planner = block_transfer_planner(info).unwrap();
        let layout = block_buffer_layout(info, planner.chunk_size()).unwrap();
        let mut queues = BlockQueues {
            queue: RuntimeQueue::Owned(queue),
            pool: BlockBufferPool {
                dma: DeviceDma::new_legacy(u64::MAX, dma),
                planner,
                size: layout.size(),
                align: layout.align(),
            },
            #[cfg(feature = "irq")]
            irq_events: Arc::new(BlockIrqEvents::default()),
        };
        let mut buf = [0_u8; 4096];

        let dma_buffer = queues
            .pool
            .alloc_owned(DmaDirection::FromDevice, buf.len())
            .unwrap();
        let request_id = queues
            .submit_owned_request(OwnedRequest {
                op: RequestOp::Read,
                lba: 4,
                block_count: 8,
                data: Some(dma_buffer.prepare_for_device()),
                flags: RequestFlags::NONE,
            })
            .unwrap();
        let completed = queues.poll_owned_until_complete(request_id).unwrap();
        let completed_dma = completed.data.unwrap();
        completed_dma.copy_from_device_to_slice(&mut buf);

        assert_eq!(buf, [8; 4096]);
        assert_eq!(
            log.snapshot(),
            [SubmittedRequest {
                op: RequestOp::Read,
                lba: 4,
                block_count: 8,
                data_len: 4096,
                segment_lengths: alloc::vec![4096],
            }]
        );
    }

    #[test]
    fn block_transfer_planner_caps_large_finite_segments() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("large-segment-block"),
                ..DeviceInfo::new(64, 512)
            },
            limits: QueueLimits {
                dma_mask: u64::MAX,
                dma_domain: dma_api::DmaDomainId::legacy_global(),
                dma_alignment: 4096,
                max_inflight: 1,
                max_blocks_per_request: 4096,
                max_segments: 1,
                max_segment_size: 2 * 1024 * 1024,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        };

        assert_eq!(
            block_transfer_planner(info).unwrap().chunk_size(),
            16 * 1024
        );
    }

    #[test]
    fn block_transfer_planner_uses_simple_limit_preference_for_unbounded_segments() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("simple-block"),
                ..DeviceInfo::new(64, 512)
            },
            limits: QueueLimits::simple(512, u64::MAX),
        };

        assert_eq!(block_transfer_planner(info).unwrap().chunk_size(), 512);
    }

    #[test]
    fn block_transfer_planner_applies_runtime_cap_without_unbounded_segment_special_case() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("unbounded-large-preference-block"),
                ..DeviceInfo::new(128, 512)
            },
            limits: QueueLimits {
                dma_mask: u64::MAX,
                dma_domain: dma_api::DmaDomainId::legacy_global(),
                dma_alignment: 4096,
                max_inflight: 1,
                max_blocks_per_request: u32::MAX,
                max_segments: 1,
                max_segment_size: usize::MAX,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        };

        assert_eq!(
            block_transfer_planner(info).unwrap().chunk_size(),
            MAX_BLOCK_BUFFER_SIZE
        );
    }

    #[test]
    fn write_block_uses_planned_multi_segment_chunks() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_alignment: 4096,
            max_inflight: 1,
            max_blocks_per_request: 8,
            max_segments: 4,
            max_segment_size: 1024,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let buf = [0x42_u8; 4096];

        block.write_block(4, &buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [SubmittedRequest {
                op: RequestOp::Write,
                lba: 4,
                block_count: 8,
                data_len: 4096,
                segment_lengths: alloc::vec![1024, 1024, 1024, 1024],
            }]
        );
    }
}
