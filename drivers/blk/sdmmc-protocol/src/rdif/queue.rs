use alloc::sync::Arc;
use core::time::Duration;

use log::{info, warn};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, CompletedRequest, CompletionSink,
    HardwareQueue, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestId, RequestOp,
    SubmissionSink, SubmitError, validate_owned_request,
};
use sdio_host2::ProgressCause;

use crate::{
    BlockProgress, BlockRequestId, OperationProgress,
    rdif::{
        config::{block_addr_for_card, device_info, map_dev_err_to_blk_err, queue_limits},
        device::BlockInitStatus,
        host::{
            ProtocolBlockRequest, ProtocolBlockSlot, abort_request, advance_block_request,
            submit_owned_read_request, submit_owned_write_request, take_completed_dma,
        },
    },
    response::CardState,
    sdio::{
        card::{CardKind, SdioSdmmc, SdioStatusRequest},
        host::{HostProgressWait, SdioIrqHost},
        init::{CardInitPreference, MmcSwitchRequest, SdioInitWait},
    },
};

const MMC_SWITCH_WRITE_BYTE: u8 = 0b11;
const MMC_FLUSH_CACHE_TRIGGER: u8 = 1;
const INIT_REGISTER_RETRY_DELAY: Duration = Duration::from_micros(100);
const INIT_POWER_UP_RETRY_DELAY: Duration = Duration::from_millis(10);

enum FlushRequest {
    Cache(MmcSwitchRequest),
    Status(SdioStatusRequest),
}

/// Queue state exclusively owned by one block runtime maintenance task.
pub struct BlockQueue<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    card: SdioSdmmc<H>,
    config: super::config::BlockConfig,
    id: usize,
    slot: ProtocolBlockSlot,
    pending: Option<ProtocolBlockRequest<H>>,
    pending_id: Option<RequestId>,
    flush: Option<(RequestId, FlushRequest)>,
    next_flush_id: usize,
    completion_irq_enabled: bool,
    init_request: Option<crate::sdio::init::SdioInitRequest<H>>,
    init_status: Option<Arc<BlockInitStatus>>,
    register_retry_after: Option<Duration>,
    supports_flush: bool,
    cache_enabled: bool,
}

impl<H> BlockQueue<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    pub(super) fn new(card: SdioSdmmc<H>, config: super::config::BlockConfig, id: usize) -> Self {
        let supports_flush = queue_supports_flush(card.kind(), None);
        Self {
            card,
            config,
            id,
            slot: ProtocolBlockSlot::default(),
            pending: None,
            pending_id: None,
            flush: None,
            next_flush_id: usize::MAX / 2,
            completion_irq_enabled: false,
            init_request: None,
            init_status: None,
            register_retry_after: None,
            supports_flush,
            cache_enabled: false,
        }
    }

    pub(super) fn new_initializing(
        card: SdioSdmmc<H>,
        config: super::config::BlockConfig,
        id: usize,
        preference: CardInitPreference,
        init_status: Arc<BlockInitStatus>,
    ) -> Result<Self, BlkError> {
        let mut queue = Self::new(card, config, id);
        queue.supports_flush = queue_supports_flush(queue.card.kind(), Some(preference));
        queue.init_status = Some(init_status);
        queue.ensure_completion_irq()?;
        queue.init_request = Some(
            queue
                .card
                .submit_init_with_preference(preference)
                .map_err(map_dev_err_to_blk_err)?,
        );
        queue.advance_initialization(ProgressCause::Submitted)?;
        Ok(queue)
    }

    fn queue_info(&self) -> QueueInfo {
        let mut limits = queue_limits(&self.config);
        limits.supports_flush = self.supports_flush;
        QueueInfo {
            id: self.id,
            device: device_info(&self.config),
            limits,
        }
    }

    fn ensure_completion_irq(&mut self) -> Result<(), BlkError> {
        if self.completion_irq_enabled && self.card.host().completion_irq_enabled() {
            return Ok(());
        }
        SdioIrqHost::enable_completion_irq(self.card.host_mut()).map_err(map_dev_err_to_blk_err)?;
        if !self.card.host().completion_irq_enabled() {
            return Err(BlkError::NotSupported);
        }
        self.completion_irq_enabled = true;
        Ok(())
    }

    /// Advances exactly one initialization state transition.
    ///
    /// Register-only continuation is returned to the runtime as a timer
    /// request. Command and data continuation remains dormant until
    /// `drain_completions` consumes an acknowledged IRQ.
    fn advance_initialization(&mut self, cause: ProgressCause) -> Result<bool, BlkError> {
        let Some(mut request) = self.init_request.take() else {
            return Ok(true);
        };
        match self.card.advance_init_request(&mut request, cause) {
            Ok(OperationProgress::Pending) => {
                self.register_retry_after = if request.take_needs_pace() {
                    Some(INIT_POWER_UP_RETRY_DELAY)
                } else if let Some(retry_after) = self.card.init_register_retry_after(&request) {
                    Some(retry_after)
                } else if self.card.init_wait_kind(&request) == SdioInitWait::Register {
                    Some(INIT_REGISTER_RETRY_DELAY)
                } else {
                    None
                };
                self.init_request = Some(request);
                Ok(false)
            }
            Ok(OperationProgress::Complete(info)) => {
                let Some(capacity_blocks) = info.capacity_blocks.filter(|capacity| *capacity != 0)
                else {
                    if let Some(status) = &self.init_status {
                        status.mark_failed();
                    }
                    return Err(BlkError::Io);
                };
                self.register_retry_after = None;
                self.config.set_capacity_blocks(capacity_blocks);
                self.cache_enabled = info
                    .ext_csd
                    .as_ref()
                    .is_some_and(crate::ext_csd::ExtCsd::cache_enabled);
                if let Some(status) = &self.init_status {
                    status.mark_ready(capacity_blocks);
                }
                info!(
                    "sdmmc block init complete: kind={:?} high_capacity={} rca={} \
                     capacity_blocks={} cache_enabled={}",
                    info.kind, info.high_capacity, info.rca, capacity_blocks, self.cache_enabled
                );
                Ok(true)
            }
            Err(error) => {
                self.register_retry_after = None;
                if let Some(status) = &self.init_status {
                    status.mark_failed();
                }
                Err(map_dev_err_to_blk_err(error))
            }
        }
    }

    fn submit_data(&mut self, mut request: OwnedRequest) -> Result<RequestId, SubmitError> {
        let op = request.op;
        let lba = request.lba;
        let block_count = request.block_count;
        let flags = request.flags;
        let Some(buffer) = request.data.take() else {
            return Err(SubmitError::new(BlkError::InvalidRequest, request));
        };
        let start_block = match block_addr_for_card(lba, self.card.is_high_capacity()) {
            Ok(start_block) => start_block,
            Err(error) => {
                request.data = Some(buffer);
                return Err(SubmitError::new(error, request));
            }
        };
        let submit = match op {
            RequestOp::Read => submit_owned_read_request(
                self.card.protocol_host_mut(),
                start_block,
                buffer,
                &mut self.slot,
                &mut self.pending,
            ),
            RequestOp::Write => submit_owned_write_request(
                self.card.protocol_host_mut(),
                start_block,
                buffer,
                &mut self.slot,
                &mut self.pending,
            ),
            _ => unreachable!(),
        };
        match submit {
            Ok(id) => {
                let id = RequestId::new(usize::from(id));
                self.pending_id = Some(id);
                self.sync_protocol_register_retry();
                Ok(id)
            }
            Err(error) => {
                let (error, buffer) = error.into_parts();
                Err(SubmitError::new(
                    error,
                    OwnedRequest {
                        op,
                        lba,
                        block_count,
                        data: Some(buffer),
                        flags,
                    },
                ))
            }
        }
    }

    fn submit_flush(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        let id = RequestId::new(self.next_flush_id);
        self.next_flush_id = self.next_flush_id.wrapping_add(1);
        let flush = if self.card.kind() == CardKind::Mmc && self.cache_enabled {
            self.card
                .submit_mmc_switch(
                    MMC_SWITCH_WRITE_BYTE,
                    crate::cmd::ext_csd::FLUSH_CACHE as u8,
                    MMC_FLUSH_CACHE_TRIGGER,
                )
                .map(FlushRequest::Cache)
        } else {
            // Linux treats a flush as complete when the volatile cache is
            // disabled. This runtime still needs an IRQ-backed completion, so
            // use CMD13 as a non-mutating transfer-state barrier.
            self.card.submit_status().map(FlushRequest::Status)
        };
        match flush {
            Ok(flush) => {
                self.flush = Some((id, flush));
                self.sync_protocol_register_retry();
                Ok(id)
            }
            Err(error) => {
                warn!("sdmmc flush submit failed: {error}");
                Err(SubmitError::new(map_dev_err_to_blk_err(error), request))
            }
        }
    }

    fn submit_one(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        if self.init_request.is_some() {
            return Err(SubmitError::new(BlkError::Retry, request));
        }
        if let Err(error) = validate_owned_request(self.queue_info(), &request) {
            return Err(SubmitError::new(error, request));
        }
        if self.pending.is_some() || self.flush.is_some() {
            return Err(SubmitError::new(BlkError::Retry, request));
        }
        if let Err(error) = self.ensure_completion_irq() {
            return Err(SubmitError::new(error, request));
        }
        match request.op {
            RequestOp::Read | RequestOp::Write => self.submit_data(request),
            RequestOp::Flush => self.submit_flush(request),
        }
    }

    fn advance_data(&mut self, cause: ProgressCause, sink: &mut dyn CompletionSink) {
        let Some(id) = self.pending_id else {
            return;
        };
        let result = advance_block_request(
            self.card.protocol_host_mut(),
            &mut self.pending,
            BlockRequestId::new(usize::from(id)),
            &mut self.slot,
            cause,
        );
        match result {
            Ok(BlockProgress::Pending) => {}
            Ok(BlockProgress::Complete) => {
                self.pending_id = None;
                let data = take_completed_dma(&mut self.slot);
                sink.complete(CompletedRequest::new(id, Ok(()), data));
            }
            Err(error) => {
                let abort = abort_request(
                    self.card.protocol_host_mut(),
                    &mut self.pending,
                    &mut self.slot,
                );
                self.pending_id = None;
                let data = take_completed_dma(&mut self.slot);
                warn!("sdmmc data request {id:?} failed: {error}; abort={abort:?}");
                let error = abort.err().unwrap_or(error);
                sink.complete(CompletedRequest::new(
                    id,
                    Err(map_dev_err_to_blk_err(error)),
                    data,
                ));
            }
        }
    }

    fn advance_flush(&mut self, cause: ProgressCause, sink: &mut dyn CompletionSink) {
        let Some((id, request)) = self.flush.take() else {
            return;
        };
        match request {
            FlushRequest::Cache(mut request) => {
                match self.card.advance_mmc_switch_request(&mut request, cause) {
                    Ok(OperationProgress::Pending) => {
                        self.flush = Some((id, FlushRequest::Cache(request)));
                    }
                    Ok(OperationProgress::Complete(())) => {
                        sink.complete(CompletedRequest::new(id, Ok(()), None));
                    }
                    Err(error) => {
                        warn!("sdmmc flush request {id:?} failed: {error}");
                        sink.complete(CompletedRequest::new(
                            id,
                            Err(map_dev_err_to_blk_err(error)),
                            None,
                        ));
                    }
                }
            }
            FlushRequest::Status(mut request) => {
                match self.card.advance_status_request(&mut request, cause) {
                    Ok(OperationProgress::Pending) => {
                        self.flush = Some((id, FlushRequest::Status(request)));
                    }
                    Ok(OperationProgress::Complete(CardState::Transfer)) => {
                        sink.complete(CompletedRequest::new(id, Ok(()), None));
                    }
                    Ok(OperationProgress::Complete(state)) => {
                        warn!("sdmmc flush status barrier ended in card state {state:?}");
                        sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
                    }
                    Err(error) => {
                        warn!("sdmmc flush status barrier {id:?} failed: {error}");
                        sink.complete(CompletedRequest::new(
                            id,
                            Err(map_dev_err_to_blk_err(error)),
                            None,
                        ));
                    }
                }
            }
        }
    }

    fn abort_flush_request(&mut self, request: &mut FlushRequest) -> Result<(), BlkError> {
        match request {
            FlushRequest::Cache(request) => self
                .card
                .abort_mmc_switch_request(request)
                .map_err(map_dev_err_to_blk_err),
            FlushRequest::Status(request) => self
                .card
                .abort_status_request(request)
                .map_err(map_dev_err_to_blk_err),
        }
    }

    fn sync_protocol_register_retry(&mut self) {
        self.register_retry_after = match self.card.protocol_progress_wait() {
            HostProgressWait::Irq => None,
            HostProgressWait::Register { retry_after } => Some(retry_after),
        };
    }
}

fn queue_supports_flush(
    _card_kind: CardKind,
    _init_preference: Option<CardInitPreference>,
) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmc_first_queue_advertises_flush_before_card_detection() {
        assert!(queue_supports_flush(
            CardKind::Sd,
            Some(CardInitPreference::MmcFirst),
        ));
    }

    #[test]
    fn detected_sd_queue_advertises_irq_backed_flush_barrier() {
        assert!(queue_supports_flush(CardKind::Sd, None));
    }
}

impl<H> HardwareQueue for BlockQueue<H>
where
    H: SdioIrqHost + Send + 'static,
    H::TransactionRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let Some(request) = requests.pop_front() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        };
        match self.submit_one(request) {
            Ok(id) => {
                sink.accepted(id);
                BatchSubmitResult::new(1, BatchSubmitDisposition::Continue)
            }
            Err(error) => {
                let disposition = if error.error == BlkError::Retry {
                    BatchSubmitDisposition::QueueFull
                } else {
                    BatchSubmitDisposition::Fatal(error.error)
                };
                requests.push_front(error.into_request());
                BatchSubmitResult::new(0, disposition)
            }
        }
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        // DWCMSHC owns only one in-flight request and the host submit primitive
        // starts it immediately, so there is no separate doorbell to publish.
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.init_request.is_some() {
            self.register_retry_after = None;
            self.advance_initialization(ProgressCause::AcknowledgedIrq)?;
            return Ok(());
        }
        self.advance_data(ProgressCause::AcknowledgedIrq, sink);
        self.advance_flush(ProgressCause::AcknowledgedIrq, sink);
        self.sync_protocol_register_retry();
        Ok(())
    }

    fn register_retry_after(&self) -> Option<Duration> {
        self.register_retry_after
    }

    fn advance_register_retry(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.register_retry_after.take().is_none() {
            return Err(BlkError::InvalidRequest);
        }
        if self.init_request.is_some() {
            return self
                .advance_initialization(ProgressCause::RegisterRetry)
                .map(|_| ());
        }
        if self.pending_id.is_some() {
            self.advance_data(ProgressCause::RegisterRetry, sink);
        } else if self.flush.is_some() {
            self.advance_flush(ProgressCause::RegisterRetry, sink);
        } else {
            return Err(BlkError::InvalidRequest);
        }
        self.sync_protocol_register_retry();
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let mut first_error = None;
        let mut remember = |result: Result<(), BlkError>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };
        if self.completion_irq_enabled {
            remember(
                SdioIrqHost::disable_completion_irq(self.card.host_mut())
                    .map_err(map_dev_err_to_blk_err),
            );
            self.completion_irq_enabled = false;
        }
        if let Some(mut request) = self.init_request.take() {
            remember(
                self.card
                    .abort_init_request(&mut request)
                    .map_err(map_dev_err_to_blk_err),
            );
            if let Some(status) = &self.init_status {
                status.mark_failed();
            }
        }
        if let Some(id) = self.pending_id.take() {
            let result = abort_request(
                self.card.protocol_host_mut(),
                &mut self.pending,
                &mut self.slot,
            )
            .map_err(map_dev_err_to_blk_err);
            let data = take_completed_dma(&mut self.slot);
            sink.complete(CompletedRequest::new(
                id,
                result.and(Err(BlkError::Io)),
                data,
            ));
            remember(result);
        }
        if let Some((id, mut request)) = self.flush.take() {
            let result = self.abort_flush_request(&mut request);
            sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
            remember(result);
        }
        first_error.map_or(Ok(()), Err)
    }
}
