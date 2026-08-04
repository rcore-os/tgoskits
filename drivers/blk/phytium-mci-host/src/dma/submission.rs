use super::*;

impl PhytiumMci {
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

    pub(crate) fn submit_prepared_data_command(
        &mut self,
        transfer: PreparedDataCommand,
        buffer: PreparedDma,
        dma: &DeviceDma,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, PreparedDmaSubmitError> {
        if let Err(err) = self.check_not_poisoned() {
            return Err(PreparedDmaSubmitError::new(err, buffer));
        }
        let (dma_direction, transfer_direction) = match transfer.direction {
            DataDirection::Read => (DmaDirection::FromDevice, BlockTransferDirection::Read),
            DataDirection::Write => (DmaDirection::ToDevice, BlockTransferDirection::Write),
            DataDirection::None => {
                return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
            }
            _ => {
                return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
            }
        };
        if buffer.direction() != dma_direction || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let id = match slot.start(BlockTransferMode::Dma, transfer_direction) {
            Ok(id) => id,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let stop_after_complete = matches!(transfer.command.index, 18 | 25);
        match self.build_prepared_dma_data_request(
            &transfer.command,
            buffer,
            transfer.block_size,
            transfer.block_count,
            id,
            transfer.direction,
            stop_after_complete,
        ) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        let phase = request
            .as_ref()
            .map(BlockRequest::phase)
            .ok_or(Error::InvalidArgument)?;
        self.abort_block_request(request, id, slot, phase)
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
        let desc_dma = self.prepare_idmac_ring(dma_addr, len)?;
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        if let Err(err) =
            self.start_idmac_transfer(cmd, block_size, block_count, direction, desc_dma)
        {
            self.release_idmac_ring_after_quiesce();
            drop(unsafe { in_flight.complete_after_quiesce() });
            return Err(err);
        }

        let progress = DmaProgress {
            buffer: DmaRequestBuffer::Bounce {
                buffer: in_flight,
                readback: (direction == DataDirection::Read).then_some((buffer, len)),
            },
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
        let desc_dma = match self.prepare_idmac_ring(buffer.dma_addr().as_u64(), len) {
            Ok(desc_dma) => desc_dma,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        match self.start_idmac_transfer(cmd, block_size, block_count, direction, desc_dma) {
            Ok(()) => {}
            Err(err) => {
                self.release_idmac_ring_after_quiesce();
                return Err(PreparedDmaSubmitError::new(err, buffer));
            }
        }

        let progress = DmaProgress {
            buffer: DmaRequestBuffer::Owned(unsafe { buffer.into_in_flight() }),
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
}
