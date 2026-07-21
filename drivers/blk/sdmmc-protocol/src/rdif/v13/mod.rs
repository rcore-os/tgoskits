//! rdif-block v0.13 ownership-domain adapter for serialized SD/MMC hosts.

mod activation;
mod domain;
mod evidence;
mod irq;
mod lifecycle;
mod queue;

pub use activation::{SdmmcActivationPrelude, SdmmcControllerActivator};
pub use evidence::{
    SdmmcEvidenceBatch, SdmmcEvidenceDisposition, SdmmcEvidenceError, SdmmcEvidenceLedger,
    SdmmcIrqFacts,
};
pub use irq::{SdmmcEvidenceEpoch, into_evidence_source, into_evidence_source_with_epoch};
