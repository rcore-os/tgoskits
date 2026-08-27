use super::*;

impl PhytiumMci {
    /// Advance a DMA request only for an explicit maintenance-thread cause.
    ///
    /// Register retries may issue a command, but only an acknowledged IRQ may
    /// consume command or data completion.
    pub fn advance_block_request_response(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<DataCommandProgress, Error> {
        let acknowledged_irq = cause == sdmmc_host::ProgressCause::AcknowledgedIrq;
        loop {
            let Some(active) = request.as_ref() else {
                return Err(Error::InvalidArgument);
            };
            if active.id() != id {
                return Err(Error::InvalidArgument);
            }
            let (cmd_index, phase, stage) = match &active.inner {
                BlockRequestKind::DmaRead {
                    cmd_index,
                    phase,
                    stage,
                    ..
                }
                | BlockRequestKind::DmaWrite {
                    cmd_index,
                    phase,
                    stage,
                    ..
                } => (*cmd_index, *phase, *stage),
            };

            match stage {
                BlockRequestStage::Command => {
                    if !acknowledged_irq && !self.command_needs_register_retry() {
                        return Ok(DataCommandProgress::Pending);
                    }
                    match self.advance_command_for_cause(acknowledged_irq) {
                        Ok(CommandProgress::Pending) => return Ok(DataCommandProgress::Pending),
                        Ok(CommandProgress::Complete) if acknowledged_irq => {
                            let response = self.take_command_response()?;
                            store_response(request, response)?;
                            set_stage(request, BlockRequestStage::Data)?;
                        }
                        Ok(CommandProgress::Complete) => {
                            return Ok(DataCommandProgress::Pending);
                        }
                        Err(err) => {
                            let _ = self.abort_block_request(request, id, slot, phase);
                            return Err(err);
                        }
                    }
                }
                BlockRequestStage::Data if !acknowledged_irq => {
                    return Ok(DataCommandProgress::Pending);
                }
                BlockRequestStage::Data => {
                    match self.consume_dma_completion(request, cmd_index, phase) {
                        Ok(BlockProgress::Pending) => return Ok(DataCommandProgress::Pending),
                        Ok(BlockProgress::Complete) => {
                            match self.finish_dma_data(request, id, slot)? {
                                DataCommandProgress::Pending => {}
                                complete => return Ok(complete),
                            }
                        }
                        Err(err) => {
                            let _ = self.abort_block_request(request, id, slot, phase);
                            return Err(err);
                        }
                    }
                }
                BlockRequestStage::Stop => {
                    return self.advance_block_stop(request, id, slot, phase, acknowledged_irq);
                }
            }
        }
    }

    pub(super) fn consume_dma_completion(
        &mut self,
        request: &mut Option<BlockRequest>,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<BlockProgress, Error> {
        let raw_idsts = self.irq.state.take_idmac_status(IDSTS_ERROR_MASK);
        let ints = self.take_latched_data_irq_status(cmd_index, phase)?;
        let Some(active) = request.as_mut() else {
            return Err(Error::InvalidArgument);
        };
        let progress = match &mut active.inner {
            BlockRequestKind::DmaRead { progress, .. } => progress,
            BlockRequestKind::DmaWrite { progress, .. } => progress,
        };

        if raw_idsts & IDSTS_ERROR_MASK != 0 {
            warn!(
                "phytium-mci IDMAC error cmd={} idsts={:#010x} rintsts={:#010x} status={:#010x} \
                 cur_desc={:#010x}_{:08x} cur_buf={:#010x}_{:08x}",
                cmd_index,
                raw_idsts,
                self.regs.rintsts().read().into_bits(),
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
        progress.data_done |= ints.data_transfer_over();
        if !progress.is_done() {
            return Ok(BlockProgress::Pending);
        }
        Ok(BlockProgress::Complete)
    }

    fn finish_dma_data(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
    ) -> Result<DataCommandProgress, Error> {
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
                    return Ok(DataCommandProgress::Pending);
                }
                *stage = BlockRequestStage::Stop;
                *stop_after_complete
            }
            _ => return Err(Error::InvalidArgument),
        };
        self.disable_idmac();
        self.release_idmac_ring_after_quiesce();
        if stop_after_complete {
            // CMD12 is part of the same multi-block transaction. Keeping the
            // IRQ generation stable preserves any late data error and any
            // fast stop-command completion latched during the transition.
            self.submit_chained_command(&CMD12)?;
            return Ok(DataCommandProgress::Pending);
        }

        let active = request.take().ok_or(Error::InvalidArgument)?;
        let response = active.response().ok_or(Error::InvalidArgument)?;
        let completed_dma = self.finish_block_request(active);
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
                if !request
                    .as_ref()
                    .is_some_and(|active| active.dma_progress_done())
                {
                    return Ok(DataCommandProgress::Pending);
                }
                let active = request.take().ok_or(Error::InvalidArgument)?;
                let response = active.response().ok_or(Error::InvalidArgument)?;
                let completed_dma = self.finish_block_request(active);
                slot.complete_with_dma(id, completed_dma)?;
                Ok(DataCommandProgress::Complete(response))
            }
            Err(err) => {
                let _ = self.abort_block_request(request, id, slot, phase);
                Err(err)
            }
        }
    }

    fn finish_block_request(&mut self, request: BlockRequest) -> Option<CompletedDma> {
        self.finish_block_request_with_quiesce(request, true)
    }

    fn finish_block_request_with_quiesce(
        &mut self,
        request: BlockRequest,
        quiesced: bool,
    ) -> Option<CompletedDma> {
        if !quiesced {
            self.poison_dma();
        }
        let completed_dma = match request.inner {
            BlockRequestKind::DmaRead { progress, .. } => {
                if quiesced {
                    progress.complete(true)
                } else {
                    progress.abort(true, false)
                }
            }
            BlockRequestKind::DmaWrite { progress, .. } => {
                if quiesced {
                    progress.complete(false)
                } else {
                    progress.abort(false, false)
                }
            }
        };
        if quiesced {
            self.release_idmac_ring_after_quiesce();
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.irq.state.end_request();
        completed_dma
    }

    pub(super) fn abort_block_request(
        &mut self,
        request: &mut Option<BlockRequest>,
        id: RequestId,
        slot: &mut BlockRequestSlot,
        phase: Phase,
    ) -> Result<(), Error> {
        let (cmd_index, stage) = request.as_ref().map_or((0, None), |request| {
            (request.cmd_index(), Some(request.stage()))
        });
        self.warn_idmac_snapshot(cmd_index, stage, phase);
        let active = request.take().ok_or(Error::InvalidArgument)?;
        self.disable_idmac();
        let fifo = self.reset_fifo(phase);
        let dma = self.reset_dma(phase);
        self.clear_all_int_status();
        self.command_state = crate::command::CommandState::Idle;
        let recovery = match (fifo, dma) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), _) | (_, Err(err)) => {
                let reset = self.reset_and_init_preserving_irq();
                self.disable_idmac();
                match reset {
                    Ok(()) => {
                        warn!(
                            "phytium-mci: recovered IDMAC {:?} error by controller reset: {err:?}",
                            phase
                        );
                        Ok(())
                    }
                    Err(reset_err) => Err(reset_err),
                }
            }
        };
        let completed_dma = self.finish_block_request_with_quiesce(active, recovery.is_ok());
        drop(completed_dma);
        slot.complete(id)?;
        recovery
    }

    fn warn_idmac_snapshot(&self, cmd_index: u8, stage: Option<BlockRequestStage>, phase: Phase) {
        let (latched_rintsts, latched_idsts) = self.irq.state.diagnostic_status();
        warn!(
            "phytium-mci abort {:?} stage={:?} cmd={} rintsts={:#010x} latched_rintsts={:#010x} \
             idsts={:#010x} latched_idsts={:#010x} idinten={:#010x} status={:#010x} ctrl={:#010x} \
             bmod={:#010x} dbaddr={:#010x}_{:08x} pldmnd={:#010x} cur_desc={:#010x}_{:08x} \
             cur_buf={:#010x}_{:08x} cmdreg={:#010x}",
            phase,
            stage,
            cmd_index,
            self.regs.rintsts().read().into_bits(),
            latched_rintsts,
            self.regs.idsts().read(),
            latched_idsts,
            self.regs.idinten().read(),
            self.regs.status().read().into_bits(),
            self.regs.ctrl().read().into_bits(),
            self.regs.bmod().read(),
            self.regs.dbaddrh().read(),
            self.regs.dbaddrl().read(),
            self.regs.pldmnd().read(),
            self.regs.dscaddrh().read(),
            self.regs.dscaddrl().read(),
            self.regs.bufaddrh().read(),
            self.regs.bufaddrl().read(),
            self.regs.cmd().read().into_bits(),
        );
        if let Some(snapshot) = self
            .idmac_ring
            .as_ref()
            .and_then(IdmacRing::diagnostic_first_descriptor)
        {
            warn!(
                "phytium-mci first IDMAC descriptor attr={:#010x} len={} addr={:#010x}_{:08x} \
                 next={:#010x}_{:08x}",
                snapshot.attribute,
                snapshot.len,
                snapshot.addr_hi,
                snapshot.addr_lo,
                snapshot.desc_hi,
                snapshot.desc_lo,
            );
        }
    }

    pub(super) fn start_idmac_transfer(
        &mut self,
        cmd: &Command,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
        desc_dma: u64,
    ) -> Result<(), Error> {
        self.regs.idinten().write(0);
        self.clear_all_int_status();
        self.regs.idsts().write(u32::MAX);
        self.irq.state.begin_request();
        self.program_data_phase(block_size, block_count);
        // Coherent descriptor memory still requires a device-visible ordering
        // barrier before the controller can fetch descriptors.
        wmb();
        self.regs.idinten().write(IDSTS_INT_ENABLE_MASK);
        self.program_idmac_registers(desc_dma);
        // Publish the descriptor base and IDMAC selection before ringing the
        // demand register or issuing the data command.
        wmb();
        self.kick_idmac();
        self.pending_data = Some(PendingData {
            direction,
            block_size,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        self.submit_command(cmd)
    }

    pub(super) fn program_idmac_registers(&self, desc_dma: u64) {
        self.regs.dbaddrl().write(desc_dma as u32);
        self.regs.dbaddrh().write((desc_dma >> 32) as u32);
        self.regs.ctrl().update(|r| {
            // CTRL.DMA_ENABLE selects the external DMA handshake on Phytium.
            // IDMAC uses only CTRL.USE_INTERNAL_DMAC.
            r.with_dma_enable(false)
                .with_use_internal_dmac(true)
                .with_int_enable(self.completion_irq_enabled())
        });
        self.regs
            .bmod()
            .write(self.regs.bmod().read() | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE);
    }

    pub(super) fn kick_idmac(&self) {
        self.regs.pldmnd().write(1);
    }

    fn disable_idmac(&mut self) {
        self.regs.idinten().write(0);
        self.regs.bmod().write(0);
        self.regs
            .ctrl()
            .update(|r| r.with_dma_enable(false).with_use_internal_dmac(false));
    }

    pub(super) fn prepare_idmac_ring(&mut self, buffer_dma: u64, len: usize) -> Result<u64, Error> {
        self.idmac_ring
            .as_mut()
            .ok_or(Error::UnsupportedCommand)?
            .prepare(buffer_dma, len)
    }

    pub(super) fn release_idmac_ring_after_quiesce(&mut self) {
        if let Some(ring) = self.idmac_ring.as_mut() {
            ring.release_after_quiesce();
        }
    }

    fn take_latched_data_irq_status(
        &mut self,
        cmd_index: u8,
        phase: Phase,
    ) -> Result<RIntSts, Error> {
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
