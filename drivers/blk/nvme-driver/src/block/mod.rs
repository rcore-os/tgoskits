//! IRQ-only NVMe block interface.

mod completion;
mod controller;
mod evidence_ledger;
mod geometry;
mod io_domain;
mod irq;
mod queue_runtime;
mod v13;

#[cfg(test)]
use completion::{CachedCompletion, CompletionStatus, drain_completion_source};
use completion::{CompletionCache, CompletionDrain, drain_owner_completions_to_cache};
pub use controller::NvmeBlockDriver;
use controller::NvmeBlockOwner;
#[cfg(test)]
use evidence_ledger::{NvmeEvidenceDisposition, NvmeEvidenceFacts, NvmeEvidenceLedger};
use geometry::{device_info, hardware_limits, limits};
use irq::{
    NvmeIrqState, irq_sources_from_queue_bits, new_initial_irq_source, new_queue_irq_source,
    new_vector_evidence_source, queue_interrupt_sources, source_queue_bits, vector_for_queue,
};
#[cfg(test)]
use queue_runtime::CommandIdentity;
#[cfg(test)]
use queue_runtime::{
    AcceptedRequest, NVME_QUEUE_EXECUTION, PrpPageAccumulator, RequestSlot, SlotState,
    prepare_request_dma,
};
use queue_runtime::{
    NvmeBlockQueue, NvmeOwnedQueue, NvmeQueueCore, NvmeQueueReinitializeInfo,
    PreparedNvmeOwnedQueue, alloc_prp_lists,
};
pub use v13::NvmeBlockActivator;

#[cfg(test)]
mod tests;
