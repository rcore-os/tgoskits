//! Linear DMA-quiescence proof sharing and owner-retaining failures.

use alloc::sync::Arc;
use core::fmt;

use rdif_block::DmaQuiesced;

use super::{ParticipantId, ShutdownError};

/// Non-cloneable internal reference to the controller's linear DMA proof.
///
/// The underlying [`DmaQuiesced`] value is never cloned. A participant must
/// return this lease through [`super::ControllerShutdown::ack_reclaimed`], which
/// drops the internal [`Arc`] before publishing its reclaimed bit.
#[derive(Debug)]
#[must_use = "return the proof lease through ack_reclaimed or retain it in quarantine"]
pub(crate) struct DmaQuiescedLease {
    pub(super) participant: ParticipantId,
    pub(super) proof: Arc<DmaQuiesced>,
}

impl DmaQuiescedLease {
    /// Borrows the immutable driver proof for owner-local resource reclaim.
    pub fn proof(&self) -> &DmaQuiesced {
        self.proof.as_ref()
    }

    /// Returns the participant identity that owns this internal reference.
    #[cfg(test)]
    pub const fn participant(&self) -> ParticipantId {
        self.participant
    }
}

/// Failed DMA-proof publication retaining the original linear value.
#[must_use = "retry publication or retain the DMA proof in named quarantine"]
pub(crate) struct DmaProofPublishFailure {
    error: ShutdownError,
    _proof: DmaQuiesced,
}

impl DmaProofPublishFailure {
    pub(super) fn new(error: ShutdownError, proof: DmaQuiesced) -> Self {
        Self {
            error,
            _proof: proof,
        }
    }

    #[cfg(test)]
    pub(super) fn into_parts(self) -> (ShutdownError, DmaQuiesced) {
        (self.error, self._proof)
    }
}

impl fmt::Debug for DmaProofPublishFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DmaProofPublishFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DmaProofPublishFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "DMA-proof publication failed: {}", self.error)
    }
}

impl core::error::Error for DmaProofPublishFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Failed reclaimed acknowledgement retaining the participant's proof lease.
#[must_use = "retry acknowledgement or retain the DMA-proof lease in quarantine"]
pub(crate) struct ReclaimAckFailure {
    error: ShutdownError,
    lease: DmaQuiescedLease,
}

impl ReclaimAckFailure {
    pub(super) fn new(error: ShutdownError, lease: DmaQuiescedLease) -> Self {
        Self { error, lease }
    }

    #[cfg(test)]
    pub(super) fn into_parts(self) -> (ShutdownError, DmaQuiescedLease) {
        (self.error, self.lease)
    }
}

impl fmt::Debug for ReclaimAckFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReclaimAckFailure")
            .field("error", &self.error)
            .field("participant", &self.lease.participant)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ReclaimAckFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reclaimed acknowledgement failed: {}",
            self.error
        )
    }
}

impl core::error::Error for ReclaimAckFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}
