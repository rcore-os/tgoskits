//! IRQ callback and linear evidence owner for one staged block source.

use alloc::{boxed::Box, sync::Arc};
use core::{fmt, num::NonZeroUsize, sync::atomic::Ordering};

use rdif_block::{
    BIrqControl, BlkError, ControllerFault, DrainedEvidence, DriverEvidenceRetirement,
    EvidenceClaimToken, EvidenceCompletion, FaultContainment, IrqEventEpoch, IrqEvidenceId,
    IrqSourceId, PendingBlockIrq, RearmPermit,
};
use thiserror::Error;

use crate::maintenance::{MaintenanceError, MaintenanceIrqAction};

/// Copy-only notification carried by the generic maintenance mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum V13MaintenanceEvent {
    /// One exact IRQ source published evidence into its linear ingress slot.
    Irq { source: IrqSourceId },
    /// One independent domain transferred a controller recovery request.
    Recovery { fault: ControllerFault },
}

/// Whether a source fault left the evidence latch untouched or transferred
/// its exact move-only claim into the fault transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FaultLatchOwnership {
    /// Capture failed before the driver or runtime claimed this source latch.
    Untouched,
    /// Exactly one claim owner must accompany the fault during recovery.
    Claimed,
    /// The latch entered a state that no complete claim owner can reset.
    Unrecoverable,
}

/// Move-only source-fault owner retained until recovery or quarantine.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct PendingSourceFault {
    pub(super) source: IrqSourceId,
    pub(super) source_epoch: IrqEventEpoch,
    pub(super) reason: BlkError,
    pub(super) containment: FaultContainment,
    pub(super) containment_error: Option<BlkError>,
    pub(super) latch_ownership: FaultLatchOwnership,
    /// Latch claims that could not become another `PendingBlockIrq` because
    /// the source slot already retained its first move-only owner.
    pub(super) conflicting_claims: [Option<EvidenceClaimToken>; 2],
}

/// Owner-local registered source, its rearm endpoint, and evidence latch.
pub(super) struct BoundEvidenceSource {
    source: IrqSourceId,
    ingress: Arc<EvidenceIngress>,
    control: BIrqControl,
    action: MaintenanceIrqAction,
    // The platform allocation and the portable endpoint are one lifetime.
    // Keep the move-only source proof beside the live action so a parent MSI-X
    // lease cannot be closed while the callback still names its vector.
    _platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    retained_pending: Option<PendingBlockIrq>,
    recovery: Option<RecoveryBoundEvidence>,
    drain: SourceDrainState,
}

/// Driver-ledger commit and one-shot source-rearm ownership for normal I/O.
#[derive(Debug)]
enum SourceDrainState {
    Idle,
    AwaitingDriver {
        evidence: IrqEvidenceId,
        rearm: Option<RearmPermit>,
    },
    DriverRaced {
        evidence: IrqEvidenceId,
        rearm: Option<RearmPermit>,
    },
    FailedRearm(RearmPermit),
    ConflictingRearm {
        first: RearmPermit,
        second: RearmPermit,
    },
}

impl BoundEvidenceSource {
    pub(super) const fn source(&self) -> IrqSourceId {
        self.source
    }

    pub(super) fn enable(&self) -> Result<(), MaintenanceError> {
        self.action.enable()
    }

    /// Closes this exact source after controller interrupt generation is masked.
    ///
    /// The source owner selects its own linear terminal state. Callers never
    /// inspect recovery state before consuming the source, so no owner can be
    /// lost between a probe and teardown.
    pub(super) fn close_after_mask(self) -> Result<ClosedSourceDisposition, SourceCloseFailure> {
        if self.recovery.is_some() {
            return self
                .close_for_recovery()
                .map(ClosedSourceDisposition::Recovery);
        }
        self.close().map(|()| ClosedSourceDisposition::Closed)
    }

    /// Closes one source on its fixed maintenance owner.
    ///
    /// The action is disabled and synchronized before owner-local protocol
    /// state is inspected. This closes the race where a final IRQ publishes
    /// evidence between a preflight check and action removal. Exact platform
    /// ownership is retired only after the IRQ framework has removed and
    /// destroyed the callback successfully.
    pub(super) fn close(self) -> Result<(), SourceCloseFailure> {
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
        if let Some(reason) = (SourceCloseInspection {
            outstanding: self.ingress.outstanding.load(Ordering::Acquire),
            pending: self.retained_pending.is_some() || self.ingress.pending.has_ready_owner(),
            fault_pending: self.ingress.fault.has_ready_owner(),
            recovery_pending: self.recovery.is_some(),
            drain_reason: self.drain.close_reason(),
            faulted: self.ingress.faulted.load(Ordering::Acquire),
        })
        .blocker()
        {
            return Err(SourceCloseFailure::new(reason, self));
        }

        let Self {
            source,
            ingress,
            control,
            action,
            _platform_source: platform_source,
            retained_pending,
            recovery,
            drain,
        } = self;
        match close_action_then_retire(
            action,
            platform_source,
            |action| {
                action.close().map_err(|failure| {
                    let (error, action) = failure.into_parts();
                    (error, Box::new(action))
                })
            },
            |platform_source| {
                // SAFETY: `MaintenanceIrqAction::close` returned success only
                // after disabling, synchronizing, removing, and destroying
                // the registered callback. No active or detached callback can
                // still name this exact platform vector.
                unsafe { platform_source.retire_after_action_close() }
            },
        ) {
            Ok(()) => Ok(()),
            Err(failure) => Err(SourceCloseFailure::new(
                SourceCloseReason::ActionClose(failure.error),
                Self {
                    source,
                    ingress,
                    control,
                    action: *failure.action,
                    _platform_source: failure.platform_source,
                    retained_pending,
                    recovery,
                    drain,
                },
            )),
        }
    }

    /// Transfers one driver recovery decision into this exact source owner.
    pub(super) fn begin_recovery(
        &mut self,
        evidence: PendingBlockIrq,
        fault: ControllerFault,
        controller_identity: NonZeroUsize,
        route: recovery::DriverEvidenceRoute,
    ) -> Result<(), RecoveryBindingFailure> {
        if self.recovery.is_some()
            || self.retained_pending.is_some()
            || self.ingress.pending.has_ready_owner()
        {
            return Err(RecoveryBindingFailure::already_bound(
                evidence, fault, route,
            ));
        }
        let evidence_id = evidence.evidence_id();
        let rearm = match self.drain.take_for_recovery(evidence_id) {
            Ok(rearm) => rearm,
            Err(()) => {
                return Err(RecoveryBindingFailure::drain_state(
                    evidence, fault, route, None,
                ));
            }
        };
        self.recovery = Some(RecoveryBoundEvidence::new_with_rearm(
            self.source,
            evidence,
            fault,
            controller_identity,
            route,
            rearm,
        )?);
        Ok(())
    }

    /// Closes the callback/action while preserving recovery-bound evidence.
    ///
    /// Device interrupt generation must already be masked by the controller.
    /// The returned owner can clear its latch only with a matching
    /// [`rdif_block::DmaQuiesced`] proof.
    fn close_for_recovery(self) -> Result<ClosedRecoverySource, SourceCloseFailure> {
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
        if self.recovery.is_none() {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::RecoveryEvidenceMissing,
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
        if !self.ingress.outstanding.load(Ordering::Acquire) {
            return Err(SourceCloseFailure::new(
                SourceCloseReason::RecoveryOwnerNotOutstanding,
                self,
            ));
        }

        let Self {
            source,
            ingress,
            control,
            action,
            _platform_source: platform_source,
            retained_pending,
            recovery,
            drain,
        } = self;
        let recovery = recovery.expect("the checked recovery owner must remain present");
        match close_action_then_retire(
            action,
            platform_source,
            |action| {
                action.close().map_err(|failure| {
                    let (error, action) = failure.into_parts();
                    (error, Box::new(action))
                })
            },
            |platform_source| {
                // SAFETY: the action close synchronized and destroyed the
                // callback, so no code can still name this exact vector.
                unsafe { platform_source.retire_after_action_close() }
            },
        ) {
            Ok(()) => Ok(ClosedRecoverySource {
                source,
                ingress,
                _control: control,
                recovery,
                _not_send: core::marker::PhantomData,
            }),
            Err(failure) => Err(SourceCloseFailure::new(
                SourceCloseReason::ActionClose(failure.error),
                Self {
                    source,
                    ingress,
                    control,
                    action: *failure.action,
                    _platform_source: failure.platform_source,
                    retained_pending,
                    recovery: Some(recovery),
                    drain,
                },
            )),
        }
    }

    pub(super) fn take_pending(&mut self) -> Option<PendingBlockIrq> {
        self.retained_pending
            .take()
            .or_else(|| self.ingress.pending.take_owner())
    }

    pub(super) fn retain_pending(
        &mut self,
        pending: PendingBlockIrq,
    ) -> Result<(), PendingRetentionFailure> {
        if self.retained_pending.is_some() || self.recovery.is_some() {
            return Err(PendingRetentionFailure {
                _existing_source: self.source,
                _pending: pending,
            });
        }
        self.retained_pending = Some(pending);
        Ok(())
    }

    pub(super) fn take_fault(&self) -> Option<PendingSourceFault> {
        self.ingress.fault.take_owner()
    }

    pub(super) fn has_pending_owner(&self) -> bool {
        self.retained_pending.is_some()
            || self.recovery.is_some()
            || self.ingress.pending.has_ready_owner()
            || self.ingress.fault.has_ready_owner()
            || self.drain.close_reason().is_some()
    }

    /// Completes the runtime latch, then commits the same identity in the
    /// driver ledger before any device source can be rearmed.
    pub(super) fn complete_drained_with(
        &mut self,
        drained: DrainedEvidence,
        commit_driver: impl FnOnce(
            IrqEvidenceId,
        ) -> Result<DriverEvidenceRetirement, BlkError>,
    ) -> Result<SourceDrainProgress, SourceServiceFailure> {
        if self.ingress.faulted.load(Ordering::Acquire) {
            return Err(SourceServiceFailure::FaultPending { _drained: drained });
        }
        // Withdraw the old owner marker before the latch clear-and-recheck.
        // If a fresh IRQ claims the now-clean latch, its callback publishes
        // `true` after this store. Clearing after `complete` would let the old
        // completion overwrite that newer generation and make close miss an
        // owner that has already left the ingress slot.
        self.ingress.outstanding.store(false, Ordering::Release);
        let completion = match drained.complete(self.ingress.latch.as_ref()) {
            Ok(completion) => completion,
            Err((drained, error)) => {
                self.ingress.outstanding.store(true, Ordering::Release);
                return Err(SourceServiceFailure::Latch {
                    _drained: drained,
                    _error: error,
                });
            }
        };
        match completion {
            EvidenceCompletion::Redeliver(pending) => {
                self.ingress.outstanding.store(true, Ordering::Release);
                Ok(SourceDrainProgress::Redelivered(pending))
            }
            EvidenceCompletion::Complete {
                evidence,
                permit,
            } => {
                self.drain
                    .begin_driver_commit(evidence, permit)
                    .map_err(|error| SourceServiceFailure::AfterDrain { _error: error })?;
                let retirement = commit_driver(evidence).map_err(|error| {
                    SourceServiceFailure::AfterDrain {
                        _error: SourceServiceError::DriverRetirement(error),
                    }
                })?;
                match self
                    .drain
                    .finish_driver_commit(evidence, retirement)
                    .map_err(|error| SourceServiceFailure::AfterDrain { _error: error })?
                {
                    DriverCommitProgress::Raced => Ok(SourceDrainProgress::DriverRaced),
                    DriverCommitProgress::Retired(None) => Ok(SourceDrainProgress::Retired),
                    DriverCommitProgress::Retired(Some(permit)) => {
                        self.rearm_after_driver_retirement(permit)?;
                        Ok(SourceDrainProgress::Retired)
                    }
                }
            }
        }
    }

    fn rearm_after_driver_retirement(
        &mut self,
        permit: RearmPermit,
    ) -> Result<(), SourceServiceFailure> {
        let transition = enable_action_then_rearm(
            permit,
            || self.action.enable(),
            |permit| {
                permit
                    .rearm(self.control.as_mut())
                    .map_err(|failure| failure.into_parts())
            },
            || self.action.disable(),
        );
        match transition {
            Ok(_) => Ok(()),
            Err(RearmTransitionFailure::Enable { permit, error }) => {
                self.drain = SourceDrainState::FailedRearm(permit);
                Err(SourceServiceFailure::AfterDrain {
                    _error: SourceServiceError::Maintenance(error),
                })
            }
            Err(RearmTransitionFailure::Rearm {
                permit,
                error,
                containment: Ok(()),
            }) => {
                self.drain = SourceDrainState::FailedRearm(permit);
                Err(SourceServiceFailure::AfterDrain {
                    _error: SourceServiceError::Rearm(error),
                })
            }
            Err(RearmTransitionFailure::Rearm {
                permit,
                error,
                containment: Err(containment),
            }) => {
                self.drain = SourceDrainState::FailedRearm(permit);
                Err(SourceServiceFailure::AfterDrain {
                    _error: SourceServiceError::RearmContainment {
                        rearm: error,
                        containment,
                    },
                })
            }
        }
    }
}

/// Runtime progress after both driver service and source-latch drain.
pub(super) enum SourceDrainProgress {
    Redelivered(PendingBlockIrq),
    DriverRaced,
    Retired,
}

enum DriverCommitProgress {
    Raced,
    Retired(Option<RearmPermit>),
}

impl SourceDrainState {
    fn take_for_recovery(
        &mut self,
        evidence: IrqEvidenceId,
    ) -> Result<Option<RearmPermit>, ()> {
        let previous = core::mem::replace(self, Self::Idle);
        match previous {
            Self::Idle => Ok(None),
            Self::DriverRaced {
                evidence: raced,
                rearm,
            } if raced == evidence => Ok(rearm),
            other => {
                *self = other;
                Err(())
            }
        }
    }

    fn begin_driver_commit(
        &mut self,
        evidence: IrqEvidenceId,
        new_rearm: Option<RearmPermit>,
    ) -> Result<(), SourceServiceError> {
        let previous = core::mem::replace(self, Self::Idle);
        let rearm = match previous {
            Self::Idle => new_rearm,
            Self::DriverRaced {
                evidence: raced,
                rearm: held,
            } if raced == evidence => match (held, new_rearm) {
                (Some(first), Some(second)) => {
                    *self = Self::ConflictingRearm { first, second };
                    return Err(SourceServiceError::DrainState(
                        "a raced evidence identity produced a second rearm permit",
                    ));
                }
                (Some(held), None) | (None, Some(held)) => Some(held),
                (None, None) => None,
            },
            other => {
                *self = other;
                return Err(SourceServiceError::DrainState(
                    "driver evidence commit started from an incompatible source state",
                ));
            }
        };
        *self = Self::AwaitingDriver { evidence, rearm };
        Ok(())
    }

    fn finish_driver_commit(
        &mut self,
        evidence: IrqEvidenceId,
        retirement: DriverEvidenceRetirement,
    ) -> Result<DriverCommitProgress, SourceServiceError> {
        let previous = core::mem::replace(self, Self::Idle);
        let Self::AwaitingDriver {
            evidence: pending,
            rearm,
        } = previous
        else {
            *self = previous;
            return Err(SourceServiceError::DrainState(
                "driver evidence retirement completed without a pending commit",
            ));
        };
        if pending != evidence {
            *self = Self::AwaitingDriver {
                evidence: pending,
                rearm,
            };
            return Err(SourceServiceError::DrainState(
                "driver retired a different evidence identity",
            ));
        }
        match retirement {
            DriverEvidenceRetirement::Retired => Ok(DriverCommitProgress::Retired(rearm)),
            DriverEvidenceRetirement::Raced => {
                *self = Self::DriverRaced { evidence, rearm };
                Ok(DriverCommitProgress::Raced)
            }
        }
    }

    const fn close_reason(&self) -> Option<SourceCloseReason> {
        match self {
            Self::Idle => None,
            Self::FailedRearm(permit) => {
                let _ = permit.evidence_id();
                Some(SourceCloseReason::FailedRearm)
            }
            Self::ConflictingRearm { first, second } => {
                let _ = (first.evidence_id(), second.evidence_id());
                Some(SourceCloseReason::FailedRearm)
            }
            Self::AwaitingDriver { .. } | Self::DriverRaced { .. } => {
                Some(SourceCloseReason::EvidencePending)
            }
        }
    }
}

/// Linear result of closing one source after device-side masking.
#[must_use = "retain recovery evidence until matching DMA quiescence"]
pub(super) enum ClosedSourceDisposition {
    /// No evidence owner remains after action and source close.
    Closed,
    /// The callback is gone but one recovery evidence owner remains linear.
    Recovery(ClosedRecoverySource),
}

struct SourceCloseInspection {
    outstanding: bool,
    pending: bool,
    fault_pending: bool,
    recovery_pending: bool,
    drain_reason: Option<SourceCloseReason>,
    faulted: bool,
}

impl SourceCloseInspection {
    const fn blocker(self) -> Option<SourceCloseReason> {
        if let Some(reason) = self.drain_reason {
            return Some(reason);
        }
        if self.fault_pending || self.faulted {
            return Some(SourceCloseReason::FaultPending);
        }
        if self.recovery_pending {
            return Some(SourceCloseReason::RecoveryPending);
        }
        if self.outstanding || self.pending {
            return Some(SourceCloseReason::EvidencePending);
        }
        None
    }
}

#[derive(Debug)]
struct ActionCloseFailure<A, T, E> {
    error: E,
    action: Box<A>,
    platform_source: Option<T>,
}

fn close_action_then_retire<A, T, E>(
    action: A,
    platform_source: Option<T>,
    close: impl FnOnce(A) -> Result<(), (E, Box<A>)>,
    retire: impl FnOnce(T),
) -> Result<(), ActionCloseFailure<A, T, E>> {
    if let Err((error, action)) = close(action) {
        return Err(ActionCloseFailure {
            error,
            action,
            platform_source,
        });
    }
    if let Some(platform_source) = platform_source {
        retire(platform_source);
    }
    Ok(())
}

/// Stable reason an owner-thread source teardown could not commit.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum SourceCloseReason {
    #[error("block IRQ action disable failed: {0}")]
    Disable(MaintenanceError),
    #[error("block IRQ action synchronization failed: {0}")]
    Synchronize(MaintenanceError),
    #[error("block IRQ source still owns undrained evidence")]
    EvidencePending,
    #[error("block IRQ source still owns an unhandled fault")]
    FaultPending,
    #[error("block IRQ source still owns recovery-bound evidence")]
    RecoveryPending,
    #[error("block IRQ source has no recovery evidence to preserve")]
    RecoveryEvidenceMissing,
    #[error("block IRQ recovery evidence no longer owns its source latch")]
    RecoveryOwnerNotOutstanding,
    #[error("block IRQ source still owns a failed rearm permit")]
    FailedRearm,
    #[error("block IRQ action close failed: {0}")]
    ActionClose(MaintenanceError),
}

/// Failed source teardown retaining the complete action, endpoint control,
/// evidence state, and exact platform-source capability.
#[must_use = "retry teardown or move the complete source owner into named quarantine"]
pub(super) struct SourceCloseFailure {
    reason: SourceCloseReason,
    source: Box<BoundEvidenceSource>,
}

impl SourceCloseFailure {
    fn new(reason: SourceCloseReason, source: BoundEvidenceSource) -> Self {
        Self {
            reason,
            source: Box::new(source),
        }
    }

    pub(super) fn into_parts(self) -> (SourceCloseReason, BoundEvidenceSource) {
        (self.reason, *self.source)
    }
}

impl fmt::Debug for SourceCloseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceCloseFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SourceCloseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "block IRQ source close failed: {}", self.reason)
    }
}

impl core::error::Error for SourceCloseFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// A driver attempted to retain a second move-only owner for one source.
pub(super) struct PendingRetentionFailure {
    _existing_source: IrqSourceId,
    _pending: PendingBlockIrq,
}

/// Failure that retains evidence until the source reaches a terminal owner
/// decision.
pub(super) enum SourceServiceFailure {
    /// Recovery was published before normal drain could rearm the source.
    FaultPending { _drained: DrainedEvidence },
    /// Latch completion failed without consuming the unique evidence owner.
    Latch {
        _drained: DrainedEvidence,
        _error: rdif_block::EvidenceLatchError,
    },
    /// The latch was cleanly consumed; the exact driver commit or one-shot
    /// rearm owner remains in [`BoundEvidenceSource::drain`].
    AfterDrain { _error: SourceServiceError },
}

/// Failure after the latch consumed its unique evidence owner.
#[derive(Debug, Error)]
pub(super) enum SourceServiceError {
    #[error("driver evidence retirement failed: {0}")]
    DriverRetirement(BlkError),
    #[error("invalid block IRQ drain state: {0}")]
    DrainState(&'static str),
    #[error("block IRQ source rearm failed: {0}")]
    Rearm(rdif_block::IrqControlError),
    #[error(
        "block IRQ source rearm failed ({rearm}) and its OS action could not be disabled \
         ({containment})"
    )]
    RearmContainment {
        rearm: rdif_block::IrqControlError,
        containment: MaintenanceError,
    },
    #[error(transparent)]
    Maintenance(#[from] MaintenanceError),
}

mod batch;
mod callback;
pub(super) mod recovery;
mod registration;
mod suspended;
mod terminal;

pub(in crate::block::activation_v13) use batch::{
    SourceCloseBatch, SourceCloseBatchFailure, SourceCloseBatchProgress,
    SourceTerminalChoiceFailure,
};
pub(super) use callback::EndpointCallbackCell;
use callback::{
    EvidenceIngress, RearmTransitionFailure, enable_action_then_rearm, source_irq_action,
};
use recovery::{ClosedRecoverySource, RecoveryBindingFailure, RecoveryBoundEvidence};
pub(super) use registration::SourceRegistrationFailure;
pub(in crate::block::activation_v13) use suspended::{
    ContainedSourceFaultSuspendFailure, QuiescedEvidenceSource, QuiescedSourceBatch,
    QuiescedSourceBatchProgress, SourceRearmBatch, SourceRearmBatchProgress,
};

#[cfg(test)]
mod tests;
