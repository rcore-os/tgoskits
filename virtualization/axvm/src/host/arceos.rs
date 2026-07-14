//! Default private ArceOS host adapter for AxVM.

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::AxResult;
use ax_memory_addr::PAGE_SIZE_4K;
use ax_std::{
    os::arceos::{api, modules},
    thread,
};
use axvm_types::{HostPhysAddr, HostVirtAddr};

#[cfg(target_arch = "x86_64")]
use crate::host::HostConsole;
use crate::{
    arch::{ArchOps, CurrentArch},
    host::{HostCpu, HostMemory, HostPlatform, HostTime},
};

/// Private default host adapter used by [`crate::AxvmRuntime`].
pub(crate) struct ArceOsHost;

const CPU_ENABLE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const CPU_ENABLE_TASK_STACK_SIZE: usize = 256 * 1024;

static ARCEOS_HOST: ArceOsHost = ArceOsHost;

pub(crate) fn arceos_host() -> &'static ArceOsHost {
    &ARCEOS_HOST
}

impl HostMemory for ArceOsHost {
    fn alloc_frame(&self) -> Option<HostPhysAddr> {
        modules::ax_alloc::global_allocator()
            .alloc_pages(1, PAGE_SIZE_4K, modules::ax_alloc::UsageKind::PageTable)
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(&self, paddr: HostPhysAddr) {
        modules::ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            1,
            modules::ax_alloc::UsageKind::PageTable,
        );
    }

    fn alloc_contiguous_frames(
        &self,
        num_frames: usize,
        frame_align: usize,
    ) -> Option<HostPhysAddr> {
        modules::ax_alloc::global_allocator()
            .alloc_pages(
                num_frames,
                frame_align.max(PAGE_SIZE_4K),
                modules::ax_alloc::UsageKind::Dma,
            )
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_contiguous_frames(&self, paddr: HostPhysAddr, num_frames: usize) {
        modules::ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            num_frames,
            modules::ax_alloc::UsageKind::Dma,
        );
    }

    fn phys_to_virt(&self, paddr: HostPhysAddr) -> HostVirtAddr {
        modules::ax_hal::mem::phys_to_virt(paddr)
    }

    fn virt_to_phys(&self, vaddr: HostVirtAddr) -> HostPhysAddr {
        modules::ax_hal::mem::virt_to_phys(vaddr)
    }
}

impl HostTime for ArceOsHost {
    type CancelToken = usize;

    #[cfg(target_arch = "x86_64")]
    fn nanos_to_ticks(&self, nanos: u64) -> u64 {
        modules::ax_hal::time::nanos_to_ticks(nanos)
    }

    fn monotonic_time(&self) -> Duration {
        modules::ax_hal::time::monotonic_time()
    }

    fn set_oneshot_timer(&self, deadline_ns: u64) {
        modules::ax_hal::time::set_oneshot_timer(deadline_ns);
    }

    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    ))]
    fn register_timer(
        &self,
        deadline_ns: u64,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> Self::CancelToken {
        crate::timer::register_timer(deadline_ns, callback)
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
    fn cancel_timer(&self, token: Self::CancelToken) {
        crate::timer::cancel_timer(token);
    }
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn monotonic_time_nanos() -> u64 {
    modules::ax_hal::time::monotonic_time_nanos()
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn handle_host_irq(vector: usize) -> Option<usize> {
    modules::ax_hal::irq::handle_irq(vector).then_some(vector)
}

pub(crate) fn dispatch_host_irq(vector: usize) {
    modules::ax_hal::irq::handle_irq(vector);
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn set_irq_enabled(raw_irq: usize, enabled: bool) {
    let gsi = match u32::try_from(raw_irq) {
        Ok(gsi) => gsi,
        Err(_) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: out of GSI range");
            return;
        }
    };
    let irq = match modules::ax_hal::irq::resolve_irq_source(
        modules::ax_hal::irq::IrqSource::AcpiGsi(gsi),
    ) {
        Ok(irq) => irq,
        Err(err) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: {err:?}");
            return;
        }
    };
    if let Err(err) = modules::ax_hal::irq::set_enable(irq, enabled) {
        warn!(
            "failed to set LoongArch passthrough IRQ {raw_irq} ({irq:?}) enabled={enabled}: \
             {err:?}"
        );
    }
}

impl HostCpu for ArceOsHost {
    type CpuMask = api::task::AxCpuMask;

    fn cpu_count(&self) -> usize {
        modules::ax_hal::cpu_num()
    }

    fn this_cpu_id(&self) -> usize {
        modules::ax_hal::percpu::this_cpu_id()
    }
}

pub(crate) fn cpu_mask_from_raw_bits(bits: usize) -> api::task::AxCpuMask {
    api::task::AxCpuMask::from_raw_bits(bits)
}

pub(crate) type ArceOsCpuMask = api::task::AxCpuMask;
pub(crate) type ArceOsWaitQueue = modules::ax_task::WaitQueue;
pub(crate) type ArceOsWaitQueueHandle = api::task::AxWaitQueueHandle;

pub(crate) fn current_task() -> ArceOsCurrentTask {
    let inner = modules::ax_task::current_thread_handle()
        .unwrap_or_else(|error| panic!("AxVM current task is unavailable: {error}"));
    ArceOsCurrentTask { inner }
}

pub(crate) fn spawn_task(mut task: ArceOsTaskInner) -> ArceOsAxTaskRef {
    let name = Arc::<str>::from(task.name.as_str());
    let entry = task
        .entry
        .take()
        .expect("an AxVM task entry may only be spawned once");
    let affinity = task.affinity.as_ref().map(scheduler_cpu_set);
    let extension = task
        .extension
        .take()
        .map(crate::task::into_thread_extension);
    // SAFETY: `into_thread_extension` transfers the unique VCpuTask allocation
    // to ax-runtime, whose composed extension releases it exactly once.
    let inner = unsafe {
        modules::ax_task::spawn_raw_with_extension_and_affinity(
            entry,
            task.name,
            task.stack_size,
            extension,
            affinity,
        )
    }
    .unwrap_or_else(|error| panic!("failed to spawn AxVM task {name}: {error}"));
    ArceOsAxTaskRef { inner, name }
}

type ArceOsTaskEntry = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct ArceOsTaskInner {
    entry: Option<ArceOsTaskEntry>,
    name: String,
    stack_size: usize,
    affinity: Option<ArceOsCpuMask>,
    extension: Option<crate::VCpuTask>,
}

impl ArceOsTaskInner {
    pub(crate) fn new<F>(entry: F, name: String, stack_size: usize) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self {
            entry: Some(Box::new(entry)),
            name,
            stack_size,
            affinity: None,
            extension: None,
        }
    }

    pub(crate) fn set_cpumask(&mut self, cpumask: ArceOsCpuMask) {
        self.affinity = Some(cpumask);
    }

    pub(crate) fn set_vcpu_extension(&mut self, extension: crate::VCpuTask) {
        self.extension = Some(extension);
    }

    pub(crate) fn id_name(&self) -> &str {
        &self.name
    }

    pub(crate) fn cpumask(&self) -> ArceOsCpuMask {
        self.affinity.unwrap_or_else(ArceOsCpuMask::full)
    }
}

#[derive(Clone)]
pub(crate) struct ArceOsAxTaskRef {
    inner: modules::ax_task::ThreadHandle,
    name: Arc<str>,
}

impl ArceOsAxTaskRef {
    pub(crate) fn cpu_id(&self) -> u32 {
        self.inner
            .wake_handle()
            .target_cpu()
            .map_or(0, modules::ax_task::CpuId::as_u32)
    }

    pub(crate) fn id_name(&self) -> &str {
        &self.name
    }

    pub(crate) fn join(self) -> i32 {
        modules::ax_task::join_thread(self.inner)
            .unwrap_or_else(|error| panic!("failed to join AxVM task {}: {error}", self.name))
    }
}

pub(crate) struct ArceOsCurrentTask {
    inner: modules::ax_task::ThreadHandle,
}

impl ArceOsCurrentTask {
    pub(crate) fn ptr_eq(&self, task: &ArceOsAxTaskRef) -> bool {
        self.inner.id() == task.inner.id()
    }

    pub(crate) fn extension(&self) -> Option<modules::ax_task::ThreadOsExtensionBorrow<'_>> {
        modules::ax_task::thread_os_extension(&self.inner)
            .ok()
            .flatten()
    }
}

fn scheduler_cpu_set(cpumask: &ArceOsCpuMask) -> modules::ax_task::CpuSet {
    let topology_len = modules::ax_hal::cpu_num();
    let mut affinity = modules::ax_task::CpuSet::empty(topology_len);
    for cpu in cpumask {
        let cpu = u32::try_from(cpu).expect("AxVM CPU index must fit the scheduler ABI");
        assert!(
            affinity.insert(modules::ax_task::CpuId::new(cpu)),
            "AxVM CPU affinity includes CPU {cpu} outside the scheduler topology"
        );
    }
    affinity
}

pub(crate) fn yield_now() {
    thread::yield_now();
}

pub(crate) fn wait_queue_wait_until(
    queue: &api::task::AxWaitQueueHandle,
    condition: impl Fn() -> bool,
) {
    api::task::ax_wait_queue_wait_until(queue, condition, None);
}

pub(crate) fn wait_queue_wake(queue: &api::task::AxWaitQueueHandle, count: u32) {
    api::task::ax_wait_queue_wake(queue, count);
}

pub(crate) fn send_ipi(cpu_id: usize) {
    if modules::ax_hal::percpu::this_cpu_id() == cpu_id {
        return;
    }
    modules::ax_hal::irq::send_ipi(
        modules::ax_hal::irq::ipi_irq(),
        modules::ax_hal::irq::IpiTarget::Other { cpu_id },
    );
}

fn send_ipi_to_all_except_current(cpu_num: usize) {
    if cpu_num <= 1 {
        return;
    }
    let cpu_id = modules::ax_hal::percpu::this_cpu_id();
    modules::ax_hal::irq::send_ipi(
        modules::ax_hal::irq::ipi_irq(),
        modules::ax_hal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num },
    );
}

#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqContext = modules::ax_hal::irq::IrqContext;
#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqError = modules::ax_hal::irq::IrqError;
#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqHandle = modules::ax_hal::irq::IrqHandle;
#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqId = modules::ax_hal::irq::IrqId;
#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqReturn = modules::ax_hal::irq::IrqReturn;
#[cfg(target_arch = "x86_64")]
pub(crate) type ArceOsIrqSource = modules::ax_hal::irq::IrqSource;
#[cfg(target_arch = "x86_64")]
pub(crate) fn request_shared_irq(
    irq: ArceOsIrqId,
    handler: impl FnMut(ArceOsIrqContext) -> ArceOsIrqReturn + Send + 'static,
) -> Result<ArceOsIrqHandle, ArceOsIrqError> {
    modules::ax_hal::irq::request_shared_irq(irq, handler)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn make_irq_id(domain: u16, hwirq: u32) -> ArceOsIrqId {
    modules::ax_hal::irq::IrqId::new(
        modules::ax_hal::irq::IrqDomainId(domain),
        modules::ax_hal::irq::HwIrq(hwirq),
    )
}

#[cfg(all(target_arch = "x86_64", not(test)))]
pub(crate) fn set_irq_enable(irq: ArceOsIrqId, enabled: bool) -> Result<(), ArceOsIrqError> {
    modules::ax_hal::irq::set_enable(irq, enabled)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn resolve_irq_source(source: ArceOsIrqSource) -> Result<ArceOsIrqId, ArceOsIrqError> {
    modules::ax_hal::irq::resolve_irq_source(source)
}

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub(crate) fn host_fdt_bootarg() -> usize {
    modules::ax_hal::dtb::get_bootarg()
}

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub(crate) fn phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    modules::ax_hal::mem::phys_to_virt(paddr)
}

#[cfg(all(
    any(feature = "fs", feature = "host-fs"),
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
pub(crate) fn shutdown_host_filesystems() -> AxResult {
    modules::ax_fs_ng::shutdown_filesystems()?;
    let released = modules::ax_fs_ng::release_block_irqs_for_passthrough();
    if released != 0 {
        info!("Released {released} host filesystem block IRQ registration(s) before passthrough");
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
impl HostConsole for ArceOsHost {
    fn write_bytes(&self, bytes: &[u8]) {
        modules::ax_hal::console::write_bytes(bytes);
    }

    fn read_bytes(&self, bytes: &mut [u8]) -> usize {
        modules::ax_hal::console::read_bytes(bytes)
    }
}

impl HostPlatform for ArceOsHost {
    fn has_hardware_support(&self) -> bool {
        CurrentArch::has_hardware_support()
    }

    fn enable_virtualization_on_current_cpu(&self) -> AxResult {
        crate::timer::init_percpu();
        crate::percpu::init_current_cpu()?;
        crate::percpu::enable_current_cpu()?;
        crate::percpu::mark_cpu_enabled(self.this_cpu_id());
        Ok(())
    }

    fn enable_virtualization_on_all_cpus(&self) -> AxResult {
        static CORES: AtomicUsize = AtomicUsize::new(0);

        info!("Enabling hardware virtualization support on all cores...");
        CORES.store(0, Ordering::Release);
        crate::percpu::reset_enabled_cpu_mask();

        let cpu_count = self.cpu_count();
        let current_cpu = self.this_cpu_id();
        info!("Core {current_cpu} is initializing hardware virtualization support...");
        self.enable_virtualization_on_current_cpu()?;
        info!("Hardware virtualization support enabled on core {current_cpu}");
        CORES.store(1, Ordering::Release);

        for cpu_id in 0..cpu_count {
            if cpu_id == current_cpu {
                continue;
            }
            let mut task = ArceOsTaskInner::new(
                move || {
                    let host = arceos_host();
                    info!("Core {cpu_id} is initializing hardware virtualization support...");
                    host.enable_virtualization_on_current_cpu()
                        .expect("failed to enable hardware virtualization");
                    info!("Hardware virtualization support enabled on core {cpu_id}");
                    let _ = CORES.fetch_add(1, Ordering::Release);
                },
                alloc::format!("axvm-hv-init-{cpu_id}"),
                CPU_ENABLE_TASK_STACK_SIZE,
            );
            task.set_cpumask(<Self as HostCpu>::CpuMask::one_shot(cpu_id));
            let _task = spawn_task(task);
            if cpu_id != self.this_cpu_id() {
                send_ipi(cpu_id);
            }
        }

        info!("Waiting for all cores to enable hardware virtualization...");
        let start = self.monotonic_time();
        let mut wait_rounds = 0usize;
        while CORES.load(Ordering::Acquire) != cpu_count {
            thread::yield_now();
            wait_rounds = wait_rounds.wrapping_add(1);
            if wait_rounds.is_multiple_of(256) {
                send_ipi_to_all_except_current(cpu_count);
            }
            if self.monotonic_time().saturating_sub(start) >= CPU_ENABLE_WAIT_TIMEOUT {
                break;
            }
        }
        CurrentArch::register_platform_irq_injector();
        let enabled_count = CORES.load(Ordering::Acquire);
        if enabled_count == cpu_count {
            info!("All cores have enabled hardware virtualization support.");
        } else {
            warn!(
                "Only {enabled_count}/{cpu_count} cores enabled hardware virtualization before \
                 timeout; continuing with host CPU mask {:#x}",
                crate::percpu::enabled_cpu_mask()
            );
        }
        Ok(())
    }
}
