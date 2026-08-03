//! Owned request progression and persistent Phytium IDMAC lifecycle.

use alloc::boxed::Box;
use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma, PreparedDma};
use log::warn;
use mbarrier::wmb;
use sdmmc_protocol::{
    block::{
        BlockProgress, BlockRequestId, BlockTransferDirection, BlockTransferMode,
        BlockTransferState, CommandProgress, DataCommandProgress,
    },
    cmd::{CMD12, Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, Phase},
    response::Response,
};

use crate::{
    host::{PendingData, PhytiumMci},
    regs::{RIntSts, RegisterBlockVolatileFieldAccess},
};

const BLOCK_SIZE: usize = 512;
const BMOD_FIXED_BURST: u32 = 1 << 1;
const BMOD_IDMAC_ENABLE: u32 = 1 << 7;
const IDSTS_NORMAL_SUMMARY: u32 = 1 << 8;
const IDSTS_ABNORMAL_SUMMARY: u32 = crate::MCI_IDSTS_ABNORMAL_SUMMARY;
const IDSTS_ERROR_MASK: u32 = crate::MCI_IDSTS_LATCH_ERROR_MASK;
const IDSTS_INT_ENABLE_MASK: u32 =
    crate::MCI_IDSTS_FATAL_BUS_ERROR | (1 << 4) | IDSTS_NORMAL_SUMMARY | IDSTS_ABNORMAL_SUMMARY;

mod idmac;
pub(crate) use idmac::{IDMAC_BUFFER_ALIGN, IdmacRing};
pub use idmac::{IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, IDMAC_MAX_BLOCKS, IDMAC_MAX_TRANSFER_SIZE};

pub(crate) struct PreparedDataCommand {
    command: Command,
    block_size: u32,
    block_count: u32,
    direction: DataDirection,
}

impl PreparedDataCommand {
    pub(crate) const fn new(
        command: Command,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
    ) -> Self {
        Self {
            command,
            block_size,
            block_count,
            direction,
        }
    }
}

struct DmaProgress {
    buffer: DmaRequestBuffer,
    data_done: bool,
}

impl DmaProgress {
    fn is_done(&self) -> bool {
        self.data_done
    }

    fn complete(self, read: bool) -> Option<CompletedDma> {
        self.buffer.complete(read)
    }

    fn abort(self, read: bool, quiesced: bool) -> Option<CompletedDma> {
        self.buffer.finish(read, quiesced)
    }
}

enum DmaRequestBuffer {
    Bounce {
        buffer: InFlightDma,
        readback: Option<(NonNull<u8>, usize)>,
    },
    Owned(InFlightDma),
}

impl DmaRequestBuffer {
    fn complete(self, read: bool) -> Option<CompletedDma> {
        self.finish(read, true)
    }

    fn finish(self, read: bool, quiesced: bool) -> Option<CompletedDma> {
        match self {
            Self::Bounce { buffer, readback } => {
                if !quiesced {
                    let _quarantined = buffer.quarantine();
                    return None;
                }
                if read {
                    let completed = unsafe { buffer.complete_after_quiesce() };
                    if let Some((dst, len)) = readback {
                        completed.copy_from_device_to_slice(unsafe {
                            core::slice::from_raw_parts_mut(dst.as_ptr(), len)
                        });
                    }
                    None
                } else {
                    drop(unsafe { buffer.complete_after_quiesce() });
                    None
                }
            }
            Self::Owned(in_flight) => {
                if !quiesced {
                    let _quarantined = in_flight.quarantine();
                    return None;
                }
                Some(unsafe { in_flight.complete_after_quiesce() })
            }
        }
    }
}

pub type RequestId = BlockRequestId;

#[derive(Default)]
pub struct BlockRequestSlot {
    next: usize,
    state: BlockTransferState,
    completed_dma: Option<CompletedDma>,
}

impl BlockRequestSlot {
    pub fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }

    pub fn start(
        &mut self,
        mode: BlockTransferMode,
        direction: BlockTransferDirection,
    ) -> Result<RequestId, Error> {
        if !matches!(self.state, BlockTransferState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        let id = RequestId::new(self.next);
        self.next = self.next.wrapping_add(1);
        self.state = BlockTransferState::Submitted {
            id,
            mode,
            direction,
        };
        Ok(id)
    }

    pub fn complete(&mut self, id: RequestId) -> Result<(), Error> {
        self.complete_with_dma(id, None)
    }

    fn complete_with_dma(
        &mut self,
        id: RequestId,
        completed_dma: Option<CompletedDma>,
    ) -> Result<(), Error> {
        if self.state.id() != Some(id) {
            return Err(Error::InvalidArgument);
        }
        self.state = BlockTransferState::Idle;
        self.completed_dma = completed_dma;
        Ok(())
    }

    pub fn state(&self) -> BlockTransferState {
        self.state
    }
}

pub struct BlockRequest {
    inner: BlockRequestKind,
}

pub struct PreparedDmaSubmitError {
    pub error: Error,
    buffer: Box<PreparedDma>,
}

impl PreparedDmaSubmitError {
    fn new(error: Error, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub fn into_buffer(self) -> PreparedDma {
        *self.buffer
    }
}

unsafe impl Send for BlockRequest {}

enum BlockRequestKind {
    DmaRead {
        id: RequestId,
        progress: DmaProgress,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    DmaWrite {
        id: RequestId,
        progress: DmaProgress,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockRequestStage {
    Command,
    Data,
    Stop,
}

impl BlockRequest {
    pub fn id(&self) -> RequestId {
        match &self.inner {
            BlockRequestKind::DmaRead { id, .. } | BlockRequestKind::DmaWrite { id, .. } => *id,
        }
    }

    fn cmd_index(&self) -> u8 {
        match &self.inner {
            BlockRequestKind::DmaRead { cmd_index, .. }
            | BlockRequestKind::DmaWrite { cmd_index, .. } => *cmd_index,
        }
    }

    fn phase(&self) -> Phase {
        match &self.inner {
            BlockRequestKind::DmaRead { phase, .. } | BlockRequestKind::DmaWrite { phase, .. } => {
                *phase
            }
        }
    }

    fn stage(&self) -> BlockRequestStage {
        match &self.inner {
            BlockRequestKind::DmaRead { stage, .. } | BlockRequestKind::DmaWrite { stage, .. } => {
                *stage
            }
        }
    }

    pub fn state(&self) -> BlockTransferState {
        match &self.inner {
            BlockRequestKind::DmaRead { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Dma,
                direction: BlockTransferDirection::Read,
            },
            BlockRequestKind::DmaWrite { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Dma,
                direction: BlockTransferDirection::Write,
            },
        }
    }

    fn response(&self) -> Option<Response> {
        match &self.inner {
            BlockRequestKind::DmaRead { response, .. }
            | BlockRequestKind::DmaWrite { response, .. } => *response,
        }
    }

    fn dma_progress_done(&self) -> bool {
        match &self.inner {
            BlockRequestKind::DmaRead { progress, .. }
            | BlockRequestKind::DmaWrite { progress, .. } => progress.is_done(),
        }
    }
}

mod completion;
mod submission;

fn store_response(request: &mut Option<BlockRequest>, response: Response) -> Result<(), Error> {
    match request.as_mut().map(|r| &mut r.inner) {
        Some(BlockRequestKind::DmaRead {
            response: stored, ..
        })
        | Some(BlockRequestKind::DmaWrite {
            response: stored, ..
        }) => {
            *stored = Some(response);
            Ok(())
        }
        None => Err(Error::InvalidArgument),
    }
}

fn set_stage(request: &mut Option<BlockRequest>, next: BlockRequestStage) -> Result<(), Error> {
    match request.as_mut().map(|r| &mut r.inner) {
        Some(BlockRequestKind::DmaRead { stage, .. })
        | Some(BlockRequestKind::DmaWrite { stage, .. }) => {
            *stage = next;
            Ok(())
        }
        None => Err(Error::InvalidArgument),
    }
}

fn block_count(size: NonZeroUsize) -> Result<u32, Error> {
    if !size.get().is_multiple_of(BLOCK_SIZE) {
        return Err(Error::InvalidArgument);
    }
    u32::try_from(size.get() / BLOCK_SIZE).map_err(|_| Error::InvalidArgument)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
