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
        self.check_not_poisoned()?;
        let id = slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read)?;
        let result = self.build_dma_read_request(start_block, buffer, size, dma, id);
        match result {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
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
        self.check_not_poisoned()?;
        let id = slot.start(BlockTransferMode::Dma, BlockTransferDirection::Write)?;
        let result = self.build_dma_write_request(start_block, buffer, size, dma, id);
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
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr)?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: Some((buffer, size.get())),
                },
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
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr)?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: None,
                },
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
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        match self.submit_idmac_transfer_mapped(&cmd, block_count, buffer.dma_addr().as_u64()) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
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
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        match self.submit_idmac_transfer_mapped(&cmd, block_count, buffer.dma_addr().as_u64()) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
                cmd_index: cmd.index,
                phase: Phase::DataWrite,
                stage: BlockRequestStage::Command,
                stop_after_complete: block_count > 1,
                response: None,
            },
        })
    }

    fn submit_idmac_transfer_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
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
        let mut ring = self.idmac_ring.take().ok_or(Error::UnsupportedCommand)?;
        let desc_dma = match ring.prepare(buffer_dma, byte_count as usize) {
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
        self.program_data_phase(BLOCK_SIZE as u32, block_count);
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
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        self.controller_data_complete = false;
        self.idmac_data_complete = false;
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
mod tests;
