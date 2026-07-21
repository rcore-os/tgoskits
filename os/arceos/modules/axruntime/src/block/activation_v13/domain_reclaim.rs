//! DMA-quiesced request and IRQ-evidence reclamation for one I/O domain.

use alloc::vec::Vec;

use rdif_block::{
    CompletionSink, DmaQuiesced, InstalledIoDomain, InterruptIoDomain, SharedIoDomainSession,
    RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired,
};

use super::{
    request_runtime::DomainRequestOwner,
    source::recovery::{
        ClosedRecoveryProgress, ClosedRecoverySource, DriverEvidenceRetireFailure,
        DriverEvidenceRoute, RecoveryRetireReason,
    },
};

const RECLAIM_SERVICE_BUDGET: usize = 64;

pub(super) fn retire_domain_recovery_sources(
    sources: &mut Vec<ClosedRecoverySource>,
    proof: &rdif_block::DmaQuiesced,
    mut retire_driver: impl FnMut(
        DriverEvidenceRoute,
        RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, DriverEvidenceRetireFailure>,
) -> Result<bool, RecoveryRetireReason> {
    let mut serviced = 0;
    while serviced < RECLAIM_SERVICE_BUDGET {
        let Some(source) = sources.pop() else {
            return Ok(true);
        };
        match source.retire_after_quiesce(proof, &mut retire_driver) {
            Ok(ClosedRecoveryProgress::More(source)) => sources.push(source),
            Ok(ClosedRecoveryProgress::Retired(_receipt)) => {}
            Err(failure) => {
                let (reason, source) = failure.into_parts();
                sources.push(source);
                return Err(reason);
            }
        }
        serviced += 1;
    }
    Ok(sources.is_empty())
}

/// Reclaims all request ownership after DMA quiescence without ending the
/// driver or runtime-gate lifecycle.
///
/// A successful call leaves both the portable domain and the request gates in
/// their quiesced, reusable state. Controller reinitialization must install a
/// new driver epoch before [`DomainRequestOwner::resume_after_reinitialize`]
/// reopens dispatch and admission.
pub(super) fn reclaim_domain_for_recovery<D>(
    domain: &mut D,
    requests: &mut DomainRequestOwner,
    proof: &DmaQuiesced,
) -> Result<(), DomainRecoveryReclaimError>
where
    D: QuiescedDomainPort + ?Sized,
{
    domain
        .reclaim_after_quiesce(proof, requests.completion_sink())
        .map_err(DomainRecoveryReclaimError::Driver)?;
    requests
        .finish_completions()
        .map_err(DomainRecoveryReclaimError::Requests)?;
    if !requests.accepted_requests_drained() {
        return Err(DomainRecoveryReclaimError::AcceptedRequestsRemain);
    }
    Ok(())
}

/// Permanently closes a domain whose accepted requests were already reclaimed.
pub(super) fn close_reclaimed_domain<D>(
    domain: &mut D,
    requests: &DomainRequestOwner,
) -> Result<(), DomainTerminalCloseError>
where
    D: QuiescedDomainPort + ?Sized,
{
    domain
        .shutdown()
        .map_err(DomainTerminalCloseError::Driver)?;
    requests
        .close_after_quiesce()
        .map_err(DomainTerminalCloseError::Lifecycle)
}

/// Performs the terminal composition used only by explicit shutdown/handoff.
pub(super) fn reclaim_domain_for_shutdown<D>(
    domain: &mut D,
    requests: &mut DomainRequestOwner,
    proof: &DmaQuiesced,
) -> Result<(), DomainQuiescedReclaimError>
where
    D: QuiescedDomainPort + ?Sized,
{
    reclaim_domain_for_recovery(domain, requests, proof)
        .map_err(DomainQuiescedReclaimError::Reclaim)?;
    close_reclaimed_domain(domain, requests).map_err(DomainQuiescedReclaimError::Close)
}

pub(super) trait QuiescedDomainPort {
    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), rdif_block::BlkError>;

    fn shutdown(&mut self) -> Result<(), rdif_block::BlkError>;
}

impl QuiescedDomainPort for InstalledIoDomain {
    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), rdif_block::BlkError> {
        self.io_mut().reclaim_after_quiesce(proof, sink)
    }

    fn shutdown(&mut self) -> Result<(), rdif_block::BlkError> {
        self.io_mut().shutdown()
    }
}

impl QuiescedDomainPort for dyn InterruptIoDomain {
    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), rdif_block::BlkError> {
        InterruptIoDomain::reclaim_after_quiesce(self, proof, sink)
    }

    fn shutdown(&mut self) -> Result<(), rdif_block::BlkError> {
        InterruptIoDomain::shutdown(self)
    }
}

impl QuiescedDomainPort for SharedIoDomainSession<'_> {
    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), rdif_block::BlkError> {
        SharedIoDomainSession::reclaim_after_quiesce(self, proof, sink)
    }

    fn shutdown(&mut self) -> Result<(), rdif_block::BlkError> {
        SharedIoDomainSession::shutdown(self)
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DomainRecoveryReclaimError {
    #[error(transparent)]
    Driver(rdif_block::BlkError),
    #[error(transparent)]
    Requests(super::request_runtime::DomainRequestServiceError),
    #[error("driver retained accepted requests after DMA-quiesced reclaim")]
    AcceptedRequestsRemain,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DomainTerminalCloseError {
    #[error(transparent)]
    Driver(rdif_block::BlkError),
    #[error(transparent)]
    Lifecycle(super::request_runtime::DomainRequestLifecycleError),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DomainQuiescedReclaimError {
    #[error(transparent)]
    Reclaim(DomainRecoveryReclaimError),
    #[error(transparent)]
    Close(DomainTerminalCloseError),
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::num::NonZeroU16;

    use rdif_block::{
        CompletionSink, ControllerEpoch, DmaQuiesced, IdList, InterruptQueueDesc,
        LogicalDeviceSelector, OwnershipDomainId, QueueExecution,
    };

    use super::*;
    use crate::block::{BlockRuntimeConfig, activation_v13::request_runtime::DomainRequestRuntime};

    #[test]
    fn recovery_reclaim_keeps_driver_and_request_gates_reusable() {
        let mut domain = RecordingDomain::default();
        let mut requests = quiesced_request_owner();
        let proof = dma_quiesced();

        reclaim_domain_for_recovery(&mut domain, &mut requests, &proof).unwrap();

        assert_eq!(domain.reclaim_calls, 1);
        assert_eq!(domain.shutdown_calls, 0);
        requests.resume_after_reinitialize().unwrap();
    }

    #[test]
    fn terminal_reclaim_closes_driver_and_request_gates_permanently() {
        let mut domain = RecordingDomain::default();
        let mut requests = quiesced_request_owner();
        let proof = dma_quiesced();

        reclaim_domain_for_shutdown(&mut domain, &mut requests, &proof).unwrap();

        assert_eq!(domain.reclaim_calls, 1);
        assert_eq!(domain.shutdown_calls, 1);
        assert!(requests.resume_after_reinitialize().is_err());
    }

    #[derive(Default)]
    struct RecordingDomain {
        reclaim_calls: usize,
        shutdown_calls: usize,
    }

    impl QuiescedDomainPort for RecordingDomain {
        fn reclaim_after_quiesce(
            &mut self,
            _proof: &DmaQuiesced,
            _sink: &mut dyn CompletionSink,
        ) -> Result<(), rdif_block::BlkError> {
            self.reclaim_calls += 1;
            Ok(())
        }

        fn shutdown(&mut self) -> Result<(), rdif_block::BlkError> {
            self.shutdown_calls += 1;
            Ok(())
        }
    }

    fn quiesced_request_owner() -> DomainRequestOwner {
        quiesced_request_owner_from_runtime(request_runtime())
    }

    fn request_runtime() -> Arc<DomainRequestRuntime> {
        let domain = OwnershipDomainId::new(1).unwrap();
        let mut sources = IdList::none();
        sources.insert(1);
        let queue = InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(1).unwrap(),
            sources,
        )
        .unwrap();
        Arc::new(
            DomainRequestRuntime::new(domain, &[queue], BlockRuntimeConfig::default()).unwrap(),
        )
    }

    fn quiesced_request_owner_from_runtime(
        runtime: Arc<DomainRequestRuntime>,
    ) -> DomainRequestOwner {
        let owner = DomainRequestOwner::new(runtime);
        owner.begin_quiesce().unwrap();
        assert!(owner.try_commit_quiesced().unwrap());
        owner
    }

    fn dma_quiesced() -> DmaQuiesced {
        // SAFETY: no hardware or DMA exists in this deterministic lifecycle test.
        unsafe { DmaQuiesced::new(ControllerEpoch::new(2), 1) }
    }
}
