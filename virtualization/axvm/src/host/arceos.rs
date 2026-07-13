//! Default private ArceOS host adapter for AxVM.

extern crate alloc;

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
))]
use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_alloc::{self, UsageKind};
use ax_errno::AxResult;
use ax_memory_addr::PAGE_SIZE_4K;
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

pub(super) static ARCEOS_HOST: ArceOsHost = ArceOsHost;

impl HostMemory for ArceOsHost {
    fn alloc_frame(&self) -> Option<HostPhysAddr> {
        ax_alloc::global_allocator()
            .alloc_pages(1, PAGE_SIZE_4K, UsageKind::PageTable)
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(&self, paddr: HostPhysAddr) {
        ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            1,
            UsageKind::PageTable,
        );
    }

    fn alloc_contiguous_frames(
        &self,
        num_frames: usize,
        frame_align: usize,
    ) -> Option<HostPhysAddr> {
        ax_alloc::global_allocator()
            .alloc_pages(num_frames, frame_align.max(PAGE_SIZE_4K), UsageKind::Dma)
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_contiguous_frames(&self, paddr: HostPhysAddr, num_frames: usize) {
        ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            num_frames,
            UsageKind::Dma,
        );
    }

    fn phys_to_virt(&self, paddr: HostPhysAddr) -> HostVirtAddr {
        ax_hal::mem::phys_to_virt(paddr)
    }

    fn virt_to_phys(&self, vaddr: HostVirtAddr) -> HostPhysAddr {
        ax_hal::mem::virt_to_phys(vaddr)
    }
}

impl HostTime for ArceOsHost {
    type CancelToken = usize;

    #[cfg(target_arch = "x86_64")]
    fn nanos_to_ticks(&self, nanos: u64) -> u64 {
        ax_hal::time::nanos_to_ticks(nanos)
    }

    fn monotonic_time(&self) -> Duration {
        ax_hal::time::monotonic_time()
    }

    #[cfg(not(target_arch = "loongarch64"))]
    fn set_oneshot_timer(&self, deadline_ns: u64) {
        ax_hal::time::set_oneshot_timer(deadline_ns);
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

#[cfg(target_arch = "loongarch64")]
pub(crate) fn set_irq_enabled(raw_irq: usize, enabled: bool) {
    let gsi = match u32::try_from(raw_irq) {
        Ok(gsi) => gsi,
        Err(_) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: out of GSI range");
            return;
        }
    };
    let irq = match ax_hal::irq::resolve_irq_source(ax_hal::irq::IrqSource::AcpiGsi(gsi)) {
        Ok(irq) => irq,
        Err(err) => {
            warn!("failed to resolve LoongArch passthrough IRQ {raw_irq}: {err:?}");
            return;
        }
    };
    if let Err(err) = ax_hal::irq::set_enable(irq, enabled) {
        warn!(
            "failed to set LoongArch passthrough IRQ {raw_irq} ({irq:?}) enabled={enabled}: \
             {err:?}"
        );
    }
}

impl HostCpu for ArceOsHost {
    type CpuMask = ax_task::AxCpuMask;

    fn cpu_count(&self) -> usize {
        ax_hal::cpu_num()
    }

    fn this_cpu_id(&self) -> usize {
        ax_hal::percpu::this_cpu_id()
    }
}

pub(crate) fn send_ipi(cpu_id: usize) {
    if ax_hal::percpu::this_cpu_id() == cpu_id {
        return;
    }
    ax_hal::irq::send_ipi(
        ax_hal::irq::ipi_irq(),
        ax_hal::irq::IpiTarget::Other { cpu_id },
    );
}

fn send_ipi_to_all_except_current(cpu_num: usize) {
    if cpu_num <= 1 {
        return;
    }
    let cpu_id = ax_hal::percpu::this_cpu_id();
    ax_hal::irq::send_ipi(
        ax_hal::irq::ipi_irq(),
        ax_hal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num },
    );
}

#[cfg(all(
    any(feature = "fs", feature = "host-fs"),
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
pub(crate) fn shutdown_host_filesystems() -> AxResult {
    ax_fs_ng::shutdown_filesystems()?;
    let released = ax_fs_ng::release_block_irqs_for_passthrough();
    if released != 0 {
        info!("Released {released} host filesystem block IRQ registration(s) before passthrough");
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
impl HostConsole for ArceOsHost {
    fn write_bytes(&self, bytes: &[u8]) {
        ax_hal::console::write_bytes(bytes);
    }

    fn read_bytes(&self, bytes: &mut [u8]) -> usize {
        ax_hal::console::read_bytes(bytes)
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
            let task = ax_task::TaskInner::new(
                move || {
                    info!("Core {cpu_id} is initializing hardware virtualization support...");
                    ARCEOS_HOST
                        .enable_virtualization_on_current_cpu()
                        .expect("failed to enable hardware virtualization");
                    info!("Hardware virtualization support enabled on core {cpu_id}");
                    let _ = CORES.fetch_add(1, Ordering::Release);
                },
                alloc::format!("axvm-hv-init-{cpu_id}"),
                ax_task::default_task_stack_size(),
            );
            task.set_cpumask(<Self as HostCpu>::CpuMask::one_shot(cpu_id));
            ax_task::spawn_task(task);
            if cpu_id != self.this_cpu_id() {
                send_ipi(cpu_id);
            }
        }

        info!("Waiting for all cores to enable hardware virtualization...");
        let start = self.monotonic_time();
        let mut wait_rounds = 0usize;
        while CORES.load(Ordering::Acquire) != cpu_count {
            ax_task::yield_now();
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
