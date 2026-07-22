//! x86 RSDP/XSDT/FADT/MADT/DSDT/SPCR generation.

use alloc::{format, vec, vec::Vec};

use acpi_tables::{
    Aml,
    aml::{Device, EISAName, IO, Interrupt, Name, ResourceTemplate},
    facs::FACS,
    fadt::{FADT, FADTBuilder, Flags, PmProfile},
    madt::{EnabledStatus, IoApic, LocalInterruptController, MADT, ProcessorLocalApic},
    rsdp::Rsdp,
    sdt::Sdt,
    xsdt::XSDT,
};
use axvm_types::InterruptTriggerMode;

use super::{
    ACPI_HEADER_LENGTH, GeneratedAcpiImage, OEM_ID, OEM_REVISION, OEM_TABLE_ID, TABLE_ALIGNMENT,
    aml_bytes, location,
};
use crate::machine::{
    InterruptControllerPlan, IoPortRange, MachinePlanError, MachinePlanResult, VmMachinePlan,
    X86ApicPlan,
};

/// Placement and CPU metadata for an x86 ACPI image.
#[derive(Clone, Copy, Debug)]
pub struct X86AcpiConfig {
    cpu_count: u8,
    load_address: u64,
}

impl X86AcpiConfig {
    /// Creates a checked ACPI placement configuration.
    pub fn new(cpu_count: usize, load_address: u64) -> MachinePlanResult<Self> {
        let cpu_count = u8::try_from(cpu_count).map_err(|_| MachinePlanError::InvalidFirmware {
            detail: format!("x86 ACPI supports at most {} vCPUs", u8::MAX),
        })?;
        if cpu_count == 0 {
            return Err(MachinePlanError::InvalidFirmware {
                detail: "x86 ACPI requires at least one vCPU".into(),
            });
        }
        if !load_address.is_multiple_of(16) {
            return Err(MachinePlanError::InvalidFirmware {
                detail: format!("ACPI load address {load_address:#x} is not 16-byte aligned"),
            });
        }
        Ok(Self {
            cpu_count,
            load_address,
        })
    }
}

/// Generates x86 RSDP/XSDT/FADT/MADT/DSDT and optional COM1 SPCR tables.
pub fn generate_x86_acpi(
    plan: &VmMachinePlan,
    config: &X86AcpiConfig,
) -> MachinePlanResult<GeneratedAcpiImage> {
    let apic = planned_x86_apic(plan)?;
    let serial = planned_x86_com1(plan)?;
    let lapic_base = checked_u32(apic.lapic().base(), "local APIC base")?;
    let ioapic_base = checked_u32(apic.ioapic().base(), "IOAPIC base")?;

    let dsdt = build_x86_dsdt(plan, serial)?;
    let madt = build_x86_madt(config.cpu_count, lapic_base, ioapic_base);
    let spcr = serial.map(build_x86_spcr).transpose()?;
    let facs = aml_bytes(&FACS::new());

    let xsdt_entry_count = 2 + usize::from(spcr.is_some());
    let xsdt_length = ACPI_HEADER_LENGTH + xsdt_entry_count * size_of::<u64>();
    let mut cursor = align_up(Rsdp::len(), TABLE_ALIGNMENT)?;
    let xsdt_offset = reserve(&mut cursor, xsdt_length)?;
    let fadt_offset = reserve_aligned(&mut cursor, FADT::len())?;
    let madt_offset = reserve_aligned(&mut cursor, madt.len())?;
    let spcr_offset = spcr
        .as_ref()
        .map(|bytes| reserve_aligned(&mut cursor, bytes.len()))
        .transpose()?;
    let dsdt_offset = reserve_aligned(&mut cursor, dsdt.len())?;
    let facs_offset = reserve_aligned(&mut cursor, facs.len())?;

    let xsdt_address = checked_address(config.load_address, xsdt_offset)?;
    let fadt_address = checked_address(config.load_address, fadt_offset)?;
    let madt_address = checked_address(config.load_address, madt_offset)?;
    let dsdt_address = checked_address(config.load_address, dsdt_offset)?;
    let facs_address = checked_address(config.load_address, facs_offset)?;

    let fadt = aml_bytes(
        &FADTBuilder::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION)
            .dsdt_64(dsdt_address)
            .firmware_ctrl_64(facs_address)
            .flag(Flags::Wbinvd)
            .flag(Flags::Headless)
            .preferred_pm_profile(PmProfile::EnterpriseServer)
            .finalize(),
    );
    let mut xsdt = XSDT::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    xsdt.add_entry(fadt_address);
    xsdt.add_entry(madt_address);
    if let Some(offset) = spcr_offset {
        xsdt.add_entry(checked_address(config.load_address, offset)?);
    }
    let xsdt = aml_bytes(&xsdt);
    let rsdp = aml_bytes(&Rsdp::new(OEM_ID, xsdt_address));

    let mut bytes = vec![0; cursor];
    copy_table(&mut bytes, 0, &rsdp)?;
    copy_table(&mut bytes, xsdt_offset, &xsdt)?;
    copy_table(&mut bytes, fadt_offset, &fadt)?;
    copy_table(&mut bytes, madt_offset, &madt)?;
    if let (Some(offset), Some(spcr)) = (spcr_offset, spcr.as_deref()) {
        copy_table(&mut bytes, offset, spcr)?;
    }
    copy_table(&mut bytes, dsdt_offset, &dsdt)?;
    copy_table(&mut bytes, facs_offset, &facs)?;

    let mut tables = vec![
        location(*b"XSDT", xsdt_offset, &xsdt),
        location(*b"FACP", fadt_offset, &fadt),
        location(*b"APIC", madt_offset, &madt),
        location(*b"DSDT", dsdt_offset, &dsdt),
        location(*b"FACS", facs_offset, &facs),
    ];
    if let (Some(offset), Some(spcr)) = (spcr_offset, spcr.as_deref()) {
        tables.push(location(*b"SPCR", offset, spcr));
    }
    Ok(GeneratedAcpiImage::new(config.load_address, bytes, tables))
}

fn planned_x86_apic(plan: &VmMachinePlan) -> MachinePlanResult<&X86ApicPlan> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::X86Apic(apic)) => Ok(apic),
        Some(_) => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate x86 ACPI from another architecture's controller plan".into(),
        }),
        None => Err(MachinePlanError::InvalidFirmware {
            detail: "cannot generate x86 ACPI without an APIC controller plan".into(),
        }),
    }
}

#[derive(Clone, Copy, Debug)]
struct PlannedX86Com1 {
    ports: IoPortRange,
    gsi: u32,
    trigger: InterruptTriggerMode,
}

fn planned_x86_com1(plan: &VmMachinePlan) -> MachinePlanResult<Option<PlannedX86Com1>> {
    plan.virtual_devices()
        .iter()
        .find(|device| device.model_id().as_str() == "x86-com1")
        .map(|device| {
            let ports = device
                .pio()
                .iter()
                .find(|resource| resource.slot().as_str() == "registers")
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!(
                        "x86 COM1 instance '{}' has no 'registers' resource",
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
                        "x86 COM1 instance '{}' has no 'irq' resource",
                        device.instance_id()
                    ),
                })?;
            Ok(PlannedX86Com1 {
                ports,
                gsi: interrupt.id(),
                trigger: interrupt.trigger(),
            })
        })
        .transpose()
}

fn build_x86_dsdt(
    plan: &VmMachinePlan,
    serial: Option<PlannedX86Com1>,
) -> MachinePlanResult<Vec<u8>> {
    let mut aml = Vec::new();
    if let Some(serial) = serial {
        let length =
            u8::try_from(serial.ports.size()).map_err(|_| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "COM1 port range {} is too large for AML",
                    serial.ports.size()
                ),
            })?;
        let io = IO::new(serial.ports.base(), serial.ports.base(), 0, length);
        let interrupt = Interrupt::new(
            true,
            serial.trigger == InterruptTriggerMode::EdgeTriggered,
            false,
            false,
            serial.gsi,
        );
        let resources = ResourceTemplate::new(vec![&interrupt, &io]);
        let hid = EISAName::new("PNP0501");
        let hid_name = Name::new("_HID".into(), &hid);
        let uid = 0u8;
        let uid_name = Name::new("_UID".into(), &uid);
        let crs_name = Name::new("_CRS".into(), &resources);
        Device::new("_SB_.COM1".into(), vec![&hid_name, &uid_name, &crs_name])
            .to_aml_bytes(&mut aml);
    }
    super::devices::append_passthrough_devices_aml(&mut aml, plan)?;
    let mut dsdt = Sdt::new(
        *b"DSDT",
        ACPI_HEADER_LENGTH as u32,
        2,
        OEM_ID,
        OEM_TABLE_ID,
        1,
    );
    dsdt.append_slice(&aml);
    Ok(aml_bytes(&dsdt))
}

fn build_x86_madt(cpu_count: u8, lapic_base: u32, ioapic_base: u32) -> Vec<u8> {
    let mut madt = MADT::new(
        OEM_ID,
        OEM_TABLE_ID,
        OEM_REVISION,
        LocalInterruptController::Address(lapic_base),
    );
    for cpu in 0..cpu_count {
        madt.add_structure(ProcessorLocalApic::new(cpu, cpu, EnabledStatus::Enabled));
    }
    madt.add_structure(IoApic::new(0, ioapic_base, 0));
    aml_bytes(&madt)
}

fn build_x86_spcr(serial: PlannedX86Com1) -> MachinePlanResult<Vec<u8>> {
    let irq = u8::try_from(serial.gsi).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "COM1 GSI {} does not fit the SPCR legacy IRQ field",
            serial.gsi
        ),
    })?;
    let mut spcr = Sdt::new(*b"SPCR", 94, 2, OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    spcr.write_u8(36, 0);
    spcr.write_u8(40, 1);
    spcr.write_u8(41, 8);
    spcr.write_u8(43, 1);
    spcr.write_u64(44, u64::from(serial.ports.base()));
    spcr.write_u8(52, 3);
    spcr.write_u8(53, irq);
    spcr.write_u32(54, serial.gsi);
    spcr.write_u8(58, 7);
    spcr.write_u8(60, 1);
    spcr.write_u8(62, 3);
    spcr.write_u16(64, u16::MAX);
    spcr.write_u16(66, u16::MAX);
    Ok(aml_bytes(&spcr))
}

fn reserve(cursor: &mut usize, length: usize) -> MachinePlanResult<usize> {
    let offset = *cursor;
    *cursor = cursor
        .checked_add(length)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "ACPI image size overflows usize".into(),
        })?;
    Ok(offset)
}

fn reserve_aligned(cursor: &mut usize, length: usize) -> MachinePlanResult<usize> {
    *cursor = align_up(*cursor, TABLE_ALIGNMENT)?;
    reserve(cursor, length)
}

fn align_up(value: usize, alignment: usize) -> MachinePlanResult<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "ACPI table alignment overflows usize".into(),
        })
}

fn checked_address(base: u64, offset: usize) -> MachinePlanResult<u64> {
    base.checked_add(offset as u64)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "ACPI guest address overflows u64".into(),
        })
}

fn checked_u32(value: u64, label: &'static str) -> MachinePlanResult<u32> {
    u32::try_from(value).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!("{label} {value:#x} exceeds the MADT address width"),
    })
}

fn copy_table(image: &mut [u8], offset: usize, table: &[u8]) -> MachinePlanResult<()> {
    let target = image.get_mut(offset..offset + table.len()).ok_or_else(|| {
        MachinePlanError::InvalidFirmware {
            detail: "ACPI table lies outside the generated image".into(),
        }
    })?;
    target.copy_from_slice(table);
    Ok(())
}
