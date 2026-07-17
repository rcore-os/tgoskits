//! IRQ-evidence consumption, terminal completion, and DMA quiescence.

use super::*;

impl Sdhci {
    fn finish_block_request(
        &mut self,
        request: BlockRequest,
    ) -> Result<CompletedBlockBacking, Error> {
        self.finish_block_request_with_quiesce(request, true)
    }

    fn finish_block_request_with_quiesce(
        &mut self,
        request: BlockRequest,
        quiesced: bool,
    ) -> Result<CompletedBlockBacking, Error> {
        if !quiesced {
            self.poison_dma();
            core::mem::forget(request);
            self.pending_data = None;
            self.active_data_cmd = 0;
            self.clear_cached_irq_status();
            return Ok(CompletedBlockBacking::default());
        }
        let completed = match request.inner {
            BlockRequestKind::FifoRead { owned_cpu, .. }
            | BlockRequestKind::FifoWrite { owned_cpu, .. } => CompletedBlockBacking {
                dma: None,
                cpu: owned_cpu,
            },
            BlockRequestKind::Read {
                stage,
                buffer,
                descriptors,
                ..
            } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                let dma = if quiesced {
                    buffer.complete(true)
                } else {
                    buffer.abort(true, false)
                };
                // SAFETY: this path is reachable only after terminal transfer
                // completion or a controller-wide quiescence proof.
                unsafe { descriptors.release_after_quiesce() };
                CompletedBlockBacking { dma, cpu: None }
            }
            BlockRequestKind::Write {
                stage,
                buffer,
                descriptors,
                ..
            } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                let dma = if quiesced {
                    buffer.complete(false)
                } else {
                    buffer.abort(false, false)
                };
                // SAFETY: this path is reachable only after terminal transfer
                // completion or a controller-wide quiescence proof.
                unsafe { descriptors.release_after_quiesce() };
                CompletedBlockBacking { dma, cpu: None }
            }
        };
        self.pending_data = None;
        self.active_data_cmd = 0;
        self.clear_cached_irq_status();
        Ok(completed)
    }

    /// Return one request's DMA backing after the controller-wide lifecycle
    /// has already produced a quiescence proof.
    pub(crate) fn reclaim_block_request_after_quiesce(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        let active = request.take().ok_or(Error::InvalidArgument)?;
        let completed = self.finish_block_request_with_quiesce(active, true)?;
        slot.complete_with_backing(id, completed)
    }

    pub(super) fn finish_dma_data(
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
            if let Err(error) = self.submit_command(&sdmmc_protocol::cmd::CMD12) {
                self.log_status("block DMA CMD12 handoff failed", 12);
                return Err(error);
            }
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed = self.finish_block_request(active)?;
        slot.complete_with_backing(id, completed)?;
        Ok(DataCommandPoll::Complete(response))
    }

    pub(super) fn poll_block_stop(
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
                let completed = self.finish_block_request(active)?;
                slot.complete_with_backing(id, completed)?;
                Ok(DataCommandPoll::Complete(response))
            }
            // Future CommandPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => Err(err),
        }
    }

    pub(super) fn poll_fifo_request(
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
                Err(err) => return Err(err),
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
            Err(err) => Err(err),
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
            if let Err(error) = self.submit_command(&sdmmc_protocol::cmd::CMD12) {
                self.log_status("block FIFO CMD12 handoff failed", 12);
                return Err(error);
            }
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed = self.finish_block_request(active)?;
        slot.complete_with_backing(id, completed)?;
        Ok(DataCommandPoll::Complete(response))
    }

    pub(super) fn abort_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        let active = request.take().ok_or(Error::InvalidArgument)?;
        let completed = self.finish_block_request_with_quiesce(active, true)?;
        slot.complete_with_backing(id, completed)?;
        Ok(())
    }

    pub(crate) fn poll_data_complete_with_adma(
        &mut self,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockPoll, Error> {
        let snapshot = self.take_data_irq_status();
        if snapshot.has_error() {
            let ctx = ErrorContext::for_cmd(phase, cmd_index);
            return Err(if snapshot.error & ERROR_INT_ADMA != 0 {
                Error::Misaligned
            } else if snapshot.error & (ERROR_INT_DATA_TIMEOUT | ERROR_INT_CMD_TIMEOUT) != 0 {
                Error::Timeout(ctx)
            } else if snapshot.error & (ERROR_INT_DATA_CRC | ERROR_INT_CMD_CRC) != 0 {
                Error::Crc(ctx)
            } else if matches!(phase, Phase::DataRead) {
                Error::ReadError(ctx)
            } else {
                Error::WriteError(ctx)
            });
        }
        if snapshot.normal & NORMAL_INT_XFER_COMPLETE != 0 {
            return Ok(BlockPoll::Complete);
        }
        Ok(BlockPoll::Pending)
    }

    fn poll_fifo_data_complete(&mut self, cmd_index: u8, phase: Phase) -> Result<BlockPoll, Error> {
        self.poll_data_complete_with_adma(cmd_index, phase)
    }
}

pub(super) fn build_descriptors_into_dma(
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

pub(super) fn poll_fifo_read_step(
    host: &mut Sdhci,
    buffer: NonNull<u8>,
    len: usize,
    block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    if *offset >= len {
        return host.poll_fifo_data_complete(cmd_index, phase);
    }

    let snapshot = host.take_fifo_irq_status(NORMAL_INT_BUFFER_READ_READY | NORMAL_INT_ERROR);
    if snapshot.has_error() {
        return poll_fifo_status(
            host,
            snapshot.normal,
            snapshot.error,
            cmd_index,
            phase,
            true,
        );
    }
    if snapshot.normal & NORMAL_INT_BUFFER_READ_READY == 0 {
        return poll_fifo_status(
            host,
            snapshot.normal,
            snapshot.error,
            cmd_index,
            phase,
            true,
        );
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
    if *offset == len {
        host.poll_fifo_data_complete(cmd_index, phase)
    } else {
        Ok(BlockPoll::Pending)
    }
}

pub(super) fn poll_fifo_write_step(
    host: &mut Sdhci,
    buffer: NonNull<u8>,
    len: usize,
    block_size: usize,
    offset: &mut usize,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    if *offset >= len {
        return host.poll_fifo_data_complete(cmd_index, phase);
    }

    let snapshot = host.take_fifo_irq_status(NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_ERROR);
    if snapshot.has_error() {
        return poll_fifo_status(
            host,
            snapshot.normal,
            snapshot.error,
            cmd_index,
            phase,
            false,
        );
    }
    if snapshot.normal & NORMAL_INT_BUFFER_WRITE_READY == 0 {
        return poll_fifo_status(
            host,
            snapshot.normal,
            snapshot.error,
            cmd_index,
            phase,
            false,
        );
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
    if *offset == len {
        host.poll_fifo_data_complete(cmd_index, phase)
    } else {
        Ok(BlockPoll::Pending)
    }
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

pub(super) fn dma_read_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    let len = size.get();
    if !len.is_multiple_of(BLOCK_SIZE) {
        return Err(Error::Misaligned);
    }
    let blocks = len / BLOCK_SIZE;
    u32::try_from(blocks).map_err(|_| Error::InvalidArgument)
}

pub(super) fn dma_write_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    dma_read_block_count(size)
}

pub(super) fn map_dma_error(err: dma_api::DmaError) -> Error {
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
