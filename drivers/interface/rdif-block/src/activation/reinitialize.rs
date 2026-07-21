//! Linear completion of one controller reinitialization epoch.

use alloc::{sync::Arc, vec::Vec};
use core::{fmt, num::NonZeroUsize};

use super::{DomainReinitPermit, OwnershipDomainId, PublicationSeal};
use crate::{ControllerEpoch, ControllerReady};

macro_rules! impl_failure_debug_display {
    ($failure:ty, $message:literal, $field:ident) => {
        impl fmt::Debug for $failure {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($failure))
                    .field(stringify!($field), &self.$field)
                    .finish_non_exhaustive()
            }
        }

        impl fmt::Display for $failure {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}: {}", $message, self.$field)
            }
        }

        impl core::error::Error for $failure {
            fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
                Some(&self.$field)
            }
        }
    };
}

/// Publication-bound controller result awaiting ownership-domain resumes.
///
/// Binding validates identity and topology, but deliberately does not publish
/// the new active epoch. The runtime must distribute every permit, collect the
/// resulting [`DomainResumed`] owners, and finish the returned commit before
/// the controller epoch can advance.
#[must_use = "distribute every permit and finish the controller epoch commit"]
pub struct BoundControllerReinitialization {
    permits: Vec<DomainReinitPermit>,
    pending_commit: PendingControllerEpochCommit,
}

impl BoundControllerReinitialization {
    pub(super) fn new(
        controller: ControllerReady,
        predecessor: ControllerEpoch,
        controller_identity: NonZeroUsize,
        seal: Arc<PublicationSeal>,
        expected_domains: Vec<OwnershipDomainId>,
        permits: Vec<DomainReinitPermit>,
    ) -> Self {
        Self {
            permits,
            pending_commit: PendingControllerEpochCommit {
                controller,
                predecessor,
                controller_identity,
                seal,
                expected_domains,
                resumed: Vec::new(),
            },
        }
    }

    /// Returns the reconstructed epoch without publishing it as active.
    pub const fn epoch(&self) -> ControllerEpoch {
        self.pending_commit.epoch()
    }

    /// Splits the transaction into move-only domain permits and its commit.
    pub fn into_resume_parts(self) -> (PendingControllerEpochCommit, Vec<DomainReinitPermit>) {
        (self.pending_commit, self.permits)
    }
}

impl fmt::Debug for BoundControllerReinitialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundControllerReinitialization")
            .field("epoch", &self.epoch())
            .field("permit_count", &self.permits.len())
            .finish_non_exhaustive()
    }
}

/// A domain permit transformed only after the portable owner resumed.
#[derive(Debug)]
#[must_use = "return this proof to the controller epoch commit"]
pub struct DomainResumed {
    permit: DomainReinitPermit,
}

impl DomainResumed {
    pub(super) const fn new(permit: DomainReinitPermit) -> Self {
        Self { permit }
    }

    pub const fn domain(&self) -> OwnershipDomainId {
        self.permit.domain
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.permit.epoch
    }
}

/// Linear collector that owns the controller-ready proof until all domains resume.
#[must_use = "collect every resumed-domain proof before publishing the new epoch"]
pub struct PendingControllerEpochCommit {
    controller: ControllerReady,
    predecessor: ControllerEpoch,
    controller_identity: NonZeroUsize,
    seal: Arc<PublicationSeal>,
    expected_domains: Vec<OwnershipDomainId>,
    resumed: Vec<DomainResumed>,
}

impl PendingControllerEpochCommit {
    pub const fn predecessor(&self) -> ControllerEpoch {
        self.predecessor
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.controller.epoch()
    }

    /// Transfers one successfully resumed domain into this exact commit.
    ///
    /// # Errors
    ///
    /// Returns a failure retaining `resumed` when it belongs to another
    /// publication, epoch, domain set, or repeats an accepted domain.
    pub fn accept_resumed(
        &mut self,
        resumed: DomainResumed,
    ) -> Result<(), DomainResumeProofFailure> {
        if let Err(error) = self.validate_resumed(&resumed) {
            return Err(DomainResumeProofFailure { error, resumed });
        }
        self.resumed.push(resumed);
        Ok(())
    }

    /// Produces the final one-shot commit only after every planned domain resumed.
    ///
    /// # Errors
    ///
    /// Returns the unchanged collector when any domain proof is missing.
    pub fn finish(self) -> Result<ControllerEpochCommit, PendingEpochCommitFailure> {
        if let Some(domain) = self.expected_domains.iter().copied().find(|domain| {
            self.resumed
                .iter()
                .all(|resumed| resumed.domain() != *domain)
        }) {
            return Err(PendingEpochCommitFailure {
                error: ControllerEpochCommitError::MissingDomain { domain },
                retained: self,
            });
        }
        Ok(ControllerEpochCommit {
            controller: self.controller,
            predecessor: self.predecessor,
            controller_identity: self.controller_identity,
            seal: self.seal,
            expected_domains: self.expected_domains,
            resumed: self.resumed,
        })
    }

    fn validate_resumed(&self, resumed: &DomainResumed) -> Result<(), ControllerEpochCommitError> {
        let permit = &resumed.permit;
        let belongs_to_publication = permit.controller_identity == self.controller_identity
            && permit.controller_cookie == self.controller_identity.get()
            && permit
                .seal
                .as_ref()
                .is_some_and(|seal| Arc::ptr_eq(seal, &self.seal));
        if !belongs_to_publication {
            return Err(ControllerEpochCommitError::ForeignPublication);
        }
        if permit.epoch != self.controller.epoch() {
            return Err(ControllerEpochCommitError::EpochMismatch {
                expected: self.controller.epoch(),
                captured: permit.epoch,
            });
        }
        if !self.expected_domains.contains(&permit.domain) {
            return Err(ControllerEpochCommitError::UnexpectedDomain {
                domain: permit.domain,
            });
        }
        if self
            .resumed
            .iter()
            .any(|accepted| accepted.domain() == permit.domain)
        {
            return Err(ControllerEpochCommitError::DuplicateDomain {
                domain: permit.domain,
            });
        }
        Ok(())
    }
}

impl fmt::Debug for PendingControllerEpochCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingControllerEpochCommit")
            .field("predecessor", &self.predecessor)
            .field("epoch", &self.epoch())
            .field("expected_domains", &self.expected_domains)
            .field("resumed_domains", &self.resumed.len())
            .finish_non_exhaustive()
    }
}

/// One-shot permission to publish a fully resumed controller epoch.
#[must_use = "commit this epoch exactly once or retain it for quarantine"]
pub struct ControllerEpochCommit {
    controller: ControllerReady,
    predecessor: ControllerEpoch,
    controller_identity: NonZeroUsize,
    seal: Arc<PublicationSeal>,
    expected_domains: Vec<OwnershipDomainId>,
    resumed: Vec<DomainResumed>,
}

impl ControllerEpochCommit {
    pub const fn predecessor(&self) -> ControllerEpoch {
        self.predecessor
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.controller.epoch()
    }

    pub(super) fn validate(
        &self,
        controller_identity: NonZeroUsize,
        seal: &Arc<PublicationSeal>,
        active_epoch: ControllerEpoch,
        planned_domains: &[OwnershipDomainId],
        shared_io_epoch: Option<ControllerEpoch>,
    ) -> Result<(), ControllerEpochCommitError> {
        if self.controller_identity != controller_identity
            || self.controller.controller_cookie() != controller_identity.get()
            || !Arc::ptr_eq(&self.seal, seal)
        {
            return Err(ControllerEpochCommitError::ForeignPublication);
        }
        if self.predecessor != active_epoch {
            return Err(ControllerEpochCommitError::ActiveEpochChanged {
                expected: self.predecessor,
                active: active_epoch,
            });
        }
        if self.controller.epoch() <= self.predecessor {
            return Err(ControllerEpochCommitError::EpochDidNotAdvance {
                active: self.predecessor,
                captured: self.controller.epoch(),
            });
        }
        if !same_domain_set(&self.expected_domains, planned_domains)
            || !same_resumed_set(&self.resumed, planned_domains)
        {
            return Err(ControllerEpochCommitError::TopologyChanged);
        }
        if let Some(shared_io_epoch) = shared_io_epoch
            && shared_io_epoch != self.controller.epoch()
        {
            return Err(ControllerEpochCommitError::SharedIoEpochMismatch {
                expected: self.controller.epoch(),
                active: shared_io_epoch,
            });
        }
        Ok(())
    }
}

impl fmt::Debug for ControllerEpochCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerEpochCommit")
            .field("predecessor", &self.predecessor)
            .field("epoch", &self.epoch())
            .field("resumed_domains", &self.resumed.len())
            .finish_non_exhaustive()
    }
}

/// Failure while joining domain-resume proofs or publishing their epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControllerEpochCommitError {
    #[error("domain-resume proof belongs to another controller publication")]
    ForeignPublication,
    #[error("domain-resume proof names unexpected ownership domain {domain:?}")]
    UnexpectedDomain { domain: OwnershipDomainId },
    #[error("domain-resume proof repeats ownership domain {domain:?}")]
    DuplicateDomain { domain: OwnershipDomainId },
    #[error("domain-resume proof is missing for ownership domain {domain:?}")]
    MissingDomain { domain: OwnershipDomainId },
    #[error("domain-resume proof epoch {captured:?} does not match {expected:?}")]
    EpochMismatch {
        expected: ControllerEpoch,
        captured: ControllerEpoch,
    },
    #[error("controller active epoch changed from {expected:?} to {active:?}")]
    ActiveEpochChanged {
        expected: ControllerEpoch,
        active: ControllerEpoch,
    },
    #[error("controller reinitialization epoch {captured:?} does not advance {active:?}")]
    EpochDidNotAdvance {
        active: ControllerEpoch,
        captured: ControllerEpoch,
    },
    #[error("controller ownership-domain topology changed during reinitialization")]
    TopologyChanged,
    #[error("shared I/O epoch {active:?} does not match reconstructed epoch {expected:?}")]
    SharedIoEpochMismatch {
        expected: ControllerEpoch,
        active: ControllerEpoch,
    },
}

/// Rejected resumed-domain proof retaining its exact move-only owner.
#[must_use = "return the resumed proof to its commit or quarantine it"]
pub struct DomainResumeProofFailure {
    error: ControllerEpochCommitError,
    resumed: DomainResumed,
}

impl DomainResumeProofFailure {
    pub const fn error(&self) -> ControllerEpochCommitError {
        self.error
    }

    pub fn into_parts(self) -> (ControllerEpochCommitError, DomainResumed) {
        (self.error, self.resumed)
    }
}

impl_failure_debug_display!(
    DomainResumeProofFailure,
    "domain resume proof was rejected",
    error
);

/// Incomplete commit retaining the controller-ready and accepted resume proofs.
#[must_use = "collect the missing domain proof or quarantine the transaction"]
pub struct PendingEpochCommitFailure {
    error: ControllerEpochCommitError,
    retained: PendingControllerEpochCommit,
}

impl PendingEpochCommitFailure {
    pub const fn error(&self) -> ControllerEpochCommitError {
        self.error
    }

    pub fn into_parts(self) -> (ControllerEpochCommitError, PendingControllerEpochCommit) {
        (self.error, self.retained)
    }
}

impl_failure_debug_display!(
    PendingEpochCommitFailure,
    "controller epoch commit is incomplete",
    error
);

/// Rejected final commit retaining every accepted domain-resume proof.
#[must_use = "retry the commit against its publication or quarantine it"]
pub struct ControllerEpochCommitFailure {
    error: ControllerEpochCommitError,
    retained: ControllerEpochCommit,
}

impl ControllerEpochCommitFailure {
    pub(super) const fn new(
        error: ControllerEpochCommitError,
        retained: ControllerEpochCommit,
    ) -> Self {
        Self { error, retained }
    }

    pub const fn error(&self) -> ControllerEpochCommitError {
        self.error
    }

    pub fn into_parts(self) -> (ControllerEpochCommitError, ControllerEpochCommit) {
        (self.error, self.retained)
    }
}

impl_failure_debug_display!(
    ControllerEpochCommitFailure,
    "controller epoch commit was rejected",
    error
);

fn same_domain_set(first: &[OwnershipDomainId], second: &[OwnershipDomainId]) -> bool {
    first.len() == second.len() && first.iter().all(|domain| second.contains(domain))
}

fn same_resumed_set(resumed: &[DomainResumed], expected: &[OwnershipDomainId]) -> bool {
    resumed.len() == expected.len()
        && resumed
            .iter()
            .all(|proof| expected.contains(&proof.domain()))
        && resumed.iter().enumerate().all(|(index, proof)| {
            resumed[..index]
                .iter()
                .all(|candidate| candidate.domain() != proof.domain())
        })
}
