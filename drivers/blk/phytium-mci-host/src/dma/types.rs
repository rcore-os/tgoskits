use alloc::boxed::Box;
use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    CoherentArray, CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma, PreparedDma,
};
use log::warn;
use sdmmc_protocol::{
    block::{
        BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
        CommandPoll, DataCommandPoll,
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
const IDMAC_DESC_ALIGN: usize = 32;
const IDMAC_MAX_BUF_SIZE: usize = 0x1000;
const IDMAC_DESC_LAST: u32 = 1 << 2;
const IDMAC_DESC_FIRST: u32 = 1 << 3;
const IDMAC_DESC_CHAIN: u32 = 1 << 4;
const IDMAC_DESC_END_RING: u32 = 1 << 5;
const IDMAC_DESC_OWN: u32 = 1 << 31;
const BMOD_FIXED_BURST: u32 = 1 << 1;
const BMOD_IDMAC_ENABLE: u32 = 1 << 7;
const IDSTS_TRANSMIT: u32 = crate::MCI_IDSTS_TRANSMIT;
const IDSTS_RECEIVE: u32 = crate::MCI_IDSTS_RECEIVE;
const IDSTS_NORMAL_SUMMARY: u32 = 1 << 8;
const IDSTS_ERROR_MASK: u32 = crate::MCI_IDSTS_ERROR_MASK;
const IDSTS_INT_ENABLE_MASK: u32 = IDSTS_TRANSMIT
    | IDSTS_RECEIVE
    | crate::MCI_IDSTS_ERROR_MASK
    | IDSTS_NORMAL_SUMMARY;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct IdmacDesc {
    attribute: u32,
    reserved0: u32,
    len: u32,
    reserved1: u32,
    addr_lo: u32,
    addr_hi: u32,
    desc_lo: u32,
    desc_hi: u32,
}

struct DmaProgress {
    descriptors: CoherentArray<IdmacDesc>,
    buffer: DmaRequestBuffer,
    desc_count: usize,
    complete: bool,
    idmac_done: bool,
    data_done: bool,
}

impl DmaProgress {
    fn keep_alive(&self) {
        let _ = self.descriptors.dma_addr();
        let _ = self.desc_count;
    }

    fn is_done(&self) -> bool {
        self.data_done && self.idmac_done
    }

    fn complete(self, read: bool) -> Option<CompletedDma> {
        self.buffer.complete(read)
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
        match self {
            Self::Bounce { buffer, readback } => {
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
            Self::Owned(in_flight) => Some(unsafe { in_flight.complete_after_quiesce() }),
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
    FifoRead {
        id: RequestId,
        buffer: NonNull<u8>,
        len: usize,
        block_size: usize,
        progress: FifoProgress,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    DmaRead {
        id: RequestId,
        progress: DmaProgress,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    FifoWrite {
        id: RequestId,
        buffer: NonNull<u8>,
        len: usize,
        block_size: usize,
        progress: FifoProgress,
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

#[derive(Clone, Copy, Debug, Default)]
struct FifoProgress {
    offset: usize,
    transfer_done: bool,
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
            BlockRequestKind::FifoRead { id, .. }
            | BlockRequestKind::FifoWrite { id, .. }
            | BlockRequestKind::DmaRead { id, .. }
            | BlockRequestKind::DmaWrite { id, .. } => *id,
        }
    }

    pub fn state(&self) -> BlockTransferState {
        match &self.inner {
            BlockRequestKind::FifoRead { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Fifo,
                direction: BlockTransferDirection::Read,
            },
            BlockRequestKind::DmaRead { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Dma,
                direction: BlockTransferDirection::Read,
            },
            BlockRequestKind::FifoWrite { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Fifo,
                direction: BlockTransferDirection::Write,
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
            BlockRequestKind::FifoRead { response, .. }
            | BlockRequestKind::FifoWrite { response, .. }
            | BlockRequestKind::DmaRead { response, .. }
            | BlockRequestKind::DmaWrite { response, .. } => *response,
        }
    }

    fn dma_progress_done(&self) -> bool {
        match &self.inner {
            BlockRequestKind::DmaRead { progress, .. }
            | BlockRequestKind::DmaWrite { progress, .. } => progress.is_done(),
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => true,
        }
    }
}
