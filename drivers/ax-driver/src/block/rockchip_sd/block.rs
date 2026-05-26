use alloc::vec::Vec;
use core::{
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::DeviceDma;
use dwmmc_host::{BlockPoll, BlockRequest, BlockRequestSlot, DwMmc, RequestId};
use log::warn;
use rdrive::DriverGeneric;
use sdmmc_protocol::{BlockTransferMode, Error, sdio::SdioHost};

use super::{BLOCK_SIZE, RockchipDwMmc};
use crate::block::SharedDriver;

pub(super) struct SdBlockDevice {
    pub(super) raw: Option<SharedDriver<RockchipDwMmc>>,
    pub(super) capacity_blocks: u64,
    pub(super) irq_enabled: AtomicBool,
    pub(super) read_queue_created: bool,
    pub(super) write_queue_created: bool,
    pub(super) irq_handler_taken: bool,
}

struct SdReadQueue {
    inner: SdBlockQueue,
}

struct SdWriteQueue {
    inner: SdBlockQueue,
}

struct SdBlockQueue {
    raw: SharedDriver<RockchipDwMmc>,
    capacity_blocks: u64,
    id: usize,
    dma: DeviceDma,
    slot: BlockRequestSlot,
    pending: Option<BlockRequest>,
    completed: Vec<rdif_block::RequestId>,
}

impl DriverGeneric for SdBlockDevice {
    fn name(&self) -> &str {
        "rockchip-sd"
    }
}

impl rdif_block::Interface for SdBlockDevice {
    fn create_read_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IReadQueue>> {
        if self.read_queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.read_queue_created = true;
            alloc::boxed::Box::new(SdReadQueue {
                inner: SdBlockQueue::new(dev.clone(), self.capacity_blocks, 0),
            }) as _
        })
    }

    fn create_write_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IWriteQueue>> {
        if self.write_queue_created {
            return None;
        }
        self.raw.as_ref().map(|dev| {
            self.write_queue_created = true;
            alloc::boxed::Box::new(SdWriteQueue {
                inner: SdBlockQueue::new(dev.clone(), self.capacity_blocks, 0),
            }) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(raw) = &self.raw {
            let mut enabled = false;
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                    warn!("rockchip-dwmmc: enable completion IRQ failed: {:?}", err);
                    return;
                }
                enabled = true;
            });
            self.irq_enabled.store(enabled, Ordering::Release);
        }
    }

    fn disable_irq(&self) {
        if let Some(raw) = &self.raw {
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                    warn!("rockchip-dwmmc: disable completion IRQ failed: {:?}", err);
                }
            });
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn take_irq_handler(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IrqHandler>> {
        if self.irq_handler_taken {
            return None;
        }
        let raw = self.raw.as_ref()?.clone();
        self.irq_handler_taken = true;
        Some(alloc::boxed::Box::new(SdBlockIrqHandler { raw }))
    }
}

struct SdBlockIrqHandler {
    raw: SharedDriver<RockchipDwMmc>,
}

impl rdif_block::IrqHandler for SdBlockIrqHandler {
    fn handle_irq(&self) -> rdif_block::Event {
        self.raw
            .try_with_mut(|raw| block_event_from_dwmmc_irq(raw.host_mut().handle_irq()))
            .unwrap_or_else(rdif_block::Event::none)
    }
}

fn block_event_from_dwmmc_irq(irq_event: dwmmc_host::Event) -> rdif_block::Event {
    match irq_event {
        dwmmc_host::Event::None => rdif_block::Event::none(),
        dwmmc_host::Event::CommandComplete
        | dwmmc_host::Event::TransferComplete
        | dwmmc_host::Event::ReceiveReady
        | dwmmc_host::Event::TransmitReady
        | dwmmc_host::Event::Error { .. }
        | dwmmc_host::Event::Other { .. } => {
            let mut event = rdif_block::Event::none();
            event.read_queue.insert(0);
            event.write_queue.insert(0);
            event
        }
    }
}

impl rdif_block::QueueInfo for SdBlockQueue {
    fn num_blocks(&self) -> usize {
        self.capacity_blocks as usize
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn id(&self) -> usize {
        self.id
    }

    fn buffer_config(&self) -> rdif_block::BuffConfig {
        rdif_block::BuffConfig {
            dma_mask: self.dma.dma_mask(),
            align: BLOCK_SIZE,
            size: BLOCK_SIZE,
        }
    }
}

impl rdif_block::QueueInfo for SdReadQueue {
    fn num_blocks(&self) -> usize {
        self.inner.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn id(&self) -> usize {
        self.inner.id()
    }

    fn buffer_config(&self) -> rdif_block::BuffConfig {
        self.inner.buffer_config()
    }
}

impl rdif_block::QueueInfo for SdWriteQueue {
    fn num_blocks(&self) -> usize {
        self.inner.num_blocks()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn id(&self) -> usize {
        self.inner.id()
    }

    fn buffer_config(&self) -> rdif_block::BuffConfig {
        self.inner.buffer_config()
    }
}

impl rdif_block::IReadQueue for SdReadQueue {
    fn submit_read(
        &mut self,
        request: rdif_block::RequestRead<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.inner.submit_read(request)
    }

    fn poll_read(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        self.inner.poll_request(request)
    }
}

impl rdif_block::IWriteQueue for SdWriteQueue {
    fn submit_write(
        &mut self,
        request: rdif_block::RequestWrite<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.inner.submit_write(request)
    }

    fn poll_write(
        &mut self,
        request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        self.inner.poll_request(request)
    }
}

impl SdBlockQueue {
    fn new(raw: SharedDriver<RockchipDwMmc>, capacity_blocks: u64, id: usize) -> Self {
        Self {
            raw,
            capacity_blocks,
            id,
            dma: axklib::dma::device_with_mask(u32::MAX as u64),
            slot: BlockRequestSlot::default(),
            pending: None,
            completed: Vec::new(),
        }
    }

    fn submit_read(
        &mut self,
        request: rdif_block::RequestRead<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.reap_pending_request()?;
        let raw = self.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.block_id, raw.is_high_capacity())?;
            let buffer = request.buffer;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(rdif_block::BlkError::Other(
                    "read buffer is not block aligned".into(),
                ));
            }
            let ptr = NonNull::new(buffer.virt)
                .ok_or_else(|| rdif_block::BlkError::Other("read buffer pointer is null".into()))?;
            let size = NonZeroUsize::new(buffer.len())
                .ok_or_else(|| rdif_block::BlkError::Other("read buffer is empty".into()))?;
            let id = submit_read_request(
                raw.host_mut(),
                start_block,
                ptr,
                size,
                &self.dma,
                &mut self.slot,
                &mut self.pending,
            )?;
            Ok(rdif_block::RequestId::new(usize::from(id)))
        })
    }

    fn submit_write(
        &mut self,
        request: rdif_block::RequestWrite<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.reap_pending_request()?;
        let raw = self.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.block_id, raw.is_high_capacity())?;
            let buffer = request.buffer;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(rdif_block::BlkError::Other(
                    "write buffer is not block aligned".into(),
                ));
            }
            let ptr = NonNull::new(buffer.virt).ok_or_else(|| {
                rdif_block::BlkError::Other("write buffer pointer is null".into())
            })?;
            let size = NonZeroUsize::new(buffer.len())
                .ok_or_else(|| rdif_block::BlkError::Other("write buffer is empty".into()))?;
            let id = submit_write_request(
                raw.host_mut(),
                start_block,
                ptr,
                size,
                &self.dma,
                &mut self.slot,
                &mut self.pending,
            )?;
            Ok(rdif_block::RequestId::new(usize::from(id)))
        })
    }

    fn poll_request(
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
            raw.host_mut().poll_block_request(
                &mut self.pending,
                RequestId::new(usize::from(request)),
                &mut self.slot,
            )
        }) {
            Ok(BlockPoll::Complete) => Ok(rdif_block::RequestStatus::Complete),
            Ok(BlockPoll::Pending) => Ok(rdif_block::RequestStatus::Pending),
            Ok(_) => Err(rdif_block::BlkError::Other(
                "DWMMC returned an unknown poll state".into(),
            )),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn pending_id(&self) -> Option<RequestId> {
        self.pending.as_ref().map(BlockRequest::id)
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

fn submit_read_request(
    host: &mut DwMmc,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: &DeviceDma,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<RequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_read_blocks(
        start_block,
        buffer,
        size,
        Some(dma),
        BlockTransferMode::Dma,
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => host
            .submit_read_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn submit_write_request(
    host: &mut DwMmc,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: &DeviceDma,
    slot: &mut BlockRequestSlot,
    pending: &mut Option<BlockRequest>,
) -> Result<RequestId, rdif_block::BlkError> {
    if pending.is_some() {
        return Err(rdif_block::BlkError::Retry);
    }
    let request = match host.submit_write_blocks(
        start_block,
        buffer,
        size,
        Some(dma),
        BlockTransferMode::Dma,
        slot,
    ) {
        Ok(request) => request,
        Err(err) if can_fallback_to_fifo(err) => host
            .submit_write_blocks(
                start_block,
                buffer,
                size,
                None,
                BlockTransferMode::Fifo,
                slot,
            )
            .map_err(map_dev_err_to_blk_err)?,
        Err(err) => return Err(map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(id)
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

fn block_addr_for_card(block_id: usize, high_capacity: bool) -> Result<u32, rdif_block::BlkError> {
    let block_id =
        u32::try_from(block_id).map_err(|_| rdif_block::BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(rdif_block::BlkError::InvalidBlockIndex(block_id as usize))
    }
}

fn map_dev_err_to_blk_err(err: Error) -> rdif_block::BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => {
            rdif_block::BlkError::NotSupported
        }
        Error::Misaligned | Error::InvalidArgument => {
            rdif_block::BlkError::Other("SD/MMC request is not block aligned".into())
        }
        _ => rdif_block::BlkError::Other("DWMMC I/O error".into()),
    }
}
