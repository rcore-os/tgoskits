//! Hard-IRQ capture and fail-closed publication for one evidence source.

use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_hal::irq::{IrqContext, IrqReturn};
use rdif_block::{
    BBlockEvidenceEndpoint, BlkError, ContainmentCause, EvidenceClaim, EvidenceClaimToken,
    EvidenceLatch, FaultContainment, IrqCapture, IrqEventEpoch, IrqEvidenceId, IrqSourceId,
    PendingBlockIrq,
};

use super::{FaultLatchOwnership, PendingSourceFault, V13MaintenanceEvent};
use crate::{
    block::activation_v13::slot::LinearEvidenceSlot,
    maintenance::{LocalIrqWake, LocalIrqWakeError, MaintenanceCauses, MaintenancePublishResult},
};

pub(super) struct EvidenceIngress {
    pub(super) source: IrqSourceId,
    pub(super) latch: Pin<alloc::boxed::Box<EvidenceLatch>>,
    pub(super) pending: LinearEvidenceSlot<PendingBlockIrq>,
    pub(super) fault: LinearEvidenceSlot<PendingSourceFault>,
    /// True from the first successful latch claim until the same claim is
    /// linearly drained. This covers the interval where the move-only owner
    /// has left the ingress slot and is held by owner-thread service code.
    pub(super) outstanding: AtomicBool,
    /// Sticky until explicit controller recovery replaces this source epoch.
    pub(super) faulted: AtomicBool,
}

impl EvidenceIngress {
    pub(super) fn new(source: IrqSourceId) -> Self {
        Self {
            source,
            latch: alloc::boxed::Box::pin(EvidenceLatch::new(source)),
            pending: LinearEvidenceSlot::new(),
            fault: LinearEvidenceSlot::new(),
            outstanding: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
        }
    }
}

pub(in crate::block::activation_v13) struct EndpointCallbackCell {
    endpoint: UnsafeCell<BBlockEvidenceEndpoint>,
    ingress: Arc<EvidenceIngress>,
}

impl EndpointCallbackCell {
    pub(super) fn new(endpoint: BBlockEvidenceEndpoint, ingress: Arc<EvidenceIngress>) -> Self {
        Self {
            endpoint: UnsafeCell::new(endpoint),
            ingress,
        }
    }

    pub(super) fn into_endpoint(self) -> BBlockEvidenceEndpoint {
        self.endpoint.into_inner()
    }

    /// Executes one operation with the callback-owned endpoint.
    ///
    /// # Safety
    ///
    /// The caller must be the IRQ framework executing this registered action
    /// non-reentrantly on its fixed CPU. The action must remain live, and task
    /// context must not access or remove the endpoint concurrently.
    unsafe fn with_endpoint<R>(&self, access: impl FnOnce(&mut BBlockEvidenceEndpoint) -> R) -> R {
        // SAFETY: guaranteed by the caller contract above.
        access(unsafe { &mut *self.endpoint.get() })
    }
}

// SAFETY: mutable endpoint access is restricted to the IRQ framework's
// non-reentrant callback. The shared ingress uses atomics and one linear slot
// per move-only owner.
unsafe impl Send for EndpointCallbackCell {}
unsafe impl Sync for EndpointCallbackCell {}

pub(super) fn source_irq_action(
    context: IrqContext,
    owner_cpu: usize,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    callback: &EndpointCallbackCell,
    source_epoch: &mut u64,
) -> IrqReturn {
    if run_on_fixed_owner_cpu(context.cpu.0, owner_cpu, || ()).is_err() {
        return publish_wrong_cpu_fault(
            &callback.ingress,
            wake,
            source_epoch,
            BlkError::Other("block IRQ arrived outside its fixed maintenance CPU"),
        );
    }
    // SAFETY: the IRQ framework serializes this action on `owner_cpu`; the
    // endpoint is not exposed to task context while the action is live.
    unsafe {
        callback.with_endpoint(|endpoint| {
            if callback.ingress.faulted.load(Ordering::Acquire) {
                return contain_source(endpoint.as_mut(), ContainmentCause::CaptureFault);
            }
            match callback.ingress.pending.try_reserve_from_irq() {
                Ok(reservation) => capture_with_reserved_slot(
                    endpoint.as_mut(),
                    &callback.ingress,
                    wake,
                    source_epoch,
                    reservation,
                ),
                Err(_) => capture_with_outstanding_owner(
                    endpoint.as_mut(),
                    &callback.ingress,
                    wake,
                    source_epoch,
                ),
            }
        })
    }
}

fn capture_with_reserved_slot(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    reservation: super::super::slot::LinearEvidenceReservation<'_, PendingBlockIrq>,
) -> IrqReturn {
    match endpoint.capture() {
        IrqCapture::Unhandled => {
            drop(reservation);
            IrqReturn::Unhandled
        }
        IrqCapture::Captured { event, masked } => {
            let action_disabled = masked.is_some();
            let publication = match ingress.latch.as_ref().claim(event, masked) {
                Ok(EvidenceClaim::Claimed(claim)) => {
                    ingress.outstanding.store(true, Ordering::Release);
                    *source_epoch = next_nonzero_epoch(*source_epoch);
                    let epoch = IrqEventEpoch::new(*source_epoch)
                        .expect("the source epoch helper skips zero");
                    let pending = PendingBlockIrq::from_claim(claim, epoch);
                    reservation.commit(pending);
                    publish_source(wake, event.source())
                }
                Ok(EvidenceClaim::Coalesced) => {
                    drop(reservation);
                    publish_source(wake, event.source())
                }
                Err(_) => {
                    drop(reservation);
                    return contain_and_publish_fault(
                        endpoint,
                        ingress,
                        wake,
                        source_epoch,
                        BlkError::Other("IRQ evidence latch rejected capture"),
                        ContainmentCause::CaptureFault,
                    );
                }
            };
            finish_capture_publication(
                endpoint,
                ingress,
                wake,
                source_epoch,
                event,
                publication,
                action_disabled,
            )
        }
        IrqCapture::Fault {
            reason,
            containment,
        } => {
            drop(reservation);
            publish_captured_fault(endpoint, ingress, wake, source_epoch, reason, containment)
        }
    }
}

/// Captures again while the first move-only owner remains in its fixed slot.
///
/// Slot occupancy is not itself a fault: an IRQ can interrupt the owner while
/// it is taking the first value. The driver ledger and evidence latch decide
/// whether this is the same capture and therefore only a rerun fact.
fn capture_with_outstanding_owner(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
) -> IrqReturn {
    match endpoint.capture() {
        IrqCapture::Unhandled => IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => {
            let action_disabled = masked.is_some();
            match claim_with_outstanding_owner(ingress.latch.as_ref(), event, masked) {
                Ok(OutstandingOwnerClaim::Coalesced) => finish_capture_publication(
                    endpoint,
                    ingress,
                    wake,
                    source_epoch,
                    event,
                    publish_source(wake, event.source()),
                    action_disabled,
                ),
                Ok(OutstandingOwnerClaim::Conflicting(claim)) => publish_conflicting_claim_fault(
                    endpoint,
                    ingress,
                    wake,
                    source_epoch,
                    event,
                    masked,
                    claim,
                ),
                Err(_) => contain_and_publish_fault(
                    endpoint,
                    ingress,
                    wake,
                    source_epoch,
                    BlkError::Other("IRQ evidence latch rejected overlapping capture"),
                    ContainmentCause::CaptureFault,
                ),
            }
        }
        IrqCapture::Fault {
            reason,
            containment,
        } => publish_captured_fault(endpoint, ingress, wake, source_epoch, reason, containment),
    }
}

pub(super) enum OutstandingOwnerClaim {
    Coalesced,
    Conflicting(EvidenceClaimToken),
}

pub(super) fn claim_with_outstanding_owner(
    latch: Pin<&EvidenceLatch>,
    evidence: IrqEvidenceId,
    masked: Option<rdif_block::MaskedSource>,
) -> Result<OutstandingOwnerClaim, rdif_block::EvidenceLatchError> {
    match latch.claim(evidence, masked)? {
        EvidenceClaim::Coalesced => Ok(OutstandingOwnerClaim::Coalesced),
        EvidenceClaim::Claimed(claim) => Ok(OutstandingOwnerClaim::Conflicting(claim)),
    }
}

fn publish_conflicting_claim_fault(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    evidence: IrqEvidenceId,
    captured_mask: Option<rdif_block::MaskedSource>,
    claim: EvidenceClaimToken,
) -> IrqReturn {
    let reservation = match ingress.fault.try_reserve_from_irq() {
        Ok(reservation) => reservation,
        Err(_) => {
            ingress.faulted.store(true, Ordering::Release);
            return contain_source(endpoint, ContainmentCause::CaptureFault);
        }
    };
    let (containment, containment_error, containment_claim) = contain_captured_evidence(
        endpoint,
        ingress.latch.as_ref(),
        evidence,
        captured_mask,
        ContainmentCause::CaptureFault,
    );
    finish_fault_owner_publication(
        ingress,
        wake,
        source_epoch,
        FaultPublication {
            reason: BlkError::Other(
                "IRQ capture minted a claim while another owner slot remained live",
            ),
            containment,
            containment_error,
            latch_ownership: FaultLatchOwnership::Claimed,
            conflicting_claims: [Some(claim), containment_claim],
        },
        reservation,
    )
}

pub(super) fn run_on_fixed_owner_cpu<T>(
    actual_cpu: usize,
    owner_cpu: usize,
    access_endpoint: impl FnOnce() -> T,
) -> Result<T, WrongOwnerCpu> {
    if actual_cpu != owner_cpu {
        return Err(WrongOwnerCpu);
    }
    Ok(access_endpoint())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WrongOwnerCpu;

fn publish_wrong_cpu_fault(
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    reason: BlkError,
) -> IrqReturn {
    let reservation = match ingress.fault.try_reserve_from_irq() {
        Ok(reservation) => reservation,
        Err(_) => {
            ingress.faulted.store(true, Ordering::Release);
            return IrqReturn::MaskLineAndWake;
        }
    };
    finish_fault_owner_publication(
        ingress,
        wake,
        source_epoch,
        FaultPublication {
            reason,
            containment: FaultContainment::Uncontained,
            containment_error: None,
            latch_ownership: FaultLatchOwnership::Untouched,
            conflicting_claims: [None, None],
        },
        reservation,
    )
}

fn publish_source(
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source: IrqSourceId,
) -> Result<MaintenancePublishResult, LocalIrqWakeError> {
    wake.publish_from_irq(MaintenanceCauses::IRQ, V13MaintenanceEvent::Irq { source })
}

fn finish_capture_publication(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    evidence: IrqEvidenceId,
    publication: Result<MaintenancePublishResult, LocalIrqWakeError>,
    action_disabled: bool,
) -> IrqReturn {
    match publication {
        Ok(MaintenancePublishResult::Published) => {
            if action_disabled {
                IrqReturn::DisableActionAndWake
            } else {
                IrqReturn::Wake
            }
        }
        Ok(MaintenancePublishResult::Overflowed) => contain_and_publish_capture_failure(
            endpoint,
            ingress,
            wake,
            source_epoch,
            evidence,
            ContainmentCause::PublicationFull,
        ),
        Err(error) => contain_and_publish_capture_failure(
            endpoint,
            ingress,
            wake,
            source_epoch,
            evidence,
            containment_cause(error),
        ),
    }
}

fn contain_and_publish_capture_failure(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    evidence: IrqEvidenceId,
    cause: ContainmentCause,
) -> IrqReturn {
    let reservation = match ingress.fault.try_reserve_from_irq() {
        Ok(reservation) => reservation,
        Err(_) => {
            ingress.faulted.store(true, Ordering::Release);
            return contain_source(endpoint, cause);
        }
    };
    let (containment, containment_error, conflicting_claim) =
        contain_captured_evidence(endpoint, ingress.latch.as_ref(), evidence, None, cause);
    let reason = match cause {
        ContainmentCause::PublicationClosed => {
            BlkError::Other("IRQ evidence publication closed after capture")
        }
        ContainmentCause::PublicationFull => {
            BlkError::Other("IRQ evidence mailbox overflowed after capture")
        }
        ContainmentCause::OwnerUnavailable => {
            BlkError::Other("IRQ evidence owner became unavailable after capture")
        }
        ContainmentCause::CaptureFault => {
            BlkError::Other("IRQ evidence capture failed containment")
        }
    };
    finish_fault_owner_publication(
        ingress,
        wake,
        source_epoch,
        FaultPublication {
            reason,
            containment,
            containment_error,
            latch_ownership: FaultLatchOwnership::Claimed,
            conflicting_claims: [conflicting_claim, None],
        },
        reservation,
    )
}

fn contain_captured_evidence(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = IrqEvidenceId, Fault = BlkError>,
    latch: Pin<&EvidenceLatch>,
    evidence: IrqEvidenceId,
    captured_mask: Option<rdif_block::MaskedSource>,
    cause: ContainmentCause,
) -> (
    FaultContainment,
    Option<BlkError>,
    Option<EvidenceClaimToken>,
) {
    let (containment, mut containment_error) = match captured_mask {
        Some(masked) => (FaultContainment::DeviceSourceMasked(masked), None),
        None => match endpoint.contain(cause) {
            Ok(masked) => (FaultContainment::DeviceSourceMasked(masked), None),
            Err(error) => (FaultContainment::Uncontained, Some(error)),
        },
    };
    let conflicting_claim = match containment {
        FaultContainment::DeviceSourceMasked(masked) => match latch.claim(evidence, Some(masked)) {
            Ok(EvidenceClaim::Coalesced) => None,
            Ok(EvidenceClaim::Claimed(claim)) => Some(claim),
            Err(_) => {
                if containment_error.is_none() {
                    containment_error = Some(BlkError::Other(
                        "IRQ evidence latch rejected containment mask",
                    ));
                }
                None
            }
        },
        FaultContainment::Uncontained => None,
    };
    (containment, containment_error, conflicting_claim)
}

fn publish_captured_fault(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = rdif_block::IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    reason: BlkError,
    containment: FaultContainment,
) -> IrqReturn {
    let reservation = match ingress.fault.try_reserve_from_irq() {
        Ok(reservation) => reservation,
        Err(_) => return contain_source(endpoint, ContainmentCause::CaptureFault),
    };
    let (containment, containment_error) = match containment {
        masked @ FaultContainment::DeviceSourceMasked(_) => (masked, None),
        FaultContainment::Uncontained => match endpoint.contain(ContainmentCause::CaptureFault) {
            Ok(masked) => (FaultContainment::DeviceSourceMasked(masked), None),
            Err(error) => (FaultContainment::Uncontained, Some(error)),
        },
    };
    finish_fault_owner_publication(
        ingress,
        wake,
        source_epoch,
        FaultPublication {
            reason,
            containment,
            containment_error,
            latch_ownership: if ingress.outstanding.load(Ordering::Acquire) {
                FaultLatchOwnership::Claimed
            } else {
                FaultLatchOwnership::Untouched
            },
            conflicting_claims: [None, None],
        },
        reservation,
    )
}

fn contain_and_publish_fault(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = rdif_block::IrqEvidenceId, Fault = BlkError>,
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    reason: BlkError,
    cause: ContainmentCause,
) -> IrqReturn {
    let reservation = match ingress.fault.try_reserve_from_irq() {
        Ok(reservation) => reservation,
        Err(_) => return contain_source(endpoint, cause),
    };
    let (containment, containment_error) = match endpoint.contain(cause) {
        Ok(masked) => (FaultContainment::DeviceSourceMasked(masked), None),
        Err(error) => (FaultContainment::Uncontained, Some(error)),
    };
    finish_fault_owner_publication(
        ingress,
        wake,
        source_epoch,
        FaultPublication {
            reason,
            containment,
            containment_error,
            latch_ownership: FaultLatchOwnership::Unrecoverable,
            conflicting_claims: [None, None],
        },
        reservation,
    )
}

struct FaultPublication {
    reason: BlkError,
    containment: FaultContainment,
    containment_error: Option<BlkError>,
    latch_ownership: FaultLatchOwnership,
    conflicting_claims: [Option<EvidenceClaimToken>; 2],
}

fn finish_fault_owner_publication(
    ingress: &EvidenceIngress,
    wake: &LocalIrqWake<V13MaintenanceEvent>,
    source_epoch: &mut u64,
    fault: FaultPublication,
    reservation: super::super::slot::LinearEvidenceReservation<'_, PendingSourceFault>,
) -> IrqReturn {
    *source_epoch = next_nonzero_epoch(*source_epoch);
    let source_epoch =
        IrqEventEpoch::new(*source_epoch).expect("the source epoch helper skips zero");
    ingress.faulted.store(true, Ordering::Release);
    reservation.commit(PendingSourceFault {
        source: ingress.source,
        source_epoch,
        reason: fault.reason,
        containment: fault.containment,
        containment_error: fault.containment_error,
        latch_ownership: fault.latch_ownership,
        conflicting_claims: fault.conflicting_claims,
    });
    match publish_source(wake, ingress.source) {
        Ok(MaintenancePublishResult::Published | MaintenancePublishResult::Overflowed) => {
            match fault.containment {
                FaultContainment::DeviceSourceMasked(_) => IrqReturn::DisableActionAndWake,
                FaultContainment::Uncontained => IrqReturn::MaskLineAndWake,
            }
        }
        Err(_) => {
            // The unique fault owner remains in the callback-owned slot. The
            // lifecycle must become fail closed before returning so a masked
            // source cannot be mistaken for successful owner notification.
            let _ = wake.fail_closed_from_irq();
            IrqReturn::MaskLineAndWake
        }
    }
}

fn contain_source(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = rdif_block::IrqEvidenceId, Fault = BlkError>,
    cause: ContainmentCause,
) -> IrqReturn {
    match endpoint.contain(cause) {
        Ok(_) => IrqReturn::DisableActionAndWake,
        Err(_) => IrqReturn::MaskLineAndWake,
    }
}

fn containment_cause(error: LocalIrqWakeError) -> ContainmentCause {
    match error {
        LocalIrqWakeError::Closed => ContainmentCause::PublicationClosed,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable { .. } => ContainmentCause::OwnerUnavailable,
    }
}

fn next_nonzero_epoch(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

#[derive(Debug)]
pub(super) enum RearmTransitionFailure<P, R, A> {
    Enable {
        permit: P,
        error: A,
    },
    Rearm {
        permit: P,
        error: R,
        containment: Result<(), A>,
    },
}

/// Restores the OS action while the device source is still masked, then
/// consumes the exact device rearm permit. A failed device rearm closes the
/// action again before returning the same move-only permit.
pub(super) fn enable_action_then_rearm<P, T, R, A>(
    permit: P,
    enable_action: impl FnOnce() -> Result<(), A>,
    rearm_source: impl FnOnce(P) -> Result<T, (P, R)>,
    disable_action: impl FnOnce() -> Result<(), A>,
) -> Result<T, RearmTransitionFailure<P, R, A>> {
    if let Err(error) = enable_action() {
        return Err(RearmTransitionFailure::Enable { permit, error });
    }
    match rearm_source(permit) {
        Ok(value) => Ok(value),
        Err((permit, error)) => Err(RearmTransitionFailure::Rearm {
            permit,
            error,
            containment: disable_action(),
        }),
    }
}
