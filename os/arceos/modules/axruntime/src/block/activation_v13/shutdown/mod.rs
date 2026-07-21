//! Pure controller-shutdown coordination shared by fixed maintenance owners.

mod proof;
mod state;
#[cfg(test)]
mod tests;

pub(crate) use proof::{DmaProofPublishFailure, DmaQuiescedLease, ReclaimAckFailure};
pub(crate) use state::{
    ControllerShutdown, ParticipantId, ShutdownError, ShutdownGeneration, ShutdownPhase,
};
#[cfg(test)]
use state::{ShutdownAckProgress, ShutdownAcknowledgement, ShutdownOperation, ShutdownSnapshot};
