//! Owned-request NVMe queue runtime domain.

mod adapter;
mod core;
mod dma;
mod dma_owner;
mod owned;
mod prp;
mod request;

#[cfg(test)]
pub(in crate::block) use core::NVME_QUEUE_EXECUTION;
pub(in crate::block) use core::{NvmeQueueCore, NvmeQueueReinitializeInfo};

pub(in crate::block) use adapter::NvmeBlockQueue;
#[cfg(test)]
pub(in crate::block) use dma::prepare_request_dma;
pub(in crate::block) use owned::{NvmeOwnedQueue, PreparedNvmeOwnedQueue};
#[cfg(test)]
pub(in crate::block) use prp::PrpPageAccumulator;
pub(in crate::block) use prp::alloc_prp_lists;
pub(in crate::block) use request::CommandIdentity;
#[cfg(test)]
pub(in crate::block) use request::{AcceptedRequest, RequestSlot, SlotState};
