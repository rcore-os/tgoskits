use super::*;

impl Sdhci {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_adma2_data_request(
        &mut self,
        cmd: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, Error> {
        self.check_not_poisoned()?;
        let transfer_direction = block_transfer_direction(direction)?;
        let id = slot.start(BlockTransferMode::Dma, transfer_direction)?;
        match self.build_bounce_adma2_data_request(
            cmd,
            buffer,
            len,
            block_size,
            block_count,
            direction,
            dma,
            id,
        ) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_prepared_adma2_data_request(
        &mut self,
        cmd: &Command,
        buffer: PreparedDma,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if let Err(err) = self.check_not_poisoned() {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let transfer_direction = match block_transfer_direction(direction) {
            Ok(direction) => direction,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let id = match slot.start(BlockTransferMode::Dma, transfer_direction) {
            Ok(id) => id,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        match self.build_prepared_adma2_data_request(
            cmd,
            buffer,
            block_size,
            block_count,
            direction,
            dma,
            id,
        ) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub(crate) fn progress_block_request(
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

    pub(crate) fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        self.abort_block_request(request, id, slot)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_bounce_adma2_data_request(
        &mut self,
        cmd: &Command,
        cpu_buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let size = validate_adma2_data_shape(block_size, block_count, len)?;
        let alignment = usize::try_from(block_size)
            .ok()
            .and_then(|value| value.checked_next_power_of_two())
            .ok_or(Error::InvalidArgument)?;
        let (buffer, phase, dma_addr) = match direction {
            DataDirection::Read => {
                let backing =
                    CpuDmaBuffer::new_zero(dma, size, alignment, DmaDirection::FromDevice)
                        .map_err(map_dma_error)?;
                let dma_addr = backing.dma_addr().as_u64();
                let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
                (
                    DmaRequestBuffer::Bounce {
                        buffer: in_flight,
                        readback: Some((cpu_buffer, len)),
                    },
                    Phase::DataRead,
                    dma_addr,
                )
            }
            DataDirection::Write => {
                let mut backing =
                    CpuDmaBuffer::new_zero(dma, size, alignment, DmaDirection::ToDevice)
                        .map_err(map_dma_error)?;
                backing.copy_to_device_from_slice(unsafe {
                    core::slice::from_raw_parts(cpu_buffer.as_ptr(), len)
                });
                let dma_addr = backing.dma_addr().as_u64();
                let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
                (
                    DmaRequestBuffer::Bounce {
                        buffer: in_flight,
                        readback: None,
                    },
                    Phase::DataWrite,
                    dma_addr,
                )
            }
            DataDirection::None => return Err(Error::InvalidArgument),
            _ => return Err(Error::InvalidArgument),
        };
        let mut desc = dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
            .map_err(map_dma_error)?;
        self.submit_adma2_data_mapped(
            cmd,
            block_size,
            block_count,
            dma_addr,
            len,
            &mut desc,
            direction,
            phase,
        )?;
        Ok(BlockRequest {
            inner: match direction {
                DataDirection::Read => BlockRequestKind::Read {
                    id,
                    buffer,
                    _desc: desc,
                    cmd_index: cmd.index,
                    phase,
                    stage: BlockRequestStage::Command,
                    stop_after_complete: command_needs_stop(cmd, block_count),
                    response: None,
                },
                DataDirection::Write => BlockRequestKind::Write {
                    id,
                    buffer,
                    _desc: desc,
                    cmd_index: cmd.index,
                    phase,
                    stage: BlockRequestStage::Command,
                    stop_after_complete: command_needs_stop(cmd, block_count),
                    response: None,
                },
                DataDirection::None => unreachable!("direction validated before DMA setup"),
                _ => unreachable!("direction validated before DMA setup"),
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_prepared_adma2_data_request(
        &mut self,
        cmd: &Command,
        buffer: PreparedDma,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if !self.supports_adma2() {
            return Err(PreparedDmaSubmitError::new(
                Error::UnsupportedCommand,
                buffer,
            ));
        }
        let expected_direction = match direction {
            DataDirection::Read => DmaDirection::FromDevice,
            DataDirection::Write => DmaDirection::ToDevice,
            DataDirection::None => {
                return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
            }
            _ => {
                return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
            }
        };
        if buffer.direction() != expected_direction || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let len = buffer.len().get();
        if let Err(err) = validate_adma2_data_shape(block_size, block_count, len) {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => unreachable!("direction validated above"),
            _ => unreachable!("direction validated above"),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => return Err(PreparedDmaSubmitError::new(map_dma_error(err), buffer)),
        };
        if let Err(err) = self.submit_adma2_data_mapped(
            cmd,
            block_size,
            block_count,
            buffer.dma_addr().as_u64(),
            len,
            &mut desc,
            direction,
            phase,
        ) {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let buffer = DmaRequestBuffer::Owned(unsafe { buffer.into_in_flight() });
        Ok(BlockRequest {
            inner: match direction {
                DataDirection::Read => BlockRequestKind::Read {
                    id,
                    buffer,
                    _desc: desc,
                    cmd_index: cmd.index,
                    phase,
                    stage: BlockRequestStage::Command,
                    stop_after_complete: command_needs_stop(cmd, block_count),
                    response: None,
                },
                DataDirection::Write => BlockRequestKind::Write {
                    id,
                    buffer,
                    _desc: desc,
                    cmd_index: cmd.index,
                    phase,
                    stage: BlockRequestStage::Command,
                    stop_after_complete: command_needs_stop(cmd, block_count),
                    response: None,
                },
                DataDirection::None => unreachable!("direction validated above"),
                _ => unreachable!("direction validated above"),
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_adma2_data_mapped(
        &mut self,
        cmd: &Command,
        block_size: u32,
        block_count: u32,
        buffer_dma: u64,
        byte_count: usize,
        desc: &mut CoherentArray<Adma2Desc32>,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<(), Error> {
        validate_adma2_data_shape(block_size, block_count, byte_count)?;
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
            block_size,
            block_count,
        });
        self.use_dma = true;
        self.select_adma2_32();
        self.write_adma_addr(desc_bus as u32);
        let response = self.submit_command(cmd);
        self.use_dma = false;
        response
    }

    pub(super) fn finish_block_request(
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

    pub(super) fn abort_block_request(
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
}
