//! AArch64 implementations of AxVM platform capability hooks.

use ax_std::os::arceos::modules::ax_hal;

use super::Aarch64Arch;
use crate::{
    AxVmResult,
    architecture::{
        BootImagePlatform, GuestBootPlatform, HostTimePlatform, capabilities::VmTimerIntegration,
    },
    ax_err_type,
};

impl HostTimePlatform for Aarch64Arch {
    const VM_TIMER_INTEGRATION: VmTimerIntegration = VmTimerIntegration::RuntimeCallback;
}

impl BootImagePlatform for Aarch64Arch {
    fn load_guest_dtb(
        loader: &crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        let bytes = dtb.as_bytes();
        let source = core::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        super::fdt::core::update_fdt(source, bytes.len(), loader.vm.clone(), &loader.config)
    }
}

impl GuestBootPlatform for Aarch64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::AxVMCrateConfig,
        provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        let guest_dtb = super::fdt::core::prepare_dtb_guest(vm_config, vm_create_config, provider)?;
        let host_ipi = ax_hal::irq::ipi_irq().hwirq.0;
        let host_timer = ax_hal::time::irq_num().hwirq.0;
        let assigned_interrupts = vm_config
            .machine_plan()
            .assigned_host_interrupts()
            .iter()
            .map(crate::machine::HostInterruptResource::input_u32)
            .collect::<alloc::vec::Vec<_>>();
        let roles =
            super::gic::Aarch64InterruptRoles::discover(super::gic::Aarch64InterruptDiscovery {
                host_ipi_intid: host_ipi,
                host_timer_intid: host_timer,
                host_fdt_bytes: super::fdt::try_get_host_fdt(),
                guest_fdt_bytes: guest_dtb.as_ref().map(|dtb| dtb.as_bytes()),
                assigned_device_intids: &assigned_interrupts,
            })?;
        vm_config.arch_mut().set_interrupt_roles(roles);
        Ok(guest_dtb)
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}

pub(crate) fn host_fdt_bytes() -> Option<&'static [u8]> {
    ax_hal::dtb::get_fdt().map(|fdt| fdt.as_slice())
}

pub(super) fn logical_cpu_id(hardware_cpu_id: usize) -> Option<usize> {
    (0..ax_hal::cpu_num())
        .find(|logical_cpu_id| ax_hal::cpu_hardware_id(*logical_cpu_id) == Some(hardware_cpu_id))
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::AxVMCrateConfig,
) -> AxVmResult<alloc::vec::Vec<u8>> {
    let memory_regions = vm.memory_regions();
    let initrd = vm.with_config(|config| {
        super::fdt::initrd_start_size_from_image_config(config.image_config.ramdisk.as_ref())
    });
    let patched = super::fdt::core::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &memory_regions,
        crate_config,
        initrd,
        true,
    )?;
    let patched = super::fdt::patch_physical_timer_interrupts(&patched)?;
    Ok(patched)
}
