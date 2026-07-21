//! Move-only reinitialization handoff between fixed maintenance owners.

use ax_kspin::SpinNoPreempt;
use rdif_block::{ControllerEpoch, DomainReinitPermit, DomainResumed, OwnershipDomainId};

/// One domain's linear permit-to-resume-proof channel.
///
/// The claimed state prevents the controller owner from publishing another
/// epoch while the domain owner is outside the handoff lock and resuming its
/// portable queue state.
pub(super) struct DomainReinitPermitCell {
    domain: OwnershipDomainId,
    state: SpinNoPreempt<DomainReinitHandoff>,
}

enum DomainReinitHandoff {
    Empty,
    Permit(DomainReinitPermit),
    Claimed { epoch: ControllerEpoch },
    Resumed(DomainResumed),
}

impl DomainReinitPermitCell {
    pub(super) const fn new(domain: OwnershipDomainId) -> Self {
        Self {
            domain,
            state: SpinNoPreempt::new(DomainReinitHandoff::Empty),
        }
    }

    pub(super) const fn domain(&self) -> OwnershipDomainId {
        self.domain
    }

    pub(super) fn publish_permit(
        &self,
        permit: DomainReinitPermit,
    ) -> Result<(), DomainPermitPublishFailure> {
        if permit.domain() != self.domain {
            return Err(DomainPermitPublishFailure {
                reason: DomainPermitPublishReason::WrongDomain {
                    expected: self.domain,
                    actual: permit.domain(),
                },
                _permit: permit,
            });
        }
        let mut state = self.state.lock();
        if !matches!(*state, DomainReinitHandoff::Empty) {
            return Err(DomainPermitPublishFailure {
                reason: DomainPermitPublishReason::Occupied {
                    domain: self.domain,
                },
                _permit: permit,
            });
        }
        *state = DomainReinitHandoff::Permit(permit);
        Ok(())
    }

    pub(super) fn take_permit(&self) -> Option<DomainReinitPermit> {
        let mut state = self.state.lock();
        let current = core::mem::replace(&mut *state, DomainReinitHandoff::Empty);
        match current {
            DomainReinitHandoff::Permit(permit) => {
                *state = DomainReinitHandoff::Claimed {
                    epoch: permit.epoch(),
                };
                Some(permit)
            }
            current => {
                *state = current;
                None
            }
        }
    }

    pub(super) fn publish_resumed(
        &self,
        resumed: DomainResumed,
    ) -> Result<(), DomainResumePublishFailure> {
        let reason = if resumed.domain() != self.domain {
            Some(DomainResumePublishReason::WrongDomain {
                expected: self.domain,
                actual: resumed.domain(),
            })
        } else {
            let state = self.state.lock();
            match &*state {
                DomainReinitHandoff::Claimed { epoch } if *epoch == resumed.epoch() => None,
                DomainReinitHandoff::Claimed { epoch } => {
                    Some(DomainResumePublishReason::EpochMismatch {
                        expected: *epoch,
                        actual: resumed.epoch(),
                    })
                }
                _ => Some(DomainResumePublishReason::PermitNotClaimed {
                    domain: self.domain,
                }),
            }
        };
        if let Some(reason) = reason {
            return Err(DomainResumePublishFailure {
                reason,
                _resumed: resumed,
            });
        }
        let mut state = self.state.lock();
        match &*state {
            DomainReinitHandoff::Claimed { epoch } if *epoch == resumed.epoch() => {
                *state = DomainReinitHandoff::Resumed(resumed);
                Ok(())
            }
            _ => Err(DomainResumePublishFailure {
                reason: DomainResumePublishReason::PermitNotClaimed {
                    domain: self.domain,
                },
                _resumed: resumed,
            }),
        }
    }

    pub(super) fn take_resumed(&self) -> Option<DomainResumed> {
        let mut state = self.state.lock();
        let current = core::mem::replace(&mut *state, DomainReinitHandoff::Empty);
        match current {
            DomainReinitHandoff::Resumed(resumed) => Some(resumed),
            current => {
                *state = current;
                None
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum DomainPermitPublishReason {
    #[error("reinitialization permit belongs to domain {actual:?}, expected {expected:?}")]
    WrongDomain {
        expected: OwnershipDomainId,
        actual: OwnershipDomainId,
    },
    #[error("domain {domain:?} already owns an unfinished reinitialization handoff")]
    Occupied { domain: OwnershipDomainId },
}

#[must_use = "retry the matching handoff or quarantine the retained permit"]
pub(super) struct DomainPermitPublishFailure {
    reason: DomainPermitPublishReason,
    _permit: DomainReinitPermit,
}

impl core::fmt::Debug for DomainPermitPublishFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DomainPermitPublishFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Display for DomainPermitPublishFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for DomainPermitPublishFailure {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum DomainResumePublishReason {
    #[error("resumed proof belongs to domain {actual:?}, expected {expected:?}")]
    WrongDomain {
        expected: OwnershipDomainId,
        actual: OwnershipDomainId,
    },
    #[error("resumed proof epoch {actual:?} does not match claimed epoch {expected:?}")]
    EpochMismatch {
        expected: ControllerEpoch,
        actual: ControllerEpoch,
    },
    #[error("domain {domain:?} has no claimed reinitialization permit")]
    PermitNotClaimed { domain: OwnershipDomainId },
}

#[must_use = "return the resumed proof to its handoff or quarantine it"]
pub(super) struct DomainResumePublishFailure {
    reason: DomainResumePublishReason,
    _resumed: DomainResumed,
}

impl core::fmt::Debug for DomainResumePublishFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DomainResumePublishFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Display for DomainResumePublishFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl core::error::Error for DomainResumePublishFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cell_does_not_fabricate_a_reinitialization_permit() {
        let domain = OwnershipDomainId::new(3).unwrap();
        let cell = DomainReinitPermitCell::new(domain);

        assert_eq!(cell.domain(), domain);
        assert!(cell.take_permit().is_none());
        assert!(cell.take_resumed().is_none());
    }
}
