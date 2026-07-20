//! Owned-request NVMe queue runtime domain.

mod adapter;
mod core;
mod dma;
mod prp;
mod request;

#[cfg(test)]
pub(in crate::block) use core::NVME_QUEUE_EXECUTION;
pub(in crate::block) use core::{NvmeQueueCore, NvmeQueueReinitializeInfo};

pub(in crate::block) use adapter::NvmeBlockQueue;
#[cfg(test)]
pub(in crate::block) use dma::prepare_request_dma;
#[cfg(test)]
pub(in crate::block) use prp::PrpPageAccumulator;
pub(in crate::block) use prp::alloc_prp_lists;
#[cfg(test)]
pub(in crate::block) use request::{AcceptedRequest, RequestSlot, SlotState};
