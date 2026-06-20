#[path = "../../block_runtime/device.rs"]
pub mod device;
#[path = "../../block_runtime/dma.rs"]
pub mod dma;
#[path = "../../block_runtime/irq.rs"]
pub mod irq;
#[path = "../../block_runtime/request.rs"]
pub mod pending;
#[path = "../../block_runtime/queue.rs"]
#[cfg(test)]
mod queue;

#[cfg(test)]
pub use device::NoopDrainWake;
pub use device::{
    BlockCompletionMode, BlockDeviceHandle, BlockDrainWake, BlockIrqAction, BlockRuntime,
    BlockRuntimeConfig, QueueRuntime, RdifBlockDevice, map_blk_err_to_ax_err,
};
pub use dma::DmaBufferGuard;
#[cfg(test)]
pub use dma::VEC_DMA_OP;
pub use irq::{BlockIrqBridge, DrainEvents};
pub use pending::{
    PendingRequest, PendingTable, PollClaim, PollProgress, RequestKey, RequestState,
    RuntimeRequestId,
};
#[cfg(test)]
pub use queue::{CompletionDrain, CompletionSink, RequestPoller};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockIoFutureState {
    New,
    Submitted(RequestKey),
    Complete,
}
