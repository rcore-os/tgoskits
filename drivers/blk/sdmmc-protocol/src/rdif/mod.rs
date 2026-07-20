//! RDIF block-device bridge for SDIO-backed SD/MMC hosts.
//!
//! This module owns the reusable queue/runtime-independent part of adapting a
//! [`crate::sdio::SdioSdmmc`] card to [`rdif_block`]. Host crates provide the
//! small controller-specific [`BlockHost`] impl that submits one owned request
//! and advances it only from acknowledged interrupt events. Controller faults
//! retain DMA ownership until a typed, non-blocking lifecycle proves hardware
//! quiescence and reconstructs the IRQ queue.

pub mod config;
pub mod device;
pub mod host;
pub mod irq;
pub mod queue;
pub mod shared_core;
pub mod staged;

pub use config::{
    BLOCK_SIZE, BlockConfig, BlockDataPath, DEFAULT_DMA_MASK, DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
    block_addr_for_card, can_fallback_to_fifo, device_info, map_dev_err_to_blk_err, queue_limits,
};
#[cfg(test)]
use device::BlockControl;
pub use device::BlockDevice;
pub use host::{
    BlockHost, CompletedHostBuffer, HostRequestBuffer, OwnedBlockSubmitError, ProtocolBlockRequest,
    ProtocolBlockSlot,
};
pub use queue::BlockQueue;
pub use rdif_block::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlkError, BlockIrqSource, CompletedRequest,
    CompletionSink, IQueue, Interface, OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle,
    QueueKind, RequestId, ServiceProgress, SubmitError, SubmitOutcome, dma_api,
};
#[cfg(test)]
use shared_core::SharedCore;
pub use staged::{ReadyBlockBuilder, StagedBlockDevice};

#[cfg(test)]
mod tests;
