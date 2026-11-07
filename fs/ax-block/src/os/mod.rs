//! OS capabilities consumed by physical block devices.
pub mod dma;
pub mod irq;
pub mod sync;
pub mod task;
pub mod time;
pub mod waiters;
pub use dma::{
    DmaDeviceResolver, dma_device, dma_op, has_dma_op, install_dma_device_resolver, install_dma_op,
};
pub use irq::{
    BlockIrqOutcome, BlockIrqRegistrar, BlockIrqRegistration, has_irq_registrar,
    register_block_irq, set_irq_registrar,
};
pub use task::{
    BlockNotification, BlockRuntimeOps, BlockThread, has_runtime_ops, runtime_ops, set_runtime_ops,
};
pub use time::{
    BlockTimeProvider, has_time_provider, monotonic_time, set_time_provider, wall_time,
};
