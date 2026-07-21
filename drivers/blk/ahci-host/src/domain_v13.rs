//! Owner-thread I/O domain for the rdif-block v0.13 AHCI boundary.

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::{NonZeroU64, NonZeroUsize},
    sync::atomic::{AtomicU64, Ordering},
};

use rdif_block::{
    AcceptedRequest, BlkError, CompletionSink, ControllerEpoch, ControllerFault, DmaQuiesced,
    DriverDeviceKey, DriverEvidenceRetirement, EvidenceServiceResult, IQueue, InterruptIoDomain,
    IrqEvidenceId, OwnedRequest, OwnershipDomainId, RecoveryEvidenceRetireFailure,
    RecoveryEvidenceRetirePermit, RecoveryEvidenceRetired, RequestId, UnacceptedRequest,
};

use crate::{
    evidence::{AhciEvidenceDisposition, AhciEvidenceLedger},
    irq::HostShared,
    queue::AhciPortQueue,
    registers::MAX_PORTS,
};

pub(crate) struct AhciV13IoDomain {
    id: OwnershipDomainId,
    ledger: Arc<AhciEvidenceLedger>,
    shared: Arc<HostShared>,
    ports: Vec<AhciDomainPort>,
    controller_cookie: NonZeroUsize,
    last_reclaim_epoch: Option<ControllerEpoch>,
    resumed_epoch: Option<ControllerEpoch>,
    recovery_epoch: Arc<AhciDomainRecoveryEpoch>,
    fault: Option<ControllerFault>,
}

/// Release-published receipt that the sole AHCI I/O domain reclaimed an epoch.
pub(crate) struct AhciDomainRecoveryEpoch {
    reclaimed: AtomicU64,
}

pub(crate) struct AhciDomainPort {
    port: usize,
    queue: Option<AhciPortQueue>,
}

impl AhciDomainPort {
    pub(crate) const fn new(port: usize, queue: Option<AhciPortQueue>) -> Self {
        Self { port, queue }
    }
}

impl AhciV13IoDomain {
    pub(crate) fn new(
        id: OwnershipDomainId,
        ledger: Arc<AhciEvidenceLedger>,
        shared: Arc<HostShared>,
        ports: Vec<AhciDomainPort>,
        controller_cookie: NonZeroUsize,
        recovery_epoch: Arc<AhciDomainRecoveryEpoch>,
    ) -> Self {
        Self {
            id,
            ledger,
            shared,
            ports,
            controller_cookie,
            last_reclaim_epoch: None,
            resumed_epoch: None,
            recovery_epoch,
            fault: None,
        }
    }

    fn port_mut(&mut self, port: usize) -> Option<&mut AhciDomainPort> {
        self.ports.iter_mut().find(|entry| entry.port == port)
    }

    fn scan_evidence(&mut self, port_facts: u32) -> Result<u32, ControllerFault> {
        let mut retained = 0_u32;
        for port in 0..MAX_PORTS {
            let bit = 1_u32 << port;
            if port_facts & bit == 0 {
                continue;
            }
            let Some(entry) = self.port_mut(port) else {
                return Err(ControllerFault::Ownership);
            };
            if let Some(queue) = &mut entry.queue {
                let scan = queue
                    .scan_v13_evidence()
                    .map_err(|_| ControllerFault::Protocol)?;
                if scan.retained {
                    retained |= bit;
                }
            } else if scan_unrouted_port(&self.shared, port)? {
                retained |= bit;
            }
        }
        Ok(retained)
    }
}

impl InterruptIoDomain for AhciV13IoDomain {
    fn domain_id(&self) -> OwnershipDomainId {
        self.id
    }

    fn queue_count(&self) -> usize {
        self.ports.len()
    }

    fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        if self.fault.is_some() {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        }
        let Some(port) = self.port_mut(queue_id) else {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        };
        if driver_key_for_port(port.port) != logical_device {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        }
        let Some(queue) = &mut port.queue else {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        };
        queue.submit_v13(id, request)
    }

    fn service_evidence(
        &mut self,
        evidence: IrqEvidenceId,
        sink: &mut dyn CompletionSink,
    ) -> Result<EvidenceServiceResult, BlkError> {
        if let Some(fault) = self.fault {
            return Ok(EvidenceServiceResult::Recover(fault));
        }
        let ledger = Arc::clone(&self.ledger);
        let batch = match ledger.begin_service(evidence) {
            Ok(batch) => batch,
            Err(_) => {
                self.fault = Some(ControllerFault::Ownership);
                return Ok(EvidenceServiceResult::Recover(ControllerFault::Ownership));
            }
        };
        let port_facts = batch.port_facts();
        let retained = match self.scan_evidence(port_facts) {
            Ok(retained) => retained,
            Err(fault) => {
                self.fault = Some(fault);
                return Ok(EvidenceServiceResult::Recover(fault));
            }
        };
        if retained == 0 {
            for port in &mut self.ports {
                if let Some(queue) = &mut port.queue
                    && queue.commit_v13_completion(sink).is_err()
                {
                    self.fault = Some(ControllerFault::Protocol);
                    return Ok(EvidenceServiceResult::Recover(ControllerFault::Protocol));
                }
            }
        }
        Ok(evidence_result(ledger.finish_service(batch, retained)))
    }

    fn commit_drained_evidence(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError> {
        self.ledger
            .commit_drained_evidence(evidence)
            .map_err(|_| BlkError::Other("AHCI I/O evidence commit is invalid"))
    }

    fn retire_recovery_evidence(
        &mut self,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Result<RecoveryEvidenceRetired, RecoveryEvidenceRetireFailure> {
        self.ledger
            .retire_after_quiesce(permit, self.controller_cookie)
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if proof.controller_cookie() != self.controller_cookie.get()
            || self
                .last_reclaim_epoch
                .is_some_and(|epoch| proof.epoch() <= epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        for port in &mut self.ports {
            if let Some(queue) = &mut port.queue {
                IQueue::reclaim_after_quiesce(queue, proof, sink)?;
            }
        }
        self.last_reclaim_epoch = Some(proof.epoch());
        self.resumed_epoch = None;
        self.recovery_epoch.publish(proof.epoch());
        Ok(())
    }

    fn resume_after_reinitialize(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        if self.last_reclaim_epoch != Some(epoch) || self.resumed_epoch == Some(epoch) {
            return Err(BlkError::InvalidDmaProof);
        }
        for port in &mut self.ports {
            if self.shared.port(port.port).epoch() != epoch.get()
                || !self.shared.port(port.port).is_online()
            {
                return Err(BlkError::InvalidDmaProof);
            }
            if let Some(queue) = &mut port.queue {
                queue.resume_v13(epoch)?;
            }
        }
        self.fault = None;
        self.resumed_epoch = Some(epoch);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        for port in &mut self.ports {
            if let Some(queue) = &mut port.queue {
                IQueue::shutdown(queue)?;
            }
        }
        Ok(())
    }
}

impl AhciDomainRecoveryEpoch {
    pub(crate) const fn new() -> Self {
        Self {
            reclaimed: AtomicU64::new(0),
        }
    }

    pub(crate) fn matches(&self, epoch: ControllerEpoch) -> bool {
        epoch.get() != 0 && self.reclaimed.load(Ordering::Acquire) == epoch.get()
    }

    fn publish(&self, epoch: ControllerEpoch) {
        self.reclaimed.store(epoch.get(), Ordering::Release);
    }
}

pub(crate) fn driver_key_for_port(port: usize) -> DriverDeviceKey {
    let value = u64::try_from(port)
        .ok()
        .and_then(|port| port.checked_add(1))
        .and_then(NonZeroU64::new)
        .unwrap_or_else(|| unreachable!("AHCI port identities fit in nonzero u64"));
    DriverDeviceKey::new(value)
}

pub(crate) const fn evidence_result(disposition: AhciEvidenceDisposition) -> EvidenceServiceResult {
    match disposition {
        AhciEvidenceDisposition::Drained => EvidenceServiceResult::Drained,
        AhciEvidenceDisposition::Retained => EvidenceServiceResult::Retained,
        AhciEvidenceDisposition::Invalid => {
            EvidenceServiceResult::Recover(ControllerFault::Ownership)
        }
    }
}

fn scan_unrouted_port(shared: &HostShared, port: usize) -> Result<bool, ControllerFault> {
    if shared.port(port).take_overflow() {
        return Err(ControllerFault::Protocol);
    }
    for _ in 0..crate::irq::IRQ_SNAPSHOT_CAPACITY {
        let Some(snapshot) = shared.port(port).pop_snapshot() else {
            break;
        };
        if snapshot.has_error() || snapshot.request_generation != 0 {
            return Err(ControllerFault::Protocol);
        }
    }
    Ok(shared.port(port).has_snapshots())
}
