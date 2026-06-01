use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{CoherentArray, DeviceDma, DmaDirection, StreamingMap};
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
const IDSTS_ABNORMAL_SUMMARY: u32 = 1 << 9;
const IDSTS_ERROR_MASK: u32 = crate::MCI_IDSTS_ERROR_MASK | IDSTS_ABNORMAL_SUMMARY;
const IDSTS_INT_ENABLE_MASK: u32 =
    crate::MCI_IDSTS_ERROR_MASK | IDSTS_NORMAL_SUMMARY | IDSTS_ABNORMAL_SUMMARY;

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
    buffer: StreamingMap<u8>,
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
}

pub type RequestId = BlockRequestId;

#[derive(Default)]
pub struct BlockRequestSlot {
    next: usize,
    state: BlockTransferState,
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
        if self.state.id() != Some(id) {
            return Err(Error::InvalidArgument);
        }
        self.state = BlockTransferState::Idle;
        Ok(())
    }

    pub fn state(&self) -> BlockTransferState {
        self.state
    }
}

pub struct BlockRequest {
    inner: BlockRequestKind,
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
}

impl PhytiumMci {
    pub fn submit_read_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        mode: BlockTransferMode,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        let id = slot.start(mode, BlockTransferDirection::Read)?;
        let result = match mode {
            BlockTransferMode::Dma => self.build_dma_read_request(
                start_block,
                buffer,
                size,
                dma.ok_or(Error::InvalidArgument)?,
                id,
            ),
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

    pub fn submit_write_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        mode: BlockTransferMode,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        let id = slot.start(mode, BlockTransferDirection::Write)?;
        let result = match mode {
            BlockTransferMode::Dma => self.build_dma_write_request(
                start_block,
                buffer,
                size,
                dma.ok_or(Error::InvalidArgument)?,
                id,
            ),
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
        self.poll_data_request_inner(request, id, slot)
    }

    fn build_fifo_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = block_count(size)?;
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
        let block_count = block_count(size)?;
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

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = block_count(size)?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.build_dma_data_request(
            &cmd,
            buffer,
            size.get(),
            BLOCK_SIZE as u32,
            block_count,
            dma,
            id,
            DataDirection::Read,
            block_count > 1,
        )
    }

    fn build_dma_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let block_count = block_count(size)?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.build_dma_data_request(
            &cmd,
            buffer,
            size.get(),
            BLOCK_SIZE as u32,
            block_count,
            dma,
            id,
            DataDirection::Write,
            block_count > 1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_dma_data_request(
        &mut self,
        cmd: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        dma: &DeviceDma,
        id: RequestId,
        direction: DataDirection,
        stop_after_complete: bool,
    ) -> Result<BlockRequest, Error> {
        let block_size_usize = usize::try_from(block_size).map_err(|_| Error::InvalidArgument)?;
        if block_size_usize == 0 || len != block_size_usize.saturating_mul(block_count as usize) {
            return Err(Error::InvalidArgument);
        }
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => return Err(Error::InvalidArgument),
            _ => return Err(Error::InvalidArgument),
        };
        let dma_direction = match direction {
            DataDirection::Read => DmaDirection::FromDevice,
            DataDirection::Write => DmaDirection::ToDevice,
            DataDirection::None => return Err(Error::InvalidArgument),
            _ => return Err(Error::InvalidArgument),
        };
        let slice = unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr(), len) };
        let mapped = dma
            .map_streaming_slice_for_device(slice, BLOCK_SIZE, dma_direction)
            .map_err(|_| Error::Misaligned)?;
        let desc_count = len.div_ceil(IDMAC_MAX_BUF_SIZE);
        let mut descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(desc_count, IDMAC_DESC_ALIGN)
            .map_err(|_| Error::Misaligned)?;
        let desc_dma = descriptors.dma_addr().as_u64();
        let desc_values = build_idmac_descriptors(
            mapped.dma_addr().as_u64(),
            desc_dma,
            len,
            IDMAC_MAX_BUF_SIZE,
        )?;
        descriptors.write_with_cpu(desc_values.len(), |dst| dst.copy_from_slice(&desc_values));
        self.start_idmac_transfer(cmd, block_size, block_count, desc_dma)?;

        let progress = DmaProgress {
            descriptors,
            buffer: mapped,
            desc_count,
            complete: false,
            idmac_done: false,
            data_done: false,
        };
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::DmaRead {
                id,
                progress,
                cmd_index: cmd.cmd,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::Write => BlockRequestKind::DmaWrite {
                id,
                progress,
                cmd_index: cmd.cmd,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::None => return Err(Error::InvalidArgument),
            _ => return Err(Error::InvalidArgument),
        };
        Ok(BlockRequest { inner })
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
            use_idmac: false,
        });
        self.data_blocks_remaining = block_count;
        self.submit_command(cmd)?;
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::FifoRead {
                id,
                buffer,
                len,
                block_size: block_size_usize,
                progress: FifoProgress::default(),
                cmd_index: cmd.cmd,
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
                progress: FifoProgress::default(),
                cmd_index: cmd.cmd,
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

    fn poll_data_request_inner(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        match request.as_ref().map(|request| &request.inner) {
            Some(BlockRequestKind::FifoRead { .. }) | Some(BlockRequestKind::FifoWrite { .. }) => {
                self.poll_fifo_request(request, id, slot)
            }
            Some(BlockRequestKind::DmaRead { .. }) | Some(BlockRequestKind::DmaWrite { .. }) => {
                self.poll_dma_request(request, id, slot)
            }
            None => Err(Error::InvalidArgument),
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
                    store_response(request, response)?;
                    set_stage(request, BlockRequestStage::Data)?;
                    return Ok(DataCommandPoll::Pending);
                }
                // Future CommandPoll variants: best-effort, treat as still pending.
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => {
                    self.abort_block_request(request, id, slot, phase);
                    return Err(err);
                }
            }
        }

        let stage = current_stage(request)?;
        if stage == BlockRequestStage::Stop {
            return self.poll_block_stop(request, id, slot, phase);
        }

        match self.poll_fifo_data_step(request, cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_fifo_data(request, id, slot),
            // Future BlockPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                self.abort_block_request(request, id, slot, phase);
                Err(err)
            }
        }
    }

    fn poll_dma_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let (cmd_index, phase, stage) = match request.as_ref().map(|request| &request.inner) {
            Some(BlockRequestKind::DmaRead {
                cmd_index,
                phase,
                stage,
                ..
            })
            | Some(BlockRequestKind::DmaWrite {
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
                    store_response(request, response)?;
                    set_stage(request, BlockRequestStage::Data)?;
                    return Ok(DataCommandPoll::Pending);
                }
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => {
                    self.abort_block_request(request, id, slot, phase);
                    return Err(err);
                }
            }
        }

        let stage = current_stage(request)?;
        if stage == BlockRequestStage::Stop {
            return self.poll_block_stop(request, id, slot, phase);
        }

        match self.poll_dma_data_step(request, cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_dma_data(request, id, slot),
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                self.abort_block_request(request, id, slot, phase);
                Err(err)
            }
        }
    }

    fn poll_dma_data_step(
        &mut self,
        request: &mut Option<BlockRequest>,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockPoll, Error> {
        let raw_idsts = self.take_idmac_status();
        let ints = self.take_data_irq_status(cmd_index, phase)?;
        let Some(active) = request.as_mut() else {
            return Err(Error::InvalidArgument);
        };
        let (progress, is_read) = match &mut active.inner {
            BlockRequestKind::DmaRead { progress, .. } => (progress, true),
            BlockRequestKind::DmaWrite { progress, .. } => (progress, false),
            _ => return Err(Error::InvalidArgument),
        };

        if raw_idsts & IDSTS_ERROR_MASK != 0 {
            warn!(
                "phytium-mci IDMAC error cmd={} idsts={:#010x} rintsts={:#010x} status={:#010x} \
                 cur_desc={:#010x}_{:08x} cur_buf={:#010x}_{:08x}",
                cmd_index,
                raw_idsts,
                self.regs.rintsts().read().into_bits(),
                self.regs.status().read().into_bits(),
                self.regs.dscaddrh().read(),
                self.regs.dscaddrl().read(),
                self.regs.bufaddrh().read(),
                self.regs.bufaddrl().read(),
            );
            return Err(Error::BusError(sdmmc_protocol::ErrorContext::for_cmd(
                phase, cmd_index,
            )));
        }
        progress.idmac_done |= raw_idsts & (IDSTS_RECEIVE | IDSTS_TRANSMIT) != 0;
        progress.data_done |= ints.data_transfer_over();
        if !progress.data_done {
            return Ok(BlockPoll::Pending);
        }

        if is_read {
            progress.buffer.complete_for_cpu_all();
        }
        progress.complete = true;
        Ok(BlockPoll::Complete)
    }

    fn finish_dma_data(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        self.disable_idmac();
        let stop_after_complete = match request.as_mut().map(|r| &mut r.inner) {
            Some(BlockRequestKind::DmaRead {
                stage,
                stop_after_complete,
                ..
            })
            | Some(BlockRequestKind::DmaWrite {
                stage,
                stop_after_complete,
                ..
            }) => {
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
        self.finish_block_request(active);
        slot.complete(id)?;
        Ok(DataCommandPoll::Complete(response))
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
                progress,
                ..
            } => poll_fifo_read_step(self, *buffer, *len, *block_size, progress, cmd_index, phase),
            BlockRequestKind::FifoWrite {
                buffer,
                len,
                block_size,
                progress,
                ..
            } => poll_fifo_write_step(self, *buffer, *len, *block_size, progress, cmd_index, phase),
            _ => Err(Error::InvalidArgument),
        }
    }

    fn finish_fifo_data(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        let stop_after_complete = match request.as_mut().map(|r| &mut r.inner) {
            Some(BlockRequestKind::FifoRead {
                stage,
                stop_after_complete,
                ..
            })
            | Some(BlockRequestKind::FifoWrite {
                stage,
                stop_after_complete,
                ..
            }) => {
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
        self.finish_block_request(active);
        slot.complete(id)?;
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
                self.finish_block_request(active);
                slot.complete(id)?;
                Ok(DataCommandPoll::Complete(response))
            }
            // Future CommandPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => {
                self.abort_block_request(request, id, slot, phase);
                Err(err)
            }
        }
    }

    fn finish_block_request(&mut self, request: BlockRequest) {
        match &request.inner {
            BlockRequestKind::DmaRead { progress, .. }
            | BlockRequestKind::DmaWrite { progress, .. } => progress.keep_alive(),
            _ => {}
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
    }

    fn abort_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        phase: Phase,
    ) {
        if let Some(active) = request.take() {
            self.finish_block_request(active);
        }
        self.disable_idmac();
        let _ = self.reset_fifo(phase);
        let _ = slot.complete(id);
    }

    fn start_idmac_transfer(
        &mut self,
        cmd: &Command,
        block_size: u32,
        block_count: u32,
        desc_dma: u64,
    ) -> Result<(), Error> {
        self.clear_all_int_status();
        self.regs.idsts().write(u32::MAX);
        self.regs.idinten().write(IDSTS_INT_ENABLE_MASK);
        self.reset_fifo(Phase::Init)?;
        self.reset_dma(Phase::Init)?;
        self.program_data_phase(block_size, block_count);
        self.program_idmac_registers(desc_dma);
        self.pending_data = Some(PendingData {
            direction: if matches!(cmd.cmd, 24 | 25) {
                DataDirection::Write
            } else {
                DataDirection::Read
            },
            block_size,
            block_count,
            use_idmac: true,
        });
        self.data_blocks_remaining = block_count;
        self.submit_command(cmd)
    }

    fn program_idmac_registers(&self, desc_dma: u64) {
        self.regs
            .ctrl()
            .update(|r| r.with_use_internal_dmac(true).with_int_enable(true));
        self.regs
            .bmod()
            .write(self.regs.bmod().read() | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE);
        self.regs.dbaddrl().write(desc_dma as u32);
        self.regs.dbaddrh().write((desc_dma >> 32) as u32);
    }

    fn disable_idmac(&mut self) {
        self.regs.idinten().write(0);
        self.regs.bmod().write(0);
        self.regs
            .ctrl()
            .update(|r| r.with_dma_enable(false).with_use_internal_dmac(false));
    }

    fn take_idmac_status(&mut self) -> u32 {
        let raw = self.regs.idsts().read();
        if raw != 0 {
            self.regs.idsts().write(raw);
        }
        let status = self.idmac_pending_status | raw;
        self.idmac_pending_status &= !(IDSTS_RECEIVE | IDSTS_TRANSMIT | IDSTS_ERROR_MASK);
        status
    }

    fn take_data_irq_status(&mut self, cmd_index: u8, phase: Phase) -> Result<RIntSts, Error> {
        let raw_status = self.regs.rintsts().read().into_bits();
        let consume = raw_status
            & (crate::MCI_INT_DATA_TRANSFER_OVER
                | crate::MCI_INT_RXDR
                | crate::MCI_INT_TXDR
                | crate::MCI_INT_ERROR_MASK);
        if consume != 0 {
            self.regs.rintsts().write(RIntSts::from_bits(consume));
        }
        let status = self.irq_pending_status | raw_status;
        self.irq_pending_status &= !(crate::MCI_INT_DATA_TRANSFER_OVER
            | crate::MCI_INT_RXDR
            | crate::MCI_INT_TXDR
            | crate::MCI_INT_ERROR_MASK);
        let ints = RIntSts::from_bits(status);
        if ints.error() {
            return Err(self.translate_int_error(ints, phase, cmd_index));
        }
        Ok(ints)
    }
}

fn poll_fifo_read_step(
    host: &mut PhytiumMci,
    buffer: NonNull<u8>,
    len: usize,
    block_size: usize,
    progress: &mut FifoProgress,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let ints = host.take_data_irq_status(cmd_index, phase)?;
    progress.transfer_done |= ints.data_transfer_over();
    let mut available_words = host.regs.status().read().fifo_count() as usize;
    let fifo = host.fifo_ptr();
    while progress.offset < len && available_words > 0 {
        let word = unsafe { fifo.read_volatile() };
        let bytes = word.to_le_bytes();
        let copy = (len - progress.offset).min(bytes.len());
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                buffer.as_ptr().add(progress.offset),
                copy,
            );
        }
        progress.offset += copy;
        available_words -= 1;
    }
    if progress.offset >= len && progress.transfer_done {
        return Ok(BlockPoll::Complete);
    }
    if block_size > 0 && (progress.offset / block_size) as u32 >= host.data_blocks_remaining {
        return Ok(BlockPoll::Pending);
    }
    Ok(BlockPoll::Pending)
}

fn poll_fifo_write_step(
    host: &mut PhytiumMci,
    buffer: NonNull<u8>,
    len: usize,
    _block_size: usize,
    progress: &mut FifoProgress,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let ints = host.take_data_irq_status(cmd_index, phase)?;
    progress.transfer_done |= ints.data_transfer_over();
    let status = host.regs.status().read();
    let depth = host.fifo_word_depth() as usize;
    let used = status.fifo_count() as usize;
    let mut free_words = depth.saturating_sub(used);
    let fifo = host.fifo_ptr();
    while progress.offset < len && free_words > 0 {
        let mut bytes = [0u8; 4];
        let copy = (len - progress.offset).min(bytes.len());
        unsafe {
            core::ptr::copy_nonoverlapping(
                buffer.as_ptr().add(progress.offset),
                bytes.as_mut_ptr(),
                copy,
            );
        }
        unsafe { fifo.write_volatile(u32::from_le_bytes(bytes)) };
        progress.offset += copy;
        free_words -= 1;
    }
    if progress.offset >= len && progress.transfer_done {
        return Ok(BlockPoll::Complete);
    }
    Ok(BlockPoll::Pending)
}

fn store_response(request: &mut Option<BlockRequest>, response: Response) -> Result<(), Error> {
    match request.as_mut().map(|r| &mut r.inner) {
        Some(BlockRequestKind::FifoRead {
            response: stored, ..
        })
        | Some(BlockRequestKind::FifoWrite {
            response: stored, ..
        })
        | Some(BlockRequestKind::DmaRead {
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
        Some(BlockRequestKind::FifoRead { stage, .. })
        | Some(BlockRequestKind::FifoWrite { stage, .. })
        | Some(BlockRequestKind::DmaRead { stage, .. })
        | Some(BlockRequestKind::DmaWrite { stage, .. }) => {
            *stage = next;
            Ok(())
        }
        None => Err(Error::InvalidArgument),
    }
}

fn current_stage(request: &Option<BlockRequest>) -> Result<BlockRequestStage, Error> {
    match request.as_ref().map(|r| &r.inner) {
        Some(BlockRequestKind::FifoRead { stage, .. })
        | Some(BlockRequestKind::FifoWrite { stage, .. })
        | Some(BlockRequestKind::DmaRead { stage, .. })
        | Some(BlockRequestKind::DmaWrite { stage, .. }) => Ok(*stage),
        None => Err(Error::InvalidArgument),
    }
}

fn block_count(size: NonZeroUsize) -> Result<u32, Error> {
    if !size.get().is_multiple_of(BLOCK_SIZE) {
        return Err(Error::InvalidArgument);
    }
    u32::try_from(size.get() / BLOCK_SIZE).map_err(|_| Error::InvalidArgument)
}

fn build_idmac_descriptors(
    buffer_dma: u64,
    desc_dma: u64,
    len: usize,
    max_segment: usize,
) -> Result<alloc::vec::Vec<IdmacDesc>, Error> {
    if len == 0 || max_segment == 0 {
        return Err(Error::InvalidArgument);
    }
    if !buffer_dma.is_multiple_of(BLOCK_SIZE as u64) {
        return Err(Error::Misaligned);
    }
    let desc_count = len.div_ceil(max_segment);
    let mut descriptors = alloc::vec::Vec::with_capacity(desc_count);
    for index in 0..desc_count {
        let offset = index * max_segment;
        let chunk_len = (len - offset).min(max_segment);
        let is_first = index == 0;
        let is_last = index + 1 == desc_count;
        let buffer_addr = buffer_dma + offset as u64;
        let next_desc = if is_last {
            0
        } else {
            desc_dma + ((index + 1) * core::mem::size_of::<IdmacDesc>()) as u64
        };
        if next_desc != 0 && !next_desc.is_multiple_of(core::mem::size_of::<IdmacDesc>() as u64) {
            return Err(Error::Misaligned);
        }
        let mut attribute = IDMAC_DESC_OWN | IDMAC_DESC_CHAIN;
        if is_first {
            attribute |= IDMAC_DESC_FIRST;
        }
        if is_last {
            attribute |= IDMAC_DESC_LAST | IDMAC_DESC_END_RING;
        }
        descriptors.push(IdmacDesc {
            attribute,
            reserved0: 0,
            len: u32::try_from(chunk_len).map_err(|_| Error::InvalidArgument)?,
            reserved1: 0,
            addr_lo: buffer_addr as u32,
            addr_hi: (buffer_addr >> 32) as u32,
            desc_lo: next_desc as u32,
            desc_hi: (next_desc >> 32) as u32,
        });
    }
    Ok(descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idmac_descriptor_builder_marks_single_descriptor_chain() {
        let descriptors = build_idmac_descriptors(0x1_2345_6000, 0x8000_0000, 4096, 4096).unwrap();

        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            descriptors[0].attribute,
            IDMAC_DESC_OWN
                | IDMAC_DESC_CHAIN
                | IDMAC_DESC_FIRST
                | IDMAC_DESC_LAST
                | IDMAC_DESC_END_RING
        );
        assert_eq!(descriptors[0].len, 4096);
        assert_eq!(descriptors[0].addr_lo, 0x2345_6000);
        assert_eq!(descriptors[0].addr_hi, 0x0000_0001);
        assert_eq!(descriptors[0].desc_lo, 0);
        assert_eq!(descriptors[0].desc_hi, 0);
    }

    #[test]
    fn idmac_descriptor_builder_chains_multiple_descriptors() {
        let descriptors =
            build_idmac_descriptors(0x4000_0000, 0x8000_0000, 0x3000, 0x1000).unwrap();

        assert_eq!(descriptors.len(), 3);
        assert_eq!(
            descriptors[0].attribute,
            IDMAC_DESC_OWN | IDMAC_DESC_CHAIN | IDMAC_DESC_FIRST
        );
        assert_eq!(
            descriptors[0].desc_lo,
            0x8000_0000 + core::mem::size_of::<IdmacDesc>() as u32
        );
        assert_eq!(descriptors[1].attribute, IDMAC_DESC_OWN | IDMAC_DESC_CHAIN);
        assert_eq!(
            descriptors[2].attribute,
            IDMAC_DESC_OWN | IDMAC_DESC_CHAIN | IDMAC_DESC_LAST | IDMAC_DESC_END_RING
        );
        assert_eq!(descriptors[2].desc_lo, 0);
    }

    use core::ptr::NonNull;

    use ::alloc::{alloc, boxed::Box};
    use sdmmc_protocol::block::BlockPoll;

    use crate::regs::{RIntSts, Status};

    #[repr(align(512))]
    struct AlignedBlock([u8; BLOCK_SIZE]);

    struct NoopDmaBuffer;

    impl NoopDmaBuffer {
        fn progress() -> DmaProgress {
            let dma = DeviceDma::new(u64::MAX, &TEST_DMA);
            let descriptors = dma
                .coherent_array_zero_with_align::<IdmacDesc>(1, IDMAC_DESC_ALIGN)
                .unwrap();
            let backing = Box::leak(Box::new(AlignedBlock([0u8; BLOCK_SIZE])));
            let buffer = dma
                .map_streaming_slice(&mut backing.0, BLOCK_SIZE, DmaDirection::FromDevice)
                .unwrap();
            DmaProgress {
                descriptors,
                buffer,
                desc_count: 1,
                complete: false,
                idmac_done: false,
                data_done: false,
            }
        }
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        unsafe fn alloc_contiguous(
            &self,
            _constraints: dma_api::DmaConstraints,
            layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            let ptr = unsafe { alloc::alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
            Some(unsafe { dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
            unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: dma_api::DmaConstraints,
            layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            let ptr = unsafe { alloc::alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
            Some(unsafe { dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
            unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn map_streaming(
            &self,
            constraints: dma_api::DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            let layout =
                core::alloc::Layout::from_size_align(size.get(), constraints.align.max(1))?;
            Ok(unsafe {
                dma_api::DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None)
            })
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}

        fn flush(&self, _addr: NonNull<u8>, _size: usize) {}
        fn invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
        fn flush_invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
        fn page_size(&self) -> usize {
            4096
        }
    }

    const RINTSTS_WORD: usize = 17;
    const STATUS_WORD: usize = 18;
    const BMOD_WORD: usize = 32;
    const PLDMND_WORD: usize = 33;
    const DBADDRL_WORD: usize = 34;
    const IDSTS_WORD: usize = 36;
    const FIFO_WORD: usize = crate::host::DEFAULT_FIFO_OFFSET / core::mem::size_of::<u32>();

    fn host_from_words(words: &mut [u32; 256]) -> PhytiumMci {
        let base = NonNull::new(words.as_mut_ptr().cast()).unwrap();
        unsafe { PhytiumMci::new(base) }
    }

    #[test]
    fn idmac_start_preserves_bus_mode_and_enables_fixed_burst() {
        let mut mmio = [0u32; 256];
        mmio[BMOD_WORD] = 0x200;
        let host = host_from_words(&mut mmio);

        host.program_idmac_registers(0x1_8000_0000);

        assert_eq!(
            mmio[BMOD_WORD],
            0x200 | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE
        );
        assert_eq!(mmio[PLDMND_WORD], 0);
        assert_eq!(mmio[DBADDRL_WORD], 0x8000_0000);
        assert_eq!(mmio[DBADDRL_WORD + 1], 1);
    }

    #[test]
    fn idmac_read_completes_when_data_done_arrives_without_idmac_done() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id: RequestId::new(3),
                progress: NoopDmaBuffer::progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });

        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(RIntSts::new().with_data_transfer_over(true).into_bits())
        };

        assert_eq!(
            host.poll_dma_data_step(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Complete
        );
    }

    #[test]
    fn idmac_read_completes_when_idmac_and_data_done_arrive_separately() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id: RequestId::new(2),
                progress: NoopDmaBuffer::progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });

        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(IDSTS_RECEIVE)
        };
        assert_eq!(
            host.poll_dma_data_step(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Pending
        );

        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(RIntSts::new().with_data_transfer_over(true).into_bits())
        };
        assert_eq!(
            host.poll_dma_data_step(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Complete
        );
    }

    #[test]
    fn fifo_read_completes_when_dto_arrives_before_fifo_is_drained() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut buffer = [0u8; 512];
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoRead {
                id: RequestId::new(1),
                buffer: NonNull::new(buffer.as_mut_ptr()).unwrap(),
                len: buffer.len(),
                block_size: BLOCK_SIZE,
                progress: FifoProgress::default(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: None,
            },
        });

        for index in 0..128 {
            mmio[FIFO_WORD + index] = index as u32;
        }

        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(RIntSts::new().with_data_transfer_over(true).into_bits());
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_fifo_count(64).into_bits());
        }
        assert_eq!(
            host.poll_fifo_data_step(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Pending)
        );

        unsafe {
            mmio.as_mut_ptr().add(RINTSTS_WORD).write_volatile(0);
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_fifo_count(64).into_bits());
        }
        assert_eq!(
            host.poll_fifo_data_step(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Complete)
        );
    }
}
