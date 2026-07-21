//! Single-owner NVMe queue domain for opaque IRQ evidence service.

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, CompletionSink, ControllerEpoch, ControllerFault, DmaQuiesced, DriverDeviceKey,
    DriverEvidenceRetirement, EvidenceServiceResult, InterruptIoDomain, IrqEvidenceId,
    OwnedRequest, OwnershipDomainId, RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired, RequestId, UnacceptedRequest,
};

use super::{
    evidence_ledger::{NvmeEvidenceDisposition, NvmeEvidenceFacts, NvmeEvidenceLedger},
    queue_runtime::NvmeOwnedQueue,
};
use crate::Namespace;

const DOMAIN_SERVICE_BUDGET: usize = 64;

/// Final runtime routing from one compact logical-device ID to its NVMe NSID.
///
/// This mapping is created only after Identify Namespace completed. It is not
/// present in discovery capabilities and cannot fabricate pre-init geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::block) struct NvmeNamespaceRoute {
    driver_key: DriverDeviceKey,
    namespace: Namespace,
    max_transfer_bytes: Option<usize>,
}

impl NvmeNamespaceRoute {
    pub(in crate::block) const fn new(
        driver_key: DriverDeviceKey,
        namespace: Namespace,
        max_transfer_bytes: Option<usize>,
    ) -> Self {
        Self {
            driver_key,
            namespace,
            max_transfer_bytes,
        }
    }
}

/// Queue collection that shares one IRQ source and one maintenance owner.
///
/// This type owns only queue-local state, a final namespace route, and the
/// driver evidence ledger. It deliberately does not retain the legacy
/// controller bundle, initialization FSM, IRQ registration, or OS wake state.
pub(in crate::block) struct NvmeIoDomain {
    id: OwnershipDomainId,
    ledger: Arc<NvmeEvidenceLedger>,
    queues: Vec<NvmeOwnedQueue>,
    namespaces: Vec<NvmeNamespaceRoute>,
    reclaim_proof: ReclaimProofTracker,
    recovery_epoch: Arc<NvmeDomainRecoveryEpoch>,
}

/// Release-published receipt that queue DMA state was reset for one epoch.
///
/// The controller owns only the other end of this receipt. It does not expose
/// queue state or allow it to call an I/O domain remotely.
pub(in crate::block) struct NvmeDomainRecoveryEpoch {
    reclaimed: AtomicU64,
}

/// Invalid final topology retaining every move-only hardware queue owner.
pub(in crate::block) struct NvmeDomainBuildFailure {
    error: BlkError,
    queues: Vec<NvmeOwnedQueue>,
}

impl NvmeIoDomain {
    pub(in crate::block) fn new(
        id: OwnershipDomainId,
        ledger: Arc<NvmeEvidenceLedger>,
        mut queues: Vec<NvmeOwnedQueue>,
        namespaces: Vec<NvmeNamespaceRoute>,
        controller_cookie: usize,
        recovery_epoch: Arc<NvmeDomainRecoveryEpoch>,
    ) -> Result<Self, NvmeDomainBuildFailure> {
        if let Err(error) = validate_domain_topology(&queues, &namespaces, controller_cookie) {
            return Err(NvmeDomainBuildFailure::new(error, queues));
        }
        for queue in &mut queues {
            if let Err(error) = queue.bind_dma_owner(controller_cookie, ControllerEpoch::INITIAL) {
                return Err(NvmeDomainBuildFailure::new(error, queues));
            }
        }
        Ok(Self {
            id,
            ledger,
            queues,
            namespaces,
            reclaim_proof: ReclaimProofTracker {
                controller_cookie,
                last_epoch: None,
                resumed_epoch: None,
            },
            recovery_epoch,
        })
    }

    fn find_queue_mut(&mut self, queue_id: usize) -> Option<&mut NvmeOwnedQueue> {
        self.queues
            .iter_mut()
            .find(|queue| queue.slot() == queue_id)
    }

    fn find_namespace(&self, driver_key: DriverDeviceKey) -> Option<NvmeNamespaceRoute> {
        self.namespaces
            .iter()
            .find(|route| route.driver_key == driver_key)
            .copied()
    }

    fn service_claimed_queues(
        &mut self,
        queue_bits: u64,
        sink: &mut dyn CompletionSink,
    ) -> Result<u64, ControllerFault> {
        let mut retained = 0_u64;
        let mut completed = 0_usize;
        for queue_id in 0..u64::BITS as usize {
            let bit = 1_u64 << queue_id;
            if queue_bits & bit == 0 {
                continue;
            }
            let queue = self
                .find_queue_mut(queue_id)
                .ok_or(ControllerFault::Ownership)?;
            let progress = queue
                .service_claimed_evidence(DOMAIN_SERVICE_BUDGET.saturating_sub(completed), sink)
                .map_err(classify_service_error)?;
            completed = completed.saturating_add(progress.completed);
            if progress.retained || completed == DOMAIN_SERVICE_BUDGET {
                retained |= bit;
            }
        }
        Ok(retained)
    }
}

fn validate_domain_topology(
    queues: &[NvmeOwnedQueue],
    namespaces: &[NvmeNamespaceRoute],
    controller_cookie: usize,
) -> Result<(), BlkError> {
    if queues.is_empty() || queues.len() > u64::BITS as usize || controller_cookie == 0 {
        return Err(BlkError::InvalidRequest);
    }
    for (slot, queue) in queues.iter().enumerate() {
        if queue.slot() != slot
            || queue.hardware_qid() == 0
            || queues[..slot]
                .iter()
                .any(|candidate| candidate.hardware_qid() == queue.hardware_qid())
        {
            return Err(BlkError::InvalidRequest);
        }
    }
    for (index, route) in namespaces.iter().enumerate() {
        if namespaces[..index]
            .iter()
            .any(|candidate| candidate.driver_key == route.driver_key)
        {
            return Err(BlkError::InvalidRequest);
        }
    }
    Ok(())
}

impl NvmeDomainBuildFailure {
    const fn new(error: BlkError, queues: Vec<NvmeOwnedQueue>) -> Self {
        Self { error, queues }
    }

    pub(in crate::block) const fn error(&self) -> BlkError {
        self.error
    }

    pub(in crate::block) fn retained_queue_count(&self) -> usize {
        self.queues.len()
    }
}

impl InterruptIoDomain for NvmeIoDomain {
    fn domain_id(&self) -> OwnershipDomainId {
        self.id
    }

    fn queue_count(&self) -> usize {
        self.queues.len()
    }

    fn submit_owned(
        &mut self,
        queue_id: usize,
        driver_key: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<rdif_block::AcceptedRequest, UnacceptedRequest> {
        let Some(route) = self.find_namespace(driver_key) else {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        };
        let Some(queue) = self.find_queue_mut(queue_id) else {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        };
        queue.submit_owned(route.namespace, route.max_transfer_bytes, id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        let ledger = Arc::clone(&self.ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => return Ok(EvidenceServiceResult::Recover(ControllerFault::Ownership)),
        };
        let facts = batch.facts();
        let retained_queues = match self.service_claimed_queues(facts.queue_bits(), sink) {
            Ok(retained) => retained,
            Err(fault) => return Ok(EvidenceServiceResult::Recover(fault)),
        };
        // Admin CQ ownership belongs to the controller-control part. An I/O
        // pass must retain that fact under the same evidence identity.
        let retained_facts = if facts.has_admin() {
            NvmeEvidenceFacts::queues(retained_queues).with_admin()
        } else {
            NvmeEvidenceFacts::queues(retained_queues)
        };
        Ok(match ledger.finish_service(batch, retained_facts) {
            NvmeEvidenceDisposition::Drained => EvidenceServiceResult::Drained,
            NvmeEvidenceDisposition::Retained => EvidenceServiceResult::Retained,
            NvmeEvidenceDisposition::Invalid => {
                EvidenceServiceResult::Recover(ControllerFault::Ownership)
            }
        })
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Other("NVMe I/O evidence commit is invalid"))
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        let owner =
            NonZeroUsize::new(self.reclaim_proof.controller_cookie).unwrap_or(NonZeroUsize::MIN);
        self.ledger.retire_after_quiesce(permit, owner)
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.reclaim_proof.validate(proof)?;
        for queue in &self.queues {
            queue.validate_quiescence(proof)?;
        }
        for queue in &mut self.queues {
            queue.reclaim_requests_after_quiesce(proof, sink)?;
        }
        self.reclaim_proof.commit(proof);
        self.recovery_epoch.publish(proof.epoch());
        Ok(())
    }

    fn resume_after_reinitialize(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        self.reclaim_proof.validate_resume(epoch)?;
        for queue in &self.queues {
            queue.validate_resume(epoch)?;
        }
        for queue in &mut self.queues {
            queue.resume_after_reinitialize(epoch)?;
        }
        self.reclaim_proof.commit_resume(epoch);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        let epoch = self.reclaim_proof.close_epoch()?;
        for queue in &self.queues {
            queue.validate_shutdown(epoch)?;
        }
        for queue in &mut self.queues {
            queue.shutdown(epoch)?;
        }
        Ok(())
    }
}

struct ReclaimProofTracker {
    controller_cookie: usize,
    last_epoch: Option<u64>,
    resumed_epoch: Option<u64>,
}

impl ReclaimProofTracker {
    fn validate(&self, proof: &DmaQuiesced) -> Result<(), BlkError> {
        if proof.epoch().get() == 0
            || proof.controller_cookie() != self.controller_cookie
            || self
                .last_epoch
                .is_some_and(|last_epoch| proof.epoch().get() <= last_epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    fn commit(&mut self, proof: &DmaQuiesced) {
        self.last_epoch = Some(proof.epoch().get());
        self.resumed_epoch = None;
    }

    fn validate_resume(&self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        if self.last_epoch != Some(epoch.get()) || self.resumed_epoch == Some(epoch.get()) {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    fn commit_resume(&mut self, epoch: ControllerEpoch) {
        self.resumed_epoch = Some(epoch.get());
    }

    fn close_epoch(&self) -> Result<ControllerEpoch, BlkError> {
        let epoch = self.last_epoch.ok_or(BlkError::InvalidDmaProof)?;
        if self.resumed_epoch == Some(epoch) {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(ControllerEpoch::new(epoch))
    }
}

impl NvmeDomainRecoveryEpoch {
    pub(in crate::block) const fn new() -> Self {
        Self {
            reclaimed: AtomicU64::new(0),
        }
    }

    fn publish(&self, epoch: ControllerEpoch) {
        debug_assert_ne!(epoch.get(), 0);
        self.reclaimed.store(epoch.get(), Ordering::Release);
    }

    pub(in crate::block) fn matches(&self, epoch: ControllerEpoch) -> bool {
        epoch.get() != 0 && self.reclaimed.load(Ordering::Acquire) == epoch.get()
    }
}

const fn classify_service_error(error: BlkError) -> ControllerFault {
    match error {
        BlkError::InvalidDmaProof => ControllerFault::Dma,
        BlkError::Busy => ControllerFault::Ownership,
        BlkError::Io
        | BlkError::InvalidRequest
        | BlkError::QueueEpochExhausted
        | BlkError::Other(_) => ControllerFault::Protocol,
        BlkError::NotSupported
        | BlkError::Retry
        | BlkError::TimedOut
        | BlkError::Cancelled
        | BlkError::Offline
        | BlkError::Quarantined
        | BlkError::NoMemory
        | BlkError::InvalidBlockIndex(_) => ControllerFault::Ownership,
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use rdif_block::{BlkError, ControllerEpoch, DmaQuiesced};

    use super::{NvmeDomainRecoveryEpoch, ReclaimProofTracker};

    #[test]
    fn recovery_receipt_publishes_only_the_exact_reclaimed_epoch() {
        let receipt = Arc::new(NvmeDomainRecoveryEpoch::new());
        assert!(!receipt.matches(ControllerEpoch::new(7)));

        receipt.publish(ControllerEpoch::new(7));

        assert!(receipt.matches(ControllerEpoch::new(7)));
        assert!(!receipt.matches(ControllerEpoch::new(6)));
        assert!(!receipt.matches(ControllerEpoch::new(8)));
    }

    #[test]
    fn one_reclaimed_epoch_allows_exactly_one_resume() {
        let mut tracker = ReclaimProofTracker {
            controller_cookie: 0x1234,
            last_epoch: None,
            resumed_epoch: None,
        };
        let epoch = ControllerEpoch::new(11);
        let proof = unsafe {
            // SAFETY: this unit test exercises only the driver-local proof
            // tracker and does not use the value to reclaim real DMA.
            DmaQuiesced::new(epoch, 0x1234)
        };

        tracker.validate(&proof).unwrap();
        tracker.commit(&proof);
        tracker.validate_resume(epoch).unwrap();
        tracker.commit_resume(epoch);

        assert_eq!(
            tracker.validate_resume(epoch),
            Err(BlkError::InvalidDmaProof)
        );
    }
}
