//! RDIF queue adapter and request-service orchestration.

use alloc::sync::Arc;

use rdif_block::{
    BlkError, CompletionSink, IQueue, OwnedRequest, QueueEventBatch, QueueInfo, RequestId,
    ServiceProgress, ServiceRerunReason, SubmitError, SubmitOutcome, validate_owned_request,
};

use super::{
    core::NvmeQueueCore,
    dma::{prepare_request_dma, restore_prepared_dma},
    request::AcceptedRequest,
};
use crate::block::NvmeBlockOwner;

const SERVICE_COMPLETION_BUDGET: usize = 64;

pub(in crate::block) struct NvmeBlockQueue {
    core: Arc<NvmeQueueCore>,
    // The controller, MMIO mapping, and PCI handoff state must outlive every
    // queue endpoint even if the interface object is moved or released first.
    // This owner excludes the external MSI-X/INTx allocation; the OS retains
    // that lease through mask, synchronization, quiesce, and shutdown.
    _owner: Arc<NvmeBlockOwner>,
    reclaim_proof: ReclaimProofTracker,
}

struct ReclaimProofTracker {
    controller_cookie: usize,
    last_epoch: Option<u64>,
}

impl NvmeBlockQueue {
    pub(in crate::block) fn new(core: Arc<NvmeQueueCore>, owner: Arc<NvmeBlockOwner>) -> Self {
        let controller_cookie = owner.controller_cookie();
        Self {
            core,
            _owner: owner,
            reclaim_proof: ReclaimProofTracker {
                controller_cookie,
                last_epoch: None,
            },
        }
    }

    fn submit_accepted(
        &self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        let (mut request, mut prepared) = prepare_request_dma(id, request)?;
        let Some(mut state) = self.core.try_claim_state() else {
            request = restore_prepared_dma(request, prepared.take());
            return Err(SubmitError::new(id, BlkError::Retry, request));
        };
        let cid = match state.alloc_cid() {
            Ok(cid) => cid,
            Err(error) => {
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, error, request));
            }
        };
        let (command, prp_list) = match state.build_command(
            self.core.namespace(),
            self.core.page_size(),
            cid,
            &request,
            prepared.as_ref(),
        ) {
            Ok(command) => command,
            Err(error) => {
                state.release_cid(cid);
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, error, request));
            }
        };
        let dma = prepared
            .take()
            // SAFETY: accepted ownership is installed before the SQ doorbell.
            // Owner-side completion or proof-gated reclaim returns it only
            // after CQ observation or full controller quiescence.
            .map(|prepared| unsafe { prepared.into_in_flight() });
        state.accept(cid, AcceptedRequest { id, request, dma }, prp_list);

        // Runtime identity and request ownership are visible before hardware
        // can assert the source consumed by the maintenance owner.
        self.core.submit_command(command);
        Ok(SubmitOutcome::Queued)
    }

    fn emit_cached_completions(
        &self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<Option<usize>, BlkError> {
        self.core.emit_owner_cached_completions(budget, sink)
    }
}

impl IQueue for NvmeBlockQueue {
    fn id(&self) -> usize {
        self.core.id()
    }

    fn info(&self) -> QueueInfo {
        self.core.queue_info()
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if let Err(error) = validate_owned_request(self.core.queue_info(), &request) {
            return Err(SubmitError::new(id, error, request));
        }
        self.submit_accepted(id, request)
    }

    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if events.queue_id() != self.core.id() {
            return Err(BlkError::InvalidRequest);
        }
        if self.core.completion_failed() {
            return Err(BlkError::Io);
        }

        let Some(emitted) = self.emit_cached_completions(SERVICE_COMPLETION_BUDGET, sink)? else {
            return Ok(events.requeue_service(ServiceRerunReason::CachedCompletions));
        };
        let remaining = SERVICE_COMPLETION_BUDGET.saturating_sub(emitted);
        if remaining == 0 {
            return Ok(events.requeue_service(ServiceRerunReason::CompletionBudget));
        }

        let drain = self.core.drain_owner_completions(remaining);
        if self.core.completion_failed() {
            return Err(BlkError::Io);
        }
        let Some(emitted_after_drain) = self.emit_cached_completions(remaining, sink)? else {
            return Ok(events.requeue_service(ServiceRerunReason::CachedCompletions));
        };

        if drain.may_have_more || emitted + emitted_after_drain == SERVICE_COMPLETION_BUDGET {
            Ok(events.requeue_service(ServiceRerunReason::CompletionBudget))
        } else if self.core.service_pending() {
            Ok(events.requeue_service(ServiceRerunReason::RetainedFacts))
        } else {
            Ok(ServiceProgress::Idle)
        }
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.reclaim_proof.validate(proof)?;
        let Some(mut state) = self.core.try_claim_state() else {
            return Err(BlkError::Busy);
        };
        state.cancel_all(sink);
        drop(state);
        self.core.clear_service_state_after_quiesce();
        self.reclaim_proof.commit(proof);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        let Some(state) = self.core.try_claim_state() else {
            return Err(BlkError::Busy);
        };
        if state.has_accepted() {
            return Err(BlkError::Busy);
        }
        drop(state);
        if self.core.completion_failed() {
            return Err(BlkError::Io);
        }
        if self.core.service_pending() {
            return Err(BlkError::Busy);
        }
        Ok(())
    }
}

impl ReclaimProofTracker {
    fn validate(&self, proof: &rdif_block::DmaQuiesced) -> Result<(), BlkError> {
        if proof.controller_cookie() != self.controller_cookie
            || self
                .last_epoch
                .is_some_and(|last_epoch| proof.epoch().get() <= last_epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    fn commit(&mut self, proof: &rdif_block::DmaQuiesced) {
        self.last_epoch = Some(proof.epoch().get());
    }
}

#[cfg(test)]
mod tests {
    use rdif_block::{ControllerEpoch, DmaQuiesced};

    use super::*;

    #[test]
    fn reclaim_proof_is_bound_to_owner_and_advances_monotonically() {
        let mut tracker = ReclaimProofTracker {
            controller_cookie: 0x51a7,
            last_epoch: None,
        };
        let wrong_owner = unsafe {
            // SAFETY: this value-only test never returns real DMA ownership.
            DmaQuiesced::new(ControllerEpoch::new(2), 0xdead)
        };
        assert_eq!(
            tracker.validate(&wrong_owner),
            Err(BlkError::InvalidDmaProof)
        );

        let current = unsafe {
            // SAFETY: this value-only test never returns real DMA ownership.
            DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
        };
        assert_eq!(tracker.validate(&current), Ok(()));
        tracker.commit(&current);
        assert_eq!(tracker.validate(&current), Err(BlkError::InvalidDmaProof));

        let stale = unsafe {
            // SAFETY: this value-only test never returns real DMA ownership.
            DmaQuiesced::new(ControllerEpoch::new(1), 0x51a7)
        };
        assert_eq!(tracker.validate(&stale), Err(BlkError::InvalidDmaProof));
    }
}
