//! Linear application of one driver IRQ-evidence service decision.

use rdif_block::{BlkError, DriverEvidenceRetirement, IrqEvidenceId, IrqServiceDecision};

use super::{BoundEvidenceSource, source::SourceDrainProgress};
use super::source::recovery::DriverEvidenceRoute;

pub(super) fn apply_domain_decision(
    source: &mut BoundEvidenceSource,
    decision: IrqServiceDecision,
    controller_identity: core::num::NonZeroUsize,
    route: DriverEvidenceRoute,
    commit_driver: impl FnOnce(
        IrqEvidenceId,
    ) -> Result<DriverEvidenceRetirement, BlkError>,
) -> Result<DomainDecisionApplied, DomainDecisionFailure> {
    match decision {
        IrqServiceDecision::Retained(pending) => source
            .retain_pending(pending)
            .map(|()| DomainDecisionApplied::EvidenceRetained)
            .map_err(|failure| DomainDecisionFailure::Retention { _failure: failure }),
        IrqServiceDecision::Drained(drained) => {
            match source.complete_drained_with(drained, commit_driver) {
                Ok(SourceDrainProgress::Redelivered(pending)) => source
                .retain_pending(pending)
                .map(|()| DomainDecisionApplied::EvidenceRetained)
                .map_err(|failure| DomainDecisionFailure::Retention { _failure: failure }),
                Ok(SourceDrainProgress::DriverRaced) => {
                    Ok(DomainDecisionApplied::EvidenceRetained)
                }
                Ok(SourceDrainProgress::Retired) => Ok(DomainDecisionApplied::EvidenceDrained),
                Err(failure) => {
                    Err(DomainDecisionFailure::SourceService { _failure: failure })
                }
            }
        }
        IrqServiceDecision::Recover { evidence, fault } => source
            .begin_recovery(evidence, fault, controller_identity, route)
            .map(|()| DomainDecisionApplied::RecoveryRequired(fault))
            .map_err(|failure| DomainDecisionFailure::RecoveryBinding { _failure: failure }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DomainDecisionApplied {
    EvidenceRetained,
    EvidenceDrained,
    RecoveryRequired(rdif_block::ControllerFault),
}

pub(super) enum DomainDecisionFailure {
    Retention {
        _failure: super::PendingRetentionFailure,
    },
    SourceService {
        _failure: super::SourceServiceFailure,
    },
    RecoveryBinding {
        _failure: super::source::recovery::RecoveryBindingFailure,
    },
}

impl core::fmt::Debug for DomainDecisionFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Retention { .. } => "DomainDecisionFailure::Retention",
            Self::SourceService { .. } => "DomainDecisionFailure::SourceService",
            Self::RecoveryBinding { .. } => "DomainDecisionFailure::RecoveryBinding",
        })
    }
}
