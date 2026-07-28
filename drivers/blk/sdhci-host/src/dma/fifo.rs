use super::*;

impl Sdhci {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_fifo_data_request(
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
