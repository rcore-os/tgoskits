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

const BMOD_SWR: u32 = 1 << 0;
const BMOD_FB: u32 = 1 << 1;
const BMOD_DE: u32 = 1 << 7;
const IDMAC_INT_TI: u32 = 1 << 0;
const IDMAC_INT_RI: u32 = 1 << 1;
const IDMAC_INT_FBE: u32 = 1 << 2;
const IDMAC_INT_DU: u32 = 1 << 4;
const IDMAC_INT_CES: u32 = 1 << 5;
const IDMAC_INT_NI: u32 = 1 << 8;
const IDMAC_INT_AI: u32 = 1 << 9;
const IDMAC_INT_CLR: u32 = IDMAC_INT_AI
    | IDMAC_INT_NI
    | IDMAC_INT_CES
    | IDMAC_INT_DU
    | IDMAC_INT_FBE
    | IDMAC_INT_RI
    | IDMAC_INT_TI;
const IDMAC_INT_ENABLE: u32 = IDMAC_INT_NI | IDMAC_INT_RI | IDMAC_INT_TI;

const DMA_POLL_LIMIT: u32 = 8_000_000;
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
        _desc: CoherentArray<IdmacDesc>,
        cmd_index: u8,
        phase: Phase,
        stage: BlockRequestStage,
        stop_after_complete: bool,
        response: Option<Response>,
    },
    Write {
        id: RequestId,
        buffer: DmaRequestBuffer,
        _desc: CoherentArray<IdmacDesc>,
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

impl DwMmc {
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
    /// See [`DwMmc::submit_read_blocks`] for the completion contract.
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
        loop {
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

            match stage {
                BlockRequestStage::Command => match self.poll_command() {
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
                        let _ = self.abort_block_request(request, id, slot, phase);
                        return Err(err);
                    }
                },
                BlockRequestStage::Data => match self.poll_dma_complete(cmd_index, phase) {
                    Ok(BlockPoll::Pending) => return Ok(DataCommandPoll::Pending),
                    Ok(BlockPoll::Complete) => match self.finish_dma_data(request, id, slot)? {
                        DataCommandPoll::Pending => {}
                        complete => return Ok(complete),
                    },
                    // Future BlockPoll variants: best-effort, treat as still pending.
                    Ok(_) => return Ok(DataCommandPoll::Pending),
                    Err(err) => {
                        let _ = self.abort_block_request(request, id, slot, phase);
                        return Err(err);
                    }
                },
                BlockRequestStage::Stop => return self.poll_block_stop(request, id, slot, phase),
            }
        }
    }

    pub fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        self.abort_block_request(request, id, slot, Phase::DataRead)
    }

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = dma_read_block_count(size)?;
        let backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::FromDevice)
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr, &mut desc)?;
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
        let block_count = dma_write_block_count(size)?;
        let mut backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::ToDevice)
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        backing.copy_to_device_from_slice(unsafe {
            core::slice::from_raw_parts(buffer.as_ptr(), size.get())
        });
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr, &mut desc)?;
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
        if buffer.direction() != DmaDirection::FromDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_read_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => {
                return Err(PreparedDmaSubmitError::new(
                    map_dma_error(err, Phase::DataRead),
                    buffer,
                ));
            }
        };
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        match self.submit_idmac_transfer_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
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
        if buffer.direction() != DmaDirection::ToDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_write_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => {
                return Err(PreparedDmaSubmitError::new(
                    map_dma_error(err, Phase::DataWrite),
                    buffer,
                ));
            }
        };
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        match self.submit_idmac_transfer_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
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
        self.data_blocks_remaining = block_count;
        self.program_fifo_interrupt_mask();
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

    fn submit_idmac_transfer_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut CoherentArray<IdmacDesc>,
    ) -> Result<(), Error> {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let (direction, phase) = match cmd.data_direction() {
            Some(sdio_host2::DataDirection::Read) => (DataDirection::Read, Phase::DataRead),
            Some(sdio_host2::DataDirection::Write) => (DataDirection::Write, Phase::DataWrite),
            None => return Err(Error::InvalidArgument),
            // Future DataDirection variants are not supported by this engine.
            Some(_) => return Err(Error::InvalidArgument),
        };
        let byte_count = block_count
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(Error::InvalidArgument)?;
        let transfer_end = buffer_dma
            .checked_add(byte_count as u64)
            .ok_or(Error::InvalidArgument)?;
        let desc_bytes = (block_count as usize)
            .checked_mul(IDMAC_DESC_SIZE)
            .ok_or(Error::InvalidArgument)?;
        let desc_dma = desc.dma_addr().as_u64();
        let desc_end = desc_dma
            .checked_add(desc_bytes as u64)
            .ok_or(Error::InvalidArgument)?;
        if transfer_end > u32::MAX as u64 + 1
            || desc_end > u32::MAX as u64 + 1
            || desc.len() < block_count as usize
        {
            return Err(Error::InvalidArgument);
        }

        desc.write_with_cpu(block_count as usize, |descs| {
            for (index, desc) in descs.iter_mut().enumerate() {
                let last = index + 1 == block_count as usize;
                let next = if last {
                    0
                } else {
                    (desc_dma as u32) + ((index + 1) * IDMAC_DESC_SIZE) as u32
                };
                *desc = IdmacDesc::chained(
                    (buffer_dma as u32) + (index * BLOCK_SIZE) as u32,
                    BLOCK_SIZE as u32,
                    next,
                    index == 0,
                    last,
                );
            }
        });

        self.clear_all_int_status();
        self.regs.idsts().write(IDMAC_INT_CLR);
        self.irq.state.clear(u32::MAX);
        self.program_data_phase(BLOCK_SIZE as u32, block_count);
        self.reset_dma_for_phase(phase)?;

        self.regs.dbaddr().write(desc_dma as u32);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.completion_irq_enabled())
        });
        self.regs.idinten().write(IDMAC_INT_ENABLE);
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        match self.submit_command(cmd) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.disable_idmac();
                let _ = self.recover_after_idmac_error(phase);
                self.clear_all_int_status();
                Err(err)
            }
        }
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
            self.data_blocks_remaining = 0;
            self.data_cmd_index = 0;
            self.irq.state.end_request();
            return Ok(None);
        }
        let completed_dma = match request.inner {
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => {
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                None
            }
            BlockRequestKind::Read { stage, buffer, .. } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                self.disable_idmac();
                self.clear_all_int_status();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
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
                self.disable_idmac();
                self.clear_all_int_status();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                if quiesced {
                    buffer.complete(false)
                } else {
                    buffer.abort(false, false)
                }
            }
        };
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
                stage,
                stop_after_complete,
                ..
            } => {
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            BlockRequestKind::Write {
                stage,
                stop_after_complete,
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
            self.submit_command(&CMD12)?;
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
        phase: Phase,
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
                let _ = self.abort_block_request(request, id, slot, phase);
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
        loop {
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

            match stage {
                BlockRequestStage::Command => match self.poll_command() {
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
                        let _ = self.abort_block_request(request, id, slot, phase);
                        return Err(err);
                    }
                },
                BlockRequestStage::Data => {
                    match self.poll_fifo_data_step(request, cmd_index, phase) {
                        Ok(BlockPoll::Pending) => return Ok(DataCommandPoll::Pending),
                        Ok(BlockPoll::Complete) => {
                            match self.finish_fifo_data(request, id, slot)? {
                                DataCommandPoll::Pending => {}
                                complete => return Ok(complete),
                            }
                        }
                        // Future BlockPoll variants: best-effort, treat as still pending.
                        Ok(_) => return Ok(DataCommandPoll::Pending),
                        Err(err) => {
                            let _ = self.abort_block_request(request, id, slot, phase);
                            return Err(err);
                        }
                    }
                }
                BlockRequestStage::Stop => return self.poll_block_stop(request, id, slot, phase),
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
                stage,
                stop_after_complete,
                ..
            }
            | BlockRequestKind::FifoWrite {
                stage,
                stop_after_complete,
                ..
            } => {
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            _ => return Err(Error::InvalidArgument),
        };
        if stop_after_complete {
            self.submit_command(&CMD12)?;
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active)?;
        drop(completed_dma);
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        slot.complete(id)?;
        Ok(DataCommandPoll::Complete(response))
    }

    fn abort_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        phase: Phase,
    ) -> Result<(), Error> {
        let active = request.take().ok_or(Error::InvalidArgument)?;
        self.disable_idmac();
        let recovery = self.recover_after_idmac_error(phase);
        self.clear_all_int_status();
        self.irq
            .state
            .clear(crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_ERROR_MASK);
        let completed_dma = self.finish_block_request_with_quiesce(active, recovery.is_ok())?;
        drop(completed_dma);
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.command_state = crate::command::CommandState::Idle;
        slot.complete(id)?;
        recovery
    }

    fn disable_idmac(&self) {
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(false)
                .with_dma_enable(false)
                .with_int_enable(false)
        });
        self.regs.idinten().write(0);
        self.regs.bmod().write(0);
    }

    fn recover_after_idmac_error(&mut self, phase: Phase) -> Result<(), Error> {
        let status = self.regs.status().read().into_bits();
        let rintsts = self.regs.rintsts().read();
        warn!(
            "dwmmc: IDMAC {:?} error state rintsts={:#010x} status={:#010x} tcbcnt={} tbbcnt={}",
            phase,
            rintsts.into_bits(),
            status,
            self.regs.tcbcnt().read(),
            self.regs.tbbcnt().read()
        );

        self.regs.ctrl().update(|r| r.with_abort_read_data(true));
        let _ = self.regs.ctrl().read();
        let fifo = self.reset_fifo();
        let dma = self.reset_dma_for_phase(phase);
        self.regs.ctrl().update(|r| r.with_abort_read_data(false));
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.command_state = crate::command::CommandState::Idle;
        match (fifo, dma) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), _) | (_, Err(err)) => {
                self.reset_and_init_preserving_irq()?;
                warn!(
                    "dwmmc: recovered IDMAC {:?} error by controller reset: {err:?}",
                    phase
                );
                Ok(())
            }
        }
    }

    fn reset_dma_for_phase(&self, phase: Phase) -> Result<(), Error> {
        self.regs.ctrl().update(|r| r.with_dma_reset(true));
        for _ in 0..DMA_POLL_LIMIT {
            if !self.regs.ctrl().read().dma_reset() {
                self.regs.bmod().write(BMOD_SWR);
                for _ in 0..DMA_POLL_LIMIT {
                    if self.regs.bmod().read() & BMOD_SWR == 0 {
                        return Ok(());
                    }
                    core::hint::spin_loop();
                }
                break;
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
    }

    fn poll_dma_complete(&mut self, cmd_index: u8, phase: Phase) -> Result<BlockPoll, Error> {
        let raw_status = self.take_data_irq_status();
        let rintsts = crate::regs::RIntSts::from_bits(raw_status);
        if rintsts.error() {
            return Err(self.translate_int_error(rintsts, phase, cmd_index));
        }
        if rintsts.data_transfer_over() {
            return Ok(BlockPoll::Complete);
        }
        Ok(BlockPoll::Pending)
    }

    fn take_data_irq_status(&mut self) -> u32 {
        let consume = crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_INT_RXDR
            | crate::DWMMC_INT_TXDR
            | crate::DWMMC_INT_ERROR_MASK;
        if self.completion_irq_enabled() {
            return self.irq.state.take(consume);
        }
        let raw_status = self.regs.rintsts().read().into_bits();
        let clear = raw_status
            & (crate::DWMMC_INT_DATA_TRANSFER_OVER
                | crate::DWMMC_INT_COMMAND_DONE
                | crate::DWMMC_INT_RXDR
                | crate::DWMMC_INT_TXDR
                | crate::DWMMC_INT_ERROR_MASK);
        if clear != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(clear));
        }
        raw_status
    }
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
    host: &mut DwMmc,
    buffer: NonNull<u8>,
    len: usize,
    _block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let raw_status = host.take_data_irq_status();
    let rintsts = crate::regs::RIntSts::from_bits(raw_status);
    if rintsts.error() {
        let err = host.translate_int_error(rintsts, phase, cmd_index);
        let _ = host.reset_fifo();
        return Err(err);
    }

    let fifo = host.fifo_ptr();
    let mut status = host.regs.status().read();
    while *offset < len && status.fifo_count() >= 2 {
        let value = unsafe { fifo.read_volatile() };
        let end = (*offset + 8).min(len);
        let block =
            unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr().add(*offset), end - *offset) };
        block.copy_from_slice(&value.to_le_bytes()[..block.len()]);
        *offset = end;
        status = host.regs.status().read();
    }

    if *offset >= len && rintsts.data_transfer_over() {
        return Ok(BlockPoll::Complete);
    }
    Ok(BlockPoll::Pending)
}

fn poll_fifo_write_step(
    host: &mut DwMmc,
    buffer: NonNull<u8>,
    len: usize,
    _block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let raw_status = host.take_data_irq_status();
    let rintsts = crate::regs::RIntSts::from_bits(raw_status);
    if rintsts.error() {
        let err = host.translate_int_error(rintsts, phase, cmd_index);
        let _ = host.reset_fifo();
        return Err(err);
    }

    let fifo = host.fifo_ptr();
    while *offset < len && !host.regs.status().read().fifo_full() {
        let end = (*offset + 8).min(len);
        let block =
            unsafe { core::slice::from_raw_parts(buffer.as_ptr().add(*offset), end - *offset) };
        let mut bytes = [0u8; 8];
        bytes[..block.len()].copy_from_slice(block);
        unsafe { fifo.write_volatile(u64::from_le_bytes(bytes)) };
        *offset = end;
    }

    if *offset >= len && rintsts.data_transfer_over() {
        return Ok(BlockPoll::Complete);
    }
    Ok(BlockPoll::Pending)
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

fn map_dma_error(err: dma_api::DmaError, phase: Phase) -> Error {
    match err {
        dma_api::DmaError::NoMemory => Error::BusError(ErrorContext::new(phase)),
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
    use super::*;

    #[test]
    fn first_descriptor_sets_owned_chained_first_read_buffer() {
        let desc = IdmacDesc::chained(0x1234_5000, 512, 0x2000, true, false);

        assert_eq!(desc.des0, DESC_OWN | DESC_CH | DESC_FS | DESC_DIC);
        assert_eq!(desc.des1, 512);
        assert_eq!(desc.des2, 0x1234_5000);
        assert_eq!(desc.des3, 0x2000);
    }

    #[test]
    fn last_descriptor_sets_last_and_terminates_chain() {
        let desc = IdmacDesc::chained(0x1234_5200, 512, 0, false, true);

        assert_eq!(desc.des0, DESC_OWN | DESC_LD);
        assert_eq!(desc.des1, 512);
        assert_eq!(desc.des2, 0x1234_5200);
        assert_eq!(desc.des3, 0);
    }

    #[test]
    fn single_descriptor_requests_completion_interrupt() {
        let desc = IdmacDesc::chained(0x1234_5000, 512, 0, true, true);

        assert_eq!(desc.des0, DESC_OWN | DESC_FS | DESC_LD);
        assert_eq!(desc.des1, 512);
        assert_eq!(desc.des2, 0x1234_5000);
        assert_eq!(desc.des3, 0);
    }

    #[test]
    fn dma_read_plan_rejects_non_block_sized_buffers() {
        let size = NonZeroUsize::new(513).unwrap();

        assert_eq!(dma_read_block_count(size), Err(Error::Misaligned));
    }

    #[test]
    fn dma_read_plan_reports_block_count() {
        let size = NonZeroUsize::new(1024).unwrap();

        assert_eq!(dma_read_block_count(size), Ok(2));
    }

    #[test]
    fn dma_write_plan_rejects_non_block_sized_buffers() {
        let size = NonZeroUsize::new(513).unwrap();

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
    fn polling_data_irq_status_clears_fifo_request_bits() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const RINTSTS_WORD: usize = 17;
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .with_receive_fifo_data_request(true)
            .with_transmit_fifo_data_request(true)
            .into_bits();
        unsafe {
            mmio.as_mut_ptr().add(RINTSTS_WORD).write_volatile(raw);
        }

        assert_eq!(host.take_data_irq_status(), raw);

        let cleared = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
        assert_eq!(cleared, raw);
    }

    #[test]
    fn irq_poll_consumes_cached_command_and_data_completion_in_one_pass() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Fifo, BlockTransferDirection::Read)
            .unwrap();
        let mut buffer = [0u8; BLOCK_SIZE];
        let cmd = cmd17(0);
        host.enable_completion_irq();
        host.data_cmd_index = cmd.index;
        host.command_state = crate::command::CommandState::Issued { cmd, polls: 0 };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_DATA_TRANSFER_OVER,
        );
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoRead {
                id,
                buffer: NonNull::new(buffer.as_mut_ptr()).unwrap(),
                len: buffer.len(),
                block_size: BLOCK_SIZE,
                offset: buffer.len(),
                cmd_index: cmd.index,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Command,
                stop_after_complete: false,
                response: None,
            },
        });

        assert!(matches!(
            host.poll_block_request(&mut request, id, &mut slot),
            Ok(BlockPoll::Complete)
        ));
        assert!(request.is_none());
        assert_eq!(host.irq.state.pending(), 0);
    }
}
