//! Owned request progression and IDMAC lifecycle.

use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, PreparedDma};
use log::warn;
use sdmmc_protocol::{
    block::{
        BlockProgress, BlockTransferDirection, BlockTransferMode, CommandProgress,
        DataCommandProgress,
    },
    cmd::{CMD12, Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, ErrorContext, Phase},
};

use crate::{
    host::{DwMmc, PendingData},
    regs::RegisterBlockVolatileFieldAccess,
};

mod idmac;
mod request;

use idmac::*;
pub use idmac::{IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, IDMAC_MAX_BLOCKS, IDMAC_MAX_TRANSFER_SIZE};
#[cfg(test)]
pub(crate) use idmac::{IDMAC_INT_AI, IDMAC_INT_FBE};
pub(crate) use idmac::{IDMAC_INT_CLR, IDMAC_INT_ERROR, IDMAC_INT_RI, IDMAC_INT_TI, IdmacRing};
pub use request::{BlockRequest, BlockRequestSlot, PreparedDmaSubmitError, RequestId};
use request::{BlockRequestKind, BlockRequestStage, DmaRequestBuffer};

const BMOD_SWR: u32 = 1 << 0;
const BMOD_FB: u32 = 1 << 1;
const BMOD_DE: u32 = 1 << 7;
pub(super) const BLOCK_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DmaTransferDirection {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DmaDataTransfer {
    command: Command,
    direction: DmaTransferDirection,
    block_size: u32,
    block_count: u32,
    byte_count: NonZeroUsize,
}

impl DmaDataTransfer {
    pub(crate) fn for_protocol(
        command: &Command,
        block_size: u32,
        block_count: u32,
        byte_count: usize,
        direction: DataDirection,
    ) -> Option<Self> {
        let expected_byte_count =
            NonZeroUsize::new(usize::try_from(block_size.checked_mul(block_count)?).ok()?)?;
        if expected_byte_count.get() != byte_count {
            return None;
        }
        let direction = match direction {
            DataDirection::Read => DmaTransferDirection::Read,
            DataDirection::Write => DmaTransferDirection::Write,
            _ => return None,
        };
        let supported = match (direction, command.index) {
            (DmaTransferDirection::Read, 17) => block_size == 512 && block_count == 1,
            (DmaTransferDirection::Read, 18) => block_size == 512 && block_count > 1,
            (DmaTransferDirection::Write, 24) => block_size == 512 && block_count == 1,
            (DmaTransferDirection::Write, 25) => block_size == 512 && block_count > 1,
            (DmaTransferDirection::Read, 6) => {
                block_size == 64
                    && block_count == 1
                    && command.response == sdmmc_protocol::response::ResponseType::R1
            }
            (DmaTransferDirection::Read, 8) => {
                block_size == 512
                    && block_count == 1
                    && command.response == sdmmc_protocol::response::ResponseType::R1
            }
            _ => false,
        };
        supported.then_some(Self {
            command: *command,
            direction,
            block_size,
            block_count,
            byte_count: expected_byte_count,
        })
    }

    fn read_blocks(start_block: u32, size: NonZeroUsize) -> Result<Self, Error> {
        let block_count = dma_read_block_count(size)?;
        let command = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        Self::for_protocol(
            &command,
            BLOCK_SIZE as u32,
            block_count,
            size.get(),
            DataDirection::Read,
        )
        .ok_or(Error::InvalidArgument)
    }

    fn write_blocks(start_block: u32, size: NonZeroUsize) -> Result<Self, Error> {
        let block_count = dma_write_block_count(size)?;
        let command = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        Self::for_protocol(
            &command,
            BLOCK_SIZE as u32,
            block_count,
            size.get(),
            DataDirection::Write,
        )
        .ok_or(Error::InvalidArgument)
    }

    fn phase(self) -> Phase {
        match self.direction {
            DmaTransferDirection::Read => Phase::DataRead,
            DmaTransferDirection::Write => Phase::DataWrite,
        }
    }

    fn dma_direction(self) -> DmaDirection {
        match self.direction {
            DmaTransferDirection::Read => DmaDirection::FromDevice,
            DmaTransferDirection::Write => DmaDirection::ToDevice,
        }
    }

    fn block_direction(self) -> BlockTransferDirection {
        match self.direction {
            DmaTransferDirection::Read => BlockTransferDirection::Read,
            DmaTransferDirection::Write => BlockTransferDirection::Write,
        }
    }

    fn protocol_direction(self) -> DataDirection {
        match self.direction {
            DmaTransferDirection::Read => DataDirection::Read,
            DmaTransferDirection::Write => DataDirection::Write,
        }
    }

    fn needs_stop(self) -> bool {
        self.block_count > 1 && matches!(self.command.index, 18 | 25)
    }
}

fn data_request(
    transfer: DmaDataTransfer,
    id: RequestId,
    buffer: DmaRequestBuffer,
) -> BlockRequest {
    let request = match transfer.direction {
        DmaTransferDirection::Read => BlockRequestKind::Read {
            id,
            buffer,
            cmd_index: transfer.command.index,
            phase: transfer.phase(),
            stage: BlockRequestStage::Command,
            stop_after_complete: transfer.needs_stop(),
            response: None,
        },
        DmaTransferDirection::Write => BlockRequestKind::Write {
            id,
            buffer,
            cmd_index: transfer.command.index,
            phase: transfer.phase(),
            stage: BlockRequestStage::Command,
            stop_after_complete: transfer.needs_stop(),
            response: None,
        },
    };
    BlockRequest { inner: request }
}

impl DwMmc {
    /// Submit one block read through the controller-lifetime IDMAC ring.
    pub fn submit_read_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        let transfer = DmaDataTransfer::read_blocks(start_block, size)?;
        self.submit_dma_data(transfer, buffer, dma, slot)
    }

    /// Submit one block write through the controller-lifetime IDMAC ring.
    pub fn submit_write_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        let transfer = DmaDataTransfer::write_blocks(start_block, size)?;
        self.submit_dma_data(transfer, buffer, dma, slot)
    }

    pub fn submit_prepared_read_blocks(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        let size = buffer.len();
        let transfer = match DmaDataTransfer::read_blocks(start_block, size) {
            Ok(transfer) => transfer,
            Err(error) => return Err(PreparedDmaSubmitError::new(error, buffer)),
        };
        self.submit_prepared_data(transfer, buffer, dma, slot)
    }

    pub fn submit_prepared_write_blocks(
        &mut self,
        start_block: u32,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        let size = buffer.len();
        let transfer = match DmaDataTransfer::write_blocks(start_block, size) {
            Ok(transfer) => transfer,
            Err(error) => return Err(PreparedDmaSubmitError::new(error, buffer)),
        };
        self.submit_prepared_data(transfer, buffer, dma, slot)
    }

    /// Advance one submitted request for an acknowledged IRQ or register retry.
    ///
    /// Command and data completion is consumed only for
    /// [`sdio_host2::ProgressCause::AcknowledgedIrq`]. Register retries may
    /// only move the command issue state toward the point where hardware owns
    /// the command.
    pub fn advance_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        cause: sdio_host2::ProgressCause,
    ) -> Result<DataCommandProgress, Error> {
        let acknowledged_irq = cause == sdio_host2::ProgressCause::AcknowledgedIrq;
        loop {
            let Some(active) = request.as_ref() else {
                return Err(Error::InvalidArgument);
            };
            if active.id() != id {
                return Err(Error::InvalidArgument);
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
            };

            match stage {
                BlockRequestStage::Command => {
                    match self.advance_command_for_cause(acknowledged_irq) {
                        Ok(CommandProgress::Pending) => return Ok(DataCommandProgress::Pending),
                        Ok(CommandProgress::Complete) if acknowledged_irq => {
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
                                }
                            }
                        }
                        Ok(CommandProgress::Complete) => return Ok(DataCommandProgress::Pending),
                        Err(err) => {
                            let _ = self.abort_block_request(request, id, slot, phase);
                            return Err(err);
                        }
                    }
                }
                BlockRequestStage::Data if !acknowledged_irq => {
                    return Ok(DataCommandProgress::Pending);
                }
                BlockRequestStage::Data => match self.consume_dma_completion(cmd_index, phase) {
                    Ok(BlockProgress::Pending) => return Ok(DataCommandProgress::Pending),
                    Ok(BlockProgress::Complete) => match self.finish_dma_data(request, id, slot)? {
                        DataCommandProgress::Pending => {}
                        complete => return Ok(complete),
                    },
                    Err(err) => {
                        let _ = self.abort_block_request(request, id, slot, phase);
                        return Err(err);
                    }
                },
                BlockRequestStage::Stop => {
                    return self.advance_block_stop(request, id, slot, phase, acknowledged_irq);
                }
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

    pub(crate) fn submit_dma_data(
        &mut self,
        transfer: DmaDataTransfer,
        buffer: NonNull<u8>,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        self.check_not_poisoned()?;
        let id = slot.start(BlockTransferMode::Dma, transfer.block_direction())?;
        match self.build_dma_data_request(transfer, buffer, dma, id) {
            Ok(request) => Ok(request),
            Err(error) => {
                let _ = slot.complete(id);
                Err(error)
            }
        }
    }

    pub(crate) fn submit_prepared_data(
        &mut self,
        transfer: DmaDataTransfer,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if let Err(error) = self.check_not_poisoned() {
            return Err(PreparedDmaSubmitError::new(error, buffer));
        }
        let id = match slot.start(BlockTransferMode::Dma, transfer.block_direction()) {
            Ok(id) => id,
            Err(error) => return Err(PreparedDmaSubmitError::new(error, buffer)),
        };
        match self.build_prepared_dma_data_request(transfer, buffer, dma, id) {
            Ok(request) => Ok(request),
            Err(error) => {
                let _ = slot.complete(id);
                Err(error)
            }
        }
    }

    fn build_dma_data_request(
        &mut self,
        transfer: DmaDataTransfer,
        buffer: NonNull<u8>,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        let mut backing = CpuDmaBuffer::new_zero(
            dma,
            transfer.byte_count,
            transfer.block_size as usize,
            transfer.dma_direction(),
        )
        .map_err(|error| map_dma_error(error, transfer.phase()))?;
        let readback = match transfer.direction {
            DmaTransferDirection::Read => Some((buffer, transfer.byte_count.get())),
            DmaTransferDirection::Write => {
                // SAFETY: The caller keeps the borrowed source alive until the
                // returned request completes, and `byte_count` was validated
                // against the submitted data phase.
                backing.copy_to_device_from_slice(unsafe {
                    core::slice::from_raw_parts(buffer.as_ptr(), transfer.byte_count.get())
                });
                None
            }
        };
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        self.submit_idmac_transfer_mapped(transfer, dma_addr)?;
        Ok(data_request(
            transfer,
            id,
            DmaRequestBuffer::Bounce {
                buffer: in_flight,
                readback,
            },
        ))
    }

    fn build_prepared_dma_data_request(
        &mut self,
        transfer: DmaDataTransfer,
        buffer: PreparedDma,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if buffer.direction() != transfer.dma_direction()
            || buffer.domain_id() != dma.domain_id()
            || buffer.len() != transfer.byte_count
        {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        match self.submit_idmac_transfer_mapped(transfer, buffer.dma_addr().as_u64()) {
            Ok(()) => {}
            Err(error) => return Err(PreparedDmaSubmitError::new(error, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(data_request(transfer, id, DmaRequestBuffer::Owned(buffer)))
    }

    fn submit_idmac_transfer_mapped(
        &mut self,
        transfer: DmaDataTransfer,
        buffer_dma: u64,
    ) -> Result<(), Error> {
        let phase = transfer.phase();
        let mut ring = self.idmac_ring.take().ok_or(Error::UnsupportedCommand)?;
        let desc_dma = match ring.prepare(buffer_dma, transfer.byte_count.get()) {
            Ok(desc_dma) => desc_dma,
            Err(err) => {
                self.idmac_ring = Some(ring);
                return Err(err);
            }
        };
        self.idmac_ring = Some(ring);

        self.clear_all_int_status();
        self.regs.idsts().write(IDMAC_INT_CLR);
        self.irq.state.clear(u32::MAX);
        self.program_data_phase(transfer.block_size, transfer.block_count);
        if let Err(err) = self.reset_dma_engine(phase) {
            self.poison_dma();
            return Err(err);
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        self.regs.dbaddr().write(desc_dma);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.completion_irq_enabled())
        });
        self.regs.idinten().write(IDMAC_INT_ENABLE);
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);

        self.pending_data = Some(PendingData {
            direction: transfer.protocol_direction(),
            block_size: transfer.block_size,
            block_count: transfer.block_count,
        });
        self.data_blocks_remaining = transfer.block_count;
        self.controller_data_complete = false;
        self.idmac_data_complete = false;
        match self.submit_command(&transfer.command) {
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
        }
        let completed_dma = match request.inner {
            BlockRequestKind::Read { stage, buffer, .. } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                self.disable_idmac();
                self.clear_all_int_status();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                self.controller_data_complete = false;
                self.idmac_data_complete = false;
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
                self.controller_data_complete = false;
                self.idmac_data_complete = false;
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
    ) -> Result<DataCommandProgress, Error> {
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
        };

        if stop_after_complete {
            // CMD12 is part of the same multi-block transaction. Preserve the
            // IRQ generation so a late data/IDMAC error cannot be discarded
            // between DTO and the stop response.
            self.submit_chained_command(&CMD12)?;
            return Ok(DataCommandProgress::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active)?;
        slot.complete_with_dma(id, completed_dma)?;
        Ok(DataCommandProgress::Complete(response))
    }

    fn advance_block_stop(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        phase: Phase,
        acknowledged_irq: bool,
    ) -> Result<DataCommandProgress, Error> {
        if !acknowledged_irq && !self.command_needs_register_retry() {
            return Ok(DataCommandProgress::Pending);
        }
        match self.advance_command_for_cause(acknowledged_irq) {
            Ok(CommandProgress::Pending) => Ok(DataCommandProgress::Pending),
            Ok(CommandProgress::Complete) => {
                let _ = self.take_command_response()?;
                let active = request.take().ok_or(Error::InvalidArgument)?;
                let response = active.response().ok_or(Error::InvalidArgument)?;
                let completed_dma = self.finish_block_request(active)?;
                slot.complete_with_dma(id, completed_dma)?;
                Ok(DataCommandProgress::Complete(response))
            }
            Err(err) => {
                let _ = self.abort_block_request(request, id, slot, phase);
                Err(err)
            }
        }
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
        self.controller_data_complete = false;
        self.idmac_data_complete = false;
        self.command_state = crate::command::CommandState::Idle;
        slot.complete(id)?;
        recovery
    }

    fn disable_idmac(&self) {
        self.regs
            .ctrl()
            .update(|r| r.with_use_internal_dmac(false).with_dma_enable(false));
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
        let dma = self.reset_dma_engine(phase);
        self.regs.ctrl().update(|r| r.with_abort_read_data(false));
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.controller_data_complete = false;
        self.idmac_data_complete = false;
        self.command_state = crate::command::CommandState::Idle;
        match (fifo, dma) {
            (Ok(()), Ok(())) => {
                if let Some(ring) = self.idmac_ring.as_mut() {
                    ring.clear_after_reset();
                }
                Ok(())
            }
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

    /// Reset both halves of the internal DMA engine before publishing a ring.
    ///
    /// Linux performs the same `CTRL_DMA_RESET` + `BMOD_SWR` sequence in
    /// `dw_mci_idmac_start_dma()`. The JH7110 controller otherwise retains
    /// FIFO/IDMAC state across direction changes and can underrun a later
    /// multi-block write before the first descriptor completes.
    fn reset_dma_engine(&self, phase: Phase) -> Result<(), Error> {
        self.regs.ctrl().update(|r| r.with_dma_reset(true));
        for _ in 0..crate::host::DWMMC_HW_POLL_LIMIT {
            if !self.regs.ctrl().read().dma_reset() {
                self.regs.bmod().write(self.regs.bmod().read() | BMOD_SWR);
                for _ in 0..crate::host::DWMMC_HW_POLL_LIMIT {
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

    fn consume_dma_completion(
        &mut self,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockProgress, Error> {
        let raw_status = self.take_data_irq_status();
        if raw_status & crate::DWMMC_LATCH_IDMAC_ERROR != 0 {
            return Err(Error::BusError(ErrorContext::for_cmd(phase, cmd_index)));
        }
        let rintsts = crate::regs::RIntSts::from_bits(raw_status);
        if rintsts.error() {
            return Err(self.translate_int_error(rintsts, phase, cmd_index));
        }
        self.controller_data_complete |= rintsts.data_transfer_over();
        self.idmac_data_complete |= raw_status & crate::DWMMC_LATCH_IDMAC_COMPLETE != 0;
        if self.controller_data_complete && self.idmac_data_complete {
            return Ok(BlockProgress::Complete);
        }
        Ok(BlockProgress::Pending)
    }

    fn take_data_irq_status(&mut self) -> u32 {
        let consume = crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_LATCH_IDMAC_COMPLETE
            | crate::DWMMC_LATCH_IDMAC_ERROR
            | crate::DWMMC_INT_ERROR_MASK;
        self.take_task_irq_status(consume)
    }
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
        dma_api::DmaError::NoMemory | dma_api::DmaError::CoherentReleaseFailed => {
            Error::BusError(ErrorContext::new(phase))
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
