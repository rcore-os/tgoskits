//! IRQ-driven owned-DMA block adapter for SDIO-backed SD/MMC hosts.

pub mod config;
pub mod device;
mod host;
mod irq;
pub mod queue;

pub use config::{
    BLOCK_SIZE, BlockConfig, DEFAULT_DMA_MASK, DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
    block_addr_for_card, device_info, map_dev_err_to_blk_err, queue_limits,
};
pub use device::BlockDevice;
pub use queue::BlockQueue;
pub use rdif_block::{
    BlkError, BlockController, CompletedRequest, ControllerEvent, ControllerState,
    ControllerUpdate, HardIrqHandler, HardwareQueue, OwnedRequest, QueueInfo, QueueLimits,
    RequestFlags, RequestId, RequestOp, SubmitError, dma_api,
};
