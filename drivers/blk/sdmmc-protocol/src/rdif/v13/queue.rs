//! Owner-only serialized request state for a combined SD/MMC domain.

use rdif_block::{
    AcceptedRequest, BlkError, CompletedRequest, CompletionSink, DmaQuiesced, HardwareQueueLimits,
    OwnedRequest, RequestFlags, RequestId, RequestOp, UnacceptedRequest,
    dma_api::{CpuDmaBuffer, DmaDirection},
};

use crate::{
    BlockPoll, BlockRequestId,
    rdif::{
        BlockConfig,
        config::{BLOCK_SIZE, BlockDataPath, block_addr_for_card, map_dev_err_to_blk_err},
        host::{BlockHost, HostRequestBuffer, OwnedBlockSubmitError},
    },
    sdio::{HostIrqSnapshot, SdioSdmmc},
};

#[derive(Clone, Copy)]
struct RequestShell {
    op: RequestOp,
    lba: u64,
    block_count: u32,
    flags: RequestFlags,
}

impl RequestShell {
    fn into_request(self, data: Option<CpuDmaBuffer>) -> OwnedRequest {
        OwnedRequest {
            op: self.op,
            lba: self.lba,
            block_count: self.block_count,
            data,
            flags: self.flags,
        }
    }
}

struct ActiveRequest {
    runtime_id: RequestId,
    host_id: Option<BlockRequestId>,
    shell: RequestShell,
    terminal_error: Option<BlkError>,
}

/// Serialized queue state borrowed only by the combined maintenance owner.
pub(super) struct SdmmcRequestQueue<H: BlockHost> {
    slot: H::Slot,
    pending: Option<H::Request>,
    active: Option<ActiveRequest>,
    shutdown: bool,
    last_reclaim_epoch: Option<rdif_block::ControllerEpoch>,
}

impl<H: BlockHost> SdmmcRequestQueue<H> {
    pub(super) fn new() -> Self {
        Self {
            slot: H::Slot::default(),
            pending: None,
            active: None,
            shutdown: false,
            last_reclaim_epoch: None,
        }
    }

    pub(super) fn submit_owned(
        &mut self,
        card: &mut SdioSdmmc<H>,
        config: &BlockConfig,
        device: rdif_block::DeviceInfo,
        limits: HardwareQueueLimits,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        if self.shutdown || self.active.is_some() || self.pending.is_some() {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        }
        if let Err(error) = rdif_block::validate_owned_request_v13(device, limits, &request) {
            return Err(UnacceptedRequest::new(id, error, request));
        }
        if let Err(error) = validate_request_buffer(config, &request) {
            return Err(UnacceptedRequest::new(id, error, request));
        }

        let OwnedRequest {
            op,
            lba,
            block_count,
            data,
            flags,
        } = request;
        let shell = RequestShell {
            op,
            lba,
            block_count,
            flags,
        };
        let Some(buffer) = data else {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                shell.into_request(None),
            ));
        };
        let host_buffer = match config.data_path() {
            BlockDataPath::Dma => HostRequestBuffer::Dma(buffer.prepare_for_device()),
            BlockDataPath::InterruptPio => HostRequestBuffer::InterruptPio(buffer),
            BlockDataPath::InitializationOnly => {
                return Err(UnacceptedRequest::new(
                    id,
                    BlkError::NotSupported,
                    shell.into_request(Some(buffer)),
                ));
            }
        };

        // Publish the driver-local request owner before the host may expose a
        // descriptor or ring a doorbell. The IRQ endpoint can now capture an
        // immediate completion without creating an ownerless request window.
        self.active = Some(ActiveRequest {
            runtime_id: id,
            host_id: None,
            shell,
            terminal_error: None,
        });
        let submitted = submit_to_host(
            card,
            op,
            lba,
            host_buffer,
            &mut self.slot,
            &mut self.pending,
        );
        match submitted {
            Ok(host_id) => {
                self.active
                    .as_mut()
                    .expect("submission published the active request before hardware visibility")
                    .host_id = Some(host_id);
                Ok(AcceptedRequest::new(id))
            }
            Err(error) => {
                self.active = None;
                let (error, host_buffer) = error.into_parts();
                Err(UnacceptedRequest::new(
                    id,
                    error,
                    shell.into_request(Some(host_buffer.into_cpu_buffer())),
                ))
            }
        }
    }

    pub(super) fn service_evidence(
        &mut self,
        card: &mut SdioSdmmc<H>,
        snapshot: HostIrqSnapshot,
        sink: &mut dyn CompletionSink,
    ) -> Result<BlockPoll, BlkError> {
        let host_id = self
            .active
            .as_ref()
            .and_then(|active| active.host_id)
            .ok_or(BlkError::InvalidRequest)?;
        match H::service_request_with_snapshot(
            card.host_mut(),
            &mut self.pending,
            host_id,
            &mut self.slot,
            snapshot,
        ) {
            Ok(BlockPoll::Pending) => Ok(BlockPoll::Pending),
            Ok(BlockPoll::Complete) => {
                self.emit_completion(Ok(()), sink);
                Ok(BlockPoll::Complete)
            }
            Err(error) => {
                let terminal = map_dev_err_to_blk_err(error);
                self.active
                    .as_mut()
                    .expect("evidence service requires one accepted request")
                    .terminal_error = Some(terminal);
                Err(terminal)
            }
        }
    }

    pub(super) fn reclaim_after_quiesce(
        &mut self,
        card: &mut SdioSdmmc<H>,
        controller_identity: usize,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if proof.controller_cookie() != controller_identity
            || self
                .last_reclaim_epoch
                .is_some_and(|epoch| proof.epoch() <= epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        if self.active.is_some() {
            H::abort_request(card.host_mut(), &mut self.pending, &mut self.slot)
                .map_err(|_| BlkError::Quarantined)?;
            let terminal = self
                .active
                .as_ref()
                .and_then(|active| active.terminal_error)
                .unwrap_or(BlkError::Cancelled);
            self.emit_completion(Err(terminal), sink);
            self.pending = None;
        }
        self.last_reclaim_epoch = Some(proof.epoch());
        Ok(())
    }

    pub(super) fn resume(&mut self) {
        self.shutdown = false;
    }

    pub(super) fn shutdown(&mut self) -> Result<(), BlkError> {
        if self.active.is_some() || self.pending.is_some() {
            return Err(BlkError::Busy);
        }
        self.shutdown = true;
        Ok(())
    }

    pub(super) const fn has_active_request(&self) -> bool {
        self.active.is_some()
    }

    fn emit_completion(&mut self, result: Result<(), BlkError>, sink: &mut dyn CompletionSink) {
        let active = self
            .active
            .take()
            .expect("completion requires one accepted SD/MMC request");
        let completed = H::take_completed_buffer(&mut self.slot);
        let (result, data) = match completed {
            Some(completed) => (result, Some(completed.into_cpu_buffer())),
            None => (Err(BlkError::Quarantined), None),
        };
        sink.complete(CompletedRequest::new(
            active.runtime_id,
            result,
            active.shell.into_request(data),
        ));
    }
}

fn submit_to_host<H: BlockHost>(
    card: &mut SdioSdmmc<H>,
    op: RequestOp,
    lba: u64,
    buffer: HostRequestBuffer,
    slot: &mut H::Slot,
    pending: &mut Option<H::Request>,
) -> Result<BlockRequestId, OwnedBlockSubmitError> {
    let start_block = match block_addr_for_card(lba, card.is_high_capacity()) {
        Ok(start_block) => start_block,
        Err(error) => return Err(OwnedBlockSubmitError::new(error, buffer)),
    };
    match op {
        RequestOp::Read => {
            H::submit_owned_read_request(card.host_mut(), start_block, buffer, slot, pending)
        }
        RequestOp::Write => {
            H::submit_owned_write_request(card.host_mut(), start_block, buffer, slot, pending)
        }
        RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => {
            Err(OwnedBlockSubmitError::new(BlkError::NotSupported, buffer))
        }
    }
}

fn validate_request_buffer(config: &BlockConfig, request: &OwnedRequest) -> Result<(), BlkError> {
    let buffer = request.data.as_ref().ok_or(BlkError::InvalidRequest)?;
    let direction_matches = match request.op {
        RequestOp::Read => matches!(
            buffer.direction(),
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Write => matches!(
            buffer.direction(),
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => false,
    };
    if !direction_matches || buffer.domain_id() != config.dma_domain {
        return Err(BlkError::InvalidRequest);
    }
    if config.uses_dma() {
        let start = buffer.dma_addr().as_u64();
        let len = u64::try_from(buffer.len().get()).map_err(|_| BlkError::InvalidRequest)?;
        let end = start.checked_add(len - 1).ok_or(BlkError::InvalidRequest)?;
        if end > config.dma_mask || !start.is_multiple_of(BLOCK_SIZE as u64) {
            return Err(BlkError::InvalidRequest);
        }
    }
    Ok(())
}
