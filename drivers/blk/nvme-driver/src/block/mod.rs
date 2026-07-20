//! IRQ-only NVMe block interface.

mod completion;
mod controller;
mod geometry;
mod irq;
mod queue_runtime;

#[cfg(test)]
use completion::{CachedCompletion, CompletionStatus, drain_completion_source};
use completion::{CompletionCache, CompletionDrain, drain_owner_completions_to_cache};
pub use controller::NvmeBlockDriver;
use controller::NvmeBlockOwner;
use geometry::{device_info, limits};
use irq::{
    NvmeIrqState, irq_sources_from_queue_bits, new_initial_irq_source, new_queue_irq_source,
    queue_interrupt_sources, source_queue_bits, vector_for_queue,
};
#[cfg(test)]
use queue_runtime::{
    AcceptedRequest, NVME_QUEUE_EXECUTION, PrpPageAccumulator, RequestSlot, SlotState,
    prepare_request_dma,
};
use queue_runtime::{NvmeBlockQueue, NvmeQueueCore, NvmeQueueReinitializeInfo, alloc_prp_lists};

#[cfg(test)]
mod tests;
