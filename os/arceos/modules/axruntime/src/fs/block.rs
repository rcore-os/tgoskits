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
use ax_lazyinit::LazyInit;
use ax_task::runtime::RuntimeStatus;

use crate::{
    sync::SpinLock,
    task::{CpuId, CpuSet, IrqWaitCell, IrqWorkerWaiter, TaskError, ThreadHandle, ThreadId},
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
    event: IrqWaitCell,
    waiter: LazyInit<RuntimeNotificationWaiter>,
}

struct RuntimeNotificationWaiter {
    owner: ThreadId,
    irq: IrqWorkerWaiter,
}

impl RuntimeNotification {
    const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
            waiter: LazyInit::new(),
        }
    }

    fn publish(&self) {
        let _result = self.event.notify();
    }

    fn wait_inner(&self, timeout: Option<Duration>) -> bool {
        let current = crate::task::current_thread_handle()
            .unwrap_or_else(|error| panic!("block notification has no scheduler thread: {error}"));
        let waiter = self.waiter.get_or_init(|| RuntimeNotificationWaiter {
            owner: current.id(),
            irq: IrqWorkerWaiter::new(current.wake_handle()),
        });
        assert_eq!(
            waiter.owner,
            current.id(),
            "one block notification must be consumed by one fixed service thread"
        );

        match timeout {
            Some(timeout) => waiter
                .irq
                .wait_timeout(&self.event, timeout)
                .unwrap_or_else(|error| panic!("block notification wait failed: {error}")),
            None => {
                waiter
                    .irq
                    .wait(&self.event)
                    .unwrap_or_else(|error| panic!("block notification wait failed: {error}"));
                false
            }
        }
    }
}

impl BlockNotification for RuntimeNotification {
    fn notify(&self) {
        self.publish();
    }

    #[track_caller]
    fn wait(&self) {
        let _timed_out = self.wait_inner(None);
    }

    #[track_caller]
    fn wait_timeout(&self, duration: Duration) -> bool {
        self.wait_inner(Some(duration))
    }
}

struct RuntimeBlockThread {
    // Joining consumes the scheduler handle. This gate protects only the
    // move-out and is always released before the potentially blocking join.
    // No IRQ path observes this state, so masking local IRQs would only widen
    // interrupt latency without adding serialization.
    task: SpinLock<Option<ThreadHandle>>,
}

impl BlockThread for RuntimeBlockThread {
    fn join(&self) {
        let Some(task) = self.task.lock().take() else {
            return;
        };
        crate::task::join_thread(task)
            .unwrap_or_else(|error| panic!("failed to join block maintenance thread: {error}"));
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
        crate::task::current_thread_id().is_ok() && !crate::guard::in_atomic_context()
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
        let cpu = u32::try_from(cpu).map_err(|_| BlockError::InvalidRequest)?;
        let mut affinity = CpuSet::empty(ax_hal::cpu_num());
        if !affinity.insert(CpuId::new(cpu)) {
            return Err(BlockError::InvalidRequest);
        }
        let task = crate::task::spawn_raw_with_affinity(
            entry,
            name,
            crate::runtime_default_task_stack_size(),
            affinity,
        )
        .map_err(task_error_to_block_error)?;
        Ok(Box::new(RuntimeBlockThread {
            task: SpinLock::new(Some(task)),
        }))
    }
}

fn task_error_to_block_error(error: TaskError) -> BlockError {
    match error {
        TaskError::InvalidConfiguration
        | TaskError::InvalidCpuCount(_)
        | TaskError::InvalidCpu(_)
        | TaskError::InvalidNice(_)
        | TaskError::InvalidRtPriority(_)
        | TaskError::InvalidRoundRobinQuantum
        | TaskError::InvalidDeadline { .. }
        | TaskError::UnsupportedDeadlineFlags(_) => BlockError::InvalidRequest,
        // Linux kthread_create_on_node() reports task-object allocation and
        // kernel-thread capacity failures as ENOMEM to kernel worker callers.
        TaskError::TimerCapacity | TaskError::ThreadCapacity => BlockError::NoMemory,
        TaskError::RuntimeFailure(status) if status == RuntimeStatus::NoMemory as u32 => {
            BlockError::NoMemory
        }
        TaskError::CpuOffline(_)
        | TaskError::CpuNotQuiescent(_)
        | TaskError::LastOnlineCpu(_)
        | TaskError::DeadlineAdmission
        | TaskError::DeadlineAffinity
        | TaskError::ActiveTimerAffinity
        | TaskError::ThreadBusy => BlockError::ResourceBusy,
        TaskError::StaleThreadId => BlockError::NotFound,
        TaskError::UnsafeContext
        | TaskError::NotInitialized
        | TaskError::InvalidRuntimeHandle
        | TaskError::CpuOwnerBorrowed
        | TaskError::CpuOwnerMismatch { .. }
        | TaskError::ExecutorOwnerMismatch { .. }
        | TaskError::CpuAlreadyOnline(_)
        | TaskError::InvalidTransition { .. }
        | TaskError::AlreadyQueued
        | TaskError::NotReady
        | TaskError::NotExited
        | TaskError::NoRunnableThread
        | TaskError::InvalidPiState
        | TaskError::InvalidPiWaitState(_)
        | TaskError::PiCycle
        | TaskError::PiChainLimit { .. }
        | TaskError::RuntimeFailure(_) => BlockError::InvalidState,
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

    #[test]
    fn block_worker_thread_capacity_matches_linux_kthread_enomem() {
        assert_eq!(
            task_error_to_block_error(TaskError::ThreadCapacity),
            BlockError::NoMemory
        );
    }
}
