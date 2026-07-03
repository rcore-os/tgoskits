use alloc::{sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull};

use log::warn;
use rdif_block::{BlkError, IQueue, Request, RequestId, RequestStatus};

use crate::{
    BlockPoll, BlockRequestId,
    rdif::{
        config::{
            BLOCK_SIZE, block_addr_for_card, device_info, map_dev_err_to_blk_err, queue_limits,
            should_split_fifo_request,
        },
        device::BlockControl,
        host::BlockHost,
        split::{SplitDirection, SplitTransfer},
    },
    sdio::SdioSdmmc,
};

pub struct BlockQueue<H>
where
    H: BlockHost,
{
    pub(super) control: Arc<BlockControl<H>>,
    pub(super) id: usize,
    pub(super) slot: H::Slot,
    pub(super) pending: Option<H::Request>,
    pub(super) split_transfer: Option<SplitTransfer>,
    pub(super) completed: Vec<RequestId>,
    pub(super) completed_owned: Vec<rdif_block::CompletedRequest>,
}

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    pub(super) fn new(control: Arc<BlockControl<H>>, id: usize) -> Self {
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

    pub(super) fn queue_info(&self) -> rdif_block::QueueInfo {
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

    pub(super) fn poll_request_inner(
        &mut self,
        request: RequestId,
    ) -> Result<RequestStatus, BlkError> {
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
