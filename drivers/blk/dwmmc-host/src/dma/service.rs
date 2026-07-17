impl DwMmc {
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
            BlockRequestKind::Read {
                stage,
                buffer,
                descriptors,
                ..
            } => {
                if stage == BlockRequestStage::Command {
                    let _ = self.take_command_response();
                }
                self.disable_idmac();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                let completed = if quiesced {
                    buffer.complete(true)
                } else {
                    buffer.abort(true, false)
                };
                // SAFETY: this path is reachable only after terminal IDMAC and
                // controller completion or controller-wide reset quiescence.
                unsafe { descriptors.release_after_quiesce() };
                completed
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
                self.disable_idmac();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                let completed = if quiesced {
                    buffer.complete(false)
                } else {
                    buffer.abort(false, false)
                };
                // SAFETY: see the read path above; both engines are terminal
                // or a reset-derived quiescence proof is held.
                unsafe { descriptors.release_after_quiesce() };
                completed
            }
        };
        self.irq.state.end_request();
        Ok(completed_dma)
    }

    /// Return one request's DMA backing after controller-wide reset has
    /// already proven the IDMAC and FIFO idle.
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
        let completed_dma = self.finish_block_request_with_quiesce(active, true)?;
        slot.complete_with_dma(id, completed_dma)
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

    fn service_stop_event(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        _phase: Phase,
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
            Err(err) => Err(err),
        }
    }

    fn service_fifo_event(
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
                    Err(err) => return Err(err),
                },
                BlockRequestStage::Data => {
                    match self.service_fifo_data_event(request, cmd_index, phase) {
                        Ok(BlockPoll::Pending) => return Ok(DataCommandPoll::Pending),
                        Ok(BlockPoll::Complete) => {
                            match self.finish_fifo_data(request, id, slot)? {
                                DataCommandPoll::Pending => {}
                                complete => return Ok(complete),
                            }
                        }
                        // Future BlockPoll variants: best-effort, treat as still pending.
                        Ok(_) => return Ok(DataCommandPoll::Pending),
                        Err(err) => return Err(err),
                    }
                }
                BlockRequestStage::Stop => return self.service_stop_event(request, id, slot, phase),
            }
        }
    }

    fn service_fifo_data_event(
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
                offset,
                transfer_done,
                ..
            } => service_fifo_read_event(self, *buffer, *len, offset, transfer_done, cmd_index, phase),
            BlockRequestKind::FifoWrite {
                buffer,
                len,
                offset,
                transfer_done,
                ..
            } => service_fifo_write_event(self, *buffer, *len, offset, transfer_done, cmd_index, phase),
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
        _phase: Phase,
    ) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        let active = request.take().ok_or(Error::InvalidArgument)?;
        self.disable_idmac();
        let completed_dma = self.finish_block_request_with_quiesce(active, true)?;
        drop(completed_dma);
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.command_state = crate::command::CommandState::Idle;
        slot.complete(id)?;
        Ok(())
    }

    fn disable_idmac(&self) {
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(false)
                .with_dma_enable(false)
        });
        self.regs.idinten().write(0);
        self.regs.bmod().write(0);
    }

    pub(crate) fn disable_dma_for_controller_recovery(&self) {
        self.disable_idmac();
    }

    fn prepare_data_irq_for_transfer(&self) {
        self.irq.state.clear_all();
    }

    fn take_idmac_data_error(&mut self, cmd_index: u8, phase: Phase) -> Option<Error> {
        let status = self.take_task_idmac_status(crate::event::DWMMC_IDMAC_INT_ERROR_MASK);
        (status != 0).then_some(Error::BusError(ErrorContext::for_cmd(phase, cmd_index)))
    }

    fn service_dma_completion_event(
        &mut self,
        request: &mut Option<BlockRequest>,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockPoll, Error> {
        let raw_status = self.take_data_irq_status();
        let idmac_status = self.take_task_idmac_status(crate::event::DWMMC_IDMAC_INT_ENABLE_MASK);
        let rintsts = crate::regs::RIntSts::from_bits(raw_status);
        if rintsts.error() {
            return Err(self.translate_int_error(rintsts, phase, cmd_index));
        }
        if idmac_status & crate::event::DWMMC_IDMAC_INT_ERROR_MASK != 0 {
            return Err(Error::BusError(ErrorContext::for_cmd(phase, cmd_index)));
        }
        let completion = match request.as_mut().map(|request| &mut request.inner) {
            Some(BlockRequestKind::Read { completion, .. })
            | Some(BlockRequestKind::Write { completion, .. }) => completion,
            _ => return Err(Error::InvalidArgument),
        };
        Ok(completion.observe(raw_status, idmac_status))
    }

    fn take_data_irq_status(&mut self) -> u32 {
        let consume = crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_INT_RXDR
            | crate::DWMMC_INT_TXDR
            | crate::DWMMC_INT_ERROR_MASK;
        self.take_task_irq_status(consume)
    }
}

const FIFO_EVENT_WORD_BUDGET: usize = 64;

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

fn service_fifo_read_event(
    host: &mut DwMmc,
    buffer: NonNull<u8>,
    len: usize,
    offset: &mut usize,
    transfer_done: &mut bool,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let raw_status = host.take_data_irq_status();
    let rintsts = crate::regs::RIntSts::from_bits(raw_status);
    if rintsts.error() {
        return Err(host.translate_int_error(rintsts, phase, cmd_index));
    }
    *transfer_done |= rintsts.data_transfer_over();

    // FIFO occupancy may only be consumed as a continuation of a device IRQ
    // snapshot. Re-entering the worker without RXDR/DTO must not turn STATUS
    // into a task-context completion poll.
    if !rintsts.receive_fifo_data_request() && !rintsts.data_transfer_over() {
        host.program_fifo_interrupt_mask();
        return Ok(BlockPoll::Pending);
    }

    let fifo = host.fifo_ptr();
    let mut status = host.regs.status().read();
    let mut serviced_words = 0;
    while *offset < len
        && status.fifo_count() != 0
        && serviced_words < FIFO_EVENT_WORD_BUDGET
    {
        let value = unsafe { fifo.read_volatile() };
        let end = (*offset + 4).min(len);
        let block =
            unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr().add(*offset), end - *offset) };
        block.copy_from_slice(&value.to_le_bytes()[..block.len()]);
        *offset = end;
        serviced_words += 1;
        status = host.regs.status().read();
    }

    if *offset >= len && *transfer_done {
        return Ok(BlockPoll::Complete);
    }
    host.program_fifo_interrupt_mask();
    Ok(BlockPoll::Pending)
}

fn service_fifo_write_event(
    host: &mut DwMmc,
    buffer: NonNull<u8>,
    len: usize,
    offset: &mut usize,
    transfer_done: &mut bool,
    cmd_index: u8,
    phase: Phase,
) -> Result<BlockPoll, Error> {
    let raw_status = host.take_data_irq_status();
    let rintsts = crate::regs::RIntSts::from_bits(raw_status);
    if rintsts.error() {
        return Err(host.translate_int_error(rintsts, phase, cmd_index));
    }
    *transfer_done |= rintsts.data_transfer_over();

    if *offset >= len && *transfer_done {
        return Ok(BlockPoll::Complete);
    }
    // TX FIFO capacity is meaningful only after the IRQ endpoint published a
    // TXDR snapshot. An empty snapshot must leave the request pending.
    if !rintsts.transmit_fifo_data_request() {
        host.program_fifo_interrupt_mask();
        return Ok(BlockPoll::Pending);
    }

    let fifo = host.fifo_ptr();
    let mut serviced_words = 0;
    while *offset < len
        && !host.regs.status().read().fifo_full()
        && serviced_words < FIFO_EVENT_WORD_BUDGET
    {
        let end = (*offset + 4).min(len);
        let block =
            unsafe { core::slice::from_raw_parts(buffer.as_ptr().add(*offset), end - *offset) };
        let mut bytes = [0u8; 4];
        bytes[..block.len()].copy_from_slice(block);
        unsafe { fifo.write_volatile(u32::from_le_bytes(bytes)) };
        *offset = end;
        serviced_words += 1;
    }

    if *offset >= len && *transfer_done {
        return Ok(BlockPoll::Complete);
    }
    host.program_fifo_interrupt_mask();
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
