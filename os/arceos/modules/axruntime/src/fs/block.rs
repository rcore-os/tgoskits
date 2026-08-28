use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_alloc::UsageKind;
use ax_fs_ng::{
    BlockError, BlockResult,
    block::runtime::{BlockIrqAction, BlockIrqSource, RdifBlockDevice, RdifBlockGroup},
    os::{
        BlockIrqOutcome, BlockIrqRegistrar, BlockIrqRegistration, BlockNotification,
        BlockRuntimeOps, BlockThread, BlockTimeProvider, FsPage, FsPageProvider,
    },
};

struct RuntimeTimeProvider;

impl BlockTimeProvider for RuntimeTimeProvider {
    fn wall_time(&self) -> Duration {
        ax_hal::time::wall_time()
    }

    fn monotonic_time(&self) -> Duration {
        ax_hal::time::monotonic_time()
    }
}

struct RuntimePageProvider;

impl FsPageProvider for RuntimePageProvider {
    fn alloc_page(&self) -> axfs_ng_vfs::VfsResult<FsPage> {
        let addr = ax_alloc::global_allocator()
            .alloc_pages(1, ax_fs_ng::os::memory::PAGE_SIZE, UsageKind::PageCache)
            .map_err(|_| axfs_ng_vfs::VfsError::NoMemory)?;
        Ok(unsafe { FsPage::from_raw(addr) })
    }

    fn dealloc_page(&self, page: FsPage) {
        ax_alloc::global_allocator().dealloc_pages(page.addr(), 1, UsageKind::PageCache);
    }

    fn virt_to_phys(&self, vaddr: usize) -> Option<usize> {
        Some(ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(vaddr)).as_usize())
    }
}

struct RuntimeNotification {
    inner: ax_task::IrqNotify,
}

impl RuntimeNotification {
    const fn new() -> Self {
        Self {
            inner: ax_task::IrqNotify::new(),
        }
    }
}

impl BlockNotification for RuntimeNotification {
    fn notify(&self) {
        self.inner.notify();
    }

    fn notify_from_irq(&self) {
        self.inner.notify_irq();
    }

    #[track_caller]
    fn wait(&self) {
        self.inner.wait();
    }

    #[track_caller]
    fn wait_timeout(&self, duration: Duration) -> bool {
        self.inner.wait_timeout(duration)
    }
}

struct RuntimeBlockThread {
    task: ax_task::AxTaskRef,
}

impl BlockThread for RuntimeBlockThread {
    fn join(&self) {
        self.task.join();
    }
}

struct RuntimeTaskOps;

static ONLINE_BLOCK_CPUS: AtomicUsize = AtomicUsize::new(1);

impl BlockRuntimeOps for RuntimeTaskOps {
    fn current_cpu(&self) -> usize {
        ax_hal::percpu::this_cpu_id()
    }

    fn online_cpu_count(&self) -> usize {
        ONLINE_BLOCK_CPUS.load(Ordering::Acquire)
    }

    fn can_block(&self) -> bool {
        ax_task::current_may_uninit().is_some() && !ax_task::in_atomic_context()
    }

    fn notification(&self) -> Arc<dyn BlockNotification> {
        Arc::new(RuntimeNotification::new())
    }

    fn spawn_pinned(
        &self,
        name: String,
        cpu: usize,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> BlockResult<Box<dyn BlockThread>> {
        if cpu >= ax_hal::cpu_num() {
            return Err(BlockError::InvalidRequest);
        }
        let task = ax_task::spawn_raw(
            move || {
                let affinity = ax_task::AxCpuMask::one_shot(cpu);
                if !ax_task::set_current_affinity(affinity) {
                    error!("failed to bind block maintenance task to CPU {cpu}");
                    return;
                }
                entry();
            },
            name,
            crate::runtime_default_task_stack_size(),
        );
        Ok(Box::new(RuntimeBlockThread { task }))
    }
}

struct RuntimeBlockIrqRegistrar;

struct RuntimeBlockIrqRegistration {
    name: String,
    handle: ax_hal::irq::IrqHandle,
}

impl BlockIrqRegistration for RuntimeBlockIrqRegistration {
    fn enable(&self) -> BlockResult {
        ax_hal::irq::enable_irq(self.handle)?;
        Ok(())
    }

    fn disable_and_synchronize(&self) -> BlockResult {
        match ax_hal::irq::disable_irq(self.handle) {
            Ok(()) | Err(ax_hal::irq::IrqError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
        match ax_hal::irq::synchronize_irq(self.handle) {
            Ok(()) | Err(ax_hal::irq::IrqError::NotFound) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for RuntimeBlockIrqRegistration {
    fn drop(&mut self) {
        if let Err(error) = ax_hal::irq::free_irq(self.handle) {
            warn!(
                "failed to free block IRQ registration {}: {error:?}",
                self.name
            );
        }
    }
}

impl BlockIrqRegistrar for RuntimeBlockIrqRegistrar {
    fn register(
        &self,
        name: String,
        irq: irq_framework::IrqId,
        cpu: usize,
        mut action: BlockIrqAction,
    ) -> BlockResult<Box<dyn BlockIrqRegistration>> {
        let request = ax_hal::irq::IrqRequest::new(move |_context| match action.run() {
            BlockIrqOutcome::Unhandled => ax_hal::irq::IrqReturn::Unhandled,
            BlockIrqOutcome::Handled => ax_hal::irq::IrqReturn::Handled,
            BlockIrqOutcome::Wake => ax_hal::irq::IrqReturn::Wake,
        })
        .execution(ax_hal::irq::IrqExecution::NonReentrant)
        .share_mode(ax_hal::irq::ShareMode::Shared)
        .auto_enable(ax_hal::irq::AutoEnable::No)
        .affinity(ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(cpu)));
        let handle = ax_hal::irq::request_irq(irq, request)?;
        Ok(Box::new(RuntimeBlockIrqRegistration { name, handle }))
    }
}

static TIME_PROVIDER: RuntimeTimeProvider = RuntimeTimeProvider;
static PAGE_PROVIDER: RuntimePageProvider = RuntimePageProvider;
static TASK_OPS: RuntimeTaskOps = RuntimeTaskOps;
static IRQ_REGISTRAR: RuntimeBlockIrqRegistrar = RuntimeBlockIrqRegistrar;

pub(super) fn init(bootargs: Option<&str>) {
    ONLINE_BLOCK_CPUS.store(1, Ordering::Release);
    ax_fs_ng::os::install(
        &TIME_PROVIDER,
        &PAGE_PROVIDER,
        &TASK_OPS,
        axklib::dma::op(),
        irq_registrar(),
        None,
    );
    ax_fs_ng::root::init_root_from_rdif_sources(
        take_rdif_block_devices(),
        take_rdif_block_groups(),
        bootargs,
    );
}

#[cfg(all(feature = "smp", feature = "ipi"))]
pub(super) fn online_smp() {
    ONLINE_BLOCK_CPUS.store(ax_hal::cpu_num().max(1), Ordering::Release);
    if let Err(error) = ax_fs_ng::block::runtime::online_smp() {
        panic!("failed to expand block runtime after SMP online: {error}");
    }
}

fn irq_registrar() -> Option<&'static dyn BlockIrqRegistrar> {
    Some(&IRQ_REGISTRAR)
}

fn take_rdif_block_devices() -> Vec<RdifBlockDevice> {
    ax_driver::block::take_rdif_block_devices()
        .into_iter()
        .map(|block| {
            let (name, bindings, controller) = block.into_parts();
            let irqs = resolve_block_irqs(bindings);
            RdifBlockDevice::new_with_irqs(name, irqs, controller)
        })
        .collect()
}

fn take_rdif_block_groups() -> Vec<RdifBlockGroup> {
    ax_driver::block::take_rdif_block_groups()
        .into_iter()
        .map(|group| {
            let (name, bindings, controller) = group.into_parts();
            let irqs = resolve_block_irqs(bindings);
            RdifBlockGroup::new_with_irqs(name, irqs, controller)
        })
        .collect()
}

fn resolve_block_irqs(bindings: Vec<ax_driver::BindingIrqBinding>) -> Vec<BlockIrqSource> {
    bindings
        .into_iter()
        .filter_map(|source| {
            resolve_block_irq(source.irq).map(|irq| BlockIrqSource {
                source_id: source.source_id,
                irq,
            })
        })
        .collect()
}

fn resolve_block_irq(irq: ax_driver::BindingIrq) -> Option<irq_framework::IrqId> {
    match crate::irq::resolve_binding_irq(irq) {
        Ok(id) => Some(id),
        Err(error) => {
            warn!("failed to resolve block IRQ: {error:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_task_ops_is_available() {
        let _ = &TASK_OPS;
    }
}
