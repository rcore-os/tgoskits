//! RDIF queue adapter and request-service orchestration.

use alloc::sync::Arc;

use rdif_block::{
    BlkError, CompletionSink, IQueue, OwnedRequest, QueueEventBatch, QueueInfo, RequestId,
    ServiceProgress, ServiceRerunReason, SubmitError, SubmitOutcome,
};

use super::core::NvmeQueueCore;
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

    pub(in crate::block) fn service_claimed_evidence(
        &self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<super::core::NvmeQueueEvidenceProgress, BlkError> {
        self.core.service_claimed_evidence(budget, sink)
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
        self.core
            .submit_owned(
                self.core.namespace(),
                self.core.max_transfer_bytes(),
                id,
                request,
            )
            .map(|_| SubmitOutcome::Queued)
            .map_err(|unaccepted| {
                let (id, error, request, _not_visible) = unaccepted.into_parts();
                SubmitError::new(id, error, request)
            })
    }

    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if events.queue_id() != self.core.id() {
            return Err(BlkError::InvalidRequest);
        }
        let progress = self.service_claimed_evidence(SERVICE_COMPLETION_BUDGET, sink)?;
        if progress.retained {
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
        self.core.reclaim_requests_after_quiesce(sink)?;
        self.reclaim_proof.commit(proof);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.core.shutdown()
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
