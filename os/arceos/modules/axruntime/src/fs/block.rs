use alloc::{boxed::Box, string::String, vec::Vec};

use ax_alloc::UsageKind;
use ax_fs_ng::{
    block::runtime::{BlockIrqSource, RdifBlockDevice},
    os::{
        BlockIrqOutcome, BlockIrqRegistrar, BlockIrqRegistration, BlockTaskOps, BlockTimeProvider,
        FsPage, FsPageProvider,
    },
};

struct RuntimeTimeProvider;

impl BlockTimeProvider for RuntimeTimeProvider {
    fn wall_time(&self) -> core::time::Duration {
        ax_hal::time::wall_time()
    }
}

struct RuntimePageProvider;

impl FsPageProvider for RuntimePageProvider {
    fn alloc_page(&self) -> ax_errno::AxResult<FsPage> {
        let addr = ax_alloc::global_allocator()
            .alloc_pages(1, ax_fs_ng::os::memory::PAGE_SIZE, UsageKind::PageCache)
            .map_err(|_| ax_errno::AxError::NoMemory)?;
        Ok(unsafe { FsPage::from_raw(addr) })
    }

    fn dealloc_page(&self, page: FsPage) {
        ax_alloc::global_allocator().dealloc_pages(page.addr(), 1, UsageKind::PageCache);
    }

    fn virt_to_phys(&self, vaddr: usize) -> Option<usize> {
        Some(ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(vaddr)).as_usize())
    }
}

static TIME_PROVIDER: RuntimeTimeProvider = RuntimeTimeProvider;
static PAGE_PROVIDER: RuntimePageProvider = RuntimePageProvider;
static TASK_OPS: RuntimeTaskOps = RuntimeTaskOps;
static BLOCK_IO_WAIT_WQ: ax_task::WaitQueue = ax_task::WaitQueue::new();
static BLOCK_DRAIN_NOTIFY: ax_task::IrqNotify = ax_task::IrqNotify::new();
#[cfg(feature = "irq")]
static IRQ_REGISTRAR: RuntimeBlockIrqRegistrar = RuntimeBlockIrqRegistrar;

struct RuntimeTaskOps;

impl BlockTaskOps for RuntimeTaskOps {
    fn current_task_id(&self) -> Option<u64> {
        ax_task::current_may_uninit().map(|curr| curr.id().as_u64())
    }

    fn can_block(&self) -> bool {
        ax_task::current_may_uninit().is_some() && !ax_task::in_atomic_context()
    }

    fn task_yield(&self) {
        ax_task::yield_now();
    }

    fn task_wait(&self) {
        BLOCK_IO_WAIT_WQ.wait();
    }

    fn task_wait_timeout(&self, dur: core::time::Duration) -> bool {
        BLOCK_IO_WAIT_WQ.wait_timeout(dur)
    }

    fn task_wait_until(&self, condition: &dyn Fn() -> bool) {
        BLOCK_IO_WAIT_WQ.wait_until(condition);
    }

    fn wake_task(&self, task_id: u64) {
        let _ = ax_task::wake_task_by_id(task_id);
    }

    fn notify_waiters(&self) {
        BLOCK_IO_WAIT_WQ.notify_all(false);
    }

    fn notify_drain(&self) {
        BLOCK_DRAIN_NOTIFY.notify();
    }

    fn notify_drain_from_irq(&self) {
        BLOCK_DRAIN_NOTIFY.notify_irq();
    }

    fn wait_for_drain_notification(&self) {
        BLOCK_DRAIN_NOTIFY.wait();
    }

    fn spawn(&self, name: String, f: Box<dyn FnOnce() + Send + 'static>) {
        ax_task::spawn_raw(f, name, crate::runtime_default_task_stack_size());
    }
}

#[cfg(feature = "irq")]
struct RuntimeBlockIrqRegistrar;

#[cfg(feature = "irq")]
struct RuntimeBlockIrqRegistration {
    _inner: crate::irq::Registration,
}

// SAFETY: The registration token is kept in the global block runtime only to
// own the IRQ action lifetime. The move-only boxed callback state is owned by
// the IRQ framework and is not exposed through this token.
unsafe impl Sync for RuntimeBlockIrqRegistration {}

#[cfg(feature = "irq")]
impl BlockIrqRegistration for RuntimeBlockIrqRegistration {}

fn map_block_irq_error(err: ax_hal::irq::IrqError) -> ax_errno::AxError {
    match err {
        ax_hal::irq::IrqError::InvalidIrq | ax_hal::irq::IrqError::InvalidCpu => {
            ax_errno::AxError::InvalidInput
        }
        ax_hal::irq::IrqError::CpuOffline | ax_hal::irq::IrqError::Unsupported => {
            ax_errno::AxError::Unsupported
        }
        ax_hal::irq::IrqError::Busy | ax_hal::irq::IrqError::InIrqContext => {
            ax_errno::AxError::ResourceBusy
        }
        ax_hal::irq::IrqError::Timeout => ax_errno::AxError::TimedOut,
        ax_hal::irq::IrqError::NoMemory => ax_errno::AxError::NoMemory,
        ax_hal::irq::IrqError::NotFound => ax_errno::AxError::NotFound,
        ax_hal::irq::IrqError::Controller => ax_errno::AxError::Io,
    }
}

#[cfg(feature = "irq")]
impl BlockIrqRegistrar for RuntimeBlockIrqRegistrar {
    fn register_shared(
        &self,
        name: String,
        irq: irq_framework::IrqId,
        mut action: Box<dyn FnMut(ax_hal::irq::IrqContext) -> BlockIrqOutcome + Send + 'static>,
    ) -> ax_errno::AxResult<Box<dyn BlockIrqRegistration>> {
        crate::irq::Registration::register_shared(name, irq, move |ctx| match action(ctx) {
            BlockIrqOutcome::Handled => ax_hal::irq::IrqReturn::Handled,
            BlockIrqOutcome::Wake => ax_hal::irq::IrqReturn::Wake,
        })
        .map(|inner| Box::new(RuntimeBlockIrqRegistration { _inner: inner }) as _)
        .map_err(map_block_irq_error)
    }
}

pub(super) fn init(bootargs: Option<&str>) {
    ax_fs_ng::os::install(
        &TIME_PROVIDER,
        &PAGE_PROVIDER,
        &TASK_OPS,
        axklib::dma::op(),
        irq_registrar(),
    );
    ax_fs_ng::root::init_root_from_rdif(take_rdif_block_devices(), bootargs);
}

#[cfg(feature = "irq")]
fn irq_registrar() -> Option<&'static dyn BlockIrqRegistrar> {
    Some(&IRQ_REGISTRAR)
}

#[cfg(not(feature = "irq"))]
fn irq_registrar() -> Option<&'static dyn BlockIrqRegistrar> {
    None
}

fn take_rdif_block_devices() -> Vec<RdifBlockDevice> {
    ax_driver::block::take_rdif_block_devices()
        .into_iter()
        .map(|block| {
            let name = String::from(block.name());
            let irqs = resolve_block_irqs(&block);
            RdifBlockDevice::new_with_irqs(name, irqs, block.into_interface())
        })
        .collect()
}

#[cfg(feature = "irq")]
fn resolve_block_irqs(block: &ax_driver::block::RdifBlockDevice) -> Vec<BlockIrqSource> {
    block
        .irq_sources()
        .iter()
        .filter_map(|source| {
            resolve_block_irq(Some(source.irq.clone())).map(|irq| BlockIrqSource {
                source_id: source.source_id,
                irq,
            })
        })
        .collect()
}

#[cfg(not(feature = "irq"))]
fn resolve_block_irqs(_block: &ax_driver::block::RdifBlockDevice) -> Vec<BlockIrqSource> {
    Vec::new()
}

#[cfg(feature = "irq")]
fn resolve_block_irq(irq: Option<ax_driver::BindingIrq>) -> Option<irq_framework::IrqId> {
    let irq = irq?;
    match crate::irq::resolve_binding_irq(irq) {
        Ok(id) => Some(id),
        Err(err) => {
            warn!("failed to resolve block IRQ: {err:?}");
            None
        }
    }
}

#[cfg(not(feature = "irq"))]
fn resolve_block_irq(_irq: Option<ax_driver::BindingIrq>) -> Option<irq_framework::IrqId> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_task_ops_spawns_without_panicking() {
        let _ = &TASK_OPS;
    }
}
