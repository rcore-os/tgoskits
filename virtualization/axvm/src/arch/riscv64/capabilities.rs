//! RISC-V implementations of AxVM platform capability hooks.

use ax_errno::{AxResult, ax_err_type};

use super::Riscv64Arch;
use crate::architecture::{
    AddressSpacePlatform, BootImagePlatform, DevicePlatform, HostTimePlatform,
};

impl DevicePlatform for Riscv64Arch {
    fn configure_interrupt_fabric(
        factories: &mut axdevice::DeviceFactoryRegistry,
        mode: axvm_types::VMInterruptMode,
        configs: &[axvm_types::EmulatedDeviceConfig],
    ) -> AxResult<crate::InterruptFabric> {
        super::irq::configure(factories, mode, configs)
    }
}

impl AddressSpacePlatform for Riscv64Arch {}

impl HostTimePlatform for Riscv64Arch {}

impl BootImagePlatform for Riscv64Arch {
    fn load_guest_dtb(
        loader: &crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxResult {
        let bytes = dtb.as_bytes();
        let source = core::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        crate::boot::fdt::update_fdt(source, bytes.len(), loader.vm.clone(), &loader.config)
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}
