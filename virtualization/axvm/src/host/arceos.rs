//! Default private ArceOS host adapter for AxVM.

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_kspin::PreemptGuard;
use ax_memory_addr::PAGE_SIZE_4K;
use ax_std::{
    os::arceos::{api, modules},
    thread,
};
use axvm_types::{HostPhysAddr, HostVirtAddr};

use crate::{
    AxVmError, AxVmResult,
    arch::{ArchOps, CurrentArch},
    host::{HostCpu, HostMemory, HostPlatform, HostTime},
    vcpu::PinnedCpuContext,
};

/// Private default host adapter used by [`crate::AxvmRuntime`].
pub(crate) struct ArceOsHost;

const CPU_ENABLE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const CPU_ENABLE_TASK_STACK_SIZE: usize = 256 * 1024;

static ARCEOS_HOST: ArceOsHost = ArceOsHost;

pub(crate) fn arceos_host() -> &'static ArceOsHost {
    &ARCEOS_HOST
}

impl ArceOsHost {
    fn enable_current_cpu_services(&self) -> AxVmResult<usize> {
        // Storage allocation may sleep or schedule and therefore deliberately
        // happens before acquiring the CPU pin.
        let mut prepared_timer = Some(crate::timer::prepare_percpu());
        let preempt_guard = PreemptGuard::new();
        let pinned_cpu = PinnedCpuContext::new(preempt_guard.cpu_pin());
        let owner_cpu = pinned_cpu.cpu_index_usize();

        let enable_result: AxVmResult<usize> = (|| {
            crate::timer::validate_percpu_owner(&pinned_cpu)?;
            crate::percpu::init_current_cpu(&pinned_cpu)?;
            crate::percpu::enable_current_cpu(&pinned_cpu)?;
            crate::timer::install_percpu(
                &pinned_cpu,
                prepared_timer
                    .take()
                    .expect("prepared timer state may only be installed once"),
            );
            Ok(owner_cpu)
        })();
        drop(preempt_guard);

        // On failure the still-owned allocation is released only after the
        // CPU pin is gone. The success path moved it into CPU-lifetime state.
        let owner_cpu = enable_result?;
        crate::timer::start_percpu_worker(owner_cpu);
        crate::percpu::mark_cpu_enabled(owner_cpu);
        Ok(owner_cpu)
    }
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
    fn monotonic_time(&self) -> Duration {
        modules::ax_hal::time::monotonic_time()
    }
}

impl HostCpu for ArceOsHost {
    type CpuMask = api::task::AxCpuMask;

    fn cpu_count(&self) -> usize {
        modules::ax_hal::cpu_num()
    }
}

pub(crate) fn cpu_mask_from_raw_bits(bits: usize) -> api::task::AxCpuMask {
    api::task::AxCpuMask::from_raw_bits(bits)
}

pub(crate) fn cpu_mask_one_shot(cpu_id: usize) -> api::task::AxCpuMask {
    api::task::AxCpuMask::one_shot(cpu_id)
}

pub(crate) type ArceOsCpuMask = api::task::AxCpuMask;
pub(crate) type ArceOsWaitQueue = modules::ax_task::WaitQueue;
pub(crate) type ArceOsWaitQueueHandle = api::task::AxWaitQueueHandle;
pub(crate) type ArceOsTaskError = modules::ax_task::TaskError;

pub(crate) fn try_current_task() -> Result<Option<ArceOsCurrentTask>, ArceOsTaskError> {
    match modules::ax_task::current_thread_handle() {
        Ok(inner) => Ok(Some(ArceOsCurrentTask { inner })),
        Err(ArceOsTaskError::NotInitialized | ArceOsTaskError::NoRunnableThread) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn in_hard_irq() -> bool {
    modules::ax_hal::irq::in_irq_context()
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

    pub(crate) fn extension(
        &self,
    ) -> Result<Option<modules::ax_task::ThreadOsExtensionBorrow<'_>>, ArceOsTaskError> {
        modules::ax_task::thread_os_extension(&self.inner)
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

pub(crate) fn wait_queue_wait_until_deadline(
    queue: &api::task::AxWaitQueueHandle,
    deadline: Duration,
    condition: impl Fn() -> bool,
) -> bool {
    api::task::ax_wait_queue_wait_until_deadline(queue, deadline, condition)
}

pub(crate) fn wait_queue_wake(queue: &api::task::AxWaitQueueHandle, count: u32) {
    api::task::ax_wait_queue_wake(queue, count);
}

pub(crate) fn send_ipi(cpu_id: usize) -> AxVmResult {
    let preempt_guard = PreemptGuard::new();
    send_ipi_from_pinned(cpu_id, &preempt_guard)
}

fn send_ipi_from_pinned(cpu_id: usize, preempt_guard: &PreemptGuard) -> AxVmResult {
    if modules::ax_hal::percpu::this_cpu_id_pinned(preempt_guard.cpu_pin()) == cpu_id {
        return Ok(());
    }
    let irq_guard = ax_kspin::IrqGuard::new();
    let status = modules::ax_hal::irq::send_ipi(
        modules::ax_hal::irq::ipi_irq(),
        modules::ax_hal::irq::CpuIpiTarget::Other {
            cpu: modules::ax_hal::irq::CpuId(cpu_id),
        },
        &irq_guard,
    );
    match status {
        modules::ax_hal::irq::IpiSendStatus::Success => Ok(()),
        modules::ax_hal::irq::IpiSendStatus::Retry => Err(AxVmError::interrupt(
            "wake target vCPU",
            "host IPI transport is temporarily busy; scheduler wake remains published",
        )),
        modules::ax_hal::irq::IpiSendStatus::Invalid => Err(AxVmError::interrupt(
            "wake target vCPU",
            "host rejected the logical CPU IPI target",
        )),
    }
}

fn send_ipi_to_all_except_current(cpu_num: usize) -> AxVmResult {
    if cpu_num <= 1 {
        return Ok(());
    }
    let preempt_guard = PreemptGuard::new();
    let current = modules::ax_hal::percpu::this_cpu_id_pinned(preempt_guard.cpu_pin());
    for cpu in 0..cpu_num {
        if cpu != current {
            send_ipi_from_pinned(cpu, &preempt_guard)?;
        }
    }
    Ok(())
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
pub fn shutdown_host_filesystems() -> AxVmResult {
    modules::ax_fs_ng::shutdown_filesystems()
        .map_err(|error| AxVmError::host("shut down host filesystems", error))?;
    let released = modules::ax_fs_ng::release_block_irqs_for_passthrough();
    if released != 0 {
        info!("Released {released} host filesystem block IRQ registration(s) before passthrough");
    }
    Ok(())
}

impl HostPlatform for ArceOsHost {
    fn has_hardware_support(&self) -> bool {
        CurrentArch::has_hardware_support()
    }

    fn enable_virtualization_on_all_cpus(&self) -> AxVmResult {
        static CORES: AtomicUsize = AtomicUsize::new(0);

        info!("Enabling hardware virtualization support on all cores...");
        CORES.store(0, Ordering::Release);
        crate::percpu::reset_enabled_cpu_mask();

        let cpu_count = self.cpu_count();
        let current_cpu = self.enable_current_cpu_services()?;
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
                    let enabled_cpu = host
                        .enable_current_cpu_services()
                        .expect("failed to enable hardware virtualization");
                    assert_eq!(
                        enabled_cpu, cpu_id,
                        "virtualization initialization task ran outside its target CPU"
                    );
                    info!("Hardware virtualization support enabled on core {cpu_id}");
                    let _ = CORES.fetch_add(1, Ordering::Release);
                },
                alloc::format!("axvm-hv-init-{cpu_id}"),
                CPU_ENABLE_TASK_STACK_SIZE,
            );
            task.set_cpumask(<Self as HostCpu>::CpuMask::one_shot(cpu_id));
            let _task = spawn_task(task);
            send_ipi(cpu_id)?;
        }

        info!("Waiting for all cores to enable hardware virtualization...");
        let start = self.monotonic_time();
        let mut wait_rounds = 0usize;
        while CORES.load(Ordering::Acquire) != cpu_count {
            thread::yield_now();
            wait_rounds = wait_rounds.wrapping_add(1);
            if wait_rounds.is_multiple_of(256) {
                send_ipi_to_all_except_current(cpu_count)?;
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
