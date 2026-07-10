//! Target architecture selection and stable internal dispatch.

use ax_errno::AxResult;
use ax_memory_addr::PhysAddr;
use axvm_types::NestedPagingConfig;

pub(crate) use crate::architecture::*;
use crate::architecture::{
    AddressSpacePlatform, BootImagePlatform, DevicePlatform, GuestBootPlatform, HostTimePlatform,
};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::Aarch64Arch as CurrentArch;
#[cfg(target_arch = "aarch64")]
pub use aarch64::ImageLoader;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::fdt;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::LoongArch64Arch as CurrentArch;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::boot as guest_platform;
#[cfg(target_arch = "loongarch64")]
pub use loongarch64::boot::ImageLoader;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::fdt;
#[cfg(not(target_arch = "loongarch64"))]
pub(crate) mod guest_platform {
    #[doc(hidden)]
    pub const SUPPORTED: bool = false;
}
#[cfg(target_arch = "riscv64")]
pub use riscv64::ImageLoader;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::Riscv64Arch as CurrentArch;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::fdt;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::X86_64Arch as CurrentArch;
#[cfg(target_arch = "x86_64")]
pub use x86_64::boot::ImageLoader;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::fdt;

/// Architecture-specific public compatibility exports.
pub mod platform {
    #[cfg(target_arch = "aarch64")]
    pub use super::aarch64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "loongarch64")]
    pub use super::loongarch64::irq::{
        register_guest_irq_route as register_loongarch_guest_irq_route,
        unregister_guest_irq_routes as unregister_loongarch_guest_irq_routes,
    };
    #[cfg(target_arch = "loongarch64")]
    pub use super::loongarch64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "riscv64")]
    pub use super::riscv64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "x86_64")]
    pub use super::x86_64::irq::{
        register_ioapic_irq_forwarding_activator as register_x86_ioapic_irq_forwarding_activator,
        register_ioapic_irq_forwarding_route as register_x86_ioapic_irq_forwarding_route,
        register_ioapic_irq_forwarding_route_with_trigger as register_x86_ioapic_irq_forwarding_route_with_trigger,
    };
    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "loongarch64"),
        any(feature = "fs", feature = "host-fs")
    ))]
    pub use crate::host::arceos::shutdown_host_filesystems;
}

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;
pub(crate) type ArchNestedPageTable = <CurrentArch as ArchOps>::NestedPageTable;

pub(crate) fn guest_page_table_levels(
    vcpu_mappings: &[(usize, Option<usize>, usize)],
) -> AxResult<usize> {
    CurrentArch::guest_page_table_levels(vcpu_mappings)
}

pub(crate) fn new_nested_page_table(levels: usize) -> AxResult<ArchNestedPageTable> {
    CurrentArch::new_nested_page_table(levels)
}

pub(crate) fn nested_paging_config(
    root_paddr: PhysAddr,
    levels: usize,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
) -> AxResult<NestedPagingConfig> {
    CurrentArch::nested_paging_config(root_paddr, levels, vcpu_mappings)
}

pub(crate) fn configure_interrupt_fabric(
    factories: &mut axdevice::DeviceFactoryRegistry,
    mode: axvm_types::VMInterruptMode,
    configs: &[axvm_types::EmulatedDeviceConfig],
) -> AxResult<crate::InterruptFabric> {
    CurrentArch::configure_interrupt_fabric(factories, mode, configs)
}

pub(crate) fn register_arch_devices(
    vm: &crate::AxVM,
    config: &crate::config::AxVMConfig,
    devices: &mut axdevice::AxVmDevices,
) -> AxResult {
    CurrentArch::register_devices(vm, config, devices)
}

pub(crate) fn append_arch_owned_regions(
    regions: &mut alloc::vec::Vec<crate::layout::GuestOwnedRegion>,
) {
    CurrentArch::append_owned_regions(regions);
}

pub(crate) fn map_arch_address_space(
    address_space: &mut axaddrspace::AddrSpace<ArchNestedPageTable>,
) -> AxResult {
    CurrentArch::map_address_space(address_space)
}

pub(crate) fn register_timer_callback() {
    CurrentArch::register_timer_callback();
}

pub(crate) fn set_oneshot_timer(deadline_ns: u64) {
    CurrentArch::set_oneshot_timer(deadline_ns);
}

pub(crate) fn init_guest_boot_resources() {
    CurrentArch::init_guest_boot_resources();
}

pub(crate) fn prepare_guest_boot(
    vm_config: &mut crate::config::AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> AxResult<Option<crate::boot::fdt::GuestDtbImage>> {
    CurrentArch::prepare_guest_boot(vm_config, vm_create_config, provider)
}

pub(crate) fn load_images_from_memory(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    images: crate::boot::StaticVmImage,
) -> AxResult {
    CurrentArch::load_images_from_memory(loader, images)
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
pub(crate) fn load_images_from_filesystem(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
) -> AxResult {
    CurrentArch::load_images_from_filesystem(loader)
}

pub(crate) fn is_x86_linux_image_config(
    config: &axvmconfig::AxVMCrateConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> bool {
    CurrentArch::is_x86_linux_image_config(config, provider)
}

pub(crate) fn default_boot_firmware_load_gpa(
    config: &axvmconfig::AxVMCrateConfig,
) -> Option<axvm_types::GuestPhysAddr> {
    CurrentArch::default_boot_firmware_load_gpa(config)
}
