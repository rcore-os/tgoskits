//! Request admission and hardware submission for FIFO and ADMA2 transfers.

use super::*;

impl Sdhci {
    /// Submit one block read using the requested transfer engine.
    ///
    /// Both `BlockTransferMode::Dma` and `BlockTransferMode::Fifo` use the
    /// same submit/poll queue contract. Runtimes that cannot use DMA can
    /// submit FIFO requests without changing the external block queue shape.
    ///
    /// A raw buffer cannot be admitted through safe code because the returned
    /// request may move to another queue worker before completion.
    ///
    /// ```compile_fail
    /// use core::{num::NonZeroUsize, ptr::NonNull};
    /// use sdhci_host::{BlockRequestSlot, BlockTransferMode, Sdhci};
    ///
    /// fn submit_borrowed_buffer(
    ///     host: &mut Sdhci,
    ///     buffer: NonNull<u8>,
    ///     size: NonZeroUsize,
    ///     slot: &mut BlockRequestSlot,
    /// ) {
    ///     let _ = host.submit_read_blocks(
    ///         0,
    ///         buffer,
    ///         size,
    ///         None,
    ///         BlockTransferMode::Fifo,
    ///         slot,
    ///     );
    /// }
    /// ```
    /// # Safety
    ///
    /// `buffer..buffer + size` must remain valid and exclusively writable by
    /// this request until it reaches terminal completion or is reclaimed
    /// after controller quiescence. The allocation must remain valid if the
    /// returned request is moved to another worker thread.
    pub unsafe fn submit_read_blocks(
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
    /// See [`Sdhci::submit_read_blocks`] for the completion contract.
    /// # Safety
    ///
    /// `buffer..buffer + size` must remain valid and immutable until this
    /// request reaches terminal completion or is reclaimed after controller
    /// quiescence. The allocation must remain valid if the returned request
    /// is moved to another worker thread.
    pub unsafe fn submit_write_blocks(
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

    /// Poll a previously submitted block request.
    pub fn service_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockPoll, Error> {
        match self.service_block_request_response(request, id, slot)? {
            DataCommandPoll::Pending => Ok(BlockPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(BlockPoll::Complete),
            // Future DataCommandPoll variants are treated as completion.
            _ => Ok(BlockPoll::Complete),
        }
    }

    pub fn service_block_request_response(
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
                Err(err) => return Err(err),
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
            Err(err) => Err(err),
        }
    }

    pub fn abort_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<(), Error> {
        self.abort_block_request(request, id, slot)
    }

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<BlockRequest, Error> {
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let block_count = dma_read_block_count(size)?;
        let backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::FromDevice)
            .map_err(map_dma_error)?;
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            dma_addr,
            &mut desc,
            DataDirection::Read,
            Phase::DataRead,
        )?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: Some((buffer, size.get())),
                },
                descriptors: InFlightAdmaDescriptors::new(desc),
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
        if !self.supports_adma2() {
            return Err(Error::UnsupportedCommand);
        }
        let block_count = dma_write_block_count(size)?;
        let mut backing = CpuDmaBuffer::new_zero(dma, size, BLOCK_SIZE, DmaDirection::ToDevice)
            .map_err(map_dma_error)?;
        backing.copy_to_device_from_slice(unsafe {
            core::slice::from_raw_parts(buffer.as_ptr(), size.get())
        });
        let dma_addr = backing.dma_addr().as_u64();
        let in_flight = unsafe { backing.prepare_for_device().into_in_flight() };
        let mut desc = dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
            .map_err(map_dma_error)?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            dma_addr,
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        )?;
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Bounce {
                    buffer: in_flight,
                    readback: None,
                },
                descriptors: InFlightAdmaDescriptors::new(desc),
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
        if !self.supports_adma2() {
            return Err(PreparedDmaSubmitError::new(
                Error::UnsupportedCommand,
                buffer,
            ));
        }
        if buffer.direction() != DmaDirection::FromDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_read_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => return Err(PreparedDmaSubmitError::new(map_dma_error(err), buffer)),
        };
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        match self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Read,
            Phase::DataRead,
        ) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Read {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
                descriptors: InFlightAdmaDescriptors::new(desc),
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
        if !self.supports_adma2() {
            return Err(PreparedDmaSubmitError::new(
                Error::UnsupportedCommand,
                buffer,
            ));
        }
        if buffer.direction() != DmaDirection::ToDevice || buffer.domain_id() != dma.domain_id() {
            return Err(PreparedDmaSubmitError::new(Error::InvalidArgument, buffer));
        }
        let block_count = match dma_write_block_count(buffer.len()) {
            Ok(block_count) => block_count,
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        };
        let mut desc = match dma
            .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        {
            Ok(desc) => desc,
            Err(err) => return Err(PreparedDmaSubmitError::new(map_dma_error(err), buffer)),
        };
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        match self.submit_adma2_blocks_mapped(
            &cmd,
            block_count,
            buffer.dma_addr().as_u64(),
            &mut desc,
            DataDirection::Write,
            Phase::DataWrite,
        ) {
            Ok(()) => {}
            Err(err) => return Err(PreparedDmaSubmitError::new(err, buffer)),
        }
        let buffer = unsafe { buffer.into_in_flight() };
        Ok(BlockRequest {
            inner: BlockRequestKind::Write {
                id,
                buffer: DmaRequestBuffer::Owned(buffer),
                descriptors: InFlightAdmaDescriptors::new(desc),
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
    /// # Safety
    ///
    /// `buffer..buffer + len` must remain valid for the requested direction
    /// and inaccessible through conflicting references until terminal
    /// completion or proof-gated reclamation. The allocation must remain
    /// valid if the returned request crosses queue-worker threads.
    pub unsafe fn submit_fifo_data_request(
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

    /// Submit an IRQ-driven FIFO request while retaining CPU-buffer ownership.
    ///
    /// The returned request owns `buffer`; neither a submit failure nor a
    /// terminal completion can lose or substitute the allocation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_owned_fifo_data_request(
        &mut self,
        cmd: &Command,
        buffer: CpuDmaBuffer,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        slot: &mut BlockRequestSlot,
    ) -> Result<BlockRequest, CpuBufferSubmitError> {
        let direction_matches = matches!(
            (buffer.direction(), direction),
            (DmaDirection::FromDevice, DataDirection::Read)
                | (DmaDirection::ToDevice, DataDirection::Write)
                | (
                    DmaDirection::Bidirectional,
                    DataDirection::Read | DataDirection::Write
                )
        );
        if !direction_matches {
            return Err(CpuBufferSubmitError::new(Error::InvalidArgument, buffer));
        }
        let buffer_ptr = buffer.cpu_ptr();
        let buffer_len = buffer.len().get();
        // SAFETY: `buffer` owns this stable allocation. On success it is
        // moved into the returned request before the caller can service that
        // request; on failure it is returned unchanged to the caller.
        match unsafe {
            self.submit_fifo_data_request(
                cmd,
                buffer_ptr,
                buffer_len,
                block_size,
                block_count,
                direction,
                slot,
            )
        } {
            Ok(mut request) => {
                // `CpuDmaBuffer` keeps its heap allocation at a stable address
                // while the request moves between queues. Installing it only
                // after successful hardware admission preserves ownership on
                // every submit-side error.
                request.retain_fifo_cpu_buffer(buffer);
                Ok(request)
            }
            Err(error) => Err(CpuBufferSubmitError::new(error, buffer)),
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
        self.ensure_command_admissible(cmd, true)?;
        self.pending_data = Some(PendingData {
            direction,
            block_size,
            block_count,
            adma_descriptor: None,
        });
        self.use_dma = false;
        if let Err(error) = self.submit_command(cmd) {
            self.pending_data = None;
            return Err(error);
        }
        let inner = match direction {
            DataDirection::Read => BlockRequestKind::FifoRead {
                id,
                buffer,
                owned_cpu: None,
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
                owned_cpu: None,
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

    pub(super) fn submit_adma2_blocks_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut CoherentArray<Adma2Desc32>,
        direction: DataDirection,
        phase: Phase,
    ) -> Result<(), Error> {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let byte_count = block_count
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(Error::InvalidArgument)? as usize;
        build_descriptors_into_dma(desc, buffer_dma, byte_count, phase)?;

        let desc_bus = desc.dma_addr().as_u64();
        let desc_end = desc_bus
            .checked_add(desc.bytes_len() as u64)
            .ok_or(Error::InvalidArgument)?;
        if desc_end > u32::MAX as u64 + 1 {
            return Err(Error::BadResponse(ErrorContext::new(phase)));
        }
        self.ensure_command_admissible(cmd, true)?;

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
            adma_descriptor: Some(desc_bus as u32),
        });
        self.use_dma = true;
        let response = self.submit_command(cmd);
        self.use_dma = false;
        if response.is_err() {
            self.pending_data = None;
        }
        response
    }
}
