//! Default private ArceOS host adapter for AxVM.

extern crate alloc;

use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::AxResult;
use ax_memory_addr::PAGE_SIZE_4K;
use ax_page_table_multiarch::PagingHandler;
use ax_std::{
    os::arceos::{api, modules},
    thread,
};
use axvm_types::{HostPhysAddr, HostVirtAddr};

#[cfg(target_arch = "x86_64")]
use crate::host::HostConsole;
use crate::host::{HostCpu, HostMemory, HostPlatform, HostTime};

/// Private default host adapter used by [`crate::AxvmRuntime`].
pub(crate) struct ArceOsHost;

static ARCEOS_HOST: ArceOsHost = ArceOsHost;

pub(crate) fn arceos_host() -> &'static ArceOsHost {
    &ARCEOS_HOST
}

impl HostMemory for ArceOsHost {
    fn alloc_frame(&self) -> Option<HostPhysAddr> {
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::alloc_frame()
    }

    fn dealloc_frame(&self, paddr: HostPhysAddr) {
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::dealloc_frame(paddr);
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
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::phys_to_virt(paddr)
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

    fn register_timer(
        &self,
        deadline_ns: u64,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> Self::CancelToken {
        crate::timer::register_timer(deadline_ns, callback)
    }

    #[cfg(target_arch = "x86_64")]
    fn cancel_timer(&self, token: Self::CancelToken) {
        crate::timer::cancel_timer(token);
    }
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn monotonic_time_nanos() -> u64 {
    modules::ax_hal::time::monotonic_time_nanos()
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn handle_host_irq(vector: usize) -> bool {
    modules::ax_hal::irq::handle_irq(vector)
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn dispatch_host_irq(vector: usize) {
    modules::ax_hal::trap::irq_handler(vector);
}

impl HostCpu for ArceOsHost {
    type CpuMask = api::task::AxCpuMask;

    fn cpu_count(&self) -> usize {
        modules::ax_hal::cpu_num()
    }

    fn this_cpu_id(&self) -> usize {
        modules::ax_hal::percpu::this_cpu_id()
    }

    fn bind_current_to_cpu(&self, cpu_id: usize) -> AxResult {
        api::task::ax_set_current_affinity(Self::CpuMask::one_shot(cpu_id))
    }
}

pub(crate) fn cpu_mask_from_raw_bits(bits: usize) -> api::task::AxCpuMask {
    api::task::AxCpuMask::from_raw_bits(bits)
}

pub(crate) type ArceOsCpuMask = api::task::AxCpuMask;
pub type ArceOsAxTaskExt = modules::ax_task::AxTaskExt;
pub type ArceOsAxTaskRef = modules::ax_task::AxTaskRef;
pub type ArceOsCurrentTask = modules::ax_task::CurrentTask;
pub type ArceOsTaskInner = modules::ax_task::TaskInner;
pub type ArceOsWaitQueue = modules::ax_task::WaitQueue;
pub type ArceOsWaitQueueHandle = api::task::AxWaitQueueHandle;
pub use modules::ax_task::TaskExt as ArceOsTaskExt;

pub(crate) fn current_task() -> ArceOsCurrentTask {
    modules::ax_task::current()
}

pub(crate) fn spawn_task(task: ArceOsTaskInner) -> ArceOsAxTaskRef {
    modules::ax_task::spawn_task(task)
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

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub fn host_fdt_bootarg() -> usize {
    modules::ax_hal::dtb::get_bootarg()
}

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub fn phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    modules::ax_hal::mem::phys_to_virt(paddr)
}

#[cfg(all(any(feature = "fs", feature = "host-fs"), target_arch = "x86_64"))]
pub fn shutdown_host_filesystems() -> AxResult {
    modules::ax_fs::shutdown_filesystems()
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
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                x86_vcpu::has_hardware_support()
            } else if #[cfg(target_arch = "riscv64")] {
                riscv_vcpu::has_hardware_support()
            } else if #[cfg(target_arch = "loongarch64")] {
                loongarch_vcpu::has_hardware_support()
            } else if #[cfg(target_arch = "aarch64")] {
                arm_vcpu::has_hardware_support()
            } else {
                false
            }
        }
    }

    fn enable_virtualization_on_current_cpu(&self) -> AxResult {
        crate::timer::init_percpu();
        crate::percpu::init_current_cpu()?;
        crate::percpu::enable_current_cpu()
    }

    fn enable_virtualization_on_all_cpus(&self) -> AxResult {
        static CORES: AtomicUsize = AtomicUsize::new(0);

        info!("Enabling hardware virtualization support on all cores...");
        CORES.store(0, Ordering::Release);

        let cpu_count = self.cpu_count();
        for cpu_id in 0..cpu_count {
            thread::spawn(move || {
                let host = arceos_host();
                info!("Core {cpu_id} is initializing hardware virtualization support...");
                host.bind_current_to_cpu(cpu_id)
                    .expect("failed to initialize CPU affinity");
                host.enable_virtualization_on_current_cpu()
                    .expect("failed to enable hardware virtualization");
                info!("Hardware virtualization support enabled on core {cpu_id}");
                let _ = CORES.fetch_add(1, Ordering::Release);
            });
        }

        info!("Waiting for all cores to enable hardware virtualization...");
        while CORES.load(Ordering::Acquire) != cpu_count {
            thread::yield_now();
        }
        crate::arch::register_platform_irq_injector();
        info!("All cores have enabled hardware virtualization support.");
        Ok(())
    }
}
