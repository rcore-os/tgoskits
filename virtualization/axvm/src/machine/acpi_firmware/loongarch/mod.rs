//! LoongArch relocatable ACPI generation for fw_cfg-aware firmware.

mod fw_cfg;
mod tables;

use alloc::{format, string::ToString, vec, vec::Vec};

use acpi_tables::{facs::FACS, rsdp::Rsdp, xsdt::XSDT};

use self::{fw_cfg::*, tables::*};
use super::{ACPI_HEADER_LENGTH, OEM_ID, OEM_REVISION, OEM_TABLE_ID, TABLE_ALIGNMENT, aml_bytes};
use crate::machine::{
    AddressRange, InterruptControllerPlan, LoongArchInterruptPlan, LoongArchPlatformPlan,
    LoongArchPowerPlan, MachinePlanError, MachinePlanResult, VmMachinePlan,
};

#[derive(Clone, Copy, Debug)]
struct AcpiMemoryRegion {
    base: u64,
    size: u64,
}

#[derive(Clone, Copy, Debug)]
struct LoongArchAcpiSerial {
    base: u64,
    size: u64,
    gsi: u32,
    clock_hz: u32,
    baud: u32,
}

#[derive(Clone, Copy, Debug)]
struct LoongArchAcpiPci {
    ecam_base: u64,
    ecam_size: u64,
    mmio_base: u64,
    mmio_size: u64,
    io_base: u64,
    io_size: u64,
}

#[derive(Clone, Copy, Debug)]
struct LoongArchAcpiInterrupts {
    eiointc_irq: u8,
    pch_msi_base: u64,
    pch_msi_start: u32,
    pch_msi_count: u32,
    pch_pic_base: u64,
    pch_pic_size: u16,
    pch_pic_gsi_base: u16,
}

#[derive(Clone, Debug)]
struct LoongArchAcpiResources {
    memory: Vec<AcpiMemoryRegion>,
    serial: Option<LoongArchAcpiSerial>,
    pci: LoongArchAcpiPci,
    interrupts: LoongArchAcpiInterrupts,
}

/// Generates relocatable LoongArch ACPI files from one finalized plan.
pub fn generate_loongarch_fw_cfg_acpi(
    plan: &VmMachinePlan,
    cpu_count: usize,
) -> MachinePlanResult<axdevice::FwCfgAcpiFiles> {
    let cpu_count = checked_cpu_count(cpu_count)?;
    let controller = planned_loongarch_controller(plan)?;
    let machine = planned_loongarch_platform(plan)?;
    let platform = resolve_acpi_resources(plan, controller, machine)?;

    build_loongarch_fw_cfg_acpi(plan, cpu_count, &platform, machine.power())
}

fn checked_cpu_count(cpu_count: usize) -> MachinePlanResult<u16> {
    let cpu_count = u16::try_from(cpu_count).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!("LoongArch ACPI supports at most {} vCPUs", u16::MAX),
    })?;
    if cpu_count == 0 {
        return Err(MachinePlanError::InvalidFirmware {
            detail: "LoongArch ACPI requires at least one vCPU".into(),
        });
    }
    Ok(cpu_count)
}

fn resolve_acpi_resources(
    plan: &VmMachinePlan,
    controller: &LoongArchInterruptPlan,
    machine: &LoongArchPlatformPlan,
) -> MachinePlanResult<LoongArchAcpiResources> {
    let memory = plan
        .guest_memory()
        .iter()
        .map(|memory| AcpiMemoryRegion {
            base: memory.base(),
            size: memory.size(),
        })
        .collect::<Vec<_>>();
    let routing = controller.routing();
    let acpi_routing = routing.acpi();
    let serial = planned_loongarch_ns16550(plan)?
        .map(|serial| -> MachinePlanResult<LoongArchAcpiSerial> {
            let gsi = u32::from(acpi_routing.pch_pic_gsi_base())
                .checked_add(serial.interrupt)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: "LoongArch serial GSI overflows".into(),
                })?;
            Ok(LoongArchAcpiSerial {
                base: serial.mmio.base(),
                size: serial.mmio.size(),
                gsi,
                clock_hz: 100_000_000,
                baud: 115_200,
            })
        })
        .transpose()?;
    let pch_pic_size = u16::try_from(controller.pch_pic().size()).map_err(|_| {
        MachinePlanError::InvalidFirmware {
            detail: format!(
                "LoongArch PCH-PIC size {:#x} exceeds the ACPI field width",
                controller.pch_pic().size()
            ),
        }
    })?;
    let pci = machine.pci();

    Ok(LoongArchAcpiResources {
        memory,
        serial,
        pci: LoongArchAcpiPci {
            ecam_base: pci.ecam().base(),
            ecam_size: pci.ecam().size(),
            mmio_base: pci.mmio().base(),
            mmio_size: pci.mmio().size(),
            io_base: pci.io().base(),
            io_size: pci.io().size(),
        },
        interrupts: LoongArchAcpiInterrupts {
            eiointc_irq: routing.eiointc_irq(),
            pch_msi_base: controller.pch_msi().base(),
            pch_msi_start: acpi_routing.pch_msi_start(),
            pch_msi_count: acpi_routing.pch_msi_count(),
            pch_pic_base: controller.pch_pic().base(),
            pch_pic_size,
            pch_pic_gsi_base: acpi_routing.pch_pic_gsi_base(),
        },
    })
}

fn build_loongarch_fw_cfg_acpi(
    plan: &VmMachinePlan,
    cpu_count: u16,
    platform: &LoongArchAcpiResources,
    power: LoongArchPowerPlan,
) -> MachinePlanResult<axdevice::FwCfgAcpiFiles> {
    let mut tables = Vec::new();
    let mut loader = Vec::new();
    push_allocate_tables(&mut loader, TABLE_ALIGNMENT as u32);

    let facs = append_table(&mut tables, &aml_bytes(&FACS::new()))?;
    let dsdt = append_sdt(&mut tables, &mut loader, &build_dsdt(plan, platform)?)?;
    let fadt_bytes = build_fadt(facs, dsdt, power);
    let fadt = append_table(&mut tables, &fadt_bytes)?;
    push_table_pointer(&mut loader, fadt + 132, 8);
    push_table_pointer(&mut loader, fadt + 140, 8);
    push_sdt_checksum(&mut loader, fadt, fadt_bytes.len())?;

    let madt = append_sdt(
        &mut tables,
        &mut loader,
        &build_madt(cpu_count, &platform.interrupts),
    )?;
    let srat = append_sdt(&mut tables, &mut loader, &build_srat(&platform.memory))?;
    let spcr = platform
        .serial
        .as_ref()
        .map(|serial| append_sdt(&mut tables, &mut loader, &build_spcr(serial)))
        .transpose()?;
    let mcfg = append_sdt(&mut tables, &mut loader, &build_mcfg(&platform.pci)?)?;

    let mut root_entries = vec![fadt, madt, srat];
    if let Some(spcr) = spcr {
        root_entries.push(spcr);
    }
    root_entries.push(mcfg);
    let xsdt = build_xsdt(&mut tables, &mut loader, &root_entries)?;
    let rsdp = aml_bytes(&Rsdp::new(OEM_ID, u64::from(xsdt)));
    push_rsdp_loader_entries(&mut loader, rsdp.len())?;

    axdevice::FwCfgAcpiFiles::new(tables, rsdp, loader).map_err(|error| {
        MachinePlanError::FirmwareEncoding {
            detail: error.to_string(),
        }
    })
}

fn build_xsdt(
    tables: &mut Vec<u8>,
    loader: &mut Vec<u8>,
    root_entries: &[u32],
) -> MachinePlanResult<u32> {
    let mut xsdt = XSDT::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    for offset in root_entries {
        xsdt.add_entry(u64::from(*offset));
    }
    let xsdt_bytes = aml_bytes(&xsdt);
    let xsdt = append_table(tables, &xsdt_bytes)?;
    for index in 0..root_entries.len() {
        let entry_offset =
            u32::try_from(ACPI_HEADER_LENGTH + index * size_of::<u64>()).map_err(|_| {
                MachinePlanError::FirmwareEncoding {
                    detail: "XSDT entry offset exceeds u32".into(),
                }
            })?;
        push_table_pointer(loader, xsdt + entry_offset, 8);
    }
    push_sdt_checksum(loader, xsdt, xsdt_bytes.len())?;
    Ok(xsdt)
}

fn planned_loongarch_controller(
    plan: &VmMachinePlan,
) -> MachinePlanResult<&LoongArchInterruptPlan> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::LoongArch(controller)) => Ok(controller),
        Some(_) => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate LoongArch ACPI from another architecture's controller plan"
                .into(),
        }),
        None => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate LoongArch ACPI without an interrupt-controller plan".into(),
        }),
    }
}

fn planned_loongarch_platform(plan: &VmMachinePlan) -> MachinePlanResult<&LoongArchPlatformPlan> {
    plan.loongarch_platform()
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "cannot generate LoongArch ACPI without a platform plan".into(),
        })
}

#[derive(Clone, Copy, Debug)]
struct PlannedLoongArchNs16550 {
    mmio: AddressRange,
    interrupt: u32,
}

fn planned_loongarch_ns16550(
    plan: &VmMachinePlan,
) -> MachinePlanResult<Option<PlannedLoongArchNs16550>> {
    plan.virtual_devices()
        .iter()
        .find(|device| device.model_id().as_str() == "ns16550a")
        .map(|device| {
            let mmio = device
                .mmio()
                .iter()
                .find(|resource| resource.slot().as_str() == "registers")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "LoongArch 16550 instance '{}' has no 'registers' resource",
                        device.instance_id()
                    ),
                })?
                .range();
            let interrupt = device
                .interrupts()
                .iter()
                .find(|resource| resource.slot().as_str() == "irq")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "LoongArch 16550 instance '{}' has no 'irq' resource",
                        device.instance_id()
                    ),
                })?
                .id();
            Ok(PlannedLoongArchNs16550 { mmio, interrupt })
        })
        .transpose()
}
