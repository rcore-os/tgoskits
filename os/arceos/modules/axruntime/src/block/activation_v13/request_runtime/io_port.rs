//! Narrow normal-I/O capability used by the runtime owner.

use rdif_block::{
    AcceptedRequest, BlkError, CompletionSink, DriverDeviceKey, EvidenceServiceResult,
    InterruptIoDomain, IrqEvidenceId, OwnedRequest, RequestId, SharedIoDomainSession,
    UnacceptedRequest,
};

/// Runtime-local view shared by split and inseparable control/I/O domains.
///
/// Reinitialization is deliberately absent. A combined domain can resume only
/// through the publication-bound linear permit on `PublishedController`.
pub(in crate::block::activation_v13) trait RuntimeIoDomainPort {
    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest>;

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError>;
}

impl RuntimeIoDomainPort for dyn InterruptIoDomain {
    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        InterruptIoDomain::submit_owned(self, queue_id, logical_device, id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        InterruptIoDomain::service_evidence(self, evidence, sink)
    }
}

impl RuntimeIoDomainPort for SharedIoDomainSession<'_> {
    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        SharedIoDomainSession::submit_owned(self, queue_id, logical_device, id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        SharedIoDomainSession::service_evidence(self, evidence, sink)
    }
}
