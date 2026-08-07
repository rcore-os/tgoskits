//! RISC-V implementations of AxVM platform capability hooks.

use std::{format, vec::Vec};

use super::Riscv64Arch;
use crate::{
    AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform, HostTimePlatform, MachinePlatform},
    ax_err_type,
};

impl HostTimePlatform for Riscv64Arch {}

impl MachinePlatform for Riscv64Arch {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture =
        crate::machine::MachineArchitecture::Riscv64;
}

impl BootImagePlatform for Riscv64Arch {
    fn load_guest_dtb(
        loader: &crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        let bytes = dtb.as_bytes();
        let source = std::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        super::fdt::core::update_fdt(source, bytes.len(), loader.vm.clone(), &loader.config)
    }
}

impl GuestBootPlatform for Riscv64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::GuestConfig,
        provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        super::fdt::core::prepare_dtb_guest(vm_config, vm_create_config, provider)
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}

pub(super) fn resolve_cpu_index(hardware_cpu_id: usize) -> Option<usize> {
    ax_std::os::arceos::modules::ax_hal::topology::resolve_cpu_index(hardware_cpu_id)
}

pub(super) fn host_cpu_count() -> usize {
    ax_std::os::arceos::modules::ax_hal::cpu_num()
}

pub(super) fn decode_plic_source(specifier: &[u32]) -> Option<super::fdt::core::DecodedInterrupt> {
    let source = specifier.first().copied().filter(|source| *source != 0)?;
    Some(super::fdt::core::DecodedInterrupt {
        source,
        trigger: axdevice_base::InterruptTriggerMode::LevelTriggered,
    })
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVMRef,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let host_fdt = super::fdt::core::try_get_host_fdt()
        .map(fdt_edit::Fdt::from_bytes)
        .transpose()
        .map_err(|err| {
            ax_err_type!(
                InvalidData,
                format!("Failed to parse host FDT while updating guest FDT: {err:#?}")
            )
        })?;
    let (serial_profile, serial_path, additional_serials, ivc_channels) = vm
        .with_planned_device_graph(|graph| {
            let serials = crate::machine::resolved_serial_devices(graph)?;
            let ivc_channels = crate::machine::resolved_ivc_channels(graph)?;
            let serial = serials
                .iter()
                .find(|serial| serial.id() == "console0")
                .ok_or_else(|| crate::AxVmError::invalid_config("RISC-V plan has no console0"))?;
            let path = match serial.firmware_binding() {
                axdevice::DeviceFirmwareBinding::FdtNode(path) => Some(path.clone()),
                _ => None,
            };
            let additional: Vec<_> = serials
                .iter()
                .filter(|serial| serial.id() != "console0")
                .map(crate::machine::ResolvedSerialDevice::profile)
                .collect();
            Ok((serial.profile(), path, additional, ivc_channels))
        })?;
    let (serial_identity, plic_profile) = vm.with_config(|config| {
        (
            config
                .serial_firmware_identity()
                .and_then(crate::machine::GuestSerialFirmwareIdentity::fdt)
                .filter(|identity| Some(&identity.node_path) == serial_path.as_ref())
                .cloned(),
            config.plic_profile().cloned(),
        )
    });
    let guest_fdt = super::fdt::core::create::patch_guest_fdt_for_runtime(
        fdt_bytes,
        &vm.memory_regions(),
        &ivc_channels,
        crate_config,
        serial_profile,
        serial_identity.as_ref(),
        &additional_serials,
        None,
        plic_profile.as_ref(),
        None,
        None,
        false,
    )?;
    super::fdt::ensure_chosen_from_host(guest_fdt, host_fdt.as_ref())
}

pub(super) fn patch_provided_fdt(
    provided_dtb: &[u8],
    _host_dtb: Option<&[u8]>,
    _crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    Ok(provided_dtb.to_vec())
}

#[cfg(test)]
mod tests {
    #[test]
    fn plic_interrupt_uses_first_nonzero_fdt_cell() {
        assert_eq!(
            super::decode_plic_source(&[8]).map(|interrupt| interrupt.source),
            Some(8)
        );
        assert_eq!(super::decode_plic_source(&[0]), None);
        assert_eq!(super::decode_plic_source(&[]), None);
    }
}
