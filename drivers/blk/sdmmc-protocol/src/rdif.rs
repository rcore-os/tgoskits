//! RDIF block-device bridge for SDIO-backed SD/MMC hosts.
//!
//! This module owns the reusable queue/runtime-independent part of adapting a
//! [`crate::sdio::SdioSdmmc`] card to [`rdif_block`]. Host crates provide the
//! small controller-specific [`BlockHost`] impl that submits and polls one
//! block request.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use log::warn;
use rdif_block::dma_api::{CompletedDma, DeviceDma, PreparedDma};
pub use rdif_block::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, BlkError, IQueue, IQueueOwned, Interface,
    IrqHandlerHandle, IrqHandlerSlot, OwnedRequest, PollError, QueueHandle, Request, RequestId,
    RequestPoll as OwnedRequestPoll, RequestStatus, SubmitError, dma_api,
};

use crate::{
    BlockPoll, BlockRequestId, BlockTransferMode, DataCommandPoll, Error,
    sdio::{
        SdioHost, SdioHost2Adapter, SdioHost2DataRequest, SdioHost2Irq, SdioIrqHandle, SdioIrqHost,
        SdioSdmmc, block_queue_ready_from_host_event,
    },
};

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_DMA_MASK: u64 = u32::MAX as u64;
pub const DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST: u32 = u16::MAX as u32 + 1;

#[derive(Clone)]
pub struct BlockConfig {
    pub name: &'static str,
    pub capacity_blocks: u64,
    pub dma_mask: u64,
    pub dma_domain: dma_api::DmaDomainId,
    pub max_blocks_per_request: u32,
    pub max_segment_size: usize,
    pub irq_driven: bool,
    pub dma: Option<DeviceDma>,
}

impl BlockConfig {
    pub fn dma(name: &'static str, capacity_blocks: u64, irq_driven: bool, dma: DeviceDma) -> Self {
        let dma_mask = dma.dma_mask();
        Self {
            name,
            capacity_blocks,
            dma_mask,
            dma_domain: dma.domain_id(),
            max_blocks_per_request: DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
            max_segment_size: usize::MAX,
            irq_driven,
            dma: Some(dma),
        }
    }

    pub const fn fifo(name: &'static str, capacity_blocks: u64, irq_driven: bool) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: DEFAULT_DMA_MASK,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            max_blocks_per_request: 1,
            max_segment_size: BLOCK_SIZE,
            irq_driven,
            dma: None,
        }
    }

    pub fn with_dma_mask(mut self, dma_mask: u64) -> Self {
        self.dma_mask = dma_mask;
        self
    }

    pub fn with_max_blocks_per_request(mut self, max_blocks_per_request: u32) -> Self {
        self.max_blocks_per_request = max_blocks_per_request;
        self
    }

    pub fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.max_segment_size = max_segment_size;
        self
    }

    pub fn with_irq_driven(mut self, irq_driven: bool) -> Self {
        self.irq_driven = irq_driven;
        self
    }

    pub fn with_dma(mut self, dma: DeviceDma) -> Self {
        self.dma_mask = dma.dma_mask();
        self.dma_domain = dma.domain_id();
        self.dma = Some(dma);
        self
    }

    pub const fn uses_dma(&self) -> bool {
        self.dma.is_some()
    }
}

pub trait BlockHost: SdioIrqHost + Send + Sync + 'static {
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
    ) -> Result<BlockRequestId, BlkError>;

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError>;

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error>;

    fn abort_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        slot: &mut Self::Slot,
    ) -> Result<(), Error>;

    fn request_id(request: &Self::Request) -> BlockRequestId;

    fn submit_owned_read_request(
        &mut self,
        _start_block: u32,
        buffer: PreparedDma,
        _slot: &mut Self::Slot,
        _pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        Err(OwnedBlockSubmitError::new(BlkError::NotSupported, buffer))
    }

    fn submit_owned_write_request(
        &mut self,
        _start_block: u32,
        buffer: PreparedDma,
        _slot: &mut Self::Slot,
        _pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        Err(OwnedBlockSubmitError::new(BlkError::NotSupported, buffer))
    }

    fn take_completed_dma(_slot: &mut Self::Slot) -> Option<CompletedDma> {
        None
    }
}

pub struct OwnedBlockSubmitError {
    error: BlkError,
    buffer: Box<PreparedDma>,
}

impl OwnedBlockSubmitError {
    fn new(error: BlkError, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    fn into_parts(self) -> (BlkError, PreparedDma) {
        (self.error, *self.buffer)
    }
}

#[derive(Default)]
pub struct ProtocolBlockSlot {
    next_id: usize,
    active_id: Option<BlockRequestId>,
    completed_dma: Option<CompletedDma>,
}

pub struct ProtocolBlockRequest<'a, H: SdioHost2Irq + 'static> {
    id: BlockRequestId,
    inner: SdioHost2DataRequest<'a, H>,
}

// SAFETY: The request guard owns no shared access to the host; it forwards
// completion/abort through `SdioHost2Adapter`'s serialized shared core.
unsafe impl<H> Send for ProtocolBlockRequest<'static, H>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
}

impl<H> BlockHost for SdioHost2Adapter<H>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    type Request = ProtocolBlockRequest<'static, H>;
    type Slot = ProtocolBlockSlot;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        _dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError> {
        submit_protocol_request(self, start_block, buffer, size, slot, pending, true)
    }

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        _dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError> {
        submit_protocol_request(self, start_block, buffer, size, slot, pending, false)
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        let Some(active) = pending.as_mut() else {
            return Err(Error::InvalidArgument);
        };
        if active.id != request {
            return Ok(BlockPoll::Pending);
        }
        match self.poll_data_request(&mut active.inner) {
            Err(err) => {
                let abort = active.inner.abort();
                *pending = None;
                slot.active_id = None;
                if let Err(abort_err) = abort {
                    warn!(
                        "sdmmc rdif: abort after poll error reported recovery error: {abort_err:?}"
                    );
                }
                Err(err)
            }
            Ok(DataCommandPoll::Pending) => Ok(BlockPoll::Pending),
            Ok(DataCommandPoll::Complete(_)) => {
                slot.completed_dma = active.inner.take_completed_dma();
                *pending = None;
                slot.active_id = None;
                Ok(BlockPoll::Complete)
            }
        }
    }

    fn abort_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        slot: &mut Self::Slot,
    ) -> Result<(), Error> {
        let result = if let Some(active) = pending.as_mut() {
            active.inner.abort()
        } else {
            Ok(())
        };
        *pending = None;
        slot.active_id = None;
        result
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        request.id
    }

    fn submit_owned_read_request(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        submit_owned_protocol_request(self, start_block, buffer, slot, pending, true)
    }

    fn submit_owned_write_request(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        submit_owned_protocol_request(self, start_block, buffer, slot, pending, false)
    }

    fn take_completed_dma(slot: &mut Self::Slot) -> Option<CompletedDma> {
        slot.completed_dma.take()
    }
}

fn submit_protocol_request<H>(
    host: &mut SdioHost2Adapter<H>,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    slot: &mut ProtocolBlockSlot,
    pending: &mut Option<ProtocolBlockRequest<'static, H>>,
    read: bool,
) -> Result<BlockRequestId, BlkError>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    if pending.is_some() || slot.active_id.is_some() {
        return Err(BlkError::Retry);
    }
    if !size.get().is_multiple_of(BLOCK_SIZE) {
        return Err(BlkError::Other("buffer is not block aligned"));
    }
    let blocks = u32::try_from(size.get() / BLOCK_SIZE).map_err(|_| BlkError::InvalidRequest)?;
    let id = BlockRequestId::new(slot.next_id);
    slot.next_id = slot.next_id.wrapping_add(1);
    let inner = if read {
        let cmd = if blocks == 1 {
            crate::cmd::cmd17(start_block)
        } else {
            crate::cmd::cmd18(start_block)
        };
        let buf: &'static mut [u8] =
            unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr(), size.get()) };
        host.submit_read_data(&cmd, buf, BLOCK_SIZE as u32, blocks)
            .map_err(map_dev_err_to_blk_err)?
    } else {
        let cmd = if blocks == 1 {
            crate::cmd::cmd24(start_block)
        } else {
            crate::cmd::cmd25(start_block)
        };
        let buf: &'static [u8] =
            unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size.get()) };
        host.submit_write_data(&cmd, buf, BLOCK_SIZE as u32, blocks)
            .map_err(map_dev_err_to_blk_err)?
    };
    slot.active_id = Some(id);
    *pending = Some(ProtocolBlockRequest { id, inner });
    Ok(id)
}

fn submit_owned_protocol_request<H>(
    host: &mut SdioHost2Adapter<H>,
    start_block: u32,
    buffer: PreparedDma,
    slot: &mut ProtocolBlockSlot,
    pending: &mut Option<ProtocolBlockRequest<'static, H>>,
    read: bool,
) -> Result<BlockRequestId, OwnedBlockSubmitError>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    if pending.is_some() || slot.active_id.is_some() {
        return Err(OwnedBlockSubmitError::new(BlkError::Retry, buffer));
    }
    if !buffer.len().get().is_multiple_of(BLOCK_SIZE) {
        return Err(OwnedBlockSubmitError::new(
            BlkError::Other("buffer is not block aligned"),
            buffer,
        ));
    }
    let blocks = match u32::try_from(buffer.len().get() / BLOCK_SIZE) {
        Ok(blocks) => blocks,
        Err(_) => return Err(OwnedBlockSubmitError::new(BlkError::InvalidRequest, buffer)),
    };
    let id = BlockRequestId::new(slot.next_id);
    slot.next_id = slot.next_id.wrapping_add(1);
    let cmd = if read {
        if blocks == 1 {
            crate::cmd::cmd17(start_block)
        } else {
            crate::cmd::cmd18(start_block)
        }
    } else if blocks == 1 {
        crate::cmd::cmd24(start_block)
    } else {
        crate::cmd::cmd25(start_block)
    };
    let direction = if read {
        sdio_host2::DataDirection::Read
    } else {
        sdio_host2::DataDirection::Write
    };
    let inner = match host.submit_dma_data(&cmd, direction, buffer, BLOCK_SIZE as u32, blocks) {
        Ok(inner) => inner,
        Err(err) => {
            return Err(OwnedBlockSubmitError::new(
                map_dev_err_to_blk_err(err.error),
                err.into_buffer(),
            ));
        }
    };
    slot.active_id = Some(id);
    *pending = Some(ProtocolBlockRequest { id, inner });
    Ok(id)
}

pub struct BlockDevice<H>
where
    H: BlockHost,
{
    control: Arc<BlockControl<H>>,
}

struct BlockControl<H>
where
    H: BlockHost,
{
    raw: SharedCore<SdioSdmmc<H>>,
    config: BlockConfig,
    irq_enabled: AtomicBool,
    queue_taken: AtomicBool,
    irq_handler: rdif_block::IrqHandlerSlot,
}

impl<H> BlockDevice<H>
where
    H: BlockHost,
{
    pub fn new(card: SdioSdmmc<H>, config: BlockConfig) -> Self {
        let raw = SharedCore::new(card);
        let irq_handle = raw.with_mut(|raw| raw.host().irq_handle());
        let irq_handler = rdif_block::IrqHandlerSlot::new(Box::new(BlockIrqHandler::<H> {
            handle: irq_handle,
            _marker: PhantomData,
        }));
        Self {
            control: Arc::new(BlockControl {
                raw,
                config,
                irq_enabled: AtomicBool::new(false),
                queue_taken: AtomicBool::new(false),
                irq_handler,
            }),
        }
    }

    pub fn config(&self) -> &BlockConfig {
        &self.control.config
    }

    fn queue_limits_with_mask(&self, dma_mask: u64) -> rdif_block::QueueLimits {
        queue_limits(&self.control.config, dma_mask)
    }
}

impl<H> BlockControl<H>
where
    H: BlockHost,
{
    fn claim_queue(&self) -> bool {
        self.queue_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release_queue(&self) {
        self.queue_taken.store(false, Ordering::Release);
    }
}

impl<H> rdif_block::DriverGeneric for BlockDevice<H>
where
    H: BlockHost,
{
    fn name(&self) -> &str {
        self.control.config.name
    }
}

impl<H> Interface for BlockDevice<H>
where
    H: BlockHost,
{
    fn device_info(&self) -> rdif_block::DeviceInfo {
        device_info(&self.control.config)
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        self.queue_limits_with_mask(self.control.config.dma_mask)
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.control.config.uses_dma() || !self.control.claim_queue() {
            return None;
        }
        Some(Box::new(BlockQueue::<H>::new(Arc::clone(&self.control), 0)) as _)
    }

    fn create_owned_queue(&mut self) -> Option<QueueHandle> {
        if self.control.config.dma.is_none() || !self.control.claim_queue() {
            return None;
        }
        Some(QueueHandle::new(Box::new(BlockQueue::<H>::new(
            Arc::clone(&self.control),
            0,
        ))))
    }

    fn enable_irq(&self) {
        if !self.control.config.irq_driven {
            self.control.irq_enabled.store(false, Ordering::Release);
            return;
        }
        let mut enabled = false;
        self.control.raw.with_mut(|raw| {
            if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                warn!(
                    "{}: enable completion IRQ failed: {:?}",
                    self.control.config.name, err
                );
                return;
            }
            enabled = raw.host().completion_irq_enabled();
        });
        self.control.irq_enabled.store(enabled, Ordering::Release);
    }

    fn disable_irq(&self) {
        self.control.raw.with_mut(|raw| {
            if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                warn!(
                    "{}: disable completion IRQ failed: {:?}",
                    self.control.config.name, err
                );
            }
        });
        self.control.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.control.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        if !self.control.config.irq_driven {
            return Vec::new();
        }
        vec![rdif_block::IrqSourceInfo::legacy(
            rdif_block::IdList::from_bits(1),
        )]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn rdif_block::IrqHandler>> {
        if !self.control.config.irq_driven || source_id != 0 {
            return None;
        }
        self.control
            .irq_handler
            .take()
            .map(|handler| Box::new(handler) as Box<dyn rdif_block::IrqHandler>)
    }
}

pub struct BlockQueue<H>
where
    H: BlockHost,
{
    control: Arc<BlockControl<H>>,
    id: usize,
    slot: H::Slot,
    pending: Option<H::Request>,
    split_transfer: Option<SplitTransfer>,
    completed: Vec<RequestId>,
    completed_owned: Vec<rdif_block::CompletedRequest>,
}

#[derive(Clone, Copy, Debug)]
enum SplitDirection {
    Read,
    Write,
}

struct SplitTransfer {
    direction: SplitDirection,
    public_id: RequestId,
    next_card_block: u32,
    block_addr_step: u32,
    buffer_addr: usize,
    next_offset: usize,
    remaining_blocks: u32,
}

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    fn new(control: Arc<BlockControl<H>>, id: usize) -> Self {
        Self {
            control,
            id,
            slot: H::Slot::default(),
            pending: None,
            split_transfer: None,
            completed: Vec::new(),
            completed_owned: Vec::new(),
        }
    }

    fn queue_info(&self) -> rdif_block::QueueInfo {
        rdif_block::IQueue::info(self)
    }

    fn submit_request_inner(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        rdif_block::validate_request(self.queue_info(), &request)?;
        self.reap_pending_request()?;
        let raw = self.control.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.lba, raw.is_high_capacity())?;
            let buffer = request
                .segments
                .first()
                .copied()
                .ok_or(BlkError::InvalidRequest)?;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(BlkError::Other("buffer is not block aligned"));
            }
            let ptr = NonNull::new(buffer.virt).ok_or(BlkError::Other("buffer pointer is null"))?;
            let size = NonZeroUsize::new(buffer.len()).ok_or(BlkError::Other("buffer is empty"))?;
            let dma = self.control.config.dma.as_ref();
            let id = match request.op {
                rdif_block::RequestOp::Read
                    if should_split_fifo_request(dma, request.block_count) =>
                {
                    self.submit_split_transfer(
                        raw,
                        SplitDirection::Read,
                        start_block,
                        ptr,
                        request.block_count,
                        raw.is_high_capacity(),
                    )?
                }
                rdif_block::RequestOp::Write
                    if should_split_fifo_request(dma, request.block_count) =>
                {
                    self.submit_split_transfer(
                        raw,
                        SplitDirection::Write,
                        start_block,
                        ptr,
                        request.block_count,
                        raw.is_high_capacity(),
                    )?
                }
                rdif_block::RequestOp::Read => {
                    let id = H::submit_read_request(
                        raw.host_mut(),
                        start_block,
                        ptr,
                        size,
                        dma,
                        &mut self.slot,
                        &mut self.pending,
                    )?;
                    RequestId::new(usize::from(id))
                }
                rdif_block::RequestOp::Write => H::submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    dma,
                    &mut self.slot,
                    &mut self.pending,
                )
                .map(|id| RequestId::new(usize::from(id)))?,
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => return Err(BlkError::NotSupported),
            };
            Ok(id)
        })
    }

    fn poll_request_inner(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(RequestStatus::Complete);
        }
        if self
            .split_transfer
            .as_ref()
            .is_some_and(|split| split.public_id != request)
        {
            return Ok(RequestStatus::Pending);
        }
        if self.split_transfer.is_some() {
            return self.poll_split_transfer(request);
        }
        self.poll_direct_request(request)
    }

    fn poll_direct_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        self.poll_host_request(BlockRequestId::new(usize::from(request)))
    }

    fn poll_host_request(&mut self, request: BlockRequestId) -> Result<RequestStatus, BlkError> {
        let raw = self.control.raw.clone();
        match raw.with_mut(|raw| {
            H::poll_block_request(raw.host_mut(), &mut self.pending, request, &mut self.slot)
        }) {
            Ok(BlockPoll::Complete) => Ok(RequestStatus::Complete),
            Ok(BlockPoll::Pending) => Ok(RequestStatus::Pending),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn submit_split_transfer(
        &mut self,
        raw: &mut SdioSdmmc<H>,
        direction: SplitDirection,
        start_block: u32,
        buffer: NonNull<u8>,
        block_count: u32,
        high_capacity: bool,
    ) -> Result<RequestId, BlkError> {
        if self.pending.is_some() || self.split_transfer.is_some() {
            return Err(BlkError::Retry);
        }
        let id = self.submit_split_child(raw, direction, start_block, buffer)?;
        let public_id = RequestId::new(usize::from(id));
        let block_addr_step = if high_capacity { 1 } else { BLOCK_SIZE as u32 };
        let remaining_blocks = block_count - 1;
        let next_card_block = if remaining_blocks == 0 {
            start_block
        } else {
            start_block
                .checked_add(block_addr_step)
                .ok_or(BlkError::InvalidRequest)?
        };
        self.split_transfer = Some(SplitTransfer {
            direction,
            public_id,
            next_card_block,
            block_addr_step,
            buffer_addr: buffer.as_ptr() as usize,
            next_offset: BLOCK_SIZE,
            remaining_blocks,
        });
        Ok(public_id)
    }

    fn submit_split_child(
        &mut self,
        raw: &mut SdioSdmmc<H>,
        direction: SplitDirection,
        card_block: u32,
        buffer: NonNull<u8>,
    ) -> Result<BlockRequestId, BlkError> {
        let block_size = NonZeroUsize::new(BLOCK_SIZE).ok_or(BlkError::InvalidRequest)?;
        match direction {
            SplitDirection::Read => H::submit_read_request(
                raw.host_mut(),
                card_block,
                buffer,
                block_size,
                None,
                &mut self.slot,
                &mut self.pending,
            ),
            SplitDirection::Write => H::submit_write_request(
                raw.host_mut(),
                card_block,
                buffer,
                block_size,
                None,
                &mut self.slot,
                &mut self.pending,
            ),
        }
    }

    fn poll_split_transfer(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let child = self.pending_id().ok_or(BlkError::InvalidRequest)?;
        let status = match self.poll_host_request(child) {
            Ok(status) => status,
            Err(err) => {
                self.split_transfer = None;
                return Err(err);
            }
        };
        match status {
            RequestStatus::Pending => Ok(RequestStatus::Pending),
            RequestStatus::Complete => self.advance_split_transfer(request),
        }
    }

    fn advance_split_transfer(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        if self
            .split_transfer
            .as_ref()
            .is_none_or(|split| split.remaining_blocks == 0)
        {
            self.split_transfer = None;
            return Ok(RequestStatus::Complete);
        }

        let mut split = self.split_transfer.take().ok_or(BlkError::InvalidRequest)?;
        if split.public_id != request {
            self.split_transfer = Some(split);
            return Ok(RequestStatus::Pending);
        }
        let ptr = split
            .buffer_addr
            .checked_add(split.next_offset)
            .and_then(|addr| NonNull::new(addr as *mut u8))
            .ok_or(BlkError::Other("buffer pointer is null"))?;
        let raw = self.control.raw.clone();
        let submit = raw.with_mut(|raw| {
            self.submit_split_child(raw, split.direction, split.next_card_block, ptr)
        });
        submit?;
        split.remaining_blocks -= 1;
        split.next_offset += BLOCK_SIZE;
        if split.remaining_blocks > 0 {
            split.next_card_block = split
                .next_card_block
                .checked_add(split.block_addr_step)
                .ok_or(BlkError::InvalidRequest)?;
        }
        self.split_transfer = Some(split);
        Ok(RequestStatus::Pending)
    }

    fn pending_id(&self) -> Option<BlockRequestId> {
        self.pending.as_ref().map(H::request_id)
    }

    fn active_request_id(&self) -> Option<RequestId> {
        self.split_transfer
            .as_ref()
            .map(|split| split.public_id)
            .or_else(|| {
                self.pending
                    .as_ref()
                    .map(|pending| RequestId::new(usize::from(H::request_id(pending))))
            })
    }

    fn reap_pending_request(&mut self) -> Result<RequestStatus, BlkError> {
        let Some(active) = self.active_request_id() else {
            return Ok(RequestStatus::Complete);
        };
        match self.poll_request_inner(active) {
            Ok(RequestStatus::Complete) => {
                self.completed.push(active);
                Ok(RequestStatus::Complete)
            }
            Ok(RequestStatus::Pending) => Err(BlkError::Retry),
            Err(err) => Err(err),
        }
    }

    fn submit_owned_request_inner(
        &mut self,
        request: OwnedRequest,
    ) -> Result<RequestId, SubmitError> {
        if self.control.config.dma.is_none() {
            return Err(SubmitError::new(BlkError::NotSupported, request));
        }
        if self.split_transfer.is_some() || !self.completed_owned.is_empty() {
            return Err(SubmitError::new(BlkError::Retry, request));
        }
        if let Err(err) = rdif_block::validate_owned_request(self.queue_info(), &request) {
            return Err(SubmitError::new(err, request));
        }
        if let Some(active) = self
            .pending
            .as_ref()
            .map(|pending| RequestId::new(usize::from(H::request_id(pending))))
        {
            match self.poll_owned_request_inner(active) {
                Ok(OwnedRequestPoll::Ready(completed)) => {
                    self.completed_owned.push(completed);
                    return Err(SubmitError::new(BlkError::Retry, request));
                }
                Ok(OwnedRequestPoll::Pending) => {
                    return Err(SubmitError::new(BlkError::Retry, request));
                }
                Err(_) => return Err(SubmitError::new(BlkError::Io, request)),
            }
        }

        let OwnedRequest {
            op,
            lba,
            block_count,
            data,
            flags,
        } = request;
        let Some(buffer) = data else {
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
        let raw = self.control.raw.clone();
        match raw.with_mut(|raw| {
            let start_block = match block_addr_for_card(lba, raw.is_high_capacity()) {
                Ok(start_block) => start_block,
                Err(err) => return Err(OwnedBlockSubmitError::new(err, buffer)),
            };
            match op {
                rdif_block::RequestOp::Read => H::submit_owned_read_request(
                    raw.host_mut(),
                    start_block,
                    buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                rdif_block::RequestOp::Write => H::submit_owned_write_request(
                    raw.host_mut(),
                    start_block,
                    buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => {
                    Err(OwnedBlockSubmitError::new(BlkError::NotSupported, buffer))
                }
            }
        }) {
            Ok(id) => Ok(RequestId::new(usize::from(id))),
            Err(err) => {
                let (error, buffer) = err.into_parts();
                Err(SubmitError::new(
                    error,
                    OwnedRequest {
                        op,
                        lba,
                        block_count,
                        data: Some(buffer),
                        flags,
                    },
                ))
            }
        }
    }

    fn poll_owned_request_inner(
        &mut self,
        request: RequestId,
    ) -> Result<OwnedRequestPoll, PollError> {
        if let Some(index) = self
            .completed_owned
            .iter()
            .position(|completed| completed.id == request)
        {
            return Ok(OwnedRequestPoll::Ready(
                self.completed_owned.swap_remove(index),
            ));
        }
        if self.split_transfer.is_some() {
            return Err(PollError::WrongQueue);
        }
        let id = BlockRequestId::new(usize::from(request));
        let Some(active) = self.pending.as_ref() else {
            return Err(PollError::UnknownRequest);
        };
        if H::request_id(active) != id {
            return Ok(OwnedRequestPoll::Pending);
        }
        let raw = self.control.raw.clone();
        match raw.with_mut(|raw| {
            H::poll_block_request(raw.host_mut(), &mut self.pending, id, &mut self.slot)
        }) {
            Ok(BlockPoll::Pending) => Ok(OwnedRequestPoll::Pending),
            Ok(BlockPoll::Complete) => {
                let completed_dma = H::take_completed_dma(&mut self.slot);
                self.pending = None;
                Ok(OwnedRequestPoll::Ready(rdif_block::CompletedRequest::new(
                    request,
                    Ok(()),
                    completed_dma,
                )))
            }
            Err(err) => {
                let raw = self.control.raw.clone();
                let abort = raw.with_mut(|raw| {
                    H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot)
                });
                let completed_dma = H::take_completed_dma(&mut self.slot);
                let result = match abort {
                    Ok(()) => Err(map_dev_err_to_blk_err(err)),
                    Err(recovery) => Err(map_dev_err_to_blk_err(recovery)),
                };
                Ok(OwnedRequestPoll::Ready(rdif_block::CompletedRequest::new(
                    request,
                    result,
                    completed_dma,
                )))
            }
        }
    }
}

impl<H> Drop for BlockQueue<H>
where
    H: BlockHost,
{
    fn drop(&mut self) {
        if self.pending.is_some() {
            let raw = self.control.raw.clone();
            raw.with_mut(|raw| {
                if let Err(err) =
                    H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot)
                {
                    warn!(
                        "sdmmc rdif: abort pending request on queue drop reported recovery error: \
                         {err:?}"
                    );
                    self.pending = None;
                }
            });
        }
        self.split_transfer = None;
        self.control.release_queue();
    }
}

// SAFETY: `BlockQueue` owns one pending request slot. The concrete host
// request object owns any borrowed request segment until task-side poll
// reports completion or error.
unsafe impl<H> IQueue for BlockQueue<H>
where
    H: BlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: device_info(&self.control.config),
            limits: queue_limits(&self.control.config, self.control.config.dma_mask),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        self.submit_request_inner(request)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        self.poll_request_inner(request)
    }
}

impl<H> IQueueOwned for BlockQueue<H>
where
    H: BlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: device_info(&self.control.config),
            limits: queue_limits(&self.control.config, self.control.config.dma_mask),
        }
    }

    fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        self.submit_owned_request_inner(request)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<OwnedRequestPoll, PollError> {
        self.poll_owned_request_inner(request)
    }

    fn cancel_request(&mut self, request: RequestId) -> Result<OwnedRequestPoll, PollError> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| RequestId::new(usize::from(H::request_id(pending))) != request)
        {
            return Err(PollError::UnknownRequest);
        }
        let raw = self.control.raw.clone();
        let result =
            raw.with_mut(|raw| H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot));
        let completed_dma = H::take_completed_dma(&mut self.slot);
        let completion = match result {
            Ok(()) => rdif_block::CompletedRequest::new(request, Err(BlkError::Io), completed_dma),
            Err(err) => rdif_block::CompletedRequest::new(
                request,
                Err(map_dev_err_to_blk_err(err)),
                completed_dma,
            ),
        };
        Ok(OwnedRequestPoll::Ready(completion))
    }

    fn shutdown(&mut self) {
        if self.pending.is_some() {
            let raw = self.control.raw.clone();
            raw.with_mut(|raw| {
                if let Err(err) =
                    H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot)
                {
                    warn!(
                        "sdmmc rdif: abort pending owned request on queue shutdown reported \
                         recovery error: {err:?}"
                    );
                    self.pending = None;
                }
            });
        }
    }
}

struct BlockIrqHandler<H>
where
    H: BlockHost,
{
    handle: <H as SdioIrqHost>::IrqHandle,
    _marker: PhantomData<H>,
}

impl<H> rdif_block::IrqHandler for BlockIrqHandler<H>
where
    H: BlockHost,
{
    fn handle_irq(&mut self) -> rdif_block::Event {
        let host_event = self.handle.handle_irq();
        let mut event = rdif_block::Event::none();
        if let Some(queue_id) = block_queue_ready_from_host_event(&host_event) {
            event.push_queue(queue_id);
        }
        event
    }
}

pub fn queue_limits(config: &BlockConfig, dma_mask: u64) -> rdif_block::QueueLimits {
    rdif_block::QueueLimits {
        dma_mask,
        dma_domain: config.dma_domain,
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

pub fn device_info(config: &BlockConfig) -> rdif_block::DeviceInfo {
    rdif_block::DeviceInfo {
        name: Some(config.name),
        ..rdif_block::DeviceInfo::new(config.capacity_blocks, BLOCK_SIZE)
    }
}

pub fn block_addr_for_card(block_id: u64, high_capacity: bool) -> Result<u32, BlkError> {
    let block_id = u32::try_from(block_id).map_err(|_| BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(BlkError::InvalidBlockIndex(block_id as u64))
    }
}

pub fn map_dev_err_to_blk_err(err: Error) -> BlkError {
    match err {
        Error::Busy => BlkError::Retry,
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => BlkError::Io,
    }
}

pub fn transfer_mode_for_dma(dma: Option<&DeviceDma>) -> BlockTransferMode {
    match dma {
        Some(_) => BlockTransferMode::Dma,
        None => BlockTransferMode::Fifo,
    }
}

fn should_split_fifo_request(dma: Option<&DeviceDma>, block_count: u32) -> bool {
    dma.is_none() && block_count > 1
}

pub fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

struct SharedCore<T> {
    inner: Arc<SharedCoreInner<T>>,
}

struct SharedCoreInner<T> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
}

struct SharedCoreGuard<'a, T> {
    inner: &'a SharedCoreInner<T>,
}

// SAFETY: `SharedCore` serializes all mutable access through a single atomic
// borrow flag. IRQ top halves use host-specific cloneable handles instead.
unsafe impl<T: Send> Send for SharedCoreInner<T> {}

// SAFETY: See the `Send` impl.
unsafe impl<T: Send> Sync for SharedCoreInner<T> {}

impl<T> SharedCore<T> {
    fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SharedCoreInner {
                value: UnsafeCell::new(value),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.enter();
        f(guard.get_mut())
    }
}

impl<T> Clone for SharedCore<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> SharedCoreInner<T> {
    fn enter(&self) -> SharedCoreGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_enter() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }

    fn try_enter(&self) -> Option<SharedCoreGuard<'_, T>> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(SharedCoreGuard { inner: self })
    }
}

impl<T> SharedCoreGuard<'_, T> {
    fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<T> Drop for SharedCoreGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use sdio_host2::{RequestPoll, SdioHost as PhysicalSdioHost, Transaction};

    use super::*;
    use crate::{
        CommandResponsePoll, DataCommandPoll, OperationPoll,
        cmd::Command,
        sdio::{ClockSpeed, HostEvent, HostEventKind},
    };

    fn block_control(config: BlockConfig) -> Arc<BlockControl<MockHost>> {
        let raw = SharedCore::new(SdioSdmmc::new(MockHost::default()));
        let irq_handle = raw.with_mut(|raw| raw.host().irq_handle());
        Arc::new(BlockControl {
            raw,
            config,
            irq_enabled: AtomicBool::new(false),
            queue_taken: AtomicBool::new(false),
            irq_handler: rdif_block::IrqHandlerSlot::new(Box::new(BlockIrqHandler::<MockHost> {
                handle: irq_handle,
                _marker: PhantomData,
            })),
        })
    }

    #[test]
    fn fifo_config_limits_single_block_requests() {
        let config = BlockConfig::fifo("test-sdmmc", 8, true);
        let limits = queue_limits(&config, DEFAULT_DMA_MASK);

        assert_eq!(limits.max_inflight, 1);
        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, BLOCK_SIZE);
        assert!(!limits.supports_flush);
    }

    #[test]
    fn disabled_irq_policy_does_not_advertise_sources() {
        let device = BlockDevice::new(
            SdioSdmmc::new(MockHost::default()),
            BlockConfig::fifo("mock-sd", 8, false),
        );

        assert!(Interface::irq_sources(&device).is_empty());
    }

    #[test]
    fn enabled_irq_handler_maps_host_event_to_queue_zero() {
        let mut device = BlockDevice::new(
            SdioSdmmc::new(MockHost::default()),
            BlockConfig::fifo("mock-sd", 8, true),
        );
        let handler = Interface::take_irq_handler(&mut device, 0).unwrap();

        let event = handler.handle_irq();

        assert!(event.queues.contains(0));
        assert!(!event.is_empty());
    }

    #[test]
    fn dma_config_exposes_one_owned_queue_while_handle_is_live() {
        let dma = DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA);
        let mut device = BlockDevice::new(
            SdioSdmmc::new(MockHost::default()),
            BlockConfig::dma("mock-sd", 8, false, dma),
        );

        assert!(Interface::create_queue(&mut device).is_none());
        let queue = Interface::create_owned_queue(&mut device);
        assert!(queue.is_some());
        assert!(Interface::create_owned_queue(&mut device).is_none());
        drop(queue);
        assert!(Interface::create_owned_queue(&mut device).is_some());
    }

    #[test]
    fn poll_request_only_completes_matching_request_id() {
        let mut queue =
            BlockQueue::<MockHost>::new(block_control(BlockConfig::fifo("mock-sd", 8, false)), 0);
        queue.pending = Some(MockRequest {
            id: BlockRequestId::new(7),
        });

        assert_eq!(
            queue.poll_request_inner(RequestId::new(8)),
            Ok(RequestStatus::Pending)
        );
        assert_eq!(
            queue.poll_request_inner(RequestId::new(7)),
            Ok(RequestStatus::Complete)
        );
        assert!(queue.pending.is_none());
    }

    #[test]
    fn unsupported_ops_are_rejected() {
        let mut queue =
            BlockQueue::<MockHost>::new(block_control(BlockConfig::fifo("mock-sd", 8, false)), 0);
        let mut segments = [];
        let request = Request {
            op: rdif_block::RequestOp::Flush,
            lba: 0,
            block_count: 0,
            segments: &mut segments,
            flags: rdif_block::RequestFlags::NONE,
        };

        assert_eq!(
            rdif_block::IQueue::submit_request(&mut queue, request),
            Err(BlkError::NotSupported)
        );
    }

    #[test]
    fn dropping_queue_aborts_pending_request() {
        let control = block_control(BlockConfig::fifo("mock-sd", 8, false));
        let raw = control.raw.clone();
        let mut backing = [0u8; BLOCK_SIZE];
        let mut segments =
            [
                unsafe {
                    rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len())
                },
            ];
        {
            let mut queue = BlockQueue::<MockHost>::new(Arc::clone(&control), 0);
            let request = Request {
                op: rdif_block::RequestOp::Read,
                lba: 0,
                block_count: 1,
                segments: &mut segments,
                flags: rdif_block::RequestFlags::NONE,
            };

            rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
        }

        assert_eq!(
            raw.with_mut(|raw| raw.host().aborts.load(Ordering::Acquire)),
            1
        );
    }

    #[test]
    fn host2_submit_failure_does_not_leak_active_slot() {
        let mut host = SdioHost2Adapter::new(Host2BlockMock {
            submit_error: Some(sdio_host2::Error::Busy),
            ..Host2BlockMock::default()
        });
        let mut slot = ProtocolBlockSlot::default();
        let mut pending = None;
        let mut backing = [0u8; BLOCK_SIZE];
        let buffer = NonNull::new(backing.as_mut_ptr()).unwrap();
        let size = NonZeroUsize::new(BLOCK_SIZE).unwrap();

        assert_eq!(
            <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
                &mut host,
                0,
                buffer,
                size,
                None,
                &mut slot,
                &mut pending,
            ),
            Err(BlkError::Retry)
        );
        assert!(pending.is_none());
        assert!(slot.active_id.is_none());

        <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
            &mut host,
            0,
            buffer,
            size,
            None,
            &mut slot,
            &mut pending,
        )
        .expect("slot should accept a new request after submit failure");
    }

    #[test]
    fn host2_poll_error_clears_pending_and_active_slot() {
        let mut host = SdioHost2Adapter::new(Host2BlockMock {
            poll_error: Some(sdio_host2::Error::Timeout),
            ..Host2BlockMock::default()
        });
        let mut slot = ProtocolBlockSlot::default();
        let mut pending = None;
        let mut backing = [0u8; BLOCK_SIZE];
        let buffer = NonNull::new(backing.as_mut_ptr()).unwrap();
        let size = NonZeroUsize::new(BLOCK_SIZE).unwrap();
        let id = <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
            &mut host,
            0,
            buffer,
            size,
            None,
            &mut slot,
            &mut pending,
        )
        .unwrap();

        assert!(matches!(
            <SdioHost2Adapter<Host2BlockMock> as BlockHost>::poll_block_request(
                &mut host,
                &mut pending,
                id,
                &mut slot,
            ),
            Err(Error::Timeout(_))
        ));
        assert!(pending.is_none());
        assert!(slot.active_id.is_none());
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
        aborts: AtomicUsize,
        read_sizes: Vec<usize>,
        write_sizes: Vec<usize>,
    }

    #[derive(Default)]
    struct MockSlot;

    struct MockRequest {
        id: BlockRequestId,
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            BLOCK_SIZE
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn map_streaming(
            &self,
            _constraints: dma_api::DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: dma_api::DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            Err(dma_api::DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
    }

    #[derive(Default)]
    struct Host2BlockMock {
        submit_error: Option<sdio_host2::Error>,
        poll_error: Option<sdio_host2::Error>,
    }

    struct Host2BlockRequest {
        done: bool,
    }

    impl PhysicalSdioHost for Host2BlockMock {
        type TransactionRequest<'a>
            = Host2BlockRequest
        where
            Self: 'a;
        type BusRequest = Host2BlockRequest;

        unsafe fn submit_transaction<'a>(
            &mut self,
            _transaction: Transaction<'a>,
        ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
        where
            Self: 'a,
        {
            if let Some(err) = self.submit_error.take() {
                return Err(err);
            }
            Ok(Host2BlockRequest { done: false })
        }

        fn poll_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
        where
            Self: 'a,
        {
            request.done = true;
            if let Some(err) = self.poll_error.take() {
                return Ok(RequestPoll::Ready(Err(err)));
            }
            Ok(RequestPoll::Ready(Ok(sdio_host2::RawResponse::empty())))
        }

        fn abort_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<(), sdio_host2::Error>
        where
            Self: 'a,
        {
            request.done = true;
            Ok(())
        }

        unsafe fn submit_bus_op(
            &mut self,
            _op: sdio_host2::BusOp,
        ) -> Result<Self::BusRequest, sdio_host2::Error> {
            Ok(Host2BlockRequest { done: false })
        }

        fn poll_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<RequestPoll<()>, sdio_host2::PollRequestError> {
            request.done = true;
            Ok(RequestPoll::Ready(Ok(())))
        }

        fn abort_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<(), sdio_host2::Error> {
            request.done = true;
            Ok(())
        }
    }

    impl SdioHost2Irq for Host2BlockMock {
        type Event = MockEvent;
        type IrqHandle = MockIrqHandle;

        fn irq_handle(&self) -> Self::IrqHandle {
            MockIrqHandle
        }
    }

    impl SdioHost for MockHost {
        type Event = MockEvent;
        type DataRequest<'a> = ();
        type BusRequest = crate::sdio::ReadyBusRequest;

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

        fn set_bus_width(&mut self, _width: crate::sdio::BusWidth) -> Result<(), Error> {
            Ok(())
        }

        fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
            Ok(())
        }

        fn submit_bus_op(&mut self, op: crate::sdio::SdioBusOp) -> Result<Self::BusRequest, Error> {
            crate::sdio::submit_ready_bus_op(self, op)
        }

        fn poll_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<OperationPoll<()>, Error> {
            crate::sdio::poll_ready_bus_op(request)
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

    impl BlockHost for MockHost {
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
        ) -> Result<BlockRequestId, BlkError> {
            self.read_sizes.push(_size.get());
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
        ) -> Result<BlockRequestId, BlkError> {
            self.write_sizes.push(_size.get());
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

        fn abort_request(
            &mut self,
            pending: &mut Option<Self::Request>,
            _slot: &mut Self::Slot,
        ) -> Result<(), Error> {
            if pending.take().is_some() {
                self.aborts.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        }

        fn request_id(request: &Self::Request) -> BlockRequestId {
            request.id
        }
    }

    impl MockHost {
        fn submit_mock_request(
            &self,
            pending: &mut Option<MockRequest>,
        ) -> Result<BlockRequestId, BlkError> {
            if pending.is_some() {
                return Err(BlkError::Retry);
            }
            let id = BlockRequestId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
            *pending = Some(MockRequest { id });
            Ok(id)
        }
    }

    #[test]
    fn fifo_read_requests_are_split_to_single_blocks() {
        let control = block_control(
            BlockConfig::fifo("mock-sd", 16, false)
                .with_max_blocks_per_request(8)
                .with_max_segment_size(8 * BLOCK_SIZE),
        );
        let raw = control.raw.clone();
        let mut queue = BlockQueue::<MockHost>::new(control, 0);
        let mut backing = [0u8; 8 * BLOCK_SIZE];
        let mut segments =
            [
                unsafe {
                    rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len())
                },
            ];
        let request = Request {
            op: rdif_block::RequestOp::Read,
            lba: 0,
            block_count: 8,
            segments: &mut segments,
            flags: rdif_block::RequestFlags::NONE,
        };

        let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
        let mut polls = 0;
        loop {
            polls += 1;
            match rdif_block::IQueue::poll_request(&mut queue, id) {
                Ok(RequestStatus::Pending) => {}
                Ok(RequestStatus::Complete) => break,
                Err(err) => panic!("split read poll failed: {err:?}"),
            }
        }
        assert_eq!(polls, 8);

        raw.with_mut(|raw| {
            assert_eq!(
                raw.host().read_sizes,
                alloc::vec![BLOCK_SIZE; 8],
                "FIFO read should avoid CMD18 multi-block requests on hosts without DMA"
            );
            assert!(raw.host().write_sizes.is_empty());
        });
    }

    #[test]
    fn fifo_write_requests_are_split_to_single_blocks() {
        let control = block_control(
            BlockConfig::fifo("mock-sd", 16, false)
                .with_max_blocks_per_request(8)
                .with_max_segment_size(8 * BLOCK_SIZE),
        );
        let raw = control.raw.clone();
        let mut queue = BlockQueue::<MockHost>::new(control, 0);
        let mut backing = [0u8; 8 * BLOCK_SIZE];
        let mut segments =
            [
                unsafe {
                    rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len())
                },
            ];
        let request = Request {
            op: rdif_block::RequestOp::Write,
            lba: 0,
            block_count: 8,
            segments: &mut segments,
            flags: rdif_block::RequestFlags::NONE,
        };

        let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
        let mut polls = 0;
        loop {
            polls += 1;
            match rdif_block::IQueue::poll_request(&mut queue, id) {
                Ok(RequestStatus::Pending) => {}
                Ok(RequestStatus::Complete) => break,
                Err(err) => panic!("split write poll failed: {err:?}"),
            }
        }
        assert_eq!(polls, 8);

        raw.with_mut(|raw| {
            assert!(raw.host().read_sizes.is_empty());
            assert_eq!(
                raw.host().write_sizes,
                alloc::vec![BLOCK_SIZE; 8],
                "FIFO write should avoid CMD25/CMD12 multi-block requests on hosts without DMA"
            );
        });
    }
}
