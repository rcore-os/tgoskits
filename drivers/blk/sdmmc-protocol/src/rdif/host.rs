use alloc::boxed::Box;
use core::fmt;

use rdif_block::{
    BlkError, InitInput, InitPoll, RecoveryCause,
    dma_api::{CompletedDma, CpuDmaBuffer, PreparedDma},
};

use crate::{
    BlockPoll, BlockRequestId, DataCommandPoll, Error,
    rdif::config::{BLOCK_SIZE, map_dev_err_to_blk_err},
    sdio::{
        host::{DeferredIrqAck, SdioHost, SdioIrqHandle, SdioIrqHost},
        host2::{
            SdioHost2Adapter, SdioHost2DataRequest, SdioHost2Irq, SdioHost2Lifecycle,
            SdioHost2Recovery,
        },
    },
};

pub trait BlockHost: SdioIrqHost + Send + 'static {
    type Request: Send + 'static;
    type Slot: Default + Send + 'static;
    type RecoveryState: Send + 'static;

    /// Install the fixed recovery storage required by the normal
    /// interrupt-backed queue.
    ///
    /// Controller-owned clock, reset, power, DMA, and IRQ capabilities must
    /// remain alive until detach or ownership handoff so recovery can rebuild
    /// the same hardware instance without rediscovery.
    fn prepare_block_runtime(&mut self);

    /// Start controller-wide recovery after the runtime has closed queue
    /// access, masked device IRQ delivery, and drained the OS IRQ action.
    fn begin_recovery(&mut self, cause: RecoveryCause) -> Result<Self::RecoveryState, Error>;

    /// Advance the hardware stop sequence without sleeping or busy-waiting.
    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()>;

    /// Begin reconstructing the controller while the queue still remains
    /// closed and all old request ownership has been reclaimed.
    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error>;

    /// Advance controller reconstruction until normal IRQ service is safe.
    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()>;

    /// Retry one destructive IRQ snapshot from the hctx's fixed worker.
    ///
    /// [`DeferredIrqAck::Acknowledged`] requires a non-empty hardware event
    /// cached by this call. A retry that acquires the register block but finds
    /// no pending source must return [`DeferredIrqAck::Unhandled`], while
    /// register exclusion remains [`DeferredIrqAck::Contended`].
    fn acknowledge_deferred_irq(&mut self) -> Result<DeferredIrqAck, Error>;

    /// Advance one request from status cached by an acknowledged IRQ.
    ///
    /// Callers invoke this exactly once for each matching queue event. The
    /// implementation must consume the cached controller snapshot without
    /// re-reading or clearing destructive global interrupt status.
    fn service_request(
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

    fn submit_owned_read_request(
        &mut self,
        start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError>;

    fn submit_owned_write_request(
        &mut self,
        start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError>;

    /// Return request backing after the controller has stopped touching it.
    fn take_completed_buffer(slot: &mut Self::Slot) -> Option<CompletedHostBuffer>;
}

/// Backing transferred from the RDIF queue into a physical host request.
pub enum HostRequestBuffer {
    Dma(PreparedDma),
    InterruptPio(CpuDmaBuffer),
}

impl HostRequestBuffer {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Dma(buffer) => buffer.len().get(),
            Self::InterruptPio(buffer) => buffer.len().get(),
        }
    }

    pub fn into_cpu_buffer(self) -> CpuDmaBuffer {
        match self {
            Self::Dma(buffer) => buffer.into_cpu_buffer(),
            Self::InterruptPio(buffer) => buffer,
        }
    }
}

/// Backing returned by a terminal or proof-gated reclaimed host request.
pub enum CompletedHostBuffer {
    Dma(CompletedDma),
    InterruptPio(CpuDmaBuffer),
}

impl CompletedHostBuffer {
    pub fn into_cpu_buffer(self) -> CpuDmaBuffer {
        match self {
            Self::Dma(buffer) => buffer.into_cpu_buffer(),
            Self::InterruptPio(buffer) => buffer,
        }
    }
}

pub struct OwnedBlockSubmitError {
    error: BlkError,
    buffer: Box<HostRequestBuffer>,
}

impl OwnedBlockSubmitError {
    /// Preserve prepared DMA ownership when a host rejects submission.
    pub fn new(error: BlkError, buffer: HostRequestBuffer) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    /// Return the typed reason submission was rejected.
    pub const fn error(&self) -> BlkError {
        self.error
    }

    /// Split the failure into its typed cause and recoverable DMA backing.
    pub fn into_parts(self) -> (BlkError, HostRequestBuffer) {
        (self.error, *self.buffer)
    }
}

impl fmt::Debug for OwnedBlockSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedBlockSubmitError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for OwnedBlockSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SD/MMC block submission failed: {}", self.error)
    }
}

impl core::error::Error for OwnedBlockSubmitError {}

#[derive(Default)]
pub struct ProtocolBlockSlot {
    pub(super) next_id: usize,
    pub(super) active_id: Option<BlockRequestId>,
    pub(super) completed: Option<CompletedHostBuffer>,
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
    H: SdioHost2Lifecycle + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
    type Request = ProtocolBlockRequest<'static, H>;
    type Slot = ProtocolBlockSlot;
    type RecoveryState = SdioHost2Recovery<H>;

    fn prepare_block_runtime(&mut self) {
        self.enable_block_lifecycle();
    }

    fn begin_recovery(&mut self, cause: RecoveryCause) -> Result<Self::RecoveryState, Error> {
        self.begin_block_recovery(cause)
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()> {
        self.poll_block_dma_quiesce(state, input)
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        self.begin_block_reinitialize(state)
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: InitInput,
    ) -> InitPoll<()> {
        self.poll_block_reinitialize(state, input)
    }

    fn acknowledge_deferred_irq(&mut self) -> Result<DeferredIrqAck, Error> {
        let event = self.irq_handle().handle_irq();
        Ok(DeferredIrqAck::from_event(&event))
    }

    fn service_request(
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
            // Keep both the request and its DMA typestate owned by the queue.
            // The runtime closes admission, drains IRQ delivery, and obtains a
            // controller-wide `DmaQuiesced` proof before calling
            // `abort_request` through `reclaim_after_quiesce`.
            Err(err) => Err(err),
            Ok(DataCommandPoll::Pending) => Ok(BlockPoll::Pending),
            Ok(DataCommandPoll::Complete(_)) => {
                slot.completed = take_protocol_completed_buffer(&mut active.inner);
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
        let result = pending
            .as_mut()
            .map_or(Ok(()), |active| active.inner.abort());
        slot.completed = pending
            .as_mut()
            .and_then(|active| take_protocol_completed_buffer(&mut active.inner));
        if result.is_ok() || slot.completed.is_some() {
            *pending = None;
            slot.active_id = None;
        }
        result
    }

    fn submit_owned_read_request(
        &mut self,
        start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        submit_owned_protocol_request(self, start_block, buffer, slot, pending, true)
    }

    fn submit_owned_write_request(
        &mut self,
        start_block: u32,
        buffer: HostRequestBuffer,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        submit_owned_protocol_request(self, start_block, buffer, slot, pending, false)
    }

    fn take_completed_buffer(slot: &mut Self::Slot) -> Option<CompletedHostBuffer> {
        slot.completed.take()
    }
}

fn submit_owned_protocol_request<H>(
    host: &mut SdioHost2Adapter<H>,
    start_block: u32,
    buffer: HostRequestBuffer,
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
    if !buffer.len().is_multiple_of(BLOCK_SIZE) {
        return Err(OwnedBlockSubmitError::new(
            BlkError::Other("buffer is not block aligned"),
            buffer,
        ));
    }
    let blocks = match u32::try_from(buffer.len() / BLOCK_SIZE) {
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
    let inner = match buffer {
        HostRequestBuffer::Dma(buffer) => {
            match host.submit_dma_data(&cmd, direction, buffer, BLOCK_SIZE as u32, blocks) {
                Ok(inner) => inner,
                Err(err) => {
                    return Err(OwnedBlockSubmitError::new(
                        map_dev_err_to_blk_err(err.error),
                        HostRequestBuffer::Dma(err.into_buffer()),
                    ));
                }
            }
        }
        HostRequestBuffer::InterruptPio(buffer) => {
            match host.submit_cpu_data(&cmd, direction, buffer, BLOCK_SIZE as u32, blocks) {
                Ok(inner) => inner,
                Err(err) => {
                    return Err(OwnedBlockSubmitError::new(
                        map_dev_err_to_blk_err(err.error),
                        HostRequestBuffer::InterruptPio(err.into_buffer()),
                    ));
                }
            }
        }
    };
    slot.active_id = Some(id);
    *pending = Some(ProtocolBlockRequest { id, inner });
    Ok(id)
}

fn take_protocol_completed_buffer<H>(
    request: &mut SdioHost2DataRequest<'_, H>,
) -> Option<CompletedHostBuffer>
where
    H: SdioHost2Irq + 'static,
{
    if let Some(buffer) = request.take_completed_dma() {
        return Some(CompletedHostBuffer::Dma(buffer));
    }
    request
        .take_completed_cpu()
        .map(CompletedHostBuffer::InterruptPio)
}
