//! Compile-time binding to the architecture selected by the build target.

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

pub(crate) fn register_vm_platform_resources(vm: &crate::AxVMRef) {
    #[cfg(target_arch = "loongarch64")]
    target::irq::register_vm_guest_irq_routes(vm);
    #[cfg(not(target_arch = "loongarch64"))]
    let _ = vm;
}

pub(crate) fn unregister_vm_platform_resources(vm_id: crate::VMId) {
    #[cfg(target_arch = "loongarch64")]
    target::irq::unregister_guest_irq_routes(vm_id);
    #[cfg(not(target_arch = "loongarch64"))]
    let _ = vm_id;
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub(crate) fn register_host_irq_forwarding_route_with_trigger(
    vm: &crate::AxVMRef,
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: crate::InterruptTriggerMode,
) -> AxVmResult {
    target::irq::register_ioapic_irq_forwarding_route_with_trigger(vm, guest_gsi, host_irq, trigger)
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub(crate) fn register_host_irq_forwarding_activator(
    vm: &crate::AxVMRef,
    guest_gsi: usize,
    activator: fn(),
) -> AxVmResult {
    target::irq::register_ioapic_irq_forwarding_activator(vm, guest_gsi, activator)
}

pub(crate) fn register_timer_source(
    deadline_source: std::sync::Arc<crate::timer::PublishedTimerDeadline>,
    notify: std::sync::Arc<ax_std::os::arceos::modules::ax_task::IrqNotify>,
) {
    CurrentArch::register_timer_source(deadline_source, notify);
}

#[cfg(not(test))]
pub(crate) fn request_timer_deadline(deadline_ns: u64) {
    CurrentArch::request_timer_deadline(deadline_ns);
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

pub(crate) fn load_images_from_memory(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    images: crate::boot::StaticVmImage,
) -> AxVmResult {
    CurrentArch::load_images_from_memory(loader, images)
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_target_implements_complete_architecture_contract() {
        assert_architecture::<CurrentArch>();
    }
}
