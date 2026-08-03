use alloc::boxed::Box;

use log::warn;
use rdif_block::{
    BlkError,
    dma_api::{CompletedDma, PreparedDma},
};
use sdio_host2::ProgressCause;

use crate::{
    BlockProgress, BlockRequestId, DataCommandProgress, Error,
    rdif::config::{BLOCK_SIZE, map_dev_err_to_blk_err},
    sdio::{
        host::SdioIrqHost,
        host2::{ProtocolDataRequest, ProtocolHost},
    },
};

pub(crate) struct OwnedBlockSubmitError {
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

    pub(super) fn into_parts(self) -> (BlkError, PreparedDma) {
        (self.error, *self.buffer)
    }
}

#[derive(Default)]
pub(crate) struct ProtocolBlockSlot {
    next_id: usize,
    active_id: Option<BlockRequestId>,
    completed_dma: Option<CompletedDma>,
}

pub(crate) struct ProtocolBlockRequest<H: SdioIrqHost + 'static> {
    id: BlockRequestId,
    inner: ProtocolDataRequest<'static, H>,
}

pub(crate) fn submit_owned_read_request<H>(
    host: &mut ProtocolHost<H>,
    start_block: u32,
    buffer: PreparedDma,
    slot: &mut ProtocolBlockSlot,
    pending: &mut Option<ProtocolBlockRequest<H>>,
) -> Result<BlockRequestId, OwnedBlockSubmitError>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    submit_owned_protocol_request(host, start_block, buffer, slot, pending, true)
}

pub(crate) fn submit_owned_write_request<H>(
    host: &mut ProtocolHost<H>,
    start_block: u32,
    buffer: PreparedDma,
    slot: &mut ProtocolBlockSlot,
    pending: &mut Option<ProtocolBlockRequest<H>>,
) -> Result<BlockRequestId, OwnedBlockSubmitError>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    submit_owned_protocol_request(host, start_block, buffer, slot, pending, false)
}

pub(crate) fn advance_block_request<H>(
    host: &mut ProtocolHost<H>,
    pending: &mut Option<ProtocolBlockRequest<H>>,
    request: BlockRequestId,
    slot: &mut ProtocolBlockSlot,
    cause: ProgressCause,
) -> Result<BlockProgress, Error>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    let Some(active) = pending.as_mut() else {
        return Err(Error::InvalidArgument);
    };
    if active.id != request {
        return Ok(BlockProgress::Pending);
    }
    match host.advance_data_request(&mut active.inner, cause) {
        Ok(DataCommandProgress::Pending) => Ok(BlockProgress::Pending),
        Ok(DataCommandProgress::Complete(_)) => {
            slot.completed_dma = active.inner.take_completed_dma();
            *pending = None;
            slot.active_id = None;
            Ok(BlockProgress::Complete)
        }
        Err(error) => {
            let recovery = host.abort_data_request(&mut active.inner);
            slot.completed_dma = active.inner.take_completed_dma();
            *pending = None;
            slot.active_id = None;
            if let Err(recovery_error) = recovery {
                warn!("SD/MMC request recovery failed after completion error: {recovery_error:?}");
                return Err(recovery_error);
            }
            Err(error)
        }
    }
}

pub(crate) fn abort_request<H>(
    host: &mut ProtocolHost<H>,
    pending: &mut Option<ProtocolBlockRequest<H>>,
    slot: &mut ProtocolBlockSlot,
) -> Result<(), Error>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    let result = if let Some(active) = pending.as_mut() {
        let result = host.abort_data_request(&mut active.inner);
        slot.completed_dma = active.inner.take_completed_dma();
        result
    } else {
        Ok(())
    };
    *pending = None;
    slot.active_id = None;
    result
}

pub(crate) fn take_completed_dma(slot: &mut ProtocolBlockSlot) -> Option<CompletedDma> {
    slot.completed_dma.take()
}

fn submit_owned_protocol_request<H>(
    host: &mut ProtocolHost<H>,
    start_block: u32,
    buffer: PreparedDma,
    slot: &mut ProtocolBlockSlot,
    pending: &mut Option<ProtocolBlockRequest<H>>,
    read: bool,
) -> Result<BlockRequestId, OwnedBlockSubmitError>
where
    H: SdioIrqHost + Send + 'static,
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
    let command = if read {
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
    let inner = match host.submit_dma_data(&command, direction, buffer, BLOCK_SIZE as u32, blocks) {
        Ok(inner) => inner,
        Err(error) => {
            return Err(OwnedBlockSubmitError::new(
                map_dev_err_to_blk_err(error.error),
                error.into_buffer(),
            ));
        }
    };
    slot.active_id = Some(id);
    *pending = Some(ProtocolBlockRequest { id, inner });
    Ok(id)
}
