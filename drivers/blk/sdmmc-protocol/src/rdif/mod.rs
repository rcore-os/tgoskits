//! RDIF block-device bridge for SDIO-backed SD/MMC hosts.
//!
//! This module owns the reusable queue/runtime-independent part of adapting a
//! [`crate::sdio::SdioSdmmc`] card to [`rdif_block`]. Host crates provide the
//! small controller-specific [`BlockHost`] impl that submits and polls one
//! block request.

pub mod config;
pub mod device;
pub mod host;
pub mod irq;
pub mod owned;
pub mod queue;
pub mod shared_core;
pub mod split;

pub use config::{
    BLOCK_SIZE, BlockConfig, DEFAULT_DMA_MASK, DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
    block_addr_for_card, can_fallback_to_fifo, device_info, map_dev_err_to_blk_err, queue_limits,
    transfer_mode_for_dma,
};
#[cfg(test)]
use device::BlockControl;
pub use device::BlockDevice;
pub use host::{BlockHost, OwnedBlockSubmitError, ProtocolBlockRequest, ProtocolBlockSlot};
pub use queue::BlockQueue;
pub use rdif_block::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, BlkError, IQueue, IQueueOwned, Interface,
    OwnedRequest, PollError, QueueHandle, Request, RequestId, RequestPoll as OwnedRequestPoll,
    RequestStatus, SubmitError, dma_api,
};
#[cfg(test)]
use shared_core::SharedCore;

#[cfg(test)]
mod tests;
