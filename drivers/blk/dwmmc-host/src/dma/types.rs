use alloc::boxed::Box;
use core::{mem::ManuallyDrop, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    CoherentArray, CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma, PreparedDma,
};
use sdmmc_protocol::{
    block::{
        BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
        CommandPoll, DataCommandPoll,
    },
    cmd::{CMD12, Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, ErrorContext, Phase},
    response::Response,
};

use crate::{
    host::{DwMmc, PendingData},
    regs::RegisterBlockVolatileFieldAccess,
};

const DESC_OWN: u32 = 1 << 31;
const DESC_CH: u32 = 1 << 4;
const DESC_FS: u32 = 1 << 3;
const DESC_LD: u32 = 1 << 2;
const DESC_DIC: u32 = 1 << 1;

const BMOD_FB: u32 = 1 << 1;
const BMOD_DE: u32 = 1 << 7;
const IDMAC_INT_ENABLE: u32 = crate::event::DWMMC_IDMAC_INT_ENABLE_MASK;
pub const IDMAC_DESC_ALIGN: usize = 16;
pub const IDMAC_DESC_SIZE: usize = core::mem::size_of::<IdmacDesc>();
const BLOCK_SIZE: usize = 512;

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

// `BlockRequest` owns the DMA mappings and descriptor buffer for one
// submitted transfer. Moving that ownership to another queue thread does not
// grant shared access to the mapped memory; completion still requires a
// mutable `DwMmc` reference and consumes the request.
unsafe impl Send for BlockRequest {}

enum BlockRequestKind {
    FifoRead {
        id: RequestId,
        buffer: NonNull<u8>,
        len: usize,
        offset: usize,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        transfer_done: bool,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    FifoWrite {
        id: RequestId,
        buffer: NonNull<u8>,
        len: usize,
        offset: usize,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        transfer_done: bool,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Read {
        id: RequestId,
        buffer: DmaRequestBuffer,
        descriptors: InFlightIdmacDescriptors,
        completion: DmaCompletionLatch,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Write {
        id: RequestId,
        buffer: DmaRequestBuffer,
        descriptors: InFlightIdmacDescriptors,
        completion: DmaCompletionLatch,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
}

/// Descriptor storage that the controller may still fetch after request drop.
///
/// Ordinary drop deliberately quarantines the coherent allocation. Terminal
/// transfer evidence or controller-wide reset is required before releasing it
/// to the DMA domain.
struct InFlightIdmacDescriptors {
    storage: ManuallyDrop<CoherentArray<IdmacDesc>>,
}

impl InFlightIdmacDescriptors {
    fn new(storage: CoherentArray<IdmacDesc>) -> Self {
        Self {
            storage: ManuallyDrop::new(storage),
        }
    }

    /// Release descriptor storage after IDMAC can no longer fetch it.
    ///
    /// # Safety
    ///
    /// The matching IDMAC and controller data engine must have reached
    /// terminal completion or been quiesced by controller-wide reset.
    unsafe fn release_after_quiesce(mut self) {
        unsafe { ManuallyDrop::drop(&mut self.storage) };
    }
}

impl Drop for InFlightIdmacDescriptors {
    fn drop(&mut self) {
        // `storage` is ManuallyDrop: ordinary request teardown intentionally
        // leaves the allocation quarantined while hardware ownership is
        // unproven.
    }
}

enum DmaRequestBuffer {
    Bounce {
        buffer: InFlightDma,
        readback: Option<(NonNull<u8>, usize)>,
    },
    Owned(InFlightDma),
}

#[derive(Default)]
struct DmaCompletionLatch {
    idmac_complete: bool,
    data_over: bool,
}

impl DmaCompletionLatch {
    fn observe(&mut self, controller_status: u32, idmac_status: u32) -> BlockPoll {
        self.idmac_complete |=
            idmac_status & crate::event::DWMMC_IDMAC_INT_TRANSFER_MASK != 0;
        self.data_over |= controller_status & crate::DWMMC_INT_DATA_TRANSFER_OVER != 0;
        if self.idmac_complete && self.data_over {
            BlockPoll::Complete
        } else {
            BlockPoll::Pending
        }
    }
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
            | BlockRequestKind::Read { id, .. }
            | BlockRequestKind::Write { id, .. } => *id,
        }
    }

    pub fn state(&self) -> BlockTransferState {
        match &self.inner {
            BlockRequestKind::FifoRead { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Fifo,
                direction: BlockTransferDirection::Read,
            },
            BlockRequestKind::FifoWrite { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Fifo,
                direction: BlockTransferDirection::Write,
            },
            BlockRequestKind::Read { id, .. } => BlockTransferState::Submitted {
                id: *id,
                mode: BlockTransferMode::Dma,
                direction: BlockTransferDirection::Read,
            },
            BlockRequestKind::Write { id, .. } => BlockTransferState::Submitted {
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
            | BlockRequestKind::Read { response, .. }
            | BlockRequestKind::Write { response, .. } => *response,
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdmacDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

impl IdmacDesc {
    pub fn chained(buffer_dma: u32, len: u32, next_desc_dma: u32, first: bool, last: bool) -> Self {
        let mut des0 = DESC_OWN;
        if !last {
            des0 |= DESC_CH | DESC_DIC;
        }
        if first {
            des0 |= DESC_FS;
        }
        if last {
            des0 |= DESC_LD;
        }
        Self {
            des0,
            des1: len,
            des2: buffer_dma,
            des3: next_desc_dma,
        }
    }
}
