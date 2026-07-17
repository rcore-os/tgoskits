//! DMA glue for the SDHCI ADMA2 data path.
//!
//! The crate is `no_std` and refuses to assume an allocator, an MMU layout,
//! or a particular cache architecture. Callers wire those concerns up via
//! `dma-api`'s [`DeviceDma`].
//!
//! ## Responsibilities split
//!
//! - **The host driver** builds the ADMA2 descriptor table inside the
//!   DMA descriptor buffer, programs the controller, and waits on the
//!   transfer-complete IRQ.
//! - **The [`DeviceDma`] impl** translates kernel/CPU pointers to the bus
//!   addresses the SDHCI sees, and performs whatever cache maintenance is
//!   needed before the device reads CPU-written memory and after the
//!   device writes CPU-read memory.
//!
//! That split keeps the SDHCI logic portable across hosted kernels,
//! bare-metal coherent systems (identity mapping, no cache ops), and
//! bare-metal incoherent systems (identity mapping + dcache flush/invalidate).

use alloc::boxed::Box;
use core::{fmt, mem::ManuallyDrop, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    CoherentArray, CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma, PreparedDma,
};
use sdmmc_protocol::{
    block::{
        BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
        CommandPoll, DataCommandPoll,
    },
    cmd::{Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, ErrorContext, Phase},
    response::Response,
};

use crate::{
    host::{PendingData, Sdhci},
    regs::*,
};

/// 32-bit ADMA2 descriptor.
///
/// Layout (little-endian, per SDHCI v3.00 §1.13):
///
/// ```text
///   0      attr[15:0]   (Valid | End | Int | Act2 | Act1)
///   2      length[15:0] (0 means 64 KiB)
///   4      address[31:0]
/// ```
#[repr(C, align(4))]
#[derive(Clone, Copy, Default)]
pub(crate) struct Adma2Desc32 {
    attr: u16,
    length: u16,
    address: u32,
}

const ADMA2_ATTR_VALID: u16 = 1 << 0;
const ADMA2_ATTR_END: u16 = 1 << 1;
const _ADMA2_ATTR_INT: u16 = 1 << 2;
// act = 0b10 → "tran" (data transfer descriptor)
const ADMA2_ATTR_ACT_TRAN: u16 = 0b10 << 4;

/// Largest single ADMA2 transfer — the length field is 16 bits and `0`
/// is interpreted as 64 KiB, but we cap a hair below to keep the math
/// trivial and to leave room for hosts whose ADMA engine refuses
/// `length == 0` (some Synopsys MSHC variants).
const ADMA2_MAX_PER_DESC: usize = 65_528; // 64 KiB - 8B, multiple of 8

/// Caller-owned scratch region for the ADMA2 descriptor table.
///
/// Sized for a worst-case 64 KiB transfer split into 4 KiB chunks (16
/// descriptors), which is the SDMA boundary the controller falls back to
/// on page boundary crossings. Bumping this constant is the only thing
/// needed to support larger contiguous transfers.
pub const ADMA2_DESC_COUNT: usize = 16;
pub const ADMA2_DESC_ALIGN: usize = 64;
const BLOCK_SIZE: usize = 512;
pub const ADMA2_MAX_TRANSFER_SIZE: usize =
    (ADMA2_DESC_COUNT * ADMA2_MAX_PER_DESC / BLOCK_SIZE) * BLOCK_SIZE;
pub const ADMA2_MAX_BLOCKS: u32 = (ADMA2_MAX_TRANSFER_SIZE / BLOCK_SIZE) as u32;
const DWC_MSHC_ADMA_BOUNDARY: u64 = 128 * 1024 * 1024;

pub type RequestId = BlockRequestId;

#[derive(Default)]
pub struct BlockRequestSlot {
    next: usize,
    state: BlockTransferState,
    completed_dma: Option<CompletedDma>,
    completed_cpu: Option<CpuDmaBuffer>,
}

impl BlockRequestSlot {
    pub fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }

    pub fn take_completed_cpu(&mut self) -> Option<CpuDmaBuffer> {
        self.completed_cpu.take()
    }
}

pub struct BlockRequest {
    inner: BlockRequestKind,
}

pub struct PreparedDmaSubmitError {
    pub error: Error,
    buffer: Box<PreparedDma>,
}

pub struct CpuBufferSubmitError {
    pub error: Error,
    buffer: Box<CpuDmaBuffer>,
}

impl CpuBufferSubmitError {
    fn new(error: Error, buffer: CpuDmaBuffer) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub fn into_buffer(self) -> CpuDmaBuffer {
        *self.buffer
    }
}

impl fmt::Debug for CpuBufferSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuBufferSubmitError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for CpuBufferSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl core::error::Error for CpuBufferSubmitError {}

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

// Owned submissions carry their DMA/CPU allocation in `BlockRequest`.
// Low-level raw-pointer submissions are unsafe and require the caller to keep
// the allocation valid and free of conflicting accesses until terminal
// completion or proof-gated reclamation. Moving the request transfers that
// exclusive access contract; completion still requires a mutable `Sdhci`
// reference and consumes the request.
unsafe impl Send for BlockRequest {}

enum BlockRequestKind {
    FifoRead {
        id: RequestId,
        buffer: NonNull<u8>,
        owned_cpu: Option<CpuDmaBuffer>,
        len: usize,
        block_size: usize,
        offset: usize,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    FifoWrite {
        id: RequestId,
        buffer: NonNull<u8>,
        owned_cpu: Option<CpuDmaBuffer>,
        len: usize,
        block_size: usize,
        offset: usize,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Read {
        id: RequestId,
        buffer: DmaRequestBuffer,
        descriptors: InFlightAdmaDescriptors,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Write {
        id: RequestId,
        buffer: DmaRequestBuffer,
        descriptors: InFlightAdmaDescriptors,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
}

/// Descriptor storage that hardware may still fetch after request teardown.
///
/// Ordinary drop deliberately quarantines the allocation. Only a terminal IRQ
/// or controller-wide quiescence proof may release it back to the DMA domain.
struct InFlightAdmaDescriptors {
    storage: ManuallyDrop<CoherentArray<Adma2Desc32>>,
}

impl InFlightAdmaDescriptors {
    fn new(storage: CoherentArray<Adma2Desc32>) -> Self {
        Self {
            storage: ManuallyDrop::new(storage),
        }
    }

    /// Release descriptor storage after the controller can no longer fetch it.
    ///
    /// # Safety
    ///
    /// The matching data engine and DMA bus-master access must be quiesced.
    unsafe fn release_after_quiesce(mut self) {
        unsafe { ManuallyDrop::drop(&mut self.storage) };
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

    fn abort(self, read: bool, quiesced: bool) -> Option<CompletedDma> {
        self.finish(read, quiesced)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockRequestStage {
    Command,
    Data,
    Stop,
}

impl BlockRequest {
    fn retain_fifo_cpu_buffer(&mut self, buffer: CpuDmaBuffer) {
        match &mut self.inner {
            BlockRequestKind::FifoRead {
                buffer: request_ptr,
                owned_cpu,
                ..
            }
            | BlockRequestKind::FifoWrite {
                buffer: request_ptr,
                owned_cpu,
                ..
            } if *request_ptr == buffer.cpu_ptr() && owned_cpu.is_none() => {
                *owned_cpu = Some(buffer);
            }
            _ => unreachable!("owned FIFO backing must match the just-built FIFO request"),
        }
    }

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

impl BlockRequestSlot {
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
        self.complete_with_backing(id, CompletedBlockBacking::default())
    }

    fn complete_with_backing(
        &mut self,
        id: RequestId,
        completed: CompletedBlockBacking,
    ) -> Result<(), Error> {
        if self.state.id() != Some(id) {
            return Err(Error::InvalidArgument);
        }
        self.state = BlockTransferState::Idle;
        self.completed_dma = completed.dma;
        self.completed_cpu = completed.cpu;
        Ok(())
    }

    pub fn state(&self) -> BlockTransferState {
        self.state
    }
}

#[derive(Default)]
struct CompletedBlockBacking {
    dma: Option<CompletedDma>,
    cpu: Option<CpuDmaBuffer>,
}

/// Build the ADMA2 descriptor table covering `[base, base+total_len)`.
///
/// `base` is the *bus* address the controller will use, already translated
/// by [`DeviceDma`]. Returns the number of descriptors written or
/// [`Error::Misaligned`] if the buffer would not fit in
/// [`ADMA2_DESC_COUNT`] entries.
pub(crate) fn build_descriptors(
    table: &mut [Adma2Desc32; ADMA2_DESC_COUNT],
    base: u64,
    total_len: usize,
    phase: Phase,
) -> Result<usize, Error> {
    if total_len == 0 {
        return Err(Error::Misaligned);
    }
    if base >> 32 != 0 {
        // 32-bit ADMA2 only addresses the low 4 GiB. 64-bit ADMA2 needs a
        // different descriptor layout we don't ship yet — surface it as a
        // capability mismatch rather than truncating silently.
        return Err(Error::BadResponse(ErrorContext::new(phase)));
    }

    let mut remaining = total_len;
    let mut offset: u64 = 0;
    let mut written = 0usize;

    while remaining > 0 {
        if written >= ADMA2_DESC_COUNT {
            return Err(Error::Misaligned);
        }
        let boundary_room = DWC_MSHC_ADMA_BOUNDARY - ((base + offset) % DWC_MSHC_ADMA_BOUNDARY);
        let chunk = remaining
            .min(ADMA2_MAX_PER_DESC)
            .min(boundary_room as usize);
        let is_last = chunk == remaining;
        let mut attr = ADMA2_ATTR_VALID | ADMA2_ATTR_ACT_TRAN;
        if is_last {
            attr |= ADMA2_ATTR_END;
        }
        table[written] = Adma2Desc32 {
            attr,
            length: chunk as u16,
            address: (base + offset) as u32,
        };
        written += 1;
        offset += chunk as u64;
        remaining -= chunk;
    }

    Ok(written)
}

mod service;
mod submit;

use service::{
    build_descriptors_into_dma, dma_read_block_count, dma_write_block_count, map_dma_error,
};
#[cfg(test)]
use service::{poll_fifo_read_step, poll_fifo_write_step};
#[cfg(test)]
mod tests;
