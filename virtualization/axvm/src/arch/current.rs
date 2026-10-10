//! Compile-time binding to the architecture selected by the build target.

#[cfg(target_arch = "loongarch64")]
pub(crate) use target::irq::LOONGARCH_MAX_IRQ_COUNT;
#[cfg(target_arch = "aarch64")]
pub(crate) use target::{Aarch64Arch as CurrentArch, Aarch64VmPlan as ArchVmPlan};
#[cfg(target_arch = "loongarch64")]
pub(crate) use target::{LoongArch64Arch as CurrentArch, LoongArchVmPlan as ArchVmPlan};
#[cfg(target_arch = "riscv64")]
pub(crate) use target::{Riscv64Arch as CurrentArch, RiscvVmPlan as ArchVmPlan};
#[cfg(target_arch = "x86_64")]
pub(crate) use target::{X86_64Arch as CurrentArch, X86VmPlan as ArchVmPlan};

#[cfg(target_arch = "aarch64")]
use super::aarch64 as target;
#[cfg(target_arch = "loongarch64")]
use super::loongarch64 as target;
#[cfg(target_arch = "riscv64")]
use super::riscv64 as target;
#[cfg(target_arch = "x86_64")]
use super::x86_64 as target;
use super::*;

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;
pub(crate) type ArchNestedPageTable = <CurrentArch as ArchOps>::NestedPageTable;

pub(crate) fn boot_vcpu_ids(count: usize) -> std::ops::Range<usize> {
    cfg_select! {
        any(target_arch = "aarch64", target_arch = "riscv64") => 0..count.min(1),
        _ => 0..count,
    }
}

pub(crate) fn initialize_cpu_on(
    vcpu: &mut crate::vm::VCpu,
    entry: crate::GuestPhysAddr,
    argument: usize,
) -> AxVmResult {
    cfg_select! {
        any(target_arch = "aarch64", target_arch = "riscv64") => vcpu.with_backend(|backend| {
            <CurrentArch as crate::architecture::ops::CpuOn>::initialize_cpu_on(
                backend, entry, argument,
            )
        }),
        _ => {
            let _ = (vcpu, entry, argument);
            Err(crate::AxVmError::unsupported(
                "CPU_ON",
                "architecture has no CPU_ON capability",
            ))
        }
    }
}

fn assert_architecture<T: Architecture>() {}
const _: fn() = assert_architecture::<CurrentArch>;

pub(crate) fn make_guest_memory_visible(addr: ax_memory_addr::VirtAddr, size: usize) {
    CurrentArch::make_guest_memory_visible(addr, size);
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn guest_fdt_policy() -> crate::boot::fdt::core::GuestFdtPolicy {
    target::fdt::guest_fdt_policy()
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn host_fdt_bootarg() -> usize {
    target::fdt::host_fdt_bootarg()
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    target::fdt::host_phys_to_virt(paddr)
}

pub(crate) fn register_platform_irq_injector() {
    #[cfg(target_arch = "loongarch64")]
    target::irq::register_platform_irq_injector();
}

pub(crate) fn init_guest_boot_resources() {
    CurrentArch::init_guest_boot_resources();
}

pub(crate) fn prepare_guest_boot(
    vm_config: &mut crate::config::AxVMConfig,
    vm_create_config: &mut axvmconfig::GuestConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
    CurrentArch::prepare_guest_boot(vm_config, vm_create_config, provider)
}

pub(crate) fn load_images_from_filesystem(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
) -> AxVmResult {
    CurrentArch::load_images_from_filesystem(loader)
}

pub(crate) fn guest_boot_policy(
    config: &axvmconfig::GuestConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> crate::config::GuestBootPolicy {
    CurrentArch::guest_boot_policy(config, provider)
}

pub(crate) fn default_boot_firmware_load_gpa(
    config: &axvmconfig::GuestConfig,
) -> Option<axvm_types::GuestPhysAddr> {
    CurrentArch::default_boot_firmware_load_gpa(config)
}

/// Completes fallible host discovery before per-CPU hardware ownership begins.
pub(crate) fn prepare_host_virtualization() -> AxVmResult {
    #[cfg(target_arch = "aarch64")]
    target::prepare_host_virtualization()?;
    Ok(())
}

/// Binds task services before sealing the architecture's execution entry.
pub(crate) fn prepare_task_services(
    resources: &crate::vm::AxVMResources,
    memory: crate::GuestMemoryPort,
) -> AxVmResult {
    cfg_select! {
        target_arch = "aarch64" => target::bind_task_memory(resources, memory),
        _ => {
            let _ = (resources, memory);
            Ok(())
        }
    }
}
