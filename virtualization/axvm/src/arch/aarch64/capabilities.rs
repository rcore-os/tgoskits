//! AArch64 implementations of AxVM platform capability hooks.

use alloc::{format, sync::Arc};

use ax_errno::{AxResult, ax_err_type};
use axdevice_base::DeviceRegistry as _;
use axvm_types::VMInterruptMode;

use super::Aarch64Arch;
use crate::architecture::{
    AddressSpacePlatform, BootImagePlatform, DevicePlatform, HostTimePlatform,
};

impl DevicePlatform for Aarch64Arch {
    fn register_devices(
        vm: &crate::AxVM,
        config: &crate::config::AxVMConfig,
        devices: &mut axdevice::AxVmDevices,
    ) -> AxResult {
        if config.interrupt_mode() == VMInterruptMode::Passthrough {
            assign_passthrough_spis(vm, config, devices);
        } else {
            register_virtual_timers(devices)?;
        }
        Ok(())
    }
}

impl AddressSpacePlatform for Aarch64Arch {}

impl HostTimePlatform for Aarch64Arch {}

impl BootImagePlatform for Aarch64Arch {
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

fn assign_passthrough_spis(
    vm: &crate::AxVM,
    config: &crate::config::AxVMConfig,
    devices: &axdevice::AxVmDevices,
) {
    let cpu_id = vm.id() - 1; // FIXME: get the real CPU id.
    let Some(gicd) = devices
        .devices()
        .find_map(|device| device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>())
    else {
        warn!("Failed to assign SPIs: No VGicD found in device list");
        return;
    };

    for spi in config.pass_through_spis() {
        gicd.assign_irq(*spi + 32, cpu_id, (0, 0, 0, cpu_id as _));
    }
}

fn register_virtual_timers(devices: &mut axdevice::AxVmDevices) -> AxResult {
    for device in axdevice::create_vtimer_devices() {
        devices
            .register(Arc::from(device) as Arc<dyn axdevice_base::Device>)
            .map_err(|err| ax_err_type!(InvalidInput, format!("register vtimer: {err:?}")))?;
    }
    Ok(())
}
