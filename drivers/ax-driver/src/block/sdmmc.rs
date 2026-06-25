use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    marker::PhantomData,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::DeviceDma;
use log::warn;
use rdrive::DriverGeneric;
use sdmmc_protocol::{
    BlockPoll, BlockRequestId, Error,
    sdio::{SdioHost, SdioIrqHandle, SdioIrqHost, SdioSdmmc, block_queue_ready_from_host_event},
};

use crate::block::SharedDriver;

pub(crate) const BLOCK_SIZE: usize = 512;

#[derive(Clone, Copy)]
pub(crate) struct SdmmcBlockConfig {
    pub name: &'static str,
    pub capacity_blocks: u64,
    pub dma_mask: u64,
    pub max_blocks_per_request: u32,
    pub max_segment_size: usize,
    pub irq_driven: bool,
    pub use_dma: bool,
}

impl SdmmcBlockConfig {
    #[cfg(any(
        feature = "k230-sdhci",
        feature = "rockchip-dwmmc",
        feature = "rockchip-sdhci",
        test
    ))]
    pub(crate) fn dma(name: &'static str, capacity_blocks: u64, irq_driven: bool) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: u32::MAX as u64,
            max_blocks_per_request: u16::MAX as u32 + 1,
            max_segment_size: usize::MAX,
            irq_driven,
            use_dma: true,
        }
    }

    #[cfg(any(
        feature = "rockchip-sdhci",
        feature = "phytium-mci",
        feature = "starfive-jh7110-dwmmc",
        test
    ))]
    pub(crate) fn fifo(name: &'static str, capacity_blocks: u64, irq_driven: bool) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: u32::MAX as u64,
            max_blocks_per_request: 1,
            max_segment_size: BLOCK_SIZE,
            irq_driven,
            use_dma: false,
        }
    }
}

pub(crate) trait SdmmcBlockHost: SdioIrqHost + Send + Sync + 'static {
    type Request: Send + 'static;
    type Slot: Default + Send + 'static;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError>;

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError>;

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error>;

    fn request_id(request: &Self::Request) -> BlockRequestId;
}

pub(crate) struct SdmmcBlockDevice<H>
where
    H: SdmmcBlockHost,
{
    raw: Option<SharedDriver<SdioSdmmc<H>>>,
    irq_handle: <H as SdioIrqHost>::IrqHandle,
    config: SdmmcBlockConfig,
    irq_enabled: AtomicBool,
    queue_created: bool,
    irq_handler_taken: bool,
}

impl<H> SdmmcBlockDevice<H>
where
    H: SdmmcBlockHost,
{
    pub(crate) fn new(raw: SharedDriver<SdioSdmmc<H>>, config: SdmmcBlockConfig) -> Self {
        let irq_handle = raw.with_mut(|raw| raw.host().irq_handle());
        Self {
            raw: Some(raw),
            irq_handle,
            config,
            irq_enabled: AtomicBool::new(false),
            queue_created: false,
            irq_handler_taken: false,
        }
    }

    fn queue_limits_with_mask(&self, dma_mask: u64) -> rdif_block::QueueLimits {
        queue_limits(&self.config, dma_mask)
    }
}

impl<H> DriverGeneric for SdmmcBlockDevice<H>
where
    H: SdmmcBlockHost,
{
    fn name(&self) -> &str {
        self.config.name
    }
}

impl<H> rdif_block::Interface for SdmmcBlockDevice<H>
where
    H: SdmmcBlockHost,
{
    fn device_info(&self) -> rdif_block::DeviceInfo {
        device_info(&self.config)
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        self.queue_limits_with_mask(self.config.dma_mask)
    }

    fn create_queue(&mut self) -> Option<Box<dyn rdif_block::IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.clone().as_ref().map(|dev| {
            self.queue_created = true;
            Box::new(SdmmcBlockQueue::<H>::new(
                dev.clone(),
                self.config.name,
                self.config.capacity_blocks,
                self.config,
                0,
            )) as _
        })
    }

    fn enable_irq(&self) {
        if !self.config.irq_driven {
            self.irq_enabled.store(false, Ordering::Release);
            return;
        }
        if let Some(raw) = &self.raw {
            let mut enabled = false;
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                    warn!(
                        "{}: enable completion IRQ failed: {:?}",
                        self.config.name, err
                    );
                    return;
                }
                enabled = raw.host().completion_irq_enabled();
            });
            self.irq_enabled.store(enabled, Ordering::Release);
        }
    }

    fn disable_irq(&self) {
        if let Some(raw) = &self.raw {
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                    warn!(
                        "{}: disable completion IRQ failed: {:?}",
                        self.config.name, err
                    );
                }
            });
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        if !self.config.irq_driven {
            return Vec::new();
        }
        vec![rdif_block::IrqSourceInfo::legacy(
            rdif_block::IdList::from_bits(1),
        )]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn rdif_block::IrqHandler>> {
        if !self.config.irq_driven || source_id != 0 {
            return None;
        }
        if self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(SdmmcBlockIrqHandler::<H> {
            handle: self.irq_handle.clone(),
            _marker: PhantomData,
        }))
    }
}

struct SdmmcBlockQueue<H>
where
    H: SdmmcBlockHost,
{
    raw: SharedDriver<SdioSdmmc<H>>,
    name: &'static str,
    capacity_blocks: u64,
    config: SdmmcBlockConfig,
    id: usize,
    dma: DeviceDma,
    slot: H::Slot,
    pending: Option<H::Request>,
    completed: Vec<rdif_block::RequestId>,
}

impl<H> SdmmcBlockQueue<H>
where
    H: SdmmcBlockHost,
{
    fn new(
        raw: SharedDriver<SdioSdmmc<H>>,
        name: &'static str,
        capacity_blocks: u64,
        config: SdmmcBlockConfig,
        id: usize,
    ) -> Self {
        let dma_mask = config.dma_mask;
        Self {
            raw,
            name,
            capacity_blocks,
            config,
            id,
            dma: axklib::dma::device_with_mask(dma_mask),
            slot: H::Slot::default(),
            pending: None,
            completed: Vec::new(),
        }
    }

    fn queue_info(&self) -> rdif_block::QueueInfo {
        rdif_block::IQueue::info(self)
    }

    fn submit_request_inner(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        rdif_block::validate_request(self.queue_info(), &request)?;
        self.reap_pending_request()?;
        let raw = self.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.lba, raw.is_high_capacity())?;
            let buffer = request
                .segments
                .first()
                .copied()
                .ok_or(rdif_block::BlkError::InvalidRequest)?;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(rdif_block::BlkError::Other("buffer is not block aligned"));
            }
            let ptr = NonNull::new(buffer.virt)
                .ok_or(rdif_block::BlkError::Other("buffer pointer is null"))?;
            let size = NonZeroUsize::new(buffer.len())
                .ok_or(rdif_block::BlkError::Other("buffer is empty"))?;
            let id = match request.op {
                rdif_block::RequestOp::Read => H::submit_read_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    transfer_dma(self.config.use_dma, &self.dma),
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Write => H::submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    transfer_dma(self.config.use_dma, &self.dma),
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => {
                    return Err(rdif_block::BlkError::NotSupported);
                }
            };
            Ok(rdif_block::RequestId::new(usize::from(id)))
        })
    }

    fn poll_request_inner(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(rdif_block::RequestStatus::Complete);
        }
        self.poll_active_request(request)
    }

    fn poll_active_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        let raw = self.raw.clone();
        match raw.with_mut(|raw| {
            H::poll_block_request(
                raw.host_mut(),
                &mut self.pending,
                BlockRequestId::new(usize::from(request)),
                &mut self.slot,
            )
        }) {
            Ok(BlockPoll::Complete) => Ok(rdif_block::RequestStatus::Complete),
            Ok(BlockPoll::Pending) => Ok(rdif_block::RequestStatus::Pending),
            Ok(_) => Err(rdif_block::BlkError::Other(
                "SD/MMC returned an unknown poll state",
            )),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn pending_id(&self) -> Option<BlockRequestId> {
        self.pending.as_ref().map(H::request_id)
    }

    fn reap_pending_request(&mut self) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        let Some(active) = self.pending_id() else {
            return Ok(rdif_block::RequestStatus::Complete);
        };
        let id = rdif_block::RequestId::new(usize::from(active));
        match self.poll_active_request(id) {
            Ok(rdif_block::RequestStatus::Complete) => {
                self.completed.push(id);
                Ok(rdif_block::RequestStatus::Complete)
            }
            Ok(rdif_block::RequestStatus::Pending) => Err(rdif_block::BlkError::Retry),
            Err(err) => Err(err),
        }
    }
}

// SAFETY: `SdmmcBlockQueue` owns a single pending request slot and host
// request state owns any segment access until task-side poll completes it.
unsafe impl<H> rdif_block::IQueue for SdmmcBlockQueue<H>
where
    H: SdmmcBlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: rdif_block::DeviceInfo {
                name: Some(self.name),
                ..rdif_block::DeviceInfo::new(self.capacity_blocks, BLOCK_SIZE)
            },
            limits: queue_limits(&self.config, self.dma.dma_mask()),
        }
    }

    fn submit_request(
        &mut self,
        request: rdif_block::Request<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.submit_request_inner(request)
    }

    fn poll_request(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        self.poll_request_inner(request)
    }
}

struct SdmmcBlockIrqHandler<H>
where
    H: SdmmcBlockHost,
{
    handle: <H as SdioIrqHost>::IrqHandle,
    _marker: PhantomData<H>,
}

impl<H> rdif_block::IrqHandler for SdmmcBlockIrqHandler<H>
where
    H: SdmmcBlockHost,
{
    fn handle_irq(&self) -> rdif_block::Event {
        let host_event = self.handle.handle_irq();
        let mut event = rdif_block::Event::none();
        if let Some(queue_id) = block_queue_ready_from_host_event(&host_event) {
            event.push_queue(queue_id);
        }
        event
    }
}

pub(crate) fn queue_limits(config: &SdmmcBlockConfig, dma_mask: u64) -> rdif_block::QueueLimits {
    rdif_block::QueueLimits {
        dma_mask,
        dma_alignment: BLOCK_SIZE,
        max_inflight: 1,
        max_blocks_per_request: config.max_blocks_per_request,
        max_segments: 1,
        max_segment_size: config.max_segment_size,
        supported_flags: rdif_block::RequestFlags::NONE,
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}

pub(crate) fn device_info(config: &SdmmcBlockConfig) -> rdif_block::DeviceInfo {
    rdif_block::DeviceInfo {
        name: Some(config.name),
        ..rdif_block::DeviceInfo::new(config.capacity_blocks, BLOCK_SIZE)
    }
}

pub(crate) fn block_addr_for_card(
    block_id: u64,
    high_capacity: bool,
) -> Result<u32, rdif_block::BlkError> {
    let block_id =
        u32::try_from(block_id).map_err(|_| rdif_block::BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(rdif_block::BlkError::InvalidBlockIndex(block_id as u64))
    }
}

pub(crate) fn map_dev_err_to_blk_err(err: Error) -> rdif_block::BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => {
            rdif_block::BlkError::NotSupported
        }
        Error::Misaligned | Error::InvalidArgument => {
            rdif_block::BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => rdif_block::BlkError::Io,
    }
}

#[cfg(any(feature = "k230-sdhci", feature = "rockchip-sdhci"))]
impl SdmmcBlockHost for sdhci_host::Sdhci {
    type Request = sdhci_host::BlockRequest;
    type Slot = sdhci_host::BlockRequestSlot;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        submit_sdhci_read_request(self, start_block, buffer, size, dma, slot, pending)
    }

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        submit_sdhci_write_request(self, start_block, buffer, size, dma, slot, pending)
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        self.poll_block_request(
            pending,
            sdhci_host::RequestId::new(usize::from(request)),
            slot,
        )
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        BlockRequestId::new(usize::from(request.id()))
    }
}

#[cfg(any(feature = "k230-sdhci", feature = "rockchip-sdhci"))]
pub(crate) fn submit_sdhci_read_request(
    host: &mut sdhci_host::Sdhci,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: Option<&DeviceDma>,
    slot: &mut sdhci_host::BlockRequestSlot,
    pending: &mut Option<sdhci_host::BlockRequest>,
) -> Result<BlockRequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_read_blocks(
        start_block,
        buffer,
        size,
        dma,
        transfer_mode_for_dma(dma),
        slot,
    ) {
        Ok(request) => request,
        Err(err) if dma.is_some() && can_fallback_to_fifo(err) => host
            .submit_read_blocks(
                start_block,
                buffer,
                size,
                None,
                sdmmc_protocol::BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(BlockRequestId::new(usize::from(id)))
}

#[cfg(any(feature = "k230-sdhci", feature = "rockchip-sdhci"))]
pub(crate) fn submit_sdhci_write_request(
    host: &mut sdhci_host::Sdhci,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: Option<&DeviceDma>,
    slot: &mut sdhci_host::BlockRequestSlot,
    pending: &mut Option<sdhci_host::BlockRequest>,
) -> Result<BlockRequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_write_blocks(
        start_block,
        buffer,
        size,
        dma,
        transfer_mode_for_dma(dma),
        slot,
    ) {
        Ok(request) => request,
        Err(err) if dma.is_some() && can_fallback_to_fifo(err) => host
            .submit_write_blocks(
                start_block,
                buffer,
                size,
                None,
                sdmmc_protocol::BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(BlockRequestId::new(usize::from(id)))
}

#[cfg(any(feature = "rockchip-dwmmc", feature = "starfive-jh7110-dwmmc"))]
impl SdmmcBlockHost for dwmmc_host::DwMmc {
    type Request = dwmmc_host::BlockRequest;
    type Slot = dwmmc_host::BlockRequestSlot;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        if pending.is_some() {
            return Err(rdif_block::BlkError::Retry);
        }
        let request = match self.submit_read_blocks(
            start_block,
            buffer,
            size,
            dma,
            transfer_mode_for_dma(dma),
            slot,
        ) {
            Ok(request) => request,
            Err(err) if dma.is_some() && can_fallback_to_fifo(err) => self
                .submit_read_blocks(
                    start_block,
                    buffer,
                    size,
                    None,
                    sdmmc_protocol::BlockTransferMode::Fifo,
                    slot,
                )
                .map_err(map_dev_err_to_blk_err)?,
            Err(err) => return Err(map_dev_err_to_blk_err(err)),
        };
        let id = request.id();
        *pending = Some(request);
        Ok(BlockRequestId::new(usize::from(id)))
    }

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        if pending.is_some() {
            return Err(rdif_block::BlkError::Retry);
        }
        let request = match self.submit_write_blocks(
            start_block,
            buffer,
            size,
            dma,
            transfer_mode_for_dma(dma),
            slot,
        ) {
            Ok(request) => request,
            Err(err) if dma.is_some() && can_fallback_to_fifo(err) => self
                .submit_write_blocks(
                    start_block,
                    buffer,
                    size,
                    None,
                    sdmmc_protocol::BlockTransferMode::Fifo,
                    slot,
                )
                .map_err(map_dev_err_to_blk_err)?,
            Err(err) => return Err(map_dev_err_to_blk_err(err)),
        };
        let id = request.id();
        *pending = Some(request);
        Ok(BlockRequestId::new(usize::from(id)))
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        self.poll_block_request(
            pending,
            dwmmc_host::RequestId::new(usize::from(request)),
            slot,
        )
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        BlockRequestId::new(usize::from(request.id()))
    }
}

#[cfg(feature = "phytium-mci")]
impl SdmmcBlockHost for phytium_mci_host::PhytiumMci {
    type Request = phytium_mci_host::BlockRequest;
    type Slot = phytium_mci_host::BlockRequestSlot;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        if pending.is_some() {
            return Err(rdif_block::BlkError::Retry);
        }
        let request = match self.submit_read_blocks(
            start_block,
            buffer,
            size,
            dma,
            transfer_mode_for_dma(dma),
            slot,
        ) {
            Ok(request) => request,
            Err(err) if dma.is_some() && can_fallback_to_fifo(err) => {
                warn!(
                    "phytium-mci: DMA read unavailable ({:?}); falling back to FIFO",
                    err
                );
                self.submit_read_blocks(
                    start_block,
                    buffer,
                    size,
                    None,
                    sdmmc_protocol::BlockTransferMode::Fifo,
                    slot,
                )
                .map_err(map_dev_err_to_blk_err)?
            }
            Err(err) => return Err(map_dev_err_to_blk_err(err)),
        };
        let id = request.id();
        *pending = Some(request);
        Ok(BlockRequestId::new(usize::from(id)))
    }

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, rdif_block::BlkError> {
        if pending.is_some() {
            return Err(rdif_block::BlkError::Retry);
        }
        let request = match self.submit_write_blocks(
            start_block,
            buffer,
            size,
            dma,
            transfer_mode_for_dma(dma),
            slot,
        ) {
            Ok(request) => request,
            Err(err) if dma.is_some() && can_fallback_to_fifo(err) => {
                warn!(
                    "phytium-mci: DMA write unavailable ({:?}); falling back to FIFO",
                    err
                );
                self.submit_write_blocks(
                    start_block,
                    buffer,
                    size,
                    None,
                    sdmmc_protocol::BlockTransferMode::Fifo,
                    slot,
                )
                .map_err(map_dev_err_to_blk_err)?
            }
            Err(err) => return Err(map_dev_err_to_blk_err(err)),
        };
        let id = request.id();
        *pending = Some(request);
        Ok(BlockRequestId::new(usize::from(id)))
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        self.poll_block_request(
            pending,
            phytium_mci_host::RequestId::new(usize::from(request)),
            slot,
        )
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        BlockRequestId::new(usize::from(request.id()))
    }
}

fn transfer_mode_for_dma(dma: Option<&DeviceDma>) -> sdmmc_protocol::BlockTransferMode {
    match dma {
        Some(_) => sdmmc_protocol::BlockTransferMode::Dma,
        None => sdmmc_protocol::BlockTransferMode::Fifo,
    }
}

fn transfer_dma(use_dma: bool, dma: &DeviceDma) -> Option<&DeviceDma> {
    use_dma.then_some(dma)
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use sdmmc_protocol::{
        CommandResponsePoll, DataCommandPoll,
        cmd::Command,
        sdio::{ClockSpeed, HostEvent, HostEventKind},
    };

    use super::*;

    #[test]
    fn disabled_irq_policy_does_not_advertise_sources() {
        let config = SdmmcBlockConfig::dma("test-sdmmc", 8, false);

        assert_eq!(queue_limits(&config, u32::MAX as u64).max_inflight, 1);
        assert_eq!(device_info(&config).name, Some("test-sdmmc"));
    }

    #[test]
    fn fifo_config_limits_single_block_requests() {
        let config = SdmmcBlockConfig::fifo("test-sdmmc", 8, true);
        let limits = queue_limits(&config, u32::MAX as u64);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, BLOCK_SIZE);
    }

    #[test]
    fn irq_policy_disabled_does_not_advertise_irq_sources() {
        let raw = SharedDriver::new(SdioSdmmc::new(MockHost::default()));
        let device = SdmmcBlockDevice::new(raw, SdmmcBlockConfig::dma("mock-sd", 8, false));

        assert!(rdif_block::Interface::irq_sources(&device).is_empty());
    }

    #[test]
    fn enabled_irq_handler_maps_host_event_to_queue_zero() {
        let raw = SharedDriver::new(SdioSdmmc::new(MockHost::default()));
        let mut device = SdmmcBlockDevice::new(raw, SdmmcBlockConfig::dma("mock-sd", 8, true));
        let handler = rdif_block::Interface::take_irq_handler(&mut device, 0).unwrap();

        let event = handler.handle_irq();

        assert!(event.queues.contains(0));
        assert!(!event.is_empty());
    }

    #[test]
    fn poll_request_only_completes_matching_request_id() {
        let raw = SharedDriver::new(SdioSdmmc::new(MockHost::default()));
        let mut queue = SdmmcBlockQueue::<MockHost> {
            raw,
            name: "mock-sd",
            capacity_blocks: 8,
            config: SdmmcBlockConfig::dma("mock-sd", 8, false),
            id: 0,
            dma: axklib::dma::device_with_mask(u32::MAX as u64),
            slot: MockSlot,
            pending: Some(MockRequest {
                id: BlockRequestId::new(7),
            }),
            completed: Vec::new(),
        };

        assert_eq!(
            queue.poll_request_inner(rdif_block::RequestId::new(8)),
            Ok(rdif_block::RequestStatus::Pending)
        );
        assert_eq!(
            queue.poll_request_inner(rdif_block::RequestId::new(7)),
            Ok(rdif_block::RequestStatus::Complete)
        );
        assert!(queue.pending.is_none());
    }

    #[derive(Clone, Default)]
    struct MockIrqHandle;

    impl SdioIrqHandle for MockIrqHandle {
        type Event = MockEvent;

        fn handle_irq(&self) -> Self::Event {
            MockEvent(HostEventKind::TransferComplete)
        }
    }

    #[derive(Clone, Copy, Default)]
    struct MockEvent(HostEventKind);

    impl HostEvent for MockEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }
    }

    #[derive(Default)]
    struct MockHost {
        irq_enabled: AtomicBool,
        next_id: AtomicUsize,
    }

    #[derive(Default)]
    struct MockSlot;

    struct MockRequest {
        id: BlockRequestId,
    }

    impl SdioHost for MockHost {
        type Event = MockEvent;
        type DataRequest<'a> = ();

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

        fn set_bus_width(&mut self, _width: sdmmc_protocol::sdio::BusWidth) -> Result<(), Error> {
            Ok(())
        }

        fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
            Ok(())
        }

        fn enable_completion_irq(&mut self) -> Result<(), Error> {
            self.irq_enabled.store(true, Ordering::Release);
            Ok(())
        }

        fn disable_completion_irq(&mut self) -> Result<(), Error> {
            self.irq_enabled.store(false, Ordering::Release);
            Ok(())
        }
    }

    impl SdioIrqHost for MockHost {
        type IrqHandle = MockIrqHandle;

        fn irq_handle(&self) -> Self::IrqHandle {
            MockIrqHandle
        }

        fn completion_irq_enabled(&self) -> bool {
            self.irq_enabled.load(Ordering::Acquire)
        }
    }

    impl SdmmcBlockHost for MockHost {
        type Request = MockRequest;
        type Slot = MockSlot;

        fn submit_read_request(
            &mut self,
            _start_block: u32,
            _buffer: NonNull<u8>,
            _size: NonZeroUsize,
            _dma: Option<&DeviceDma>,
            _slot: &mut Self::Slot,
            pending: &mut Option<Self::Request>,
        ) -> Result<BlockRequestId, rdif_block::BlkError> {
            self.submit_mock_request(pending)
        }

        fn submit_write_request(
            &mut self,
            _start_block: u32,
            _buffer: NonNull<u8>,
            _size: NonZeroUsize,
            _dma: Option<&DeviceDma>,
            _slot: &mut Self::Slot,
            pending: &mut Option<Self::Request>,
        ) -> Result<BlockRequestId, rdif_block::BlkError> {
            self.submit_mock_request(pending)
        }

        fn poll_block_request(
            &mut self,
            pending: &mut Option<Self::Request>,
            request: BlockRequestId,
            _slot: &mut Self::Slot,
        ) -> Result<BlockPoll, Error> {
            match pending.as_ref() {
                Some(active) if active.id == request => {
                    *pending = None;
                    Ok(BlockPoll::Complete)
                }
                Some(_) => Ok(BlockPoll::Pending),
                None => Ok(BlockPoll::Complete),
            }
        }

        fn request_id(request: &Self::Request) -> BlockRequestId {
            request.id
        }
    }

    impl MockHost {
        fn submit_mock_request(
            &self,
            pending: &mut Option<MockRequest>,
        ) -> Result<BlockRequestId, rdif_block::BlkError> {
            if pending.is_some() {
                return Err(rdif_block::BlkError::Retry);
            }
            let id = BlockRequestId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
            *pending = Some(MockRequest { id });
            Ok(id)
        }
    }
}
