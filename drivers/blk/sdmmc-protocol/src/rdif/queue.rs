use alloc::sync::Arc;

use log::warn;
use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, IQueue, IdList, OwnedRequest, QueueEventBatch,
    QueueExecution, QueueInfo, QueueKind, RequestFlags, RequestId, RequestOp, ServiceProgress,
    SubmitError, SubmitOutcome,
    dma_api::{CpuDmaBuffer, DmaDirection},
};

use crate::{
    BlockPoll, BlockRequestId,
    rdif::{
        config::{
            BLOCK_SIZE, BlockDataPath, block_addr_for_card, device_info, map_dev_err_to_blk_err,
            queue_limits,
        },
        device::BlockControl,
        host::{BlockHost, HostRequestBuffer, OwnedBlockSubmitError},
    },
    sdio::host::SDMMC_BLOCK_QUEUE_ID,
};

const SDMMC_COMPLETION_SOURCE_ID: usize = 0;

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

struct ActiveOwnedRequest {
    runtime_id: RequestId,
    host_id: BlockRequestId,
    shell: RequestShell,
    terminal_error: Option<BlkError>,
}

/// One serialized, interrupt-driven SD/MMC request queue.
///
/// The runtime owns request IDs. The host owns a separate controller-local ID
/// and the selected DMA or PIO backing until terminal completion or proven
/// controller quiescence.
pub struct BlockQueue<H>
where
    H: BlockHost,
{
    pub(super) control: Arc<BlockControl<H>>,
    pub(super) id: usize,
    pub(super) slot: H::Slot,
    pub(super) pending: Option<H::Request>,
    active: Option<ActiveOwnedRequest>,
    shutdown: bool,
}

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    pub(super) fn new(control: Arc<BlockControl<H>>, id: usize) -> Self {
        debug_assert_eq!(id, SDMMC_BLOCK_QUEUE_ID);
        Self {
            control,
            id,
            slot: H::Slot::default(),
            pending: None,
            active: None,
            shutdown: false,
        }
    }

    pub(super) fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: device_info(&self.control.config),
            limits: queue_limits(&self.control.config, self.control.config.dma_mask),
            kind: QueueKind::Interrupt {
                sources: IdList::from_bits(1 << SDMMC_COMPLETION_SOURCE_ID),
            },
            execution: QueueExecution::Serialized,
        }
    }

    fn validate_request_buffer(&self, request: &OwnedRequest) -> Result<(), BlkError> {
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
        if !direction_matches || buffer.domain_id() != self.control.config.dma_domain {
            return Err(BlkError::InvalidRequest);
        }

        if self.control.config.uses_dma() {
            let start = buffer.dma_addr().as_u64();
            let len = u64::try_from(buffer.len().get()).map_err(|_| BlkError::InvalidRequest)?;
            let end = start.checked_add(len - 1).ok_or(BlkError::InvalidRequest)?;
            if end > self.control.config.dma_mask || !start.is_multiple_of(BLOCK_SIZE as u64) {
                return Err(BlkError::InvalidRequest);
            }
        }
        Ok(())
    }

    fn submit_owned_inner(
        &mut self,
        runtime_id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if self.shutdown {
            return Err(SubmitError::new(runtime_id, BlkError::Offline, request));
        }
        if !self
            .control
            .irq_enabled
            .load(core::sync::atomic::Ordering::Acquire)
        {
            return Err(SubmitError::new(runtime_id, BlkError::Offline, request));
        }
        if self.active.is_some() || self.pending.is_some() {
            return Err(SubmitError::new(runtime_id, BlkError::Retry, request));
        }
        if !self.control.config.supports_runtime_queue() {
            return Err(SubmitError::new(
                runtime_id,
                BlkError::NotSupported,
                request,
            ));
        }
        if let Err(error) = rdif_block::validate_owned_request(self.queue_info(), &request) {
            return Err(SubmitError::new(runtime_id, error, request));
        }
        if let Err(error) = self.validate_request_buffer(&request) {
            return Err(SubmitError::new(runtime_id, error, request));
        }

        // Acquire before destructuring the request so contention returns the
        // exact CPU/DMA ownership without preparing or submitting it.
        let raw = self.control.raw.clone();
        let mut raw = match raw.try_borrow_mut() {
            Ok(raw) => raw,
            Err(_) => return Err(SubmitError::new(runtime_id, BlkError::Retry, request)),
        };

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
            return Err(SubmitError::new(
                runtime_id,
                BlkError::InvalidRequest,
                shell.into_request(None),
            ));
        };
        let host_buffer = match self.control.config.data_path() {
            BlockDataPath::Dma => HostRequestBuffer::Dma(buffer.prepare_for_device()),
            BlockDataPath::InterruptPio => HostRequestBuffer::InterruptPio(buffer),
            BlockDataPath::InitializationOnly => {
                return Err(SubmitError::new(
                    runtime_id,
                    BlkError::NotSupported,
                    shell.into_request(Some(buffer)),
                ));
            }
        };
        let submitted = match block_addr_for_card(lba, raw.is_high_capacity()) {
            Ok(start_block) => match op {
                RequestOp::Read => H::submit_owned_read_request(
                    raw.host_mut(),
                    start_block,
                    host_buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                RequestOp::Write => H::submit_owned_write_request(
                    raw.host_mut(),
                    start_block,
                    host_buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => Err(
                    OwnedBlockSubmitError::new(BlkError::NotSupported, host_buffer),
                ),
            },
            Err(error) => Err(OwnedBlockSubmitError::new(error, host_buffer)),
        };

        match submitted {
            Ok(host_id) => {
                debug_assert!(self.pending.is_some());
                self.active = Some(ActiveOwnedRequest {
                    runtime_id,
                    host_id,
                    shell,
                    terminal_error: None,
                });
                Ok(SubmitOutcome::Queued)
            }
            Err(error) => {
                let (error, host_buffer) = error.into_parts();
                Err(SubmitError::new(
                    runtime_id,
                    error,
                    shell.into_request(Some(host_buffer.into_cpu_buffer())),
                ))
            }
        }
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

    fn service_active(
        &mut self,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        let host_id = self
            .active
            .as_ref()
            .map(|active| active.host_id)
            .ok_or(BlkError::InvalidRequest)?;
        let raw = self.control.raw.clone();
        let mut raw = match raw.try_borrow_mut() {
            Ok(raw) => raw,
            // Queue and lifecycle entry points belong to one maintenance
            // owner. Contention therefore indicates re-entrancy or a broken
            // ownership boundary and must enter recovery instead of turning
            // into an unbounded retry path.
            Err(_) => return Err(BlkError::Busy),
        };
        let serviced =
            H::service_request(raw.host_mut(), &mut self.pending, host_id, &mut self.slot);

        match serviced {
            Ok(BlockPoll::Pending) => Ok(ServiceProgress::Idle),
            Ok(BlockPoll::Complete) => {
                self.emit_completion(Ok(()), sink);
                Ok(ServiceProgress::Idle)
            }
            Err(error) => {
                let terminal = map_dev_err_to_blk_err(error);
                self.active
                    .as_mut()
                    .expect("event service requires an active request")
                    .terminal_error = Some(terminal);
                // Hardware errors close the hctx and enter controller-wide
                // recovery. Returning the DMA buffer here would require a
                // synchronous reset/abort and would fabricate quiescence.
                Err(terminal)
            }
        }
    }

    fn event_targets_active(&self, events: &QueueEventBatch<'_>) -> bool {
        self.active.is_some()
            && events.queue_id() == self.id
            && events.affected_queues().contains(self.id)
    }
}

impl<H> IQueue for BlockQueue<H>
where
    H: BlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        self.submit_owned_inner(id, request)
    }

    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if self.shutdown
            || !self
                .control
                .irq_enabled
                .load(core::sync::atomic::Ordering::Acquire)
        {
            return Err(BlkError::Offline);
        }
        if events.queue_id() != self.id {
            return Err(BlkError::InvalidRequest);
        }
        if !self.event_targets_active(events) {
            return Ok(ServiceProgress::Idle);
        }
        self.service_active(sink)
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if proof.controller_cookie() != self.control.controller_cookie() {
            return Err(BlkError::InvalidDmaProof);
        }
        if self.active.is_none() {
            return Ok(());
        }
        let raw = self.control.raw.clone();
        // Quiescence is already proven, but another bounded controller step
        // may still own the software core. Preserve request ownership and let
        // the lifecycle worker retry instead of spinning here.
        let mut raw = raw.try_borrow_mut().map_err(|_| BlkError::Busy)?;
        if let Err(error) = H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot) {
            warn!("sdmmc rdif: proof-gated request reclaim failed: {error:?}");
            return Err(BlkError::Quarantined);
        }
        drop(raw);
        let terminal = self
            .active
            .as_ref()
            .and_then(|active| active.terminal_error)
            .unwrap_or(BlkError::Cancelled);
        self.emit_completion(Err(terminal), sink);
        self.pending = None;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        if self.shutdown {
            return Ok(());
        }
        if self.active.is_some() || self.pending.is_some() {
            return Err(BlkError::Busy);
        }
        self.shutdown = true;
        self.control.release_queue();
        Ok(())
    }
}
