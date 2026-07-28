use alloc::sync::Arc;
use core::hint::spin_loop;

use log::{info, warn};
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, CompletedRequest, CompletionSink,
    HardwareQueue, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestId, RequestOp,
    SubmissionSink, SubmitError, validate_owned_request,
};

use crate::{
    BlockPoll, BlockRequestId, OperationPoll,
    rdif::{
        config::{block_addr_for_card, device_info, map_dev_err_to_blk_err, queue_limits},
        device::BlockInitStatus,
        host::BlockHost,
    },
    response::CardState,
    sdio::{
        card::{CardKind, SdioSdmmc, SdioStatusRequest},
        host::SdioHost,
        init::{CardInitPreference, MmcSwitchRequest, SdioInitWait},
    },
};

const MMC_SWITCH_WRITE_BYTE: u8 = 0b11;
const MMC_FLUSH_CACHE_TRIGGER: u8 = 1;
const INIT_DEADLINE_MS: u64 = 5_000;
const INIT_PACE_MS: u64 = 10;
const INIT_FALLBACK_SPIN_LIMIT: usize = 10_000_000;

enum FlushRequest {
    Cache(MmcSwitchRequest),
    Status(SdioStatusRequest),
}

/// Queue state exclusively owned by one block runtime maintenance task.
pub struct BlockQueue<H>
where
    H: BlockHost,
{
    card: SdioSdmmc<H>,
    config: super::config::BlockConfig,
    id: usize,
    slot: H::Slot,
    pending: Option<H::Request>,
    pending_id: Option<RequestId>,
    flush: Option<(RequestId, FlushRequest)>,
    next_flush_id: usize,
    completion_irq_enabled: bool,
    init_request: Option<H::InitRequest>,
    init_status: Option<Arc<BlockInitStatus>>,
    init_started_ms: Option<u64>,
    init_spins: usize,
    supports_flush: bool,
    cache_enabled: bool,
}

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    pub(super) fn new(card: SdioSdmmc<H>, config: super::config::BlockConfig, id: usize) -> Self {
        let supports_flush = queue_supports_flush(card.kind(), None);
        Self {
            card,
            config,
            id,
            slot: H::Slot::default(),
            pending: None,
            pending_id: None,
            flush: None,
            next_flush_id: usize::MAX / 2,
            completion_irq_enabled: false,
            init_request: None,
            init_status: None,
            init_started_ms: None,
            init_spins: 0,
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
        queue.init_started_ms = queue.card.host().now_ms();
        queue.init_request =
            Some(H::begin_card_init(&mut queue.card, preference).map_err(map_dev_err_to_blk_err)?);
        queue.advance_initialization()?;
        Ok(queue)
    }

    fn queue_info(&self) -> QueueInfo {
        let mut limits = queue_limits(&self.config, self.config.dma_mask);
        limits.supports_flush = self.supports_flush;
        QueueInfo {
            id: self.id,
            device: device_info(&self.config),
            limits,
        }
    }

    fn ensure_completion_irq(&mut self) -> Result<(), BlkError> {
        if self.completion_irq_enabled {
            return Ok(());
        }
        SdioHost::enable_completion_irq(self.card.host_mut()).map_err(map_dev_err_to_blk_err)?;
        if !self.card.host().completion_irq_enabled() {
            return Err(BlkError::NotSupported);
        }
        self.completion_irq_enabled = true;
        Ok(())
    }

    fn init_deadline_expired(&self) -> bool {
        match (self.init_started_ms, self.card.host().now_ms()) {
            (Some(started), Some(now)) => now.saturating_sub(started) >= INIT_DEADLINE_MS,
            _ => self.init_spins >= INIT_FALLBACK_SPIN_LIMIT,
        }
    }

    fn pace_initialization(&mut self) -> Result<(), BlkError> {
        if let Some(started) = self.card.host().now_ms() {
            while self
                .card
                .host()
                .now_ms()
                .is_some_and(|now| now.saturating_sub(started) < INIT_PACE_MS)
            {
                if self.init_deadline_expired() {
                    return Err(BlkError::TimedOut);
                }
                spin_loop();
            }
            return Ok(());
        }
        for _ in 0..10_000 {
            spin_loop();
        }
        Ok(())
    }

    /// Advances register-only init work until the protocol submits a command
    /// or data transaction. Calls reached from `drain_completions` have
    /// consumed one acknowledged IRQ before entering this method.
    fn advance_initialization(&mut self) -> Result<bool, BlkError> {
        let Some(mut request) = self.init_request.take() else {
            return Ok(true);
        };
        loop {
            if self.init_deadline_expired() {
                if let Some(status) = &self.init_status {
                    status.mark_failed();
                }
                return Err(BlkError::TimedOut);
            }
            self.init_spins = self.init_spins.saturating_add(1);
            match H::advance_card_init(&mut self.card, &mut request) {
                Ok(OperationPoll::Pending) => {
                    if H::take_init_needs_pace(&mut request)
                        && let Err(error) = self.pace_initialization()
                    {
                        if let Some(status) = &self.init_status {
                            status.mark_failed();
                        }
                        return Err(error);
                    }
                    if H::init_wait_kind(&self.card, &request) == SdioInitWait::Irq {
                        self.init_request = Some(request);
                        return Ok(false);
                    }
                    spin_loop();
                }
                Ok(OperationPoll::Complete(info)) => {
                    let Some(capacity_blocks) =
                        info.capacity_blocks.filter(|capacity| *capacity != 0)
                    else {
                        if let Some(status) = &self.init_status {
                            status.mark_failed();
                        }
                        return Err(BlkError::Io);
                    };
                    self.config.capacity_blocks = capacity_blocks;
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
                        info.kind,
                        info.high_capacity,
                        info.rca,
                        capacity_blocks,
                        self.cache_enabled
                    );
                    return Ok(true);
                }
                Err(error) => {
                    if let Some(status) = &self.init_status {
                        status.mark_failed();
                    }
                    return Err(map_dev_err_to_blk_err(error));
                }
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
            RequestOp::Read => H::submit_owned_read_request(
                self.card.host_mut(),
                start_block,
                buffer,
                &mut self.slot,
                &mut self.pending,
            ),
            RequestOp::Write => H::submit_owned_write_request(
                self.card.host_mut(),
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
        if self.card.kind() != CardKind::Mmc {
            return Err(SubmitError::new(BlkError::NotSupported, request));
        }
        let id = RequestId::new(self.next_flush_id);
        self.next_flush_id = self.next_flush_id.wrapping_add(1);
        let flush = if self.cache_enabled {
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

    fn drain_data(&mut self, sink: &mut dyn CompletionSink) {
        let Some(id) = self.pending_id else {
            return;
        };
        let result = H::poll_block_request(
            self.card.host_mut(),
            &mut self.pending,
            BlockRequestId::new(usize::from(id)),
            &mut self.slot,
        );
        match result {
            Ok(BlockPoll::Pending) => {}
            Ok(BlockPoll::Complete) => {
                self.pending_id = None;
                let data = H::take_completed_dma(&mut self.slot);
                sink.complete(CompletedRequest::new(id, Ok(()), data));
            }
            Err(error) => {
                let abort =
                    H::abort_request(self.card.host_mut(), &mut self.pending, &mut self.slot);
                self.pending_id = None;
                let data = H::take_completed_dma(&mut self.slot);
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

    fn drain_flush(&mut self, sink: &mut dyn CompletionSink) {
        let Some((id, request)) = self.flush.take() else {
            return;
        };
        match request {
            FlushRequest::Cache(mut request) => {
                match self.card.poll_mmc_switch_request(&mut request) {
                    Ok(OperationPoll::Pending) => {
                        self.flush = Some((id, FlushRequest::Cache(request)));
                    }
                    Ok(OperationPoll::Complete(())) => {
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
                match self.card.poll_status_request(&mut request) {
                    Ok(OperationPoll::Pending) => {
                        self.flush = Some((id, FlushRequest::Status(request)));
                    }
                    Ok(OperationPoll::Complete(CardState::Transfer)) => {
                        sink.complete(CompletedRequest::new(id, Ok(()), None));
                    }
                    Ok(OperationPoll::Complete(state)) => {
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
}

fn queue_supports_flush(card_kind: CardKind, init_preference: Option<CardInitPreference>) -> bool {
    card_kind == CardKind::Mmc || matches!(init_preference, Some(CardInitPreference::MmcFirst))
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
}

impl<H> HardwareQueue for BlockQueue<H>
where
    H: BlockHost,
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
            self.advance_initialization()?;
            return Ok(());
        }
        self.drain_data(sink);
        self.drain_flush(sink);
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.completion_irq_enabled {
            let _ = SdioHost::disable_completion_irq(self.card.host_mut());
            self.completion_irq_enabled = false;
        }
        if self.init_request.take().is_some()
            && let Some(status) = &self.init_status
        {
            status.mark_failed();
        }
        if let Some(id) = self.pending_id.take() {
            let result = H::abort_request(self.card.host_mut(), &mut self.pending, &mut self.slot)
                .map_err(map_dev_err_to_blk_err);
            let data = H::take_completed_dma(&mut self.slot);
            sink.complete(CompletedRequest::new(
                id,
                result.and(Err(BlkError::Io)),
                data,
            ));
        }
        if let Some((id, _)) = self.flush.take() {
            sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
        }
        Ok(())
    }
}
