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
use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    CoherentArray, CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, InFlightDma, PreparedDma,
};
use sdmmc_protocol::{
    block::{
        BlockProgress, BlockRequestId, BlockTransferDirection, BlockTransferMode,
        BlockTransferState, CommandProgress as CommandPoll, DataCommandProgress,
    },
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    response::Response,
};

use crate::{
    command::CommandState,
    host::{PendingData, Sdhci},
    regs::*,
};

mod request;

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

/// 96-bit ADMA2 descriptor used for 64-bit system addresses in pre-v4 mode.
#[repr(C, align(4))]
#[derive(Clone, Copy, Default)]
pub(crate) struct Adma2Desc64 {
    attr: u16,
    length: u16,
    address_low: u32,
    address_high: u32,
}

pub(crate) enum Adma2DescriptorTable {
    Addr32(CoherentArray<Adma2Desc32>),
    Addr64(CoherentArray<Adma2Desc64>),
}

impl Adma2DescriptorTable {
    pub(crate) fn allocate(dma: &DeviceDma, use_64bit: bool) -> Result<Self, Error> {
        if use_64bit {
            dma.coherent_array_zero_with_align::<Adma2Desc64>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
                .map(Self::Addr64)
                .map_err(map_dma_error)
        } else {
            dma.coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
                .map(Self::Addr32)
                .map_err(map_dma_error)
        }
    }

    pub(crate) fn is_64bit(&self) -> bool {
        matches!(self, Self::Addr64(_))
    }

    pub(crate) fn dma_addr(&self) -> u64 {
        match self {
            Self::Addr32(table) => table.dma_addr().as_u64(),
            Self::Addr64(table) => table.dma_addr().as_u64(),
        }
    }

    pub(crate) fn bytes_len(&self) -> usize {
        match self {
            Self::Addr32(table) => table.bytes_len(),
            Self::Addr64(table) => table.bytes_len(),
        }
    }

    pub(crate) fn build(
        &mut self,
        base: u64,
        total_len: usize,
        phase: Phase,
    ) -> Result<usize, Error> {
        match self {
            Self::Addr32(desc) => build_descriptors32_into_dma(desc, base, total_len, phase),
            Self::Addr64(desc) => build_descriptors64_into_dma(desc, base, total_len),
        }
    }
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

/// Controller-owned scratch region for the depth-one ADMA2 queue.
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
pub const DWC_MSHC_ADMA_BOUNDARY: usize = 128 * 1024 * 1024;

pub(crate) type RequestId = BlockRequestId;

#[derive(Default)]
pub(crate) struct BlockRequestSlot {
    next: usize,
    state: BlockTransferState,
    completed_dma: Option<CompletedDma>,
}

impl BlockRequestSlot {
    pub fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }
}

pub(crate) struct BlockRequest {
    inner: BlockRequestKind,
}

pub(crate) struct PreparedDmaSubmitError {
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

// `BlockRequest` owns the payload DMA mapping for one submitted transfer.
// The depth-one controller keeps its fixed descriptor table; moving the
// request to another queue thread does not grant shared access to either
// resource. Completion still requires a mutable `Sdhci` and consumes the
// request.
unsafe impl Send for BlockRequest {}

enum BlockRequestKind {
    Read {
        id: RequestId,
        buffer: DmaRequestBuffer,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Write {
        id: RequestId,
        buffer: DmaRequestBuffer,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
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
    pub fn id(&self) -> RequestId {
        match &self.inner {
            BlockRequestKind::Read { id, .. } | BlockRequestKind::Write { id, .. } => *id,
        }
    }

    fn response(&self) -> Option<Response> {
        match &self.inner {
            BlockRequestKind::Read { response, .. } | BlockRequestKind::Write { response, .. } => {
                *response
            }
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
    // ADMA2 transfer addresses are word aligned; reject rather than relying
    // on controller-specific rounding.
    if base & 0x3 != 0 {
        return Err(Error::Misaligned);
    }
    if base >> 32 != 0 {
        // 32-bit ADMA2 only addresses the low 4 GiB. 64-bit ADMA2 needs a
        // different descriptor layout we don't ship yet — surface it as a
        // capability mismatch rather than truncating silently.
        return Err(Error::BadResponse(ErrorContext::new(phase)));
    }
    if total_len as u64 > (u32::MAX as u64 + 1).saturating_sub(base) {
        return Err(Error::BadResponse(ErrorContext::new(phase)));
    }

    let mut remaining = total_len;
    let mut offset: u64 = 0;
    let mut written = 0usize;

    while remaining > 0 {
        if written >= ADMA2_DESC_COUNT {
            return Err(Error::Misaligned);
        }
        let boundary = DWC_MSHC_ADMA_BOUNDARY as u64;
        let boundary_room = boundary - ((base + offset) % boundary);
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

fn build_descriptors32_into_dma(
    desc: &mut CoherentArray<Adma2Desc32>,
    base: u64,
    total_len: usize,
    phase: Phase,
) -> Result<usize, Error> {
    if desc.len() < ADMA2_DESC_COUNT {
        return Err(Error::InvalidArgument);
    }
    let mut table = [Adma2Desc32::default(); ADMA2_DESC_COUNT];
    let written = build_descriptors(&mut table, base, total_len, phase)?;
    desc.write_with_cpu(ADMA2_DESC_COUNT, |descs| {
        descs.copy_from_slice(&table);
    });
    Ok(written)
}

fn build_descriptors64(
    table: &mut [Adma2Desc64; ADMA2_DESC_COUNT],
    base: u64,
    total_len: usize,
) -> Result<usize, Error> {
    if total_len == 0 || base & 0x3 != 0 {
        return Err(Error::Misaligned);
    }
    base.checked_add(total_len as u64)
        .ok_or(Error::InvalidArgument)?;

    let mut remaining = total_len;
    let mut offset = 0_u64;
    let mut written = 0;
    while remaining > 0 {
        if written >= ADMA2_DESC_COUNT {
            return Err(Error::Misaligned);
        }
        let boundary = DWC_MSHC_ADMA_BOUNDARY as u64;
        let boundary_room = boundary - ((base + offset) % boundary);
        let chunk = remaining
            .min(ADMA2_MAX_PER_DESC)
            .min(boundary_room as usize);
        let mut attr = ADMA2_ATTR_VALID | ADMA2_ATTR_ACT_TRAN;
        if chunk == remaining {
            attr |= ADMA2_ATTR_END;
        }
        let address = base + offset;
        table[written] = Adma2Desc64 {
            attr,
            length: chunk as u16,
            address_low: address as u32,
            address_high: (address >> 32) as u32,
        };
        written += 1;
        offset += chunk as u64;
        remaining -= chunk;
    }
    Ok(written)
}

fn build_descriptors64_into_dma(
    desc: &mut CoherentArray<Adma2Desc64>,
    base: u64,
    total_len: usize,
) -> Result<usize, Error> {
    if desc.len() < ADMA2_DESC_COUNT {
        return Err(Error::InvalidArgument);
    }
    let mut table = [Adma2Desc64::default(); ADMA2_DESC_COUNT];
    let written = build_descriptors64(&mut table, base, total_len)?;
    desc.write_with_cpu(ADMA2_DESC_COUNT, |descs| {
        descs.copy_from_slice(&table);
    });
    Ok(written)
}

#[cfg(test)]
fn dma_read_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    let len = size.get();
    if !len.is_multiple_of(BLOCK_SIZE) {
        return Err(Error::Misaligned);
    }
    let blocks = len / BLOCK_SIZE;
    u32::try_from(blocks).map_err(|_| Error::InvalidArgument)
}

#[cfg(test)]
fn dma_write_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    dma_read_block_count(size)
}

fn block_transfer_direction(direction: DataDirection) -> Result<BlockTransferDirection, Error> {
    match direction {
        DataDirection::Read => Ok(BlockTransferDirection::Read),
        DataDirection::Write => Ok(BlockTransferDirection::Write),
        DataDirection::None => Err(Error::InvalidArgument),
        _ => Err(Error::InvalidArgument),
    }
}

fn validate_adma2_data_shape(
    block_size: u32,
    block_count: u32,
    len: usize,
) -> Result<NonZeroUsize, Error> {
    if block_size == 0 || block_size > 0x0fff || block_count == 0 || block_count > u16::MAX.into() {
        return Err(Error::InvalidArgument);
    }
    let expected_len = usize::try_from(block_size)
        .ok()
        .and_then(|size| {
            usize::try_from(block_count)
                .ok()
                .and_then(|count| size.checked_mul(count))
        })
        .ok_or(Error::InvalidArgument)?;
    if len != expected_len {
        return Err(Error::InvalidArgument);
    }
    NonZeroUsize::new(len).ok_or(Error::InvalidArgument)
}

fn command_needs_stop(cmd: &Command, block_count: u32) -> bool {
    block_count > 1 && matches!(cmd.index, 18 | 25)
}

pub(crate) fn map_dma_error(err: dma_api::DmaError) -> Error {
    match err {
        dma_api::DmaError::NoMemory | dma_api::DmaError::CoherentReleaseFailed => {
            Error::BusError(ErrorContext::new(Phase::DataRead))
        }
        dma_api::DmaError::LayoutError(_)
        | dma_api::DmaError::DmaMaskNotMatch { .. }
        | dma_api::DmaError::AlignMismatch { .. }
        | dma_api::DmaError::SegmentTooLarge { .. }
        | dma_api::DmaError::BoundaryCross { .. }
        | dma_api::DmaError::NullPointer
        | dma_api::DmaError::ZeroSizedBuffer => Error::InvalidArgument,
    }
}

#[cfg(test)]
mod tests;
