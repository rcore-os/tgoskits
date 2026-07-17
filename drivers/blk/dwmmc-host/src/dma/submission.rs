impl DwMmc {
    /// Submit one block read using the requested transfer engine.
    ///
    /// Both `BlockTransferMode::Dma` and `BlockTransferMode::Fifo` use the
    /// same submit/event-service queue contract. Runtimes that cannot use DMA can
    /// submit FIFO requests without changing the external block queue shape.
    pub(crate) fn submit_read_blocks(
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
    pub(crate) fn submit_write_blocks(
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

    /// Consume one already-acknowledged IRQ snapshot for an active request.
    ///
    /// Runtime callers reach this only through the queue event path. An empty
    /// mailbox means no progress; it never grants permission to inspect or
    /// acknowledge destructive interrupt status from task context.
    pub(crate) fn service_block_request_response(
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
                return self.service_fifo_event(request, id, slot);
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
            if let Some(error) = self.take_idmac_data_error(cmd_index, phase) {
                return Err(error);
            }

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
                    Err(err) => return Err(err),
                },
                BlockRequestStage::Data => match self.service_dma_completion_event(request, cmd_index, phase) {
                    Ok(BlockPoll::Pending) => return Ok(DataCommandPoll::Pending),
                    Ok(BlockPoll::Complete) => match self.finish_dma_data(request, id, slot)? {
                        DataCommandPoll::Pending => {}
                        complete => return Ok(complete),
                    },
                    // Future BlockPoll variants: best-effort, treat as still pending.
                    Ok(_) => return Ok(DataCommandPoll::Pending),
                    Err(err) => return Err(err),
                },
                BlockRequestStage::Stop => return self.service_stop_event(request, id, slot, phase),
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
        let prepared = backing.prepare_for_device();
        let dma_addr = prepared.dma_addr().as_u64();
        let mut desc = dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr, &mut desc)?;
        let in_flight = unsafe { prepared.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: Some((buffer, size.get())),
                },
                descriptors: InFlightIdmacDescriptors::new(desc),
                completion: DmaCompletionLatch::default(),
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
        let prepared = backing.prepare_for_device();
        let dma_addr = prepared.dma_addr().as_u64();
        let mut desc = dma
            .coherent_array_zero_with_align::<IdmacDesc>(block_count as usize, IDMAC_DESC_ALIGN)
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, dma_addr, &mut desc)?;
        let in_flight = unsafe { prepared.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: None,
                },
                descriptors: InFlightIdmacDescriptors::new(desc),
                completion: DmaCompletionLatch::default(),
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
                descriptors: InFlightIdmacDescriptors::new(desc),
                completion: DmaCompletionLatch::default(),
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
                descriptors: InFlightIdmacDescriptors::new(desc),
                completion: DmaCompletionLatch::default(),
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
        self.ensure_runtime_data_command_can_issue()?;
        self.pending_data = Some(PendingData {
            direction,
            block_size,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        self.program_fifo_interrupt_mask();
        if let Err(error) = self.submit_command(cmd) {
            self.pending_data = None;
            self.data_blocks_remaining = 0;
            self.enable_completion_irq();
            return Err(error);
        }
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::FifoRead {
                id,
                buffer,
                len,
                offset: 0,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                transfer_done: false,
                stop_after_complete,
                response: None,
            },
            DataDirection::Write => BlockRequestKind::FifoWrite {
                id,
                buffer,
                len,
                offset: 0,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                transfer_done: false,
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
        let direction = match cmd.data_direction() {
            Some(sdio_host2::DataDirection::Read) => DataDirection::Read,
            Some(sdio_host2::DataDirection::Write) => DataDirection::Write,
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

        self.ensure_runtime_data_command_can_issue()?;
        let irq = self.irq.clone();
        let register_owner = irq.state.try_begin_task_update().ok_or(Error::Busy)?;
        self.ensure_runtime_data_command_can_issue()?;

        // Coherent descriptor storage still needs an ordering barrier before
        // the IDMAC is allowed to fetch it on weakly ordered architectures.
        mbarrier::wmb();
        self.prepare_data_irq_for_transfer();
        self.program_data_phase(BLOCK_SIZE as u32, block_count);

        self.regs.dbaddr().write(desc_dma as u32);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.completion_irq_enabled())
        });
        self.regs.idinten().write(IDMAC_INT_ENABLE);
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);
        // Publish the complete IDMAC configuration before the command engine
        // can begin the matching data phase.
        mbarrier::wmb();

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        self.activate_admitted_data_command(cmd, &register_owner);
        Ok(())
    }

}
