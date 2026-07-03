use alloc::boxed::Box;
use core::{num::NonZeroUsize, ptr::NonNull};

use log::warn;
use rdif_block::{
    BlkError,
    dma_api::{CompletedDma, DeviceDma, PreparedDma},
};

use crate::{
    BlockPoll, BlockRequestId, DataCommandPoll, Error,
    rdif::config::{BLOCK_SIZE, map_dev_err_to_blk_err},
    sdio::{
        host::{SdioHost, SdioIrqHost},
        host2::{SdioHost2Adapter, SdioHost2DataRequest, SdioHost2Irq},
    },
};

pub trait BlockHost: SdioIrqHost + Send + 'static {
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
    pub(super) fn new(error: BlkError, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub(super) fn into_parts(self) -> (BlkError, PreparedDma) {
        (self.error, *self.buffer)
    }
}

#[derive(Default)]
pub struct ProtocolBlockSlot {
    pub(super) next_id: usize,
    pub(super) active_id: Option<BlockRequestId>,
    pub(super) completed_dma: Option<CompletedDma>,
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
