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
//! That split keeps the SDHCI logic portable across hosted Linux (where
//! `DeviceDma` typically calls `dma_map_single`), bare-metal coherent
//! systems (identity mapping, no cache ops), and bare-metal incoherent
//! systems (identity mapping + dcache flush/invalidate).

use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{DArray, DeviceDma, DmaDirection, SArrayPtr};
use sdmmc_protocol::{
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Adma2AttemptError {
    Fallback(Error),
    Fatal(Error),
}

impl Adma2AttemptError {
    pub(crate) const fn fallback(err: Error) -> Self {
        Self::Fallback(err)
    }

    const fn fatal(err: Error) -> Self {
        Self::Fatal(err)
    }

    pub(crate) const fn can_fallback_to_pio(self) -> bool {
        matches!(self, Self::Fallback(_))
    }

    pub(crate) const fn into_error(self) -> Error {
        match self {
            Self::Fallback(err) | Self::Fatal(err) => err,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(usize);

impl RequestId {
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<RequestId> for usize {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

#[derive(Default)]
pub struct AsyncRequestSlot {
    next: usize,
    active: Option<RequestId>,
}

pub struct AsyncDmaRequest {
    inner: AsyncDmaRequestKind,
}

// `AsyncDmaRequest` owns the DMA mappings and descriptor buffer for one
// submitted transfer. Moving that ownership to another queue thread does not
// grant shared access to the mapped memory; completion still requires a
// mutable `Sdhci` reference and consumes the request.
unsafe impl Send for AsyncDmaRequest {}

enum AsyncDmaRequestKind {
    Read {
        id: RequestId,
        map: SArrayPtr<u8>,
        _desc: DArray<Adma2Desc32>,
        cmd_index: u8,
        phase: Phase,
        stop_after_complete: bool,
    },
    Write {
        id: RequestId,
        _map: SArrayPtr<u8>,
        _desc: DArray<Adma2Desc32>,
        cmd_index: u8,
        phase: Phase,
        stop_after_complete: bool,
    },
}

impl AsyncDmaRequest {
    pub fn id(&self) -> RequestId {
        match &self.inner {
            AsyncDmaRequestKind::Read { id, .. } | AsyncDmaRequestKind::Write { id, .. } => *id,
        }
    }
}

impl AsyncRequestSlot {
    pub fn start(&mut self) -> Result<RequestId, Error> {
        if self.active.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let id = RequestId::new(self.next);
        self.next = self.next.wrapping_add(1);
        self.active = Some(id);
        Ok(id)
    }

    pub fn complete(&mut self, id: RequestId) -> Result<(), Error> {
        if self.active != Some(id) {
            return Err(Error::InvalidArgument);
        }
        self.active = None;
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
        let chunk = remaining.min(ADMA2_MAX_PER_DESC);
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
    pub(crate) fn try_blocking_adma2_read_transfer(
        &mut self,
        cmd: &Command,
        buf: &mut [u8],
        block_size: u32,
        expected_block_count: u32,
    ) -> Result<Response, Adma2AttemptError> {
        if !self.supports_adma2() || block_size as usize != BLOCK_SIZE || buf.is_empty() {
            return Err(Adma2AttemptError::fallback(Error::UnsupportedCommand));
        }
        let dma = self
            .dma
            .clone()
            .ok_or_else(|| Adma2AttemptError::fallback(Error::UnsupportedCommand))?;
        let size = NonZeroUsize::new(buf.len())
            .ok_or_else(|| Adma2AttemptError::fallback(Error::InvalidArgument))?;
        let block_count = dma_read_block_count(size).map_err(Adma2AttemptError::fallback)?;
        if block_count != expected_block_count {
            return Err(Adma2AttemptError::fallback(Error::InvalidArgument));
        }
        let map = dma
            .map_single_array(buf, BLOCK_SIZE, DmaDirection::FromDevice)
            .map_err(|err| Adma2AttemptError::fallback(map_dma_error(err)))?;
        let mut desc = dma
            .array_zero_with_align::<Adma2Desc32>(
                ADMA2_DESC_COUNT,
                ADMA2_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| Adma2AttemptError::fallback(map_dma_error(err)))?;

        let response = self
            .dma_data_transfer_mapped(
                cmd,
                block_count,
                map.dma_addr().as_u64(),
                &mut desc,
                DataDirection::Read,
                Phase::DataRead,
            )
            .map_err(Adma2AttemptError::fatal)?;
        map.prepare_read_all();
        Ok(response)
    }

    pub(crate) fn try_blocking_adma2_write_transfer(
        &mut self,
        cmd: &Command,
        buf: &[u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Response, Adma2AttemptError> {
        if !self.supports_adma2() || block_size as usize != BLOCK_SIZE || buf.is_empty() {
            return Err(Adma2AttemptError::fallback(Error::UnsupportedCommand));
        }
        let dma = self
            .dma
            .clone()
            .ok_or_else(|| Adma2AttemptError::fallback(Error::UnsupportedCommand))?;
        let size = NonZeroUsize::new(buf.len())
            .ok_or_else(|| Adma2AttemptError::fallback(Error::InvalidArgument))?;
        let computed_block_count =
            dma_write_block_count(size).map_err(Adma2AttemptError::fallback)?;
        if computed_block_count != block_count {
            return Err(Adma2AttemptError::fallback(Error::InvalidArgument));
        }
        let map = dma
            .map_single_array(buf, BLOCK_SIZE, DmaDirection::ToDevice)
            .map_err(|err| Adma2AttemptError::fallback(map_dma_error(err)))?;
        map.confirm_write_all();

        let mut desc = dma
            .array_zero_with_align::<Adma2Desc32>(
                ADMA2_DESC_COUNT,
                ADMA2_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| Adma2AttemptError::fallback(map_dma_error(err)))?;

        self.dma_data_transfer_mapped(
            cmd,
            block_count,
            map.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        )
        .map_err(Adma2AttemptError::fatal)
    }

    /// Read whole 512-byte blocks using the controller's 32-bit ADMA2 engine.
    ///
    /// `start_block` is the card address to place in CMD17/CMD18. Callers
    /// that know whether the card uses byte or sector addressing must apply
    /// that translation before calling this method.
    pub fn dma_read_blocks_into(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
    ) -> Result<(), Error> {
        let mut slot = AsyncRequestSlot::default();
        let request = self.submit_dma_read_blocks(start_block, buffer, size, dma, &mut slot)?;
        self.wait_async_dma_request(request, &mut slot)?;
        Ok(())
    }

    /// Write whole 512-byte blocks using the controller's 32-bit ADMA2 engine.
    ///
    /// `start_block` is the card address to place in CMD24/CMD25. Callers
    /// that know whether the card uses byte or sector addressing must apply
    /// that translation before calling this method.
    pub fn dma_write_blocks_from(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
    ) -> Result<(), Error> {
        let mut slot = AsyncRequestSlot::default();
        let request = self.submit_dma_write_blocks(start_block, buffer, size, dma, &mut slot)?;
        self.wait_async_dma_request(request, &mut slot)
    }

    /// Submit one ADMA2 read request for an external block queue.
    ///
    /// The request remains active after this method returns. Callers complete
    /// it by calling [`Sdhci::poll_async_dma_request`], typically after OS glue
    /// observes the controller IRQ and wakes the queue/future.
    pub fn submit_dma_read_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut AsyncRequestSlot,
    ) -> Result<AsyncDmaRequest, Error> {
        let id = slot.start()?;
        match self.build_dma_read_request(start_block, buffer, size, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    /// Submit one ADMA2 write request for an external block queue.
    ///
    /// This is the async counterpart to the synchronous
    /// [`SdioHost::write_data`] protocol method.
    pub fn submit_dma_write_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut AsyncRequestSlot,
    ) -> Result<AsyncDmaRequest, Error> {
        let id = slot.start()?;
        match self.build_dma_write_request(start_block, buffer, size, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    /// Poll a previously submitted ADMA2 block request.
    ///
    /// `Ok(())` means complete. `Error::Timeout` means the request is still in
    /// flight and platform glue maps it to `BlkError::Retry`.
    pub fn poll_async_dma_request(
        &mut self,
        request: &mut Option<AsyncDmaRequest>,
        id: RequestId,
        slot: &mut AsyncRequestSlot,
    ) -> Result<(), Error> {
        let Some(active) = request.as_ref() else {
            return Err(Error::InvalidArgument);
        };
        if active.id() != id {
            return Err(Error::InvalidArgument);
        }

        let (cmd_index, phase) = match &active.inner {
            AsyncDmaRequestKind::Read {
                cmd_index, phase, ..
            }
            | AsyncDmaRequestKind::Write {
                cmd_index, phase, ..
            } => (*cmd_index, *phase),
        };

        match self.poll_data_complete_with_adma(cmd_index, phase) {
            Ok(PollResult::Pending) => Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index))),
            Ok(PollResult::Complete) => {
                let active = request.take().ok_or(Error::InvalidArgument)?;
                self.finish_async_dma_request(active)?;
                slot.complete(id)
            }
            Err(err) => {
                self.abort_async_dma_request(request, id, slot);
                Err(err)
            }
        }
    }

    fn dma_data_transfer_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut dma_api::DArray<Adma2Desc32>,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<Response, Error> {
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

        self.dma_data_transfer_prepared(cmd, block_count, desc_bus as u32, direction, phase)
    }

    fn dma_data_transfer_prepared(
        &mut self,
        cmd: &Command,
        block_count: u32,
        desc_bus: u32,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<Response, Error> {
        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.use_dma = true;
        self.select_adma2_32();
        self.write_adma_addr(desc_bus);

        let result = self.issue_command(cmd).and_then(|response| {
            self.wait_data_complete_with_adma(self.active_data_cmd, phase)?;
            Ok(response)
        });
        self.use_dma = false;
        if result.is_err() {
            self.recover_after_adma2_error();
        }
        result
    }

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<AsyncDmaRequest, Error> {
        let block_count = dma_read_block_count(size)?;
        let map = dma
            .map_single_array(
                unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size.get()) },
                BLOCK_SIZE,
                DmaDirection::FromDevice,
            )
            .map_err(map_dma_error)?;
        let mut desc = dma
            .array_zero_with_align::<Adma2Desc32>(
                ADMA2_DESC_COUNT,
                ADMA2_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_dma_blocks_mapped(
            &cmd,
            block_count,
            map.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Read,
            Phase::DataRead,
        )?;
        Ok(AsyncDmaRequest {
            inner: AsyncDmaRequestKind::Read {
                id,
                map,
                _desc: desc,
                cmd_index: cmd.cmd,
                phase: Phase::DataRead,
                stop_after_complete: block_count > 1,
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
    ) -> Result<AsyncDmaRequest, Error> {
        let block_count = dma_write_block_count(size)?;
        let map = dma
            .map_single_array(
                unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size.get()) },
                BLOCK_SIZE,
                DmaDirection::ToDevice,
            )
            .map_err(map_dma_error)?;
        map.confirm_write_all();
        let mut desc = dma
            .array_zero_with_align::<Adma2Desc32>(
                ADMA2_DESC_COUNT,
                ADMA2_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_dma_blocks_mapped(
            &cmd,
            block_count,
            map.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        )?;
        Ok(AsyncDmaRequest {
            inner: AsyncDmaRequestKind::Write {
                id,
                _map: map,
                _desc: desc,
                cmd_index: cmd.cmd,
                phase: Phase::DataWrite,
                stop_after_complete: block_count > 1,
            },
        })
    }

    fn submit_dma_blocks_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut DArray<Adma2Desc32>,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<Response, Error> {
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
        let response = self.issue_command(cmd);
        self.use_dma = false;
        response
    }

    fn wait_async_dma_request(
        &mut self,
        request: AsyncDmaRequest,
        slot: &mut AsyncRequestSlot,
    ) -> Result<(), Error> {
        let id = request.id();
        let mut request = Some(request);
        loop {
            match self.poll_async_dma_request(&mut request, id, slot) {
                Err(Error::Timeout(_)) => core::hint::spin_loop(),
                result => return result,
            }
        }
    }

    fn finish_async_dma_request(&mut self, request: AsyncDmaRequest) -> Result<(), Error> {
        match request.inner {
            AsyncDmaRequestKind::Read {
                map,
                stop_after_complete,
                ..
            } => {
                map.prepare_read_all();
                if stop_after_complete {
                    let _ = self.issue_command(&sdmmc_protocol::cmd::CMD12);
                }
            }
            AsyncDmaRequestKind::Write {
                stop_after_complete,
                ..
            } => {
                if stop_after_complete {
                    let _ = self.issue_command(&sdmmc_protocol::cmd::CMD12);
                }
            }
        }
        self.pending_data = None;
        self.active_data_cmd = 0;
        Ok(())
    }

    fn abort_async_dma_request(
        &mut self,
        request: &mut Option<AsyncDmaRequest>,
        id: RequestId,
        slot: &mut AsyncRequestSlot,
    ) {
        let _ = request.take();
        self.recover_after_adma2_error();
        let _ = slot.complete(id);
    }

    fn recover_after_adma2_error(&mut self) {
        self.use_dma = false;
        self.pending_data = None;
        self.active_data_cmd = 0;
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
        let _ = self.reset_cmd();
        let _ = self.reset_dat();
    }

    fn poll_data_complete_with_adma(
        &mut self,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<PollResult, Error> {
        let (status, err) = self.take_data_irq_status();
        if status & NORMAL_INT_XFER_COMPLETE != 0 {
            return Ok(PollResult::Complete);
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
        Ok(PollResult::Pending)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollResult {
    Pending,
    Complete,
}

fn build_descriptors_into_dma(
    desc: &mut dma_api::DArray<Adma2Desc32>,
    base: u64,
    total_len: usize,
    phase: Phase,
) -> Result<usize, Error> {
    if desc.len() < ADMA2_DESC_COUNT {
        return Err(Error::InvalidArgument);
    }
    let mut table = [Adma2Desc32::default(); ADMA2_DESC_COUNT];
    let written = build_descriptors(&mut table, base, total_len, phase)?;
    desc.write_with(ADMA2_DESC_COUNT, |descs| {
        descs.copy_from_slice(&table);
    });
    Ok(written)
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
        | dma_api::DmaError::NullPointer
        | dma_api::DmaError::ZeroSizedBuffer => Error::InvalidArgument,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn async_request_slot_rejects_second_request_until_completed() {
        let mut slot = AsyncRequestSlot::default();
        let first = slot.start().unwrap();

        assert_eq!(slot.start(), Err(Error::UnsupportedCommand));
        assert_eq!(
            slot.complete(RequestId::new(usize::from(first) + 1)),
            Err(Error::InvalidArgument)
        );
        assert_eq!(slot.complete(first), Ok(()));
        assert!(slot.start().is_ok());
    }

    #[test]
    fn async_dma_request_can_cross_queue_thread_boundary() {
        fn assert_send<T: Send>() {}

        assert_send::<AsyncDmaRequest>();
        assert_send::<AsyncRequestSlot>();
    }

    #[test]
    fn adma2_presubmit_errors_allow_pio_fallback() {
        let err = Adma2AttemptError::fallback(Error::UnsupportedCommand);

        assert!(err.can_fallback_to_pio());
        assert_eq!(err.into_error(), Error::UnsupportedCommand);
    }

    #[test]
    fn adma2_postsubmit_errors_do_not_allow_pio_fallback() {
        let err =
            Adma2AttemptError::fatal(Error::Timeout(ErrorContext::for_cmd(Phase::DataRead, 18)));

        assert!(!err.can_fallback_to_pio());
        assert!(matches!(err.into_error(), Error::Timeout(_)));
    }
}
