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
        self.check_not_poisoned()?;
        let id = slot.start(mode, BlockTransferDirection::Read)?;
        let result = match mode {
            BlockTransferMode::Dma => self.build_dma_read_request(
                start_block,
                buffer,
                size,
                dma.ok_or(Error::UnsupportedCommand)?,
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
        self.check_not_poisoned()?;
        let id = slot.start(mode, BlockTransferDirection::Write)?;
        let result = match mode {
            BlockTransferMode::Dma => self.build_dma_write_request(
                start_block,
                buffer,
                size,
                dma.ok_or(Error::UnsupportedCommand)?,
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
        let Some(active) = request.as_ref() else {
            return Err(Error::InvalidArgument);
        };
        if active.id() != id {
            return Err(Error::InvalidArgument);
        }
        self.service_data_event(request, id, slot)
    }

    pub fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        self.abort_block_request(request, id, slot, Phase::DataRead)
    }

    /// Return one request's DMA backing after controller-wide recovery has
    /// already proven that FIFO and IDMAC accesses have stopped.
    pub(crate) fn reclaim_block_request_after_quiesce(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        if request.as_ref().map(BlockRequest::id) != Some(id) {
            return Err(Error::InvalidArgument);
        }
        let active = request.take().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active);
        slot.complete_with_dma(id, completed_dma)
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
        let mut backing = CpuDmaBuffer::new_zero(
            dma,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            block_size_usize,
            dma_direction,
        )
        .map_err(|_| Error::Misaligned)?;
        if direction == DataDirection::Write {
            backing.copy_to_device_from_slice(unsafe {
                core::slice::from_raw_parts(buffer.as_ptr(), len)
            });
        }
        let dma_addr = backing.dma_addr().as_u64();
        let prepared = backing.prepare_for_device();
        let desc_count = len.div_ceil(IDMAC_MAX_BUF_SIZE);
        let mut descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(desc_count, IDMAC_DESC_ALIGN)
            .map_err(|_| Error::Misaligned)?;
        let desc_dma = descriptors.dma_addr().as_u64();
        let desc_values = build_idmac_descriptors(dma_addr, desc_dma, len, IDMAC_MAX_BUF_SIZE)?;
        descriptors.write_with_cpu(desc_values.len(), |dst| dst.copy_from_slice(&desc_values));
        self.start_idmac_transfer(cmd, block_size, block_count, desc_dma)?;
        let in_flight = unsafe {
            // SAFETY: `start_idmac_transfer` has crossed its infallible
            // admitted-activation boundary, so hardware now owns this
            // prepared backing until IRQ completion or recovery quiescence.
            prepared.into_in_flight()
        };

        let progress = DmaProgress {
            descriptors,
            buffer: DmaRequestBuffer::Bounce {
                buffer: in_flight,
                readback: (direction == DataDirection::Read).then_some((buffer, len)),
            },
            desc_count,
            complete: false,
            idmac_done: false,
            data_done: false,
        };
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::DmaRead {
                id,
                progress,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::Write => BlockRequestKind::DmaWrite {
                id,
                progress,
                cmd_index: cmd.index,
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
        let block_count = match block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.build_prepared_dma_data_request(
            &cmd,
            buffer,
            BLOCK_SIZE as u32,
            block_count,
            dma,
            id,
            DataDirection::Read,
            block_count > 1,
        )
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
        let block_count = match block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.build_prepared_dma_data_request(
            &cmd,
            buffer,
            BLOCK_SIZE as u32,
            block_count,
            dma,
            id,
            DataDirection::Write,
            block_count > 1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_prepared_dma_data_request(
        &mut self,
        cmd: &Command,
        buffer: PreparedDma,
        block_size: u32,
        block_count: u32,
        dma: &DeviceDma,
        id: RequestId,
        direction: DataDirection,
        stop_after_complete: bool,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        let block_size_usize = match usize::try_from(block_size) {
            Ok(block_size) => block_size,
            Err(_) => return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer)),
        };
        if block_size_usize == 0
            || buffer.len().get() != block_size_usize.saturating_mul(block_count as usize)
        {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => {
                return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
            }
            _ => return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer)),
        };
        let len = buffer.len().get();
        let desc_count = len.div_ceil(IDMAC_MAX_BUF_SIZE);
        let mut descriptors =
            match dma.coherent_array_zero_with_align::<IdmacDesc>(desc_count, IDMAC_DESC_ALIGN) {
                Ok(descriptors) => descriptors,
                Err(_) => return Err(PreparedDmaSubmitError::new(Error::Misaligned, buffer)),
            };
        let desc_dma = descriptors.dma_addr().as_u64();
        let desc_values = match build_idmac_descriptors(
            buffer.dma_addr().as_u64(),
            desc_dma,
            len,
            IDMAC_MAX_BUF_SIZE,
        ) {
            Ok(desc_values) => desc_values,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        descriptors.write_with_cpu(desc_values.len(), |dst| dst.copy_from_slice(&desc_values));
        match self.start_idmac_transfer(cmd, block_size, block_count, desc_dma) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }

        let progress = DmaProgress {
            descriptors,
            buffer: DmaRequestBuffer::Owned(unsafe { buffer.into_in_flight() }),
            desc_count,
            complete: false,
            idmac_done: false,
            data_done: false,
        };
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::DmaRead {
                id,
                progress,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::Write => BlockRequestKind::DmaWrite {
                id,
                progress,
                cmd_index: cmd.index,
                phase,
                stage: BlockRequestStage::Command,
                stop_after_complete,
                response: None,
            },
            DataDirection::None => {
                unreachable!("DataDirection::None returned before DMA request construction")
            }
            _ => unreachable!("unsupported DataDirection returned before DMA request construction"),
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
            use_idmac: false,
        });
        self.data_blocks_remaining = block_count;
        if let Err(error) = self.submit_command(cmd) {
            self.pending_data = None;
            self.data_blocks_remaining = 0;
            return Err(error);
        }
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::FifoRead {
                id,
                buffer,
                len,
                block_size: block_size_usize,
                progress: FifoProgress::default(),
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
                progress: FifoProgress::default(),
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

}
