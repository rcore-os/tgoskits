//! Linear retirement of IRQ evidence after controller DMA quiescence.

use alloc::boxed::Box;
use core::{fmt, marker::PhantomData, num::NonZeroUsize, pin::Pin};

use rdif_block::{
    ControllerEpoch, ControllerFault, DmaQuiesced, EvidenceRetireError, FaultContainment,
    IrqSourceId, PendingBlockIrq, QuiescedEvidence, QuiescedEvidenceCompletion, RearmPermit,
    RecoveryEvidenceRetireFailure, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired as DriverEvidenceRetired,
};

use super::{EvidenceIngress, FaultLatchOwnership, PendingSourceFault};

#[cfg(test)]
use rdif_block::IrqEvidenceId;

/// Portable driver owner that originally serviced one IRQ evidence identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::block::activation_v13) enum DriverEvidenceRoute {
    Control,
    Io,
}

/// Terminal driver-ledger receipt paired with its exact local owner route.
#[derive(Debug)]
pub(super) struct RoutedDriverEvidenceRetired {
    route: DriverEvidenceRoute,
    receipt: DriverEvidenceRetired,
}

impl RoutedDriverEvidenceRetired {
    pub(super) const fn route(&self) -> DriverEvidenceRoute {
        self.route
    }

    pub(super) const fn receipt(&self) -> &DriverEvidenceRetired {
        &self.receipt
    }
}

/// Driver-ledger failure that preserves the exact move-only retirement permit.
#[must_use = "retry with the same driver route or quarantine the retained permit"]
pub(in crate::block::activation_v13) struct DriverEvidenceRetireFailure {
    error: rdif_block::BlkError,
    permit: RecoveryEvidenceRetirePermit,
}

impl DriverEvidenceRetireFailure {
    pub(in crate::block::activation_v13) const fn new(
        error: rdif_block::BlkError,
        permit: RecoveryEvidenceRetirePermit,
    ) -> Self {
        Self { error, permit }
    }

    pub(in crate::block::activation_v13) fn from_driver(
        failure: RecoveryEvidenceRetireFailure,
    ) -> Self {
        let (error, permit) = failure.into_parts();
        Self { error, permit }
    }

    fn into_parts(self) -> (rdif_block::BlkError, RecoveryEvidenceRetirePermit) {
        (self.error, self.permit)
    }
}

/// Recovery-bound evidence retained after the driver rejects normal drain.
#[derive(Debug)]
pub(super) struct RecoveryBoundEvidence {
    source: IrqSourceId,
    controller_identity: NonZeroUsize,
    route: DriverEvidenceRoute,
    owner: RecoveryEvidenceOwner,
    fault: ControllerFault,
    rearm: Option<RearmPermit>,
}

impl RecoveryBoundEvidence {
    /// Binds a driver recovery decision to its exact runtime source.
    #[cfg(test)]
    pub(super) fn new(
        source: IrqSourceId,
        pending: PendingBlockIrq,
        fault: ControllerFault,
        controller_identity: NonZeroUsize,
    ) -> Result<Self, RecoveryBindingFailure> {
        Self::new_with_rearm(
            source,
            pending,
            fault,
            controller_identity,
            DriverEvidenceRoute::Control,
            None,
        )
    }

    pub(super) fn new_with_rearm(
        source: IrqSourceId,
        pending: PendingBlockIrq,
        fault: ControllerFault,
        controller_identity: NonZeroUsize,
        route: DriverEvidenceRoute,
        rearm: Option<RearmPermit>,
    ) -> Result<Self, RecoveryBindingFailure> {
        let captured = pending.evidence_id().source();
        if captured != source {
            return Err(RecoveryBindingFailure {
                reason: RecoveryBindingReason::WrongSource {
                    configured: source,
                    captured,
                },
                pending,
                fault,
                route,
                rearm,
            });
        }
        Ok(Self {
            source,
            controller_identity,
            route,
            owner: RecoveryEvidenceOwner::Pending(pending),
            fault,
            rearm,
        })
    }

    /// Retires the latch without rearming after matching DMA quiescence.
    pub(super) fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        latch: Pin<&rdif_block::EvidenceLatch>,
        retire_driver: impl FnOnce(
            DriverEvidenceRoute,
            RecoveryEvidenceRetirePermit,
        ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
    ) -> Result<RecoveryEvidenceProgress, RecoveryRetireFailure> {
        let Self {
            source,
            controller_identity,
            route,
            owner,
            fault,
            rearm,
        } = self;
        if let Some(rearm) = rearm
            && let Err(failure) =
                rearm.retire_after_quiesce(proof, controller_identity.get())
        {
            let (rearm, _error) = failure.into_parts();
            return Err(RecoveryRetireFailure {
                reason: RecoveryRetireReason::ForeignController,
                owner: Self {
                    source,
                    controller_identity,
                    route,
                    owner,
                    fault,
                    rearm: Some(rearm),
                },
            });
        }
        let quiesced = match owner {
            RecoveryEvidenceOwner::Pending(pending) => {
                match pending.retire_after_quiesce(proof, controller_identity.get()) {
                    Ok(quiesced) => quiesced,
                    Err(failure) => {
                        let (error, pending) = failure.into_parts();
                        return Err(RecoveryRetireFailure {
                            reason: error.into(),
                            owner: Self {
                                source,
                                controller_identity,
                                route,
                                owner: RecoveryEvidenceOwner::Pending(pending),
                                fault,
                                rearm: None,
                            },
                        });
                    }
                }
            }
            RecoveryEvidenceOwner::Quiesced(quiesced) => quiesced,
            RecoveryEvidenceOwner::DriverRetire(permit) => {
                return retire_driver_recovery_evidence(
                    source,
                    controller_identity,
                    route,
                    fault,
                    permit,
                    retire_driver,
                );
            }
        };
        match quiesced.complete(latch) {
            Ok(QuiescedEvidenceCompletion::Complete { permit }) => {
                retire_driver_recovery_evidence(
                    source,
                    controller_identity,
                    route,
                    fault,
                    permit,
                    retire_driver,
                )
            }
            Ok(QuiescedEvidenceCompletion::Redeliver(quiesced)) => {
                Ok(RecoveryEvidenceProgress::More(Self {
                    source,
                    controller_identity,
                    route,
                    owner: RecoveryEvidenceOwner::Quiesced(quiesced),
                    fault,
                    rearm: None,
                }))
            }
            Err((quiesced, error)) => Err(RecoveryRetireFailure {
                reason: RecoveryRetireReason::Latch(error),
                owner: Self {
                    source,
                    controller_identity,
                    route,
                    owner: RecoveryEvidenceOwner::Quiesced(quiesced),
                    fault,
                    rearm: None,
                },
            }),
        }
    }
}

fn retire_driver_recovery_evidence(
    source: IrqSourceId,
    controller_identity: NonZeroUsize,
    route: DriverEvidenceRoute,
    fault: ControllerFault,
    permit: RecoveryEvidenceRetirePermit,
    retire_driver: impl FnOnce(
        DriverEvidenceRoute,
        RecoveryEvidenceRetirePermit,
    ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
) -> Result<RecoveryEvidenceProgress, RecoveryRetireFailure> {
    match retire_driver(route, permit) {
        Ok(receipt) => Ok(RecoveryEvidenceProgress::Retired(
            RecoveryEvidenceRetired {
                source,
                fault,
                driver: RoutedDriverEvidenceRetired { route, receipt },
            },
        )),
        Err(failure) => {
            let (error, permit) = failure.into_parts();
            Err(RecoveryRetireFailure {
                reason: RecoveryRetireReason::DriverLedger(error),
                owner: RecoveryBoundEvidence {
                    source,
                    controller_identity,
                    route,
                    owner: RecoveryEvidenceOwner::DriverRetire(permit),
                    fault,
                    rearm: None,
                },
            })
        }
    }
}

/// A contained callback fault bound to one exact controller recovery epoch.
///
/// Unlike a driver-requested recovery, a callback fault may have happened
/// before an evidence claim existed. `FaultLatchOwnership` records whether no
/// claim is expected or exactly one claim must be retired. Ambiguous or
/// already-faulted latch state never enters this type.
#[derive(Debug)]
pub(super) struct ContainedSourceFaultRecovery {
    source: IrqSourceId,
    controller_identity: NonZeroUsize,
    quiesce_epoch: ControllerEpoch,
    route: DriverEvidenceRoute,
    transaction: Box<ContainedSourceFaultTransaction>,
}

impl ContainedSourceFaultRecovery {
    /// Validates containment and the complete single-owner latch transaction.
    pub(super) fn bind(
        source: IrqSourceId,
        mut fault: PendingSourceFault,
        pending: Option<PendingBlockIrq>,
        controller_identity: NonZeroUsize,
        quiesce_epoch: ControllerEpoch,
        route: DriverEvidenceRoute,
    ) -> Result<Self, ContainedSourceFaultBindingFailure> {
        let fail = ContainedSourceFaultBindingFailure::new;
        if fault.source != source {
            return Err(fail(
                ContainedSourceFaultBindingReason::WrongSource {
                    configured: source,
                    captured: fault.source,
                },
                fault,
                pending,
            ));
        }
        let masked = match fault.containment {
            FaultContainment::DeviceSourceMasked(masked) => masked,
            FaultContainment::Uncontained => {
                return Err(fail(
                    ContainedSourceFaultBindingReason::Uncontained,
                    fault,
                    pending,
                ));
            }
        };
        if fault.containment_error.is_some() {
            return Err(fail(
                ContainedSourceFaultBindingReason::ContainmentIncomplete,
                fault,
                pending,
            ));
        }
        let claim_count = usize::from(pending.is_some())
            + fault
                .conflicting_claims
                .iter()
                .filter(|claim| claim.is_some())
                .count();
        let expected_claims = match fault.latch_ownership {
            FaultLatchOwnership::Untouched => 0,
            FaultLatchOwnership::Claimed => 1,
            FaultLatchOwnership::Unrecoverable => {
                return Err(fail(
                    ContainedSourceFaultBindingReason::UnrecoverableLatch,
                    fault,
                    pending,
                ));
            }
        };
        if claim_count != expected_claims {
            return Err(fail(
                ContainedSourceFaultBindingReason::ClaimOwnershipMismatch {
                    expected: expected_claims,
                    actual: claim_count,
                },
                fault,
                pending,
            ));
        }

        let pending = match pending {
            Some(pending) => Some(pending),
            None => fault
                .conflicting_claims
                .iter_mut()
                .find_map(Option::take)
                .map(|claim| PendingBlockIrq::from_claim(claim, fault.source_epoch)),
        };
        if let Some(evidence) = pending.as_ref().map(PendingBlockIrq::evidence_id) {
            if evidence.source() != source {
                return Err(fail(
                    ContainedSourceFaultBindingReason::WrongClaimSource {
                        configured: source,
                        captured: evidence.source(),
                    },
                    fault,
                    pending,
                ));
            }
            if evidence.device_generation() != masked.lifecycle_generation() {
                return Err(fail(
                    ContainedSourceFaultBindingReason::MaskGenerationMismatch {
                        evidence: evidence.device_generation().get(),
                        masked: masked.lifecycle_generation().get(),
                    },
                    fault,
                    pending,
                ));
            }
        }

        Ok(Self {
            source,
            controller_identity,
            quiesce_epoch,
            route,
            transaction: Box::new(ContainedSourceFaultTransaction {
                fault,
                evidence: pending.map(ContainedFaultEvidenceOwner::Pending),
            }),
        })
    }

    /// Retires the exact latch claim, if any, without authorizing source rearm.
    pub(super) fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        latch: Pin<&rdif_block::EvidenceLatch>,
        retire_driver: impl FnOnce(
            DriverEvidenceRoute,
            RecoveryEvidenceRetirePermit,
        ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
    ) -> Result<ContainedSourceFaultRecoveryProgress, ContainedSourceFaultRetireFailure> {
        if proof.controller_cookie() != self.controller_identity.get() {
            return Err(ContainedSourceFaultRetireFailure {
                reason: ContainedSourceFaultRetireReason::ForeignController,
                owner: self,
            });
        }
        if proof.epoch() != self.quiesce_epoch {
            return Err(ContainedSourceFaultRetireFailure {
                reason: ContainedSourceFaultRetireReason::StaleEpoch {
                    expected: self.quiesce_epoch,
                    actual: proof.epoch(),
                },
                owner: self,
            });
        }
        let Self {
            source,
            controller_identity,
            quiesce_epoch,
            route,
            transaction,
        } = self;
        let ContainedSourceFaultTransaction { fault, evidence } = *transaction;
        let Some(evidence) = evidence else {
            return Ok(ContainedSourceFaultRecoveryProgress::Retired(
                ContainedSourceFaultRetired {
                    source,
                    fault,
                    driver: None,
                },
            ));
        };
        let quiesced = match evidence {
            ContainedFaultEvidenceOwner::Pending(pending) => {
                match pending.retire_after_quiesce(proof, controller_identity.get()) {
                    Ok(quiesced) => quiesced,
                    Err(failure) => {
                        let (_error, pending) = failure.into_parts();
                        return Err(ContainedSourceFaultRetireFailure {
                            reason: ContainedSourceFaultRetireReason::ForeignController,
                            owner: Self {
                                source,
                                controller_identity,
                                quiesce_epoch,
                                route,
                                transaction: Box::new(ContainedSourceFaultTransaction {
                                    fault,
                                    evidence: Some(ContainedFaultEvidenceOwner::Pending(pending)),
                                }),
                            },
                        });
                    }
                }
            }
            ContainedFaultEvidenceOwner::Quiesced(quiesced) => quiesced,
            ContainedFaultEvidenceOwner::DriverRetire(permit) => {
                return retire_contained_driver_evidence(
                    source,
                    controller_identity,
                    quiesce_epoch,
                    route,
                    fault,
                    permit,
                    retire_driver,
                );
            }
        };
        match quiesced.complete(latch) {
            Ok(QuiescedEvidenceCompletion::Complete { permit }) => {
                retire_contained_driver_evidence(
                    source,
                    controller_identity,
                    quiesce_epoch,
                    route,
                    fault,
                    permit,
                    retire_driver,
                )
            }
            Ok(QuiescedEvidenceCompletion::Redeliver(quiesced)) => {
                Ok(ContainedSourceFaultRecoveryProgress::More(Self {
                    source,
                    controller_identity,
                    quiesce_epoch,
                    route,
                    transaction: Box::new(ContainedSourceFaultTransaction {
                        fault,
                        evidence: Some(ContainedFaultEvidenceOwner::Quiesced(quiesced)),
                    }),
                }))
            }
            Err((quiesced, error)) => Err(ContainedSourceFaultRetireFailure {
                reason: ContainedSourceFaultRetireReason::Latch(error),
                owner: Self {
                    source,
                    controller_identity,
                    quiesce_epoch,
                    route,
                    transaction: Box::new(ContainedSourceFaultTransaction {
                        fault,
                        evidence: Some(ContainedFaultEvidenceOwner::Quiesced(quiesced)),
                    }),
                },
            }),
        }
    }
}

fn retire_contained_driver_evidence(
    source: IrqSourceId,
    controller_identity: NonZeroUsize,
    quiesce_epoch: ControllerEpoch,
    route: DriverEvidenceRoute,
    fault: PendingSourceFault,
    permit: RecoveryEvidenceRetirePermit,
    retire_driver: impl FnOnce(
        DriverEvidenceRoute,
        RecoveryEvidenceRetirePermit,
    ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
) -> Result<ContainedSourceFaultRecoveryProgress, ContainedSourceFaultRetireFailure> {
    match retire_driver(route, permit) {
        Ok(receipt) => Ok(ContainedSourceFaultRecoveryProgress::Retired(
            ContainedSourceFaultRetired {
                source,
                fault,
                driver: Some(RoutedDriverEvidenceRetired { route, receipt }),
            },
        )),
        Err(failure) => {
            let (error, permit) = failure.into_parts();
            Err(ContainedSourceFaultRetireFailure {
                reason: ContainedSourceFaultRetireReason::DriverLedger(error),
                owner: ContainedSourceFaultRecovery {
                    source,
                    controller_identity,
                    quiesce_epoch,
                    route,
                    transaction: Box::new(ContainedSourceFaultTransaction {
                        fault,
                        evidence: Some(ContainedFaultEvidenceOwner::DriverRetire(permit)),
                    }),
                },
            })
        }
    }
}

/// Move-only callback-fault state retained through DMA quiescence.
///
/// This transaction is boxed because `PendingSourceFault` deliberately keeps
/// every conflicting claim owner for quarantine. Keeping that rare recovery
/// state out of the surrounding typestates prevents every normal source
/// transition and error result from inheriting its stack footprint.
#[derive(Debug)]
struct ContainedSourceFaultTransaction {
    fault: PendingSourceFault,
    evidence: Option<ContainedFaultEvidenceOwner>,
}

#[derive(Debug)]
enum ContainedFaultEvidenceOwner {
    Pending(PendingBlockIrq),
    Quiesced(QuiescedEvidence),
    DriverRetire(RecoveryEvidenceRetirePermit),
}

/// Why a callback fault cannot enter recoverable containment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum ContainedSourceFaultBindingReason {
    #[error("source fault belongs to {captured:?}, not runtime source {configured:?}")]
    WrongSource {
        configured: IrqSourceId,
        captured: IrqSourceId,
    },
    #[error("source fault did not mask its exact device source")]
    Uncontained,
    #[error("source fault reported an additional containment failure")]
    ContainmentIncomplete,
    #[error("source fault left its evidence latch unrecoverably faulted")]
    UnrecoverableLatch,
    #[error("source fault expected {expected} claim owners, observed {actual}")]
    ClaimOwnershipMismatch { expected: usize, actual: usize },
    #[error("fault claim belongs to {captured:?}, not runtime source {configured:?}")]
    WrongClaimSource {
        configured: IrqSourceId,
        captured: IrqSourceId,
    },
    #[error("fault claim generation {evidence} does not match mask generation {masked}")]
    MaskGenerationMismatch { evidence: u64, masked: u64 },
}

/// Failed contained-fault binding retaining the fault and claim owner.
#[must_use = "quarantine or retry with the complete source-fault transaction"]
pub(super) struct ContainedSourceFaultBindingFailure {
    reason: ContainedSourceFaultBindingReason,
    owners: Box<ContainedSourceFaultBindingOwners>,
}

impl ContainedSourceFaultBindingFailure {
    fn new(
        reason: ContainedSourceFaultBindingReason,
        fault: PendingSourceFault,
        pending: Option<PendingBlockIrq>,
    ) -> Self {
        Self {
            reason,
            owners: Box::new(ContainedSourceFaultBindingOwners { fault, pending }),
        }
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        ContainedSourceFaultBindingReason,
        PendingSourceFault,
        Option<PendingBlockIrq>,
    ) {
        let ContainedSourceFaultBindingOwners { fault, pending } = *self.owners;
        (self.reason, fault, pending)
    }
}

struct ContainedSourceFaultBindingOwners {
    fault: PendingSourceFault,
    pending: Option<PendingBlockIrq>,
}

impl fmt::Debug for ContainedSourceFaultBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainedSourceFaultBindingFailure")
            .field("reason", &self.reason)
            .field("source", &self.owners.fault.source)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ContainedSourceFaultBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for ContainedSourceFaultBindingFailure {}

/// Bounded post-quiescence progress for one contained callback fault.
#[derive(Debug)]
pub(super) enum ContainedSourceFaultRecoveryProgress {
    More(ContainedSourceFaultRecovery),
    Retired(ContainedSourceFaultRetired),
}

/// Diagnostic owner returned only after the fault and latch are retired.
#[derive(Debug)]
pub(super) struct ContainedSourceFaultRetired {
    source: IrqSourceId,
    fault: PendingSourceFault,
    driver: Option<RoutedDriverEvidenceRetired>,
}

impl ContainedSourceFaultRetired {
    #[cfg(test)]
    pub(super) const fn source(&self) -> IrqSourceId {
        self.source
    }

    /// Commits the runtime latch reset after exact evidence retirement.
    ///
    /// The callback action was disabled and synchronized before this receipt
    /// could be created. The fault slot and any ordinary pending slot were
    /// transferred into the recovery owner, so no producer can publish a new
    /// owner until the action is explicitly armed again.
    pub(super) fn clear_runtime_latches(
        self,
        ingress: &EvidenceIngress,
    ) -> Option<RoutedDriverEvidenceRetired> {
        let Self {
            source,
            fault: _fault,
            driver,
        } = self;
        debug_assert_eq!(source, ingress.source);
        debug_assert!(!ingress.pending.has_ready_owner());
        debug_assert!(!ingress.fault.has_ready_owner());
        ingress
            .outstanding
            .store(false, core::sync::atomic::Ordering::Release);
        ingress
            .faulted
            .store(false, core::sync::atomic::Ordering::Release);
        driver
    }
}

/// Why a DMA proof could not retire a contained callback fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum ContainedSourceFaultRetireReason {
    #[error("DMA-quiescence proof belongs to another controller")]
    ForeignController,
    #[error("DMA-quiescence epoch is stale: expected {expected:?}, observed {actual:?}")]
    StaleEpoch {
        expected: ControllerEpoch,
        actual: ControllerEpoch,
    },
    #[error("contained source-fault latch retirement failed: {0}")]
    Latch(rdif_block::EvidenceLatchError),
    #[error("driver recovery-evidence retirement failed: {0}")]
    DriverLedger(rdif_block::BlkError),
}

/// Failed retirement retaining the complete contained fault transaction.
#[must_use = "retry with the matching proof or quarantine the retained fault owner"]
pub(super) struct ContainedSourceFaultRetireFailure {
    reason: ContainedSourceFaultRetireReason,
    owner: ContainedSourceFaultRecovery,
}

impl ContainedSourceFaultRetireFailure {
    pub(super) fn into_parts(
        self,
    ) -> (
        ContainedSourceFaultRetireReason,
        ContainedSourceFaultRecovery,
    ) {
        (self.reason, self.owner)
    }
}

impl fmt::Debug for ContainedSourceFaultRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainedSourceFaultRetireFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ContainedSourceFaultRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for ContainedSourceFaultRetireFailure {}

#[derive(Debug)]
enum RecoveryEvidenceOwner {
    Pending(PendingBlockIrq),
    Quiesced(QuiescedEvidence),
    DriverRetire(RecoveryEvidenceRetirePermit),
}

/// Invalid attempt to bind one recovery decision to a runtime source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum RecoveryBindingReason {
    /// The portable evidence names another exact IRQ source.
    #[error("recovery evidence source {captured:?} does not match runtime source {configured:?}")]
    WrongSource {
        configured: IrqSourceId,
        captured: IrqSourceId,
    },
    /// The runtime source already owns another recovery transaction.
    #[error("block IRQ source already owns recovery evidence")]
    AlreadyBound,
    /// A normal-drain transaction cannot be transferred into this recovery.
    #[error("block IRQ source has an incompatible normal-drain transaction")]
    DrainState,
}

/// Failed recovery binding retaining the move-only evidence owner.
#[must_use = "retry with the matching source or quarantine the retained evidence owner"]
pub(in crate::block::activation_v13) struct RecoveryBindingFailure {
    reason: RecoveryBindingReason,
    pending: PendingBlockIrq,
    fault: ControllerFault,
    route: DriverEvidenceRoute,
    rearm: Option<RearmPermit>,
}

impl RecoveryBindingFailure {
    pub(super) fn already_bound(
        pending: PendingBlockIrq,
        fault: ControllerFault,
        route: DriverEvidenceRoute,
    ) -> Self {
        Self {
            reason: RecoveryBindingReason::AlreadyBound,
            pending,
            fault,
            route,
            rearm: None,
        }
    }

    pub(super) fn drain_state(
        pending: PendingBlockIrq,
        fault: ControllerFault,
        route: DriverEvidenceRoute,
        rearm: Option<RearmPermit>,
    ) -> Self {
        Self {
            reason: RecoveryBindingReason::DrainState,
            pending,
            fault,
            route,
            rearm,
        }
    }

    #[cfg(test)]
    pub(super) fn into_parts(
        self,
    ) -> (
        RecoveryBindingReason,
        PendingBlockIrq,
        ControllerFault,
        DriverEvidenceRoute,
        Option<RearmPermit>,
    ) {
        (
            self.reason,
            self.pending,
            self.fault,
            self.route,
            self.rearm,
        )
    }
}

impl fmt::Debug for RecoveryBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryBindingFailure")
            .field("reason", &self.reason)
            .field("evidence", &self.pending.evidence_id())
            .field("fault", &self.fault)
            .field("route", &self.route)
            .field("has_rearm", &self.rearm.is_some())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RecoveryBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for RecoveryBindingFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Reason quiesced evidence could not reach its terminal latch state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum RecoveryRetireReason {
    /// The proof belongs to another controller instance.
    #[error("DMA-quiescence proof belongs to another controller")]
    ForeignController,
    /// Runtime attempted retirement without a stable controller owner.
    #[error("controller identity is zero")]
    InvalidControllerIdentity,
    /// The proof belongs to the same controller but not the recovery cycle
    /// that captured the contained callback fault.
    #[error("DMA-quiescence epoch is stale: expected {expected:?}, observed {actual:?}")]
    StaleEpoch {
        expected: ControllerEpoch,
        actual: ControllerEpoch,
    },
    /// The exact source latch rejected its retained owner.
    #[error("recovery evidence latch retirement failed: {0}")]
    Latch(rdif_block::EvidenceLatchError),
    /// The portable driver retained or rejected its exact ledger identity.
    #[error("driver recovery-evidence retirement failed: {0}")]
    DriverLedger(rdif_block::BlkError),
    /// A source cannot carry normal driver recovery and callback-fault
    /// recovery at the same time.
    #[error("block IRQ source owns conflicting recovery transactions")]
    ConflictingRecoveryOwners,
}

impl From<EvidenceRetireError> for RecoveryRetireReason {
    fn from(error: EvidenceRetireError) -> Self {
        match error {
            EvidenceRetireError::ForeignController => Self::ForeignController,
            EvidenceRetireError::InvalidControllerIdentity => Self::InvalidControllerIdentity,
        }
    }
}

impl From<ContainedSourceFaultRetireReason> for RecoveryRetireReason {
    fn from(reason: ContainedSourceFaultRetireReason) -> Self {
        match reason {
            ContainedSourceFaultRetireReason::ForeignController => Self::ForeignController,
            ContainedSourceFaultRetireReason::StaleEpoch { expected, actual } => {
                Self::StaleEpoch { expected, actual }
            }
            ContainedSourceFaultRetireReason::Latch(error) => Self::Latch(error),
            ContainedSourceFaultRetireReason::DriverLedger(error) => Self::DriverLedger(error),
        }
    }
}

/// Failed recovery retirement retaining the exact evidence owner.
#[must_use = "retry after fixing the proof or quarantine the retained recovery owner"]
pub(super) struct RecoveryRetireFailure {
    reason: RecoveryRetireReason,
    owner: RecoveryBoundEvidence,
}

impl RecoveryRetireFailure {
    pub(super) fn into_parts(self) -> (RecoveryRetireReason, RecoveryBoundEvidence) {
        (self.reason, self.owner)
    }
}

impl fmt::Debug for RecoveryRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecoveryRetireFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RecoveryRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for RecoveryRetireFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Bounded progress while clearing a synchronized recovery evidence latch.
#[derive(Debug)]
pub(super) enum RecoveryEvidenceProgress {
    /// A coalesced capture requires another owner-thread pass.
    More(RecoveryBoundEvidence),
    /// The evidence and one-shot mask token reached terminal ownership.
    Retired(RecoveryEvidenceRetired),
}

/// Terminal diagnostic receipt for one recovery-bound IRQ fact.
#[derive(Debug)]
pub(in crate::block::activation_v13) struct RecoveryEvidenceRetired {
    source: IrqSourceId,
    fault: ControllerFault,
    driver: RoutedDriverEvidenceRetired,
}

impl RecoveryEvidenceRetired {
    #[cfg(test)]
    pub(super) const fn source(&self) -> IrqSourceId {
        self.source
    }

    #[cfg(test)]
    pub(super) const fn evidence_id(&self) -> IrqEvidenceId {
        self.driver.receipt.evidence_id()
    }

    #[cfg(test)]
    pub(super) const fn fault(&self) -> ControllerFault {
        self.fault
    }

    pub(super) fn into_driver_receipt(self) -> RoutedDriverEvidenceRetired {
        self.driver
    }
}

/// Source whose callback/action is closed while recovery evidence remains.
#[must_use = "retire the retained evidence after matching DMA quiescence"]
pub(in crate::block::activation_v13) struct ClosedRecoverySource {
    pub(super) source: IrqSourceId,
    pub(super) ingress: alloc::sync::Arc<EvidenceIngress>,
    pub(super) _control: rdif_block::BIrqControl,
    pub(super) recovery: RecoveryBoundEvidence,
    pub(super) _not_send: PhantomData<*mut ()>,
}

impl ClosedRecoverySource {
    /// Clears the recovery evidence without rearming the closed source.
    pub(in crate::block::activation_v13) fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        retire_driver: impl FnOnce(
            DriverEvidenceRoute,
            RecoveryEvidenceRetirePermit,
        ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
    ) -> Result<ClosedRecoveryProgress, ClosedRecoveryRetireFailure> {
        let Self {
            source,
            ingress,
            _control: control,
            recovery,
            _not_send,
        } = self;
        match recovery.retire_after_quiesce(
            proof,
            ingress.latch.as_ref(),
            retire_driver,
        ) {
            Ok(RecoveryEvidenceProgress::More(recovery)) => {
                Ok(ClosedRecoveryProgress::More(Self {
                    source,
                    ingress,
                    _control: control,
                    recovery,
                    _not_send,
                }))
            }
            Ok(RecoveryEvidenceProgress::Retired(receipt)) => {
                ingress
                    .outstanding
                    .store(false, core::sync::atomic::Ordering::Release);
                Ok(ClosedRecoveryProgress::Retired(receipt))
            }
            Err(failure) => {
                let (reason, recovery) = failure.into_parts();
                Err(ClosedRecoveryRetireFailure {
                    reason,
                    source: alloc::boxed::Box::new(Self {
                        source,
                        ingress,
                        _control: control,
                        recovery,
                        _not_send,
                    }),
                })
            }
        }
    }
}

/// Progress from one bounded post-quiescence source-retirement pass.
#[must_use = "retain another-pass owners until evidence reaches its terminal state"]
pub(in crate::block::activation_v13) enum ClosedRecoveryProgress {
    More(ClosedRecoverySource),
    Retired(RecoveryEvidenceRetired),
}

/// Failed post-quiescence source retirement retaining every owner.
#[must_use = "retry or move the complete closed source into named quarantine"]
pub(in crate::block::activation_v13) struct ClosedRecoveryRetireFailure {
    reason: RecoveryRetireReason,
    source: alloc::boxed::Box<ClosedRecoverySource>,
}

impl ClosedRecoveryRetireFailure {
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (RecoveryRetireReason, ClosedRecoverySource) {
        (self.reason, *self.source)
    }
}

impl fmt::Debug for ClosedRecoveryRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClosedRecoveryRetireFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ClosedRecoveryRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for ClosedRecoveryRetireFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.reason)
    }
}
