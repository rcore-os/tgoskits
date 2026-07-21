//! Recovery suspension that preserves one registered IRQ action and source.

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, num::NonZeroUsize, sync::atomic::Ordering};

use rdif_block::{
    ControllerEpoch, DmaQuiesced, RecoveryEvidenceRetirePermit,
    RecoveryEvidenceRetired as DriverEvidenceRetired,
};

use super::{
    BoundEvidenceSource, EvidenceIngress, FaultLatchOwnership, PendingSourceFault,
    RecoveryBoundEvidence, SourceCloseFailure, SourceCloseReason,
    batch::{
        LinearOwnerBatch, LinearOwnerBatchProgress, LinearOwnerTransition,
        LinearOwnerTransitionFailure, SourceCloseBatch, SourceTerminalChoiceFailure,
        advance_linear_owner_batch,
    },
    recovery::{
        ContainedSourceFaultBindingReason, ContainedSourceFaultRecovery,
        ContainedSourceFaultRecoveryProgress, DriverEvidenceRetireFailure, DriverEvidenceRoute,
        RecoveryEvidenceProgress, RecoveryRetireReason, RoutedDriverEvidenceRetired,
    },
};
use crate::maintenance::{MaintenanceError, MaintenanceIrqAction};

/// Incrementally retires recovery evidence for a set of synchronized sources.
///
/// The batch owns every source throughout the transition. A bounded advance
/// either returns another complete batch, transfers every source into the
/// re-arm typestate, or returns a failure that still owns the failed source,
/// all unvisited sources, and all sources completed by earlier steps.
#[must_use = "advance, retry, or quarantine every retained IRQ source owner"]
pub(in crate::block::activation_v13) struct QuiescedSourceBatch {
    owners: LinearOwnerBatch<QuiescedEvidenceSource, QuiescedSourceReady>,
}

impl QuiescedSourceBatch {
    /// Starts one recovery-evidence retirement transaction.
    pub(in crate::block::activation_v13) fn new(sources: Vec<QuiescedEvidenceSource>) -> Self {
        Self {
            owners: LinearOwnerBatch::new(sources),
        }
    }

    /// Advances at most `budget` source owners with the matching DMA proof.
    pub(in crate::block::activation_v13) fn advance(
        self,
        proof: &DmaQuiesced,
        budget: NonZeroUsize,
        mut retire_driver: impl FnMut(
            DriverEvidenceRoute,
            RecoveryEvidenceRetirePermit,
        ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
    ) -> Result<QuiescedSourceBatchProgress, QuiescedSourceBatchFailure> {
        let progress = advance_linear_owner_batch(self.owners, budget, |source| {
            match source.retire_after_quiesce(proof, &mut retire_driver) {
                Ok(QuiescedSourceProgress::More(source)) => {
                    Ok(LinearOwnerTransition::Retained(source))
                }
                Ok(QuiescedSourceProgress::Ready(source)) => {
                    Ok(LinearOwnerTransition::Completed(source))
                }
                Err(failure) => {
                    let (reason, source) = failure.into_parts();
                    Err(LinearOwnerTransitionFailure::new(reason, source))
                }
            }
        });

        match progress {
            Ok(LinearOwnerBatchProgress::More(owners)) => {
                Ok(QuiescedSourceBatchProgress::More(Self { owners }))
            }
            Ok(LinearOwnerBatchProgress::Complete(ready)) => {
                Ok(QuiescedSourceBatchProgress::Ready(SourceRearmBatch {
                    owners: LinearOwnerBatch::new(ready),
                }))
            }
            Err(failure) => {
                let (reason, owners) = failure.into_parts();
                Err(QuiescedSourceBatchFailure {
                    reason,
                    batch: Box::new(Self { owners }),
                })
            }
        }
    }

    /// Returns the number of source owners that still retain recovery evidence.
    pub(in crate::block::activation_v13) fn pending_len(&self) -> usize {
        self.owners.pending_len()
    }

    /// Returns the number of source owners already ready to re-arm.
    pub(in crate::block::activation_v13) fn ready_len(&self) -> usize {
        self.owners.completed_len()
    }
}

/// Bounded progress while retiring a vector of recovery-bound sources.
#[must_use = "continue retirement or re-arm every resulting source owner"]
pub(in crate::block::activation_v13) enum QuiescedSourceBatchProgress {
    /// At least one source retains evidence or has not yet been visited.
    More(QuiescedSourceBatch),
    /// Every source is synchronized and its recovery evidence is retired.
    Ready(SourceRearmBatch),
}

/// Failed vector retirement retaining every source owner.
#[must_use = "retry with the matching proof or quarantine the complete source batch"]
pub(in crate::block::activation_v13) struct QuiescedSourceBatchFailure {
    reason: RecoveryRetireReason,
    batch: Box<QuiescedSourceBatch>,
}

impl QuiescedSourceBatchFailure {
    /// Returns the failure reason and the complete retryable source batch.
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (RecoveryRetireReason, QuiescedSourceBatch) {
        (self.reason, *self.batch)
    }
}

impl fmt::Debug for QuiescedSourceBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescedSourceBatchFailure")
            .field("reason", &self.reason)
            .field("pending", &self.batch.pending_len())
            .field("ready", &self.batch.ready_len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for QuiescedSourceBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for QuiescedSourceBatchFailure {}

/// Sources whose recovery evidence is retired but whose actions remain disabled.
#[must_use = "advance, retry, or quarantine every retained IRQ source owner"]
pub(in crate::block::activation_v13) struct SourceRearmBatch {
    owners: LinearOwnerBatch<QuiescedSourceReady, BoundEvidenceSource>,
}

impl SourceRearmBatch {
    /// Chooses terminal action close instead of controller reinitialization.
    ///
    /// The choice is valid only before any source action has been rearmed.
    /// This prevents a terminal shutdown from silently mixing live and closed
    /// source actions after a partial rearm pass.
    pub(in crate::block::activation_v13) fn choose_terminal_close(
        self,
    ) -> Result<SourceCloseBatch, SourceTerminalChoiceFailure> {
        if !self.owners.completed.is_empty() {
            return Err(SourceTerminalChoiceFailure {
                armed: self.owners.completed.len(),
                batch: Box::new(self),
            });
        }
        Ok(SourceCloseBatch::new(self.owners.pending))
    }

    /// Enables at most `budget` retained actions on their fixed owner CPU.
    pub(in crate::block::activation_v13) fn advance(
        self,
        budget: NonZeroUsize,
    ) -> Result<SourceRearmBatchProgress, SourceRearmBatchFailure> {
        let progress = advance_linear_owner_batch(self.owners, budget, |source| {
            match source.arm_for_reinitialize() {
                Ok(source) => Ok(LinearOwnerTransition::Completed(source)),
                Err(failure) => {
                    let (error, source) = failure.into_parts();
                    Err(LinearOwnerTransitionFailure::new(error, source))
                }
            }
        });

        match progress {
            Ok(LinearOwnerBatchProgress::More(owners)) => {
                Ok(SourceRearmBatchProgress::More(Self { owners }))
            }
            Ok(LinearOwnerBatchProgress::Complete(sources)) => {
                Ok(SourceRearmBatchProgress::Armed(sources))
            }
            Err(failure) => {
                let (error, owners) = failure.into_parts();
                Err(SourceRearmBatchFailure {
                    error,
                    batch: Box::new(Self { owners }),
                })
            }
        }
    }

    /// Returns the number of actions not yet enabled.
    pub(in crate::block::activation_v13) fn pending_len(&self) -> usize {
        self.owners.pending_len()
    }

    /// Returns the number of actions already enabled by earlier bounded steps.
    pub(in crate::block::activation_v13) fn armed_len(&self) -> usize {
        self.owners.completed_len()
    }
}

/// Bounded progress while enabling retained source actions.
#[must_use = "continue re-arm or retain every armed source owner"]
pub(in crate::block::activation_v13) enum SourceRearmBatchProgress {
    /// At least one retained source action has not yet been enabled.
    More(SourceRearmBatch),
    /// Every action is enabled and returned as its normal bound source owner.
    Armed(Vec<BoundEvidenceSource>),
}

/// Failed vector re-arm retaining disabled and already-enabled source owners.
#[must_use = "retry action enable or quarantine the complete source batch"]
pub(in crate::block::activation_v13) struct SourceRearmBatchFailure {
    error: MaintenanceError,
    batch: Box<SourceRearmBatch>,
}

impl SourceRearmBatchFailure {
    /// Returns the action error and the complete retryable source batch.
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (MaintenanceError, SourceRearmBatch) {
        (self.error, *self.batch)
    }
}

impl fmt::Debug for SourceRearmBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRearmBatchFailure")
            .field("error", &self.error)
            .field("pending", &self.batch.pending_len())
            .field("armed", &self.batch.armed_len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SourceRearmBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block IRQ source batch re-arm failed: {}",
            self.error
        )
    }
}

impl core::error::Error for SourceRearmBatchFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl BoundEvidenceSource {
    /// Transfers one contained callback fault into DMA-quiesced retirement.
    ///
    /// The source action is disabled and synchronized before any move-only
    /// evidence owner leaves the bound source. A contained fault may proceed
    /// only when its latch contract names either no claim or exactly one
    /// complete claim owned by this source transaction.
    pub(in crate::block::activation_v13) fn suspend_contained_fault_after_mask(
        mut self,
        fault: PendingSourceFault,
        controller_identity: NonZeroUsize,
        quiesce_epoch: ControllerEpoch,
        route: DriverEvidenceRoute,
    ) -> Result<QuiescedEvidenceSource, ContainedSourceFaultSuspendFailure> {
        if let Err(error) = self.action.disable() {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::Disable(error),
                self,
                fault,
            ));
        }
        if let Err(error) = self.action.synchronize() {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::Synchronize(error),
                self,
                fault,
            ));
        }
        if self.ingress.fault.has_ready_owner() {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::AnotherFaultPublished,
                self,
                fault,
            ));
        }
        if self.recovery.is_some() {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::RecoveryAlreadyBound,
                self,
                fault,
            ));
        }
        if let Some(reason) = self.drain.close_reason() {
            let reason = match reason {
                SourceCloseReason::FailedRearm => {
                    ContainedSourceFaultSuspendReason::FailedRearm
                }
                _ => ContainedSourceFaultSuspendReason::EvidenceDrainPending,
            };
            return Err(ContainedSourceFaultSuspendFailure::new(reason, self, fault));
        }
        let source_pending_owners = usize::from(self.retained_pending.is_some())
            + usize::from(self.ingress.pending.has_ready_owner());
        if source_pending_owners > 1 {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::MultipleSourceEvidenceOwners,
                self,
                fault,
            ));
        }
        let outstanding = self.ingress.outstanding.load(Ordering::Acquire);
        let expected_outstanding = match fault.latch_ownership {
            FaultLatchOwnership::Untouched => false,
            FaultLatchOwnership::Claimed => true,
            FaultLatchOwnership::Unrecoverable => outstanding,
        };
        if outstanding != expected_outstanding {
            return Err(ContainedSourceFaultSuspendFailure::new(
                ContainedSourceFaultSuspendReason::OutstandingStateMismatch {
                    expected: expected_outstanding,
                    actual: outstanding,
                },
                self,
                fault,
            ));
        }

        let pending = self
            .retained_pending
            .take()
            .or_else(|| self.ingress.pending.take_owner());
        let contained_fault = match ContainedSourceFaultRecovery::bind(
            self.source,
            fault,
            pending,
            controller_identity,
            quiesce_epoch,
            route,
        ) {
            Ok(contained_fault) => contained_fault,
            Err(failure) => {
                let (reason, fault, pending) = failure.into_parts();
                self.retained_pending = pending;
                return Err(ContainedSourceFaultSuspendFailure::new(
                    ContainedSourceFaultSuspendReason::Binding(reason),
                    self,
                    fault,
                ));
            }
        };

        let Self {
            source,
            ingress,
            control,
            action,
            _platform_source: platform_source,
            retained_pending: _,
            recovery: _,
            drain: _,
        } = self;
        Ok(QuiescedEvidenceSource {
            source,
            ingress,
            control,
            action,
            platform_source,
            controller_identity,
            quiesce_epoch,
            recovery: None,
            contained_fault: Some(contained_fault),
        })
    }

    /// Disables and synchronizes one source without destroying its action.
    ///
    /// Unlike terminal close, this transition retains the callback, exact
    /// platform vector, and driver control endpoint so the same fixed owner can
    /// arm them before driving controller reinitialization.
    pub(in crate::block::activation_v13) fn suspend_after_mask(
        self,
        controller_identity: NonZeroUsize,
        quiesce_epoch: ControllerEpoch,
    ) -> Result<QuiescedEvidenceSource, SourceCloseFailure> {
        if let Err(error) = self.action.disable() {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::Disable(error),
                self,
            ));
        }
        if let Err(error) = self.action.synchronize() {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::Synchronize(error),
                self,
            ));
        }
        if self.retained_pending.is_some() || self.ingress.pending.has_ready_owner() {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::EvidencePending,
                self,
            ));
        }
        if self.ingress.fault.has_ready_owner() || self.ingress.faulted.load(Ordering::Acquire) {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::FaultPending,
                self,
            ));
        }
        if let Some(reason) = self.drain.close_reason() {
            return Err(SourceCloseFailure::new(reason, self));
        }
        let outstanding = self.ingress.outstanding.load(Ordering::Acquire);
        if self.recovery.is_some() && !outstanding {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::RecoveryOwnerNotOutstanding,
                self,
            ));
        }
        if self.recovery.is_none() && outstanding {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::EvidencePending,
                self,
            ));
        }

        let Self {
            source,
            ingress,
            control,
            action,
            _platform_source,
            retained_pending: _,
            recovery,
            drain: _,
        } = self;
        Ok(QuiescedEvidenceSource {
            source,
            ingress,
            control,
            action,
            platform_source: _platform_source,
            controller_identity,
            quiesce_epoch,
            recovery,
            contained_fault: None,
        })
    }
}

/// Why a contained callback fault could not enter synchronized recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum ContainedSourceFaultSuspendReason {
    #[error("block IRQ action disable failed: {0}")]
    Disable(MaintenanceError),
    #[error("block IRQ action synchronization failed: {0}")]
    Synchronize(MaintenanceError),
    #[error("another block IRQ source fault remains published")]
    AnotherFaultPublished,
    #[error("block IRQ source already owns normal recovery evidence")]
    RecoveryAlreadyBound,
    #[error("block IRQ source still owns a failed rearm permit")]
    FailedRearm,
    #[error("block IRQ source still owns an uncommitted evidence drain")]
    EvidenceDrainPending,
    #[error("block IRQ source exposes more than one ordinary evidence owner")]
    MultipleSourceEvidenceOwners,
    #[error("source-fault outstanding state mismatch: expected {expected}, observed {actual}")]
    OutstandingStateMismatch { expected: bool, actual: bool },
    #[error(transparent)]
    Binding(ContainedSourceFaultBindingReason),
}

/// Failed contained-fault suspension retaining both linear owners.
#[must_use = "retry the complete transaction or move source and fault into named quarantine"]
pub(in crate::block::activation_v13) struct ContainedSourceFaultSuspendFailure {
    reason: ContainedSourceFaultSuspendReason,
    source: Box<BoundEvidenceSource>,
    fault: Box<PendingSourceFault>,
}

impl ContainedSourceFaultSuspendFailure {
    fn new(
        reason: ContainedSourceFaultSuspendReason,
        source: BoundEvidenceSource,
        fault: PendingSourceFault,
    ) -> Self {
        Self {
            reason,
            source: Box::new(source),
            fault: Box::new(fault),
        }
    }

    /// Returns the error and every move-only owner needed for retry or
    /// quarantine.
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (
        ContainedSourceFaultSuspendReason,
        BoundEvidenceSource,
        PendingSourceFault,
    ) {
        (self.reason, *self.source, *self.fault)
    }
}

impl fmt::Debug for ContainedSourceFaultSuspendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContainedSourceFaultSuspendFailure")
            .field("reason", &self.reason)
            .field("source", &self.fault.source)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ContainedSourceFaultSuspendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for ContainedSourceFaultSuspendFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Disabled and synchronized source retained across DMA quiescence.
#[must_use = "retire recovery evidence and re-arm this exact source owner"]
pub(in crate::block::activation_v13) struct QuiescedEvidenceSource {
    source: rdif_block::IrqSourceId,
    ingress: alloc::sync::Arc<EvidenceIngress>,
    control: rdif_block::BIrqControl,
    action: MaintenanceIrqAction,
    platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    controller_identity: NonZeroUsize,
    quiesce_epoch: ControllerEpoch,
    recovery: Option<RecoveryBoundEvidence>,
    contained_fault: Option<ContainedSourceFaultRecovery>,
}

impl QuiescedEvidenceSource {
    /// Retires recovery evidence with the matching controller DMA proof.
    pub(in crate::block::activation_v13) fn retire_after_quiesce(
        self,
        proof: &DmaQuiesced,
        retire_driver: impl FnOnce(
            DriverEvidenceRoute,
            RecoveryEvidenceRetirePermit,
        ) -> Result<DriverEvidenceRetired, DriverEvidenceRetireFailure>,
    ) -> Result<QuiescedSourceProgress, QuiescedSourceRetireFailure> {
        if proof.controller_cookie() != self.controller_identity.get() {
            return Err(QuiescedSourceRetireFailure {
                reason: RecoveryRetireReason::ForeignController,
                source: Box::new(self),
            });
        }
        if proof.epoch() != self.quiesce_epoch {
            return Err(QuiescedSourceRetireFailure {
                reason: RecoveryRetireReason::StaleEpoch {
                    expected: self.quiesce_epoch,
                    actual: proof.epoch(),
                },
                source: Box::new(self),
            });
        }
        let Self {
            source,
            ingress,
            control,
            action,
            platform_source,
            controller_identity,
            quiesce_epoch,
            recovery,
            contained_fault,
        } = self;
        if recovery.is_some() && contained_fault.is_some() {
            return Err(QuiescedSourceRetireFailure {
                reason: RecoveryRetireReason::ConflictingRecoveryOwners,
                source: Box::new(Self {
                    source,
                    ingress,
                    control,
                    action,
                    platform_source,
                    controller_identity,
                    quiesce_epoch,
                    recovery,
                    contained_fault,
                }),
            });
        }
        if let Some(contained_fault) = contained_fault {
            return match contained_fault.retire_after_quiesce(
                proof,
                ingress.latch.as_ref(),
                retire_driver,
            ) {
                Ok(ContainedSourceFaultRecoveryProgress::More(contained_fault)) => {
                    Ok(QuiescedSourceProgress::More(Self {
                        source,
                        ingress,
                        control,
                        action,
                        platform_source,
                        controller_identity,
                        quiesce_epoch,
                        recovery: None,
                        contained_fault: Some(contained_fault),
                    }))
                }
                Ok(ContainedSourceFaultRecoveryProgress::Retired(receipt)) => {
                    let driver_retired = receipt.clear_runtime_latches(&ingress);
                    Ok(QuiescedSourceProgress::Ready(QuiescedSourceReady {
                        source,
                        ingress,
                        control,
                        action,
                        platform_source,
                        driver_retired,
                    }))
                }
                Err(failure) => {
                    let (reason, contained_fault) = failure.into_parts();
                    Err(QuiescedSourceRetireFailure {
                        reason: reason.into(),
                        source: Box::new(Self {
                            source,
                            ingress,
                            control,
                            action,
                            platform_source,
                            controller_identity,
                            quiesce_epoch,
                            recovery: None,
                            contained_fault: Some(contained_fault),
                        }),
                    })
                }
            };
        }
        let Some(recovery) = recovery else {
            return Ok(QuiescedSourceProgress::Ready(QuiescedSourceReady {
                source,
                ingress,
                control,
                action,
                platform_source,
                driver_retired: None,
            }));
        };
        match recovery.retire_after_quiesce(proof, ingress.latch.as_ref(), retire_driver) {
            Ok(RecoveryEvidenceProgress::More(recovery)) => {
                Ok(QuiescedSourceProgress::More(Self {
                    source,
                    ingress,
                    control,
                    action,
                    platform_source,
                    controller_identity,
                    quiesce_epoch,
                    recovery: Some(recovery),
                    contained_fault: None,
                }))
            }
            Ok(RecoveryEvidenceProgress::Retired(receipt)) => {
                ingress.outstanding.store(false, Ordering::Release);
                Ok(QuiescedSourceProgress::Ready(QuiescedSourceReady {
                    source,
                    ingress,
                    control,
                    action,
                    platform_source,
                    driver_retired: Some(receipt.into_driver_receipt()),
                }))
            }
            Err(failure) => {
                let (reason, recovery) = failure.into_parts();
                Err(QuiescedSourceRetireFailure {
                    reason,
                    source: Box::new(Self {
                        source,
                        ingress,
                        control,
                        action,
                        platform_source,
                        controller_identity,
                        quiesce_epoch,
                        recovery: Some(recovery),
                        contained_fault: None,
                    }),
                })
            }
        }
    }
}

/// Bounded result while retiring a synchronized source's recovery evidence.
#[must_use = "retain the source until it is ready to re-arm"]
pub(in crate::block::activation_v13) enum QuiescedSourceProgress {
    More(QuiescedEvidenceSource),
    Ready(QuiescedSourceReady),
}

/// Source whose evidence is retired while its action remains disabled.
#[must_use = "arm the same source before controller reinitialization"]
pub(in crate::block::activation_v13) struct QuiescedSourceReady {
    pub(super) source: rdif_block::IrqSourceId,
    pub(super) ingress: alloc::sync::Arc<EvidenceIngress>,
    pub(super) control: rdif_block::BIrqControl,
    pub(super) action: MaintenanceIrqAction,
    pub(super) platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    pub(super) driver_retired: Option<RoutedDriverEvidenceRetired>,
}

impl QuiescedSourceReady {
    /// Enables the retained OS action before the controller may emit init IRQs.
    pub(in crate::block::activation_v13) fn arm_for_reinitialize(
        self,
    ) -> Result<BoundEvidenceSource, SourceResumeFailure> {
        let _driver_retired = self.driver_retired;
        if let Err(error) = self.action.enable() {
            return Err(SourceResumeFailure {
                error,
                source: Box::new(Self {
                    driver_retired: _driver_retired,
                    ..self
                }),
            });
        }
        Ok(BoundEvidenceSource {
            source: self.source,
            ingress: self.ingress,
            control: self.control,
            action: self.action,
            _platform_source: self.platform_source,
            retained_pending: None,
            recovery: None,
            drain: super::SourceDrainState::Idle,
        })
    }
}

/// Failed recovery-evidence retirement retaining the disabled source action.
#[must_use = "retry with the matching proof or quarantine the complete source owner"]
pub(in crate::block::activation_v13) struct QuiescedSourceRetireFailure {
    reason: RecoveryRetireReason,
    source: Box<QuiescedEvidenceSource>,
}

impl QuiescedSourceRetireFailure {
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (RecoveryRetireReason, QuiescedEvidenceSource) {
        (self.reason, *self.source)
    }
}

impl fmt::Debug for QuiescedSourceRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuiescedSourceRetireFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for QuiescedSourceRetireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for QuiescedSourceRetireFailure {}

/// Failed action re-arm retaining the complete synchronized source owner.
#[must_use = "retry action enable or quarantine the complete source owner"]
pub(in crate::block::activation_v13) struct SourceResumeFailure {
    error: MaintenanceError,
    source: Box<QuiescedSourceReady>,
}

impl SourceResumeFailure {
    pub(in crate::block::activation_v13) fn into_parts(
        self,
    ) -> (MaintenanceError, QuiescedSourceReady) {
        (self.error, *self.source)
    }
}

impl fmt::Debug for SourceResumeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceResumeFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SourceResumeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block IRQ source action re-arm failed: {}",
            self.error
        )
    }
}

impl core::error::Error for SourceResumeFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}
