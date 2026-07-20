impl PhytiumMci {
    fn service_data_event(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandPoll, Error> {
        match request.as_ref().map(|request| &request.inner) {
            Some(BlockRequestKind::FifoRead { .. }) | Some(BlockRequestKind::FifoWrite { .. }) => {
                self.service_fifo_event(request, id, slot)
            }
            Some(BlockRequestKind::DmaRead { .. }) | Some(BlockRequestKind::DmaWrite { .. }) => {
                self.service_dma_event(request, id, slot)
            }
            None => Err(Error::InvalidArgument),
        }
    }

    fn service_fifo_event(
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
                }
                // Future CommandPoll variants: best-effort, treat as still pending.
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => return Err(err),
            }
        }

        let stage = current_stage(request)?;
        if stage == BlockRequestStage::Stop {
            return self.service_stop_event(request, id, slot, phase);
        }

        match self.service_fifo_data_event(request, cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_fifo_data(request, id, slot),
            // Future BlockPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => Err(err),
        }
    }

    fn service_dma_event(
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
                }
                Ok(_) => return Ok(DataCommandPoll::Pending),
                Err(err) => return Err(err),
            }
        }

        let stage = current_stage(request)?;
        if stage == BlockRequestStage::Stop {
            return self.service_stop_event(request, id, slot, phase);
        }

        match self.service_dma_data_event(request, cmd_index, phase) {
            Ok(BlockPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(BlockPoll::Complete) => self.finish_dma_data(request, id, slot),
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => Err(err),
        }
    }

    fn service_dma_data_event(
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
        let progress = match &mut active.inner {
            BlockRequestKind::DmaRead { progress, .. } => progress,
            BlockRequestKind::DmaWrite { progress, .. } => progress,
            _ => return Err(Error::InvalidArgument),
        };

        if raw_idsts & IDSTS_ERROR_MASK != 0 {
            warn!(
                "phytium-mci IDMAC error cmd={} idsts={:#010x} rintsts={:#010x} status={:#010x} \
                 cur_desc={:#010x}_{:08x} cur_buf={:#010x}_{:08x}",
                cmd_index,
                raw_idsts,
                ints.into_bits(),
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
        if !progress.is_done() {
            return Ok(BlockPoll::Pending);
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
        let stop_after_complete = match request.as_mut().map(|r| &mut r.inner) {
            Some(BlockRequestKind::DmaRead {
                stage,
                stop_after_complete,
                progress,
                ..
            })
            | Some(BlockRequestKind::DmaWrite {
                stage,
                stop_after_complete,
                progress,
                ..
            }) => {
                if !progress.is_done() {
                    return Ok(DataCommandPoll::Pending);
                }
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            _ => return Err(Error::InvalidArgument),
        };
        self.disable_idmac();
        if stop_after_complete {
            self.submit_command(&CMD12)?;
            return Ok(DataCommandPoll::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active);
        slot.complete_with_dma(id, completed_dma)?;
        Ok(DataCommandPoll::Complete(response))
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
                block_size,
                progress,
                ..
            } => service_fifo_read_event(self, *buffer, *len, *block_size, progress, cmd_index, phase),
            BlockRequestKind::FifoWrite {
                buffer,
                len,
                block_size,
                progress,
                ..
            } => service_fifo_write_event(self, *buffer, *len, *block_size, progress, cmd_index, phase),
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
        let completed_dma = self.finish_block_request(active);
        drop(completed_dma);
        slot.complete(id)?;
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
                if !request
                    .as_ref()
                    .is_some_and(|active| active.dma_progress_done())
                {
                    return Ok(DataCommandPoll::Pending);
                }
                let active = request.take().ok_or(Error::InvalidArgument)?;
                let response = active.response().ok_or(Error::InvalidArgument)?;
                let completed_dma = self.finish_block_request(active);
                slot.complete_with_dma(id, completed_dma)?;
                Ok(DataCommandPoll::Complete(response))
            }
            // Future CommandPoll variants: best-effort, treat as still pending.
            Ok(_) => Ok(DataCommandPoll::Pending),
            Err(err) => Err(err),
        }
    }

    fn finish_block_request(&mut self, request: BlockRequest) -> Option<CompletedDma> {
        let completed_dma = match request.inner {
            BlockRequestKind::DmaRead { progress, .. } => {
                progress.keep_alive();
                progress.complete(true)
            }
            BlockRequestKind::DmaWrite { progress, .. } => {
                progress.keep_alive();
                progress.complete(false)
            }
            BlockRequestKind::FifoRead { .. } | BlockRequestKind::FifoWrite { .. } => None,
        };
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.irq.state.end_request();
        completed_dma
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
        self.command_state = crate::command::CommandState::Idle;
        let completed_dma = self.finish_block_request(active);
        drop(completed_dma);
        slot.complete(id)?;
        Ok(())
    }

    fn start_idmac_transfer(
        &mut self,
        cmd: &Command,
        block_size: u32,
        block_count: u32,
        desc_dma: u64,
    ) -> Result<(), Error> {
        self.ensure_runtime_data_command_can_issue()?;
        let irq = self.irq.clone();
        let register_owner = irq.state.try_begin_task_update().ok_or(Error::Busy)?;
        self.ensure_runtime_data_command_can_issue()?;
        self.prepare_data_irq_for_transfer();
        self.regs.idinten().write(0);
        self.program_data_phase(block_size, block_count);
        self.program_idmac_registers(desc_dma);
        self.regs.idinten().write(IDSTS_INT_ENABLE_MASK);
        let data = PendingData {
            direction: if matches!(cmd.index, 24 | 25) {
                DataDirection::Write
            } else {
                DataDirection::Read
            },
            block_size,
            block_count,
            use_idmac: true,
        };
        self.data_blocks_remaining = block_count;
        self.activate_admitted_data_command(cmd, data, &register_owner);
        Ok(())
    }

    fn program_idmac_registers(&self, desc_dma: u64) {
        self.regs.dbaddrl().write(desc_dma as u32);
        self.regs.dbaddrh().write((desc_dma >> 32) as u32);
        self.regs.ctrl().update(|r| {
            r.with_dma_enable(true)
                .with_use_internal_dmac(true)
                .with_int_enable(self.completion_irq_enabled())
        });
        self.regs
            .bmod()
            .write(self.regs.bmod().read() | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE);
        self.regs.pldmnd().write(1);
    }

    pub(crate) fn disable_idmac(&mut self) {
        self.regs.idinten().write(0);
        self.regs.bmod().write(0);
        self.regs
            .ctrl()
            .update(|r| r.with_dma_enable(false).with_use_internal_dmac(false));
    }

    fn take_idmac_status(&mut self) -> u32 {
        let mask = IDSTS_RECEIVE | IDSTS_TRANSMIT | IDSTS_ERROR_MASK;
        self.irq.state.take_idmac_status(mask)
    }

    fn prepare_data_irq_for_transfer(&self) {
        self.irq.state.clear_all();
    }

    fn take_data_irq_status(&mut self, cmd_index: u8, phase: Phase) -> Result<RIntSts, Error> {
        let mask = crate::MCI_INT_DATA_TRANSFER_OVER
            | crate::MCI_INT_RXDR
            | crate::MCI_INT_TXDR
            | crate::MCI_INT_ERROR_MASK;
        let status = self.irq.state.take_status(mask);
        let ints = RIntSts::from_bits(status);
        if ints.error() {
            return Err(self.translate_int_error(ints, phase, cmd_index));
        }
        Ok(ints)
    }
}

const FIFO_EVENT_WORD_BUDGET: usize = 64;

fn service_fifo_read_event(
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
    if !ints.receive_fifo_data_request() && !ints.data_transfer_over() {
        return Ok(BlockPoll::Pending);
    }
    let mut available_words = (host.regs.status().read().fifo_count() as usize)
        .min(FIFO_EVENT_WORD_BUDGET);
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

fn service_fifo_write_event(
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
    if progress.offset >= len && progress.transfer_done {
        return Ok(BlockPoll::Complete);
    }
    if !ints.transmit_fifo_data_request() {
        return Ok(BlockPoll::Pending);
    }
    let status = host.regs.status().read();
    let depth = host.fifo_word_depth() as usize;
    let used = status.fifo_count() as usize;
    let mut free_words = depth.saturating_sub(used).min(FIFO_EVENT_WORD_BUDGET);
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
