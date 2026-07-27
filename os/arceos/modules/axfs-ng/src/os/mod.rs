pub mod dma;
pub mod irq;
pub mod memory;
pub mod sync;
pub mod task;
pub mod time;

pub use dma::{dma_op, has_dma_op, install_dma_op};
pub use irq::{
    BlockIrqOutcome, BlockIrqRegistrar, BlockIrqRegistration, has_irq_registrar,
    register_shared_block_irq, set_irq_registrar,
};
pub use memory::{
    FsPage, FsPageProvider, alloc_page, has_page_provider, install_page_provider, virt_to_phys,
};
pub use task::{
    BlockTaskOps, current_task_id, has_task_ops, notify_drain, notify_drain_from_irq,
    notify_waiters, set_task_ops, spawn_task, task_can_block, task_wait, task_wait_timeout,
    task_wait_until, task_yield, wait_for_drain_notification, wake_task,
};
pub use time::{BlockTimeProvider, has_time_provider, set_time_provider, wall_time};

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) use self::irq::block_irq_outcome_and_ready_hold_for_test;

/// Installs all OS capabilities used by ax-fs-ng.
pub fn install(
    time_provider: &'static dyn time::BlockTimeProvider,
    page_provider: &'static dyn memory::FsPageProvider,
    task_ops: &'static dyn task::BlockTaskOps,
    dma_op: &'static dyn dma_api::DmaOp,
    irq_registrar: Option<&'static dyn irq::BlockIrqRegistrar>,
) {
    time::set_time_provider(time_provider);
    memory::install_page_provider(page_provider);
    task::set_task_ops(task_ops);
    dma::install_dma_op(dma_op);
    if let Some(irq_registrar) = irq_registrar {
        irq::set_irq_registrar(irq_registrar);
    }
}
