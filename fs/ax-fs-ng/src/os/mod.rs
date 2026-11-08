pub(crate) use ax_block::os::dma;
pub mod entropy;
pub(crate) use ax_block::os::irq;
pub mod memory;
pub(crate) use ax_block::os::{sync, task, time, waiters};
pub use entropy::{FsEntropyProvider, fill_entropy, has_entropy_provider, set_entropy_provider};
pub use memory::{
    FsPage, FsPageProvider, alloc_page, has_page_provider, install_page_provider, virt_to_phys,
};
#[cfg(feature = "ext4")]
pub(crate) use task::has_runtime_ops;
pub(crate) use task::{BlockNotification, BlockRuntimeOps, BlockThread, runtime_ops};
#[cfg(any(feature = "ext4", feature = "vfs"))]
pub(crate) use time::monotonic_time;
pub(crate) use time::wall_time;

/// Installs all OS capabilities used by ax-fs-ng.
pub fn install(
    time_provider: &'static dyn time::BlockTimeProvider,
    page_provider: &'static dyn memory::FsPageProvider,
    runtime_ops: &'static dyn task::BlockRuntimeOps,
    dma_op: &'static dyn dma_api::DmaOp,
    irq_registrar: Option<&'static dyn irq::BlockIrqRegistrar>,
    entropy_provider: Option<&'static dyn entropy::FsEntropyProvider>,
) {
    time::set_time_provider(time_provider);
    memory::install_page_provider(page_provider);
    task::set_runtime_ops(runtime_ops);
    dma::install_dma_op(dma_op);
    if let Some(irq_registrar) = irq_registrar {
        irq::set_irq_registrar(irq_registrar);
    }
    if let Some(entropy_provider) = entropy_provider {
        entropy::set_entropy_provider(entropy_provider);
    }
    #[cfg(feature = "vfs")]
    crate::file::start_background_writeback(runtime_ops);
    if let Err(error) = crate::file::start_background_readahead(runtime_ops) {
        log::warn!("file readahead worker startup failed: {error:?}; later requests will retry");
    }
}
