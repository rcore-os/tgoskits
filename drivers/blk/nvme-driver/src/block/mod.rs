//! IRQ-only NVMe block interface.

mod completion;
mod controller;
mod geometry;
mod irq;
mod queue_runtime;

#[cfg(test)]
use completion::{CachedCompletion, CompletionStatus, drain_completion_source};
use completion::{
    CompletionCache, CompletionDrain, IrqCompletionContinuation,
    drain_hardware_completions_to_cache,
};
pub use controller::NvmeBlockDriver;
use controller::NvmeBlockOwner;
use geometry::{device_info, limits};
use irq::{
    irq_sources_from_queue_bits, new_initial_irq_handler, new_queue_irq_handler,
    queue_interrupt_sources, source_queue_bits, unique_interrupt_vectors, vector_for_queue,
};
#[cfg(test)]
use queue_runtime::{
    AcceptedRequest, PrpPageAccumulator, RequestSlot, SlotState, prepare_request_dma,
};
use queue_runtime::{NvmeBlockQueue, NvmeQueueCore, NvmeQueueReinitializeInfo, alloc_prp_lists};

#[cfg(test)]
mod tests;
