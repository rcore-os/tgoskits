//! RISC-V implementations of AxVM platform capability hooks.

use std::{format, vec::Vec};

use super::Riscv64Arch;
use crate::{
    AxVmResult,
    architecture::{Architecture, BootImagePlatform, GuestBootPlatform, MachinePlatform},
    ax_err_type,
};

impl Architecture for Riscv64Arch {}

impl MachinePlatform for Riscv64Arch {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture =
        crate::machine::MachineArchitecture::Riscv64;
}

impl BootImagePlatform for Riscv64Arch {
    fn load_guest_dtb(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
        dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        let bytes = dtb.as_bytes();
        let source = std::ptr::NonNull::new(bytes.as_ptr() as *mut u8)
            .ok_or_else(|| ax_err_type!(InvalidData, "Guest DTB pointer is null"))?;
        crate::boot::fdt::core::create::update_fdt(
            source,
            bytes.len(),
            &mut *loader.vm,
            &loader.config,
        )
    }
}

impl GuestBootPlatform for Riscv64Arch {
    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::GuestConfig,
        provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        crate::boot::fdt::core::prepare_dtb_guest(vm_config, vm_create_config, provider)
    }
}

pub(crate) fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub(crate) fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}

pub(super) fn resolve_cpu_index(hardware_cpu_id: usize) -> Option<usize> {
    ax_std::os::arceos::modules::ax_hal::topology::resolve_cpu_index(hardware_cpu_id)
}

pub(super) fn host_cpu_count() -> usize {
    ax_std::os::arceos::modules::ax_hal::cpu_num()
}

pub(super) fn decode_external_source(
    specifier: &[u32],
) -> AxVmResult<Option<crate::boot::fdt::core::DecodedInterrupt>> {
    use ax_plat::irq::ControllerIrqTrigger;
    let (source, controller_trigger) = match specifier {
        [source] => (*source, None),
        [source, flags] => {
            let trigger = match flags {
                1 => ControllerIrqTrigger::EdgeRising,
                2 => ControllerIrqTrigger::EdgeFailling,
                4 => ControllerIrqTrigger::LevelHigh,
                8 => ControllerIrqTrigger::LevelLow,
                _ => {
                    return Err(crate::AxVmError::invalid_config(format!(
                        "unsupported RISC-V physical interrupt flags {flags:#x}"
                    )));
                }
            };
            (*source, Some(trigger))
        }
        _ => {
            return Err(crate::AxVmError::invalid_config(
                "RISC-V external interrupt requires a PLIC or APLIC specifier",
            ));
        }
    };
    if source == 0 {
        return Err(crate::AxVmError::invalid_config(
            "RISC-V physical interrupt source is zero",
        ));
    }
    let trigger = match controller_trigger {
        Some(ControllerIrqTrigger::EdgeRising | ControllerIrqTrigger::EdgeFailling) => {
            axdevice_base::InterruptTriggerMode::EdgeTriggered
        }
        _ => axdevice_base::InterruptTriggerMode::LevelTriggered,
    };
    Ok(Some(crate::boot::fdt::core::DecodedInterrupt {
        source,
        trigger,
        controller_trigger,
    }))
}

pub(super) fn patch_runtime_fdt(
    fdt_bytes: &[u8],
    vm: &crate::AxVM,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let initrd = vm
        .config()
        .image_config
        .ramdisk
        .as_ref()
        .and_then(|ramdisk| {
            let size = ramdisk.size?;
            Some((ramdisk.load_gpa.as_usize() as u64, size as u64))
        });
    let host_fdt = crate::boot::fdt::core::try_get_host_fdt()
        .map(fdt_edit::Fdt::from_bytes)
        .transpose()
        .map_err(|err| {
            ax_err_type!(
                InvalidData,
                format!("Failed to parse host FDT while updating guest FDT: {err:#?}")
            )
        })?;
    let machine_plic =
        vm.config().plic_profile().cloned().ok_or_else(|| {
            crate::AxVmError::invalid_config("RISC-V machine profile has no PLIC")
        })?;
    let (serial_profile, serial_path, additional_serials, devices, plic_profile) = vm
        .with_planned_device_graph(|graph| {
            let serials = crate::machine::resolved_serial_devices(graph)?;
            let firmware = crate::boot::fdt::device::resolve_fdt_firmware(graph)?;
            let plic_profile =
                plic_profile_from_contribution(&firmware.specials, &serials, &machine_plic)?;
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
            Ok((
                serial.profile(),
                path,
                additional,
                firmware.devices,
                plic_profile,
            ))
        })?;
    let serial_identity = {
        let config = vm.config();
        config
            .serial_firmware_identity()
            .and_then(crate::machine::GuestSerialFirmwareIdentity::fdt)
            .filter(|identity| Some(&identity.node_path) == serial_path.as_ref())
            .cloned()
    };
    let guest_fdt = crate::boot::fdt::core::create::patch_guest_fdt_for_runtime(
        crate::boot::fdt::core::create::GuestFdtRuntimePatch {
            fdt_bytes,
            memory_regions: &vm.memory_regions(),
            devices: &devices,
            crate_config,
            serial_profile,
            serial_identity: serial_identity.as_ref(),
            additional_serials: &additional_serials,
            gic_profile: None,
            plic_profile: Some(&plic_profile),
            timer_profile: None,
            initrd_start_size: initrd,
            create_chosen: false,
        },
    )?;
    super::fdt::ensure_chosen_from_host(guest_fdt, host_fdt.as_ref())
}

fn plic_profile_from_contribution(
    specials: &[crate::boot::fdt::device::ResolvedFdtSpecial],
    serials: &[crate::machine::ResolvedSerialDevice],
    machine: &crate::machine::GuestPlicProfile,
) -> AxVmResult<crate::machine::GuestPlicProfile> {
    use crate::boot::fdt::device::ResolvedFdtSpecialKind;

    let mut controllers = specials
        .iter()
        .filter(|special| matches!(special.kind, ResolvedFdtSpecialKind::InterruptController(_)));
    let controller = controllers
        .next()
        .ok_or_else(|| crate::AxVmError::invalid_config("RISC-V FDT has no PLIC contribution"))?;
    if controllers.next().is_some() {
        return Err(crate::AxVmError::unsupported(
            "resolve RISC-V FDT topology",
            "multiple interrupt-controller contributions are not supported",
        ));
    }
    if controller.kind
        != ResolvedFdtSpecialKind::InterruptController(axdevice_base::InterruptControllerId::new(0))
        || controller.node_name != "plic"
        || controller.compatible.len() != 1
        || controller
            .compatible
            .first()
            .is_none_or(|compatible| compatible != "riscv,plic0")
        || !controller.interrupts.is_empty()
        || !controller.properties.is_empty()
    {
        return Err(crate::AxVmError::invalid_config(
            "RISC-V FDT PLIC identity differs from the runtime controller",
        ));
    }
    let [(base, length)] = controller.registers.as_slice() else {
        return Err(crate::AxVmError::invalid_config(
            "RISC-V FDT PLIC contribution must resolve one MMIO window",
        ));
    };
    if (*base, *length) != (machine.base as u64, machine.length as u64) {
        return Err(crate::AxVmError::invalid_config(
            "RISC-V FDT PLIC resources differ from the machine profile",
        ));
    }
    let consoles = specials
        .iter()
        .filter(|special| special.kind == ResolvedFdtSpecialKind::Console)
        .collect::<std::vec::Vec<_>>();
    if consoles.len() != serials.len()
        || serials
            .iter()
            .any(|serial| consoles.iter().all(|console| console.id != serial.id()))
        || consoles.iter().any(|console| {
            serials
                .iter()
                .find(|serial| serial.id() == console.id)
                .is_none_or(|serial| {
                    !crate::boot::fdt::device::fdt_console_matches_serial(
                        console,
                        serial,
                        axdevice_base::InterruptControllerId::new(0),
                    )
                })
        })
    {
        return Err(crate::AxVmError::invalid_config(
            "RISC-V console contributions differ from resolved serial devices",
        ));
    }
    if specials.len() != 1 + consoles.len() {
        return Err(crate::AxVmError::unsupported(
            "resolve RISC-V FDT topology",
            "the graph contains an unsupported special contribution",
        ));
    }
    Ok(crate::machine::GuestPlicProfile {
        node_path: machine.node_path.clone(),
        node_phandle: machine.node_phandle,
        base: usize::try_from(*base)
            .map_err(|_| crate::AxVmError::invalid_config("PLIC base exceeds usize"))?,
        length: usize::try_from(*length)
            .map_err(|_| crate::AxVmError::invalid_config("PLIC length exceeds usize"))?,
    })
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
    use ax_plat::irq::ControllerIrqTrigger;
    use axdevice_base::InterruptTriggerMode;

    use super::decode_external_source;
    use crate::{AxVmError, boot::fdt::core::DecodedInterrupt};

    #[test]
    fn external_interrupt_decodes_plic_and_aplic_specifiers() {
        assert_eq!(
            decode_external_source(&[8]).unwrap(),
            Some(DecodedInterrupt {
                source: 8,
                trigger: InterruptTriggerMode::LevelTriggered,
                controller_trigger: None,
            })
        );

        for (flags, trigger, controller_trigger) in [
            (
                1,
                InterruptTriggerMode::EdgeTriggered,
                ControllerIrqTrigger::EdgeRising,
            ),
            (
                2,
                InterruptTriggerMode::EdgeTriggered,
                ControllerIrqTrigger::EdgeFailling,
            ),
            (
                4,
                InterruptTriggerMode::LevelTriggered,
                ControllerIrqTrigger::LevelHigh,
            ),
            (
                8,
                InterruptTriggerMode::LevelTriggered,
                ControllerIrqTrigger::LevelLow,
            ),
        ] {
            assert_eq!(
                decode_external_source(&[8, flags]).unwrap(),
                Some(DecodedInterrupt {
                    source: 8,
                    trigger,
                    controller_trigger: Some(controller_trigger),
                })
            );
        }

        for specifier in [&[][..], &[0], &[0, 4], &[8, 0], &[8, 3], &[8, 4, 1]] {
            assert!(matches!(
                decode_external_source(specifier),
                Err(AxVmError::InvalidConfig { .. })
            ));
        }
    }
}
