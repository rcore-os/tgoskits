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
        BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
        CommandPoll, DataCommandPoll,
    },
    cmd::{Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, ErrorContext, Phase},
    response::Response,
};

use crate::{
    command::CommandState,
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
}

impl BlockRequestSlot {
    pub fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
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
// mutable `Sdhci` reference and consumes the request.
unsafe impl Send for BlockRequest {}

enum BlockRequestKind {
    FifoRead {
        id: RequestId,
        buffer: NonNull<u8>,
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
        _desc: CoherentArray<Adma2Desc32>,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Write {
        id: RequestId,
        buffer: DmaRequestBuffer,
        _desc: CoherentArray<Adma2Desc32>,
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

impl Sdhci {
    /// Submit one block read using the requested transfer engine.
    ///
    /// Both `BlockTransferMode::Dma` and `BlockTransferMode::Fifo` use the
    /// same submit/poll queue contract. Runtimes that cannot use DMA can
    /// submit FIFO requests without changing the external block queue shape.
    pub fn submit_read_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        mode: BlockTransferMode,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        self.check_not_poisoned()?;
        let id = slot.start(mode, BlockTransferDirection::Read)?;
        let result = match mode {
            BlockTransferMode::Dma => {
                let dma = dma.ok_or(Error::UnsupportedCommand)?;
                self.build_dma_read_request(start_block, buffer, size, dma, id)
            }
            BlockTransferMode::Fifo => self.build_fifo_read_request(start_block, buffer, size, id),
            // Future BlockTransferMode variants are not supported by this controller.
            _ => Err(Error::UnsupportedCommand),
        };
        match result {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    /// Submit one block write using the requested transfer engine.
    ///
    /// See [`Sdhci::submit_read_blocks`] for the completion contract.
    pub fn submit_write_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        mode: BlockTransferMode,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        self.check_not_poisoned()?;
        let id = slot.start(mode, BlockTransferDirection::Write)?;
        let result = match mode {
            BlockTransferMode::Dma => {
                let dma = dma.ok_or(Error::UnsupportedCommand)?;
                self.build_dma_write_request(start_block, buffer, size, dma, id)
            }
            BlockTransferMode::Fifo => self.build_fifo_write_request(start_block, buffer, size, id),
            // Future BlockTransferMode variants are not supported by this controller.
            _ => Err(Error::UnsupportedCommand),
        };
        match result {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub fn submit_prepared_read_blocks(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if let Err(err) = self.check_not_poisoned() {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let id = match slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read) {
            Ok(id) => id,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        match self.build_prepared_dma_read_request(start_block, buffer, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub fn submit_prepared_write_blocks(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if let Err(err) = self.check_not_poisoned() {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let id = match slot.start(BlockTransferMode::Dma, BlockTransferDirection::Write) {
            Ok(id) => id,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        match self.build_prepared_dma_write_request(start_block, buffer, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    /// Poll a previously submitted block request.
    pub fn poll_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockPoll, Error> {
        match self.poll_block_request_response(request, id, slot)? {
            DataCommandPoll::Pending => Ok(BlockPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(BlockPoll::Complete),
            // Future DataCommandPoll variants are treated as completion.
            _ => Ok(BlockPoll::Complete),
        }
    }

    pub fn poll_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let Some(active) = request.as_ref() else {
            return Err(Error::InvalidArgument);
        };
        if active.id() != id {
            return Err(Error::InvalidArgument);
        }

        if matches!(
            active.inner,
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. }
        ) {
            return self.poll_fifo_request(request, id, slot);
        }

        let (cmd_index, phase, stage) = match &active.inner {
            BlockRequestKind::Read {
                cmd_index,
                phase,
                stage,
                ..
            }
            | BlockRequestKind::Write {
                cmd_index,
                phase,
                stage,
                ..
            } => (*cmd_index, *phase, *stage),
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => {
                unreachable!()
            }
        };

        if stage == BlockRequestStage::Command {
            match self.poll_command() {
                Ok(CommandPoll::Pending) => return Ok(DataCommandPoll::Pending),
                Ok(CommandPoll::Complete) => {
                    let response = self.take_command_response()?;
                    if let Some(active) = request.as_mut() {
                        match &mut active.inner {
                            BlockRequestKind::Read {
                                stage,
                                response: stored_response,
                                ..
                            }
                            | BlockRequestKind::Write {
                                stage,
                                response: stored_response,
                                ..
                            } => {
                                *stage = BlockRequestStage::Data;
                                *stored_response = Some(response);
                            }
                            BlockRequestKind::FifoRead { .. }
                            | BlockRequestKind::FifoWrite { .. } => unreachable!(),
                        }
                    }
                }
                // Future CommandPoll variants: best-effort, treat as still pending.
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => {
                    let _ = self.abort_block_request(request, id, slot);
                    return Err(err);
                }
            }
        }

        if stage == BlockRequestStage::Stop {
            return self.poll_block_stop(request, id, slot);
        }

        match self.poll_data_complete_with_adma(cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_dma_data(request, id, slot),
            // Future BlockPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                let _ = self.abort_block_request(request, id, slot);
                Err(err)
            }
        }
    }

    pub fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        self.abort_block_request(request, id, slot)
    }

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let block_count = dma_read_block_count(size)?;
        let backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::FromDevice)
            .map_err(map_dma_error)?;
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            dma_addr,
            &mut desc,
            DataDirection::Read,
            Phase::DataRead,
        )?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: Some((buffer, size.get())),
                },
                _desc: desc,
                cmd_index: cmd.index,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Command,
                stop_after_complete: block_count > 1,
                response: None,
            },
        })
    }

    fn build_dma_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let block_count = dma_write_block_count(size)?;
        let mut backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::ToDevice)
            .map_err(map_dma_error)?;
        backing.copy_to_device_from_slice(unsafe {
            core::slice::from_raw_parts(buffer.as_ptr(), size.get())
        });
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            dma_addr,
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        )?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: None,
                },
                _desc: desc,
                cmd_index: cmd.index,
                phase: Phase::DataWrite,
                stage: BlockRequestStage::Command,
                stop_after_complete: block_count > 1,
                response: None,
            },
        })
    }

    fn build_prepared_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if !self.supports_adma2() {
            return Err(PreparedDmaSubmitError::new(
                Error::UnsupportedCommand,
                buffer,
            ));
        }
        if buffer.direction() != DmaDirection::FromDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_read_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => return Err(PreparedDmaSubmitError::new(map_dma_error(err), buffer)),
        };
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        match self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Read,
            Phase::DataRead,
        ) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
                _desc: desc,
                cmd_index: cmd.index,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Command,
                stop_after_complete: block_count > 1,
                response: None,
            },
        })
    }

    fn build_prepared_dma_write_request(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if !self.supports_adma2() {
            return Err(PreparedDmaSubmitError::new(
                Error::UnsupportedCommand,
                buffer,
            ));
        }
        if buffer.direction() != DmaDirection::ToDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_write_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => return Err(PreparedDmaSubmitError::new(map_dma_error(err), buffer)),
        };
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        match self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        ) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
                _desc: desc,
                cmd_index: cmd.index,
                phase: Phase::DataWrite,
                stage: BlockRequestStage::Command,
                stop_after_complete: block_count > 1,
                response: None,
            },
        })
    }

    fn build_fifo_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = dma_read_block_count(size)?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.build_fifo_data_request(
            &cmd,
            buffer,
            size.get(),
            BLOCK_SIZE as u32,
            block_count,
            id,
            DataDirection::Read,
            block_count > 1,
        )
    }

    fn build_fifo_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = dma_write_block_count(size)?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.build_fifo_data_request(
            &cmd,
            buffer,
            size.get(),
            BLOCK_SIZE as u32,
            block_count,
            id,
            DataDirection::Write,
            block_count > 1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_fifo_data_request(
        &mut self,
        cmd: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        self.check_not_poisoned()?;
        let transfer_direction = match direction {
            DataDirection::Read => BlockTransferDirection::Read,
            DataDirection::Write => BlockTransferDirection::Write,
            DataDirection::None => return Err(Error::InvalidArgument),
            // Future DataDirection variants are not supported by this engine.
            _ => return Err(Error::InvalidArgument),
        };
        let id = slot.start(BlockTransferMode::Fifo, transfer_direction)?;
        match self.build_fifo_data_request(
            cmd,
            buffer,
            len,
            block_size,
            block_count,
            id,
            direction,
            false,
        ) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_fifo_data_request(
        &mut self,
        cmd: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        id: RequestId,
        direction: DataDirection,
        stop_after_complete: bool,
    ) -> Result<BlockRequest, Error> {
        let block_size_usize = usize::try_from(block_size).map_err(|_| Error::InvalidArgument)?;
        let block_count_usize = usize::try_from(block_count).map_err(|_| Error::InvalidArgument)?;
        if block_size_usize == 0
            || block_count_usize == 0
            || len != block_size_usize.saturating_mul(block_count_usize)
        {
            return Err(Error::InvalidArgument);
        }
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => return Err(Error::InvalidArgument),
            // Future DataDirection variants are not supported by this engine.
            _ => return Err(Error::InvalidArgument),
        };
        self.pending_data = Some(PendingData {
            direction,
            block_size,
            block_count,
        });
        self.use_dma = false;
        self.submit_command(cmd)?;
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::FifoRead {
                id,
                buffer,
                len,
                block_size: block_size_usize,
                offset: 0,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::Write => BlockRequestKind::FifoWrite {
                id,
                buffer,
                len,
                block_size: block_size_usize,
                offset: 0,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::None => return Err(Error::InvalidArgument),
            // Future DataDirection variants are not supported by this engine.
            _ => return Err(Error::InvalidArgument),
        };
        Ok(BlockRequest { inner })
    }

    fn submit_adma2_blocks_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut CoherentArray<Adma2Desc32>,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<(), Error> {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let byte_count = block_count
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(Error::InvalidArgument)? as usize;
        build_descriptors_into_dma(desc, buffer_dma, byte_count, phase)?;

        let desc_bus = desc.dma_addr().as_u64();
        let desc_end = desc_bus
            .checked_add(desc.bytes_len() as u64)
            .ok_or(Error::InvalidArgument)?;
        if desc_end > u32::MAX as u64 + 1 {
            return Err(Error::BadResponse(ErrorContext::new(phase)));
        }

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.use_dma = true;
        self.select_adma2_32();
        self.write_adma_addr(desc_bus as u32);
        let response = self.submit_command(cmd);
        self.use_dma = false;
        response
    }

    fn finish_block_request(
        &mut self,
        request: BlockRequest,
    ) -> Result<Option<CompletedDma>, Error> {
        self.finish_block_request_with_quiesce(request, true)
    }

    fn finish_block_request_with_quiesce(
        &mut self,
        request: BlockRequest,
        quiesced: bool,
    ) -> Result<Option<CompletedDma>, Error> {
        if !quiesced {
            self.poison_dma();
            core::mem::forget(request);
            self.pending_data = None;
            self.active_data_cmd = 0;
            self.irq.state.end_request();
            return Ok(None);
        }
        let completed_dma = match request.inner {
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => None,
            BlockRequestKind::Read { stage, buffer, .. } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                if quiesced {
                    buffer.complete(true)
                } else {
                    buffer.abort(true, false)
                }
            }
            BlockRequestKind::Write { stage, buffer, .. } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                if quiesced {
                    buffer.complete(false)
                } else {
                    buffer.abort(false, false)
                }
            }
        };
        self.pending_data = None;
        self.active_data_cmd = 0;
        self.irq.state.end_request();
        Ok(completed_dma)
    }

    fn finish_dma_data(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let Some(active) = request.as_mut() else {
            return Err(Error::InvalidArgument);
        };

        let stop_after_complete = match &mut active.inner {
            BlockRequestKind::Read {
                stop_after_complete,
                stage,
                ..
            } => {
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            BlockRequestKind::Write {
                stop_after_complete,
                stage,
                ..
            } => {
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => {
                return Err(Error::InvalidArgument);
            }
        };

        if stop_after_complete {
            self.submit_command(&sdmmc_protocol::cmd::CMD12)?;
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active)?;
        slot.complete_with_dma(id, completed_dma)?;
        Ok(DataCommandPoll::Complete(response))
    }

    fn poll_block_stop(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        match self.poll_command() {
            Ok(CommandPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(CommandPoll::Complete) => {
                let _ = self.take_command_response()?;
                let active = request.take().ok_or(Error::InvalidArgument)?;
                let response = active.response().ok_or(Error::InvalidArgument)?;
                let completed_dma = self.finish_block_request(active)?;
                slot.complete_with_dma(id, completed_dma)?;
                Ok(DataCommandPoll::Complete(response))
            }
            // Future CommandPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                let _ = self.abort_block_request(request, id, slot);
                Err(err)
            }
        }
    }

    fn poll_fifo_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let (cmd_index, phase, stage) = match request.as_ref().map(|request| &request.inner) {
            Some(BlockRequestKind::FifoRead {
                cmd_index,
                phase,
                stage,
                ..
            })
            | Some(BlockRequestKind::FifoWrite {
                cmd_index,
                phase,
                stage,
                ..
            }) => (*cmd_index, *phase, *stage),
            _ => return Err(Error::InvalidArgument),
        };

        if stage == BlockRequestStage::Command {
            match self.poll_command() {
                Ok(CommandPoll::Pending) => return Ok(DataCommandPoll::Pending),
                Ok(CommandPoll::Complete) => {
                    let response = self.take_command_response()?;
                    if let Some(active) = request.as_mut() {
                        match &mut active.inner {
                            BlockRequestKind::FifoRead {
                                response: stored_response,
                                ..
                            }
                            | BlockRequestKind::FifoWrite {
                                response: stored_response,
                                ..
                            } => *stored_response = Some(response),
                            _ => return Err(Error::InvalidArgument),
                        }
                    }
                    set_fifo_stage(request, BlockRequestStage::Data)?;
                }
                // Future CommandPoll variants: best-effort, treat as still pending.
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => {
                    let _ = self.abort_block_request(request, id, slot);
                    return Err(err);
                }
            }
        }

        let stage = match request.as_ref().map(|request| &request.inner) {
            Some(BlockRequestKind::FifoRead { stage, .. })
            | Some(BlockRequestKind::FifoWrite { stage, .. }) => *stage,
            _ => return Err(Error::InvalidArgument),
        };

        if stage == BlockRequestStage::Stop {
            return self.poll_block_stop(request, id, slot);
        }

        match self.poll_fifo_data_step(request, cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_fifo_data(request, id, slot),
            // Future BlockPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                let _ = self.abort_block_request(request, id, slot);
                Err(err)
            }
        }
    }

    fn poll_fifo_data_step(
        &mut self,
        request: &mut Option<BlockRequest>,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockPoll, Error> {
        let Some(active) = request.as_mut() else {
            return Err(Error::InvalidArgument);
        };
        match &mut active.inner {
            BlockRequestKind::FifoRead {
                buffer,
                len,
                block_size,
                offset,
                ..
            } => poll_fifo_read_step(self, *buffer, *len, *block_size, offset, cmd_index, phase),
            BlockRequestKind::FifoWrite {
                buffer,
                len,
                block_size,
                offset,
                ..
            } => poll_fifo_write_step(self, *buffer, *len, *block_size, offset, cmd_index, phase),
            _ => Err(Error::InvalidArgument),
        }
    }

    fn finish_fifo_data(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let Some(active) = request.as_mut() else {
            return Err(Error::InvalidArgument);
        };
        let stop_after_complete = match &mut active.inner {
            BlockRequestKind::FifoRead {
                stop_after_complete,
                stage,
                ..
            }
            | BlockRequestKind::FifoWrite {
                stop_after_complete,
                stage,
                ..
            } => {
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            _ => return Err(Error::InvalidArgument),
        };

        if stop_after_complete {
            self.submit_command(&sdmmc_protocol::cmd::CMD12)?;
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active)?;
        drop(completed_dma);
        slot.complete(id)?;
        Ok(DataCommandPoll::Complete(response))
    }

    fn abort_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        let active = request.take().ok_or(Error::InvalidArgument)?;
        let recovery = self.recover_after_adma2_error();
        let completed_dma = self.finish_block_request_with_quiesce(active, recovery.is_ok())?;
        drop(completed_dma);
        slot.complete(id)?;
        recovery
    }

    fn recover_after_adma2_error(&mut self) -> Result<(), Error> {
        let was_irq_enabled = self.completion_irq_enabled();
        self.use_dma = false;
        self.pending_data = None;
        self.active_data_cmd = 0;
        self.command_state = CommandState::Idle;
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
        self.clear_cached_irq_status();

        let cmd = self.reset_cmd();
        let dat = self.reset_dat();
        match (cmd, dat) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), _) | (_, Err(err)) => {
                let fallback = self.reset_all();
                self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
                self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
                self.clear_cached_irq_status();
                self.restore_completion_irq_after_reset(was_irq_enabled);
                fallback.map_err(|_| err)
            }
        }
    }

    pub(crate) fn poll_data_complete_with_adma(
        &mut self,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockPoll, Error> {
        let (status, err) = self.take_data_irq_status();
        if status & NORMAL_INT_XFER_COMPLETE != 0 {
            return Ok(BlockPoll::Complete);
        }
        if status & NORMAL_INT_ERROR != 0 {
            let ctx = ErrorContext::for_cmd(phase, cmd_index);
            return Err(if err & ERROR_INT_ADMA != 0 {
                Error::Misaligned
            } else if err & (ERROR_INT_DATA_TIMEOUT | ERROR_INT_CMD_TIMEOUT) != 0 {
                Error::Timeout(ctx)
            } else if err & (ERROR_INT_DATA_CRC | ERROR_INT_CMD_CRC) != 0 {
                Error::Crc(ctx)
            } else if matches!(phase, Phase::DataRead) {
                Error::ReadError(ctx)
            } else {
                Error::WriteError(ctx)
            });
        }
        Ok(BlockPoll::Pending)
    }

    fn poll_fifo_data_complete(
        &mut self,
        cmd_index: u8,
        phase: Phase,
        write: bool,
    ) -> Result<BlockPoll, Error> {
        match self.poll_data_complete_with_adma(cmd_index, phase)? {
            BlockPoll::Pending if !data_line_inhibited(self) => Ok(BlockPoll::Complete),
            // Some DWCMSHC instances can miss the polling-visible transfer
            // complete bit for PIO writes. Once the FIFO path has pushed the
            // last word, DAT0 high is the card-side busy release signal; the
            // buffer-write-ready bit is not guaranteed to remain asserted at
            // that point.
            BlockPoll::Pending if write && fifo_write_not_busy(self) => Ok(BlockPoll::Complete),
            poll => Ok(poll),
        }
    }
}

fn build_descriptors_into_dma(
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

fn set_fifo_stage(
    request: &mut Option<BlockRequest>,
    next: BlockRequestStage,
) -> Result<(), Error> {
    let Some(active) = request.as_mut() else {
        return Err(Error::InvalidArgument);
    };
    match &mut active.inner {
        BlockRequestKind::FifoRead { stage, .. } | BlockRequestKind::FifoWrite { stage, .. } => {
            *stage = next;
            Ok(())
        }
        _ => Err(Error::InvalidArgument),
    }
}

fn poll_fifo_read_step(
    host: &mut Sdhci,
    buffer: NonNull<u8>,
    len: usize,
    block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    if *offset >= len {
        return host.poll_fifo_data_complete(cmd_index, phase, false);
    }

    let (status, error) =
        host.take_fifo_irq_status(NORMAL_INT_BUFFER_READ_READY | NORMAL_INT_ERROR);
    if status & NORMAL_INT_ERROR != 0 {
        return poll_fifo_status(host, status, error, cmd_index, phase, true);
    }
    if status & NORMAL_INT_BUFFER_READ_READY == 0
        && !fifo_present_state_ready(host, PRESENT_BUFFER_READ_ENABLE)
    {
        return poll_fifo_status(host, status, error, cmd_index, phase, true);
    }

    let end = (*offset + block_size).min(len);
    let block =
        unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr().add(*offset), end - *offset) };
    for word_chunk in block.chunks_mut(4) {
        let word = host.read_u32(REG_BUFFER_DATA_PORT);
        let bytes = word.to_le_bytes();
        for (i, b) in word_chunk.iter_mut().enumerate() {
            *b = bytes[i];
        }
    }
    *offset = end;
    Ok(BlockPoll::Pending)
}

fn poll_fifo_write_step(
    host: &mut Sdhci,
    buffer: NonNull<u8>,
    len: usize,
    block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    if *offset >= len {
        return host.poll_fifo_data_complete(cmd_index, phase, true);
    }

    let (status, error) =
        host.take_fifo_irq_status(NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_ERROR);
    if status & NORMAL_INT_ERROR != 0 {
        return poll_fifo_status(host, status, error, cmd_index, phase, false);
    }
    if status & NORMAL_INT_BUFFER_WRITE_READY == 0
        && !fifo_present_state_ready(host, PRESENT_BUFFER_WRITE_ENABLE)
    {
        return poll_fifo_status(host, status, error, cmd_index, phase, false);
    }

    let end = (*offset + block_size).min(len);
    let block = unsafe { core::slice::from_raw_parts(buffer.as_ptr().add(*offset), end - *offset) };
    for word_chunk in block.chunks(4) {
        let mut bytes = [0u8; 4];
        for (i, b) in word_chunk.iter().enumerate() {
            bytes[i] = *b;
        }
        host.write_u32(REG_BUFFER_DATA_PORT, u32::from_le_bytes(bytes));
    }
    *offset = end;
    Ok(BlockPoll::Pending)
}

fn fifo_present_state_ready(host: &Sdhci, ready_mask: u32) -> bool {
    host.read_u32(REG_PRESENT_STATE) & ready_mask != 0
}

fn data_line_inhibited(host: &Sdhci) -> bool {
    host.read_u32(REG_PRESENT_STATE) & PRESENT_DAT_INHIBIT != 0
}

fn fifo_write_not_busy(host: &Sdhci) -> bool {
    host.read_u32(REG_PRESENT_STATE) & PRESENT_DAT0_LINE_SIGNAL_LEVEL != 0
}

fn poll_fifo_status(
    host: &mut Sdhci,
    status: u16,
    error: u16,
    cmd_index: u8,
    phase: Phase,
    read: bool,
) -> Result<BlockPoll, Error> {
    if status & NORMAL_INT_ERROR == 0 {
        return Ok(BlockPoll::Pending);
    }

    log::info!(
        "sdhci: data buffer cached status CMD{} normal={:#06x} error={:#06x}",
        cmd_index,
        status,
        error
    );
    host.log_status("data buffer error", cmd_index);
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
    host.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
    let _ = host.reset_cmd();
    let _ = host.reset_dat();
    let ctx = ErrorContext::for_cmd(phase, cmd_index);
    Err(
        if error & (ERROR_INT_DATA_TIMEOUT | ERROR_INT_CMD_TIMEOUT) != 0 {
            Error::Timeout(ctx)
        } else if error & (ERROR_INT_DATA_CRC | ERROR_INT_CMD_CRC) != 0 {
            Error::Crc(ctx)
        } else if read {
            Error::ReadError(ctx)
        } else {
            Error::WriteError(ctx)
        },
    )
}

fn dma_read_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    let len = size.get();
    if !len.is_multiple_of(BLOCK_SIZE) {
        return Err(Error::Misaligned);
    }
    let blocks = len / BLOCK_SIZE;
    u32::try_from(blocks).map_err(|_| Error::InvalidArgument)
}

fn dma_write_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    dma_read_block_count(size)
}

fn map_dma_error(err: dma_api::DmaError) -> Error {
    match err {
        dma_api::DmaError::NoMemory => Error::BusError(ErrorContext::new(Phase::DataRead)),
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
mod tests {
    use core::ptr::NonNull;

    use sdmmc_protocol::response::Response;

    use super::*;

    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    fn empty_table() -> [Adma2Desc32; ADMA2_DESC_COUNT] {
        [Adma2Desc32 {
            attr: 0,
            length: 0,
            address: 0,
        }; ADMA2_DESC_COUNT]
    }

    #[test]
    fn single_descriptor_for_small_buffer() {
        let mut table = empty_table();
        let n = build_descriptors(&mut table, 0x1000_0000, 512, Phase::DataRead).unwrap();
        assert_eq!(n, 1);
        assert_eq!(table[0].length, 512);
        assert_eq!(table[0].address, 0x1000_0000);
        // Valid + End + Tran action
        assert_eq!(
            table[0].attr,
            ADMA2_ATTR_VALID | ADMA2_ATTR_END | ADMA2_ATTR_ACT_TRAN
        );
    }

    #[test]
    fn splits_across_max_chunk() {
        let mut table = empty_table();
        let total = ADMA2_MAX_PER_DESC + 4096;
        let n = build_descriptors(&mut table, 0x2000_0000, total, Phase::DataRead).unwrap();
        assert_eq!(n, 2);
        assert_eq!(table[0].length as usize, ADMA2_MAX_PER_DESC);
        // first descriptor must NOT have END
        assert!(table[0].attr & ADMA2_ATTR_END == 0);
        // second descriptor covers the tail and has END
        assert_eq!(table[1].length, 4096);
        assert!(table[1].attr & ADMA2_ATTR_END != 0);
        assert_eq!(table[1].address, 0x2000_0000 + ADMA2_MAX_PER_DESC as u32);
    }

    #[test]
    fn splits_at_dwcmshc_128m_boundary() {
        let mut table = empty_table();
        let base = DWC_MSHC_ADMA_BOUNDARY - 1024;
        let n = build_descriptors(&mut table, base, 4096, Phase::DataRead).unwrap();

        assert_eq!(n, 2);
        assert_eq!(table[0].length, 1024);
        assert_eq!(table[0].address, base as u32);
        assert!(table[0].attr & ADMA2_ATTR_END == 0);
        assert_eq!(table[1].length, 3072);
        assert_eq!(table[1].address, DWC_MSHC_ADMA_BOUNDARY as u32);
        assert!(table[1].attr & ADMA2_ATTR_END != 0);
    }

    #[test]
    fn rejects_64bit_bus_address() {
        let mut table = empty_table();
        let err = build_descriptors(&mut table, 0x1_0000_0000, 512, Phase::DataRead).unwrap_err();
        assert!(matches!(err, Error::BadResponse(_)));
    }

    #[test]
    fn rejects_zero_length() {
        let mut table = empty_table();
        let err = build_descriptors(&mut table, 0, 0, Phase::DataRead).unwrap_err();
        assert!(matches!(err, Error::Misaligned));
    }

    #[test]
    fn sdhci_dma_read_plan_rejects_non_block_sized_buffers() {
        let size = core::num::NonZeroUsize::new(513).unwrap();
        assert_eq!(dma_read_block_count(size), Err(Error::Misaligned));
    }

    #[test]
    fn sdhci_dma_read_plan_reports_block_count() {
        let size = core::num::NonZeroUsize::new(1024).unwrap();
        assert_eq!(dma_read_block_count(size), Ok(2));
    }

    #[test]
    fn sdhci_dma_write_plan_rejects_non_block_sized_buffers() {
        let size = core::num::NonZeroUsize::new(513).unwrap();
        assert_eq!(dma_write_block_count(size), Err(Error::Misaligned));
    }

    #[test]
    fn block_request_slot_rejects_second_request_until_completed() {
        let mut slot = BlockRequestSlot::default();
        let first = slot
            .start(BlockTransferMode::Dma, BlockTransferDirection::Read)
            .unwrap();

        assert_eq!(
            slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read),
            Err(Error::UnsupportedCommand)
        );
        assert_eq!(
            slot.complete(RequestId::new(usize::from(first) + 1)),
            Err(Error::InvalidArgument)
        );
        assert_eq!(slot.complete(first), Ok(()));
        assert!(
            slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read)
                .is_ok()
        );
    }

    #[test]
    fn block_request_can_cross_queue_thread_boundary() {
        fn assert_send<T: Send>() {}

        assert_send::<BlockRequest>();
        assert_send::<BlockRequestSlot>();
    }

    #[test]
    fn block_poll_consumes_data_complete_cached_with_command_complete() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Fifo, BlockTransferDirection::Write)
            .unwrap();
        let buffer = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoWrite {
                id,
                buffer,
                len: 0,
                block_size: BLOCK_SIZE,
                offset: 0,
                cmd_index: 24,
                phase: Phase::DataWrite,
                stage: BlockRequestStage::Command,
                stop_after_complete: false,
                response: None,
            },
        });
        host.command_state = CommandState::Complete {
            response: Response::Empty,
        };
        host.enable_completion_irq();
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE,
            0,
        );

        assert_eq!(
            host.poll_block_request(&mut request, id, &mut slot),
            Ok(BlockPoll::Complete)
        );
        assert!(request.is_none());
        assert!(matches!(slot.state(), BlockTransferState::Idle));
    }

    #[test]
    fn fifo_write_step_accepts_present_state_ready_without_irq_status() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut buffer = [0x5au8; BLOCK_SIZE];
        buffer[BLOCK_SIZE - 4..].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
        let mut offset = 0;
        host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_WRITE_ENABLE);

        assert_eq!(
            poll_fifo_write_step(
                &mut host,
                ptr,
                buffer.len(),
                BLOCK_SIZE,
                &mut offset,
                24,
                Phase::DataWrite,
            ),
            Ok(BlockPoll::Pending)
        );

        assert_eq!(offset, BLOCK_SIZE);
        assert_eq!(host.read_u32(REG_BUFFER_DATA_PORT), 0x1122_3344);
    }

    #[test]
    fn fifo_read_step_accepts_present_state_ready_without_irq_status() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut buffer = [0u8; BLOCK_SIZE];
        let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
        let mut offset = 0;
        host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_READ_ENABLE);
        host.write_u32(REG_BUFFER_DATA_PORT, 0xaabb_ccdd);

        assert_eq!(
            poll_fifo_read_step(
                &mut host,
                ptr,
                4,
                BLOCK_SIZE,
                &mut offset,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Pending)
        );

        assert_eq!(offset, 4);
        assert_eq!(&buffer[..4], &0xaabb_ccddu32.to_le_bytes());
    }

    #[test]
    fn fifo_data_complete_accepts_dat_inhibit_clear_without_irq_status() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut buffer = [0u8; BLOCK_SIZE];
        let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
        let mut offset = BLOCK_SIZE;
        host.write_u32(REG_PRESENT_STATE, 0);

        assert_eq!(
            poll_fifo_read_step(
                &mut host,
                ptr,
                BLOCK_SIZE,
                BLOCK_SIZE,
                &mut offset,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Complete)
        );
    }

    #[test]
    fn fifo_write_complete_waits_while_dat0_busy_without_xfer_irq() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut buffer = [0u8; BLOCK_SIZE];
        let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
        let mut offset = BLOCK_SIZE;
        host.write_u32(REG_PRESENT_STATE, PRESENT_DAT_INHIBIT);

        assert_eq!(
            poll_fifo_write_step(
                &mut host,
                ptr,
                BLOCK_SIZE,
                BLOCK_SIZE,
                &mut offset,
                24,
                Phase::DataWrite,
            ),
            Ok(BlockPoll::Pending)
        );
    }

    #[test]
    fn fifo_write_complete_accepts_dat0_ready_without_xfer_irq_or_write_ready() {
        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut buffer = [0u8; BLOCK_SIZE];
        let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
        let mut offset = BLOCK_SIZE;
        host.write_u32(
            REG_PRESENT_STATE,
            PRESENT_DAT_INHIBIT | PRESENT_DAT0_LINE_SIGNAL_LEVEL,
        );

        assert_eq!(
            poll_fifo_write_step(
                &mut host,
                ptr,
                BLOCK_SIZE,
                BLOCK_SIZE,
                &mut offset,
                24,
                Phase::DataWrite,
            ),
            Ok(BlockPoll::Complete)
        );
    }
}
