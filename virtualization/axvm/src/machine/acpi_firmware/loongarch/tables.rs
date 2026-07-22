//! LoongArch AML and standard ACPI table bodies.

use alloc::{format, vec, vec::Vec};

use acpi_tables::{
    Aml,
    aml::{
        AddressSpace, AddressSpaceCacheable, Device, EISAName, Interrupt, Memory32Fixed, Name,
        ResourceTemplate,
    },
    fadt::{FADTBuilder, Flags, PmProfile},
    gas::{AccessSize, AddressSpace as GasAddressSpace, GAS},
    mcfg::MCFG,
    sdt::Sdt,
    srat::{MemoryAffinity, SRAT},
};

use super::{
    super::{ACPI_HEADER_LENGTH, OEM_ID, OEM_REVISION, OEM_TABLE_ID, aml_bytes},
    AcpiMemoryRegion, LoongArchAcpiInterrupts, LoongArchAcpiPci, LoongArchAcpiResources,
    LoongArchAcpiSerial,
};
use crate::machine::{LoongArchPowerPlan, MachinePlanError, MachinePlanResult};

pub(super) fn build_dsdt(
    plan: &crate::machine::VmMachinePlan,
    platform: &LoongArchAcpiResources,
) -> MachinePlanResult<Vec<u8>> {
    let mut aml = Vec::new();
    if let Some(serial) = platform.serial {
        append_serial_aml(&mut aml, serial)?;
    }
    append_pci_aml(&mut aml, &platform.pci)?;
    super::super::devices::append_passthrough_devices_aml(&mut aml, plan)?;

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

pub(super) fn build_fadt(facs: u32, dsdt: u32, power: LoongArchPowerPlan) -> Vec<u8> {
    let mut builder = FADTBuilder::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION)
        .firmware_ctrl_64(u64::from(facs))
        .dsdt_64(u64::from(dsdt))
        .flag(Flags::HwReducedAcpi)
        .flag(Flags::ResetRegSup)
        .preferred_pm_profile(PmProfile::EnterpriseServer);
    builder.reset_reg = GAS::new(
        GasAddressSpace::SystemMemory,
        8,
        0,
        AccessSize::ByteAccess,
        power.reset_register(),
    );
    builder.reset_value = power.reset_value();
    builder.sleep_control_reg = GAS::new(
        GasAddressSpace::SystemMemory,
        8,
        0,
        AccessSize::ByteAccess,
        power.sleep_control_register(),
    );
    builder.sleep_status_reg = GAS::new(
        GasAddressSpace::SystemMemory,
        8,
        0,
        AccessSize::ByteAccess,
        power.sleep_status_register(),
    );
    aml_bytes(&builder.finalize())
}

pub(super) fn build_madt(cpu_count: u16, interrupt: &LoongArchAcpiInterrupts) -> Vec<u8> {
    let mut structures = Vec::new();
    for cpu in 0..cpu_count {
        structures.extend_from_slice(&[17, 15, 1]);
        structures.extend_from_slice(&u32::from(cpu).to_le_bytes());
        structures.extend_from_slice(&u32::from(cpu).to_le_bytes());
        structures.extend_from_slice(&1u32.to_le_bytes());
    }
    structures.extend_from_slice(&[20, 13, 1, interrupt.eiointc_irq, 0]);
    structures.extend_from_slice(&u64::MAX.to_le_bytes());
    structures.extend_from_slice(&[21, 19, 1]);
    structures.extend_from_slice(&interrupt.pch_msi_base.to_le_bytes());
    structures.extend_from_slice(&interrupt.pch_msi_start.to_le_bytes());
    structures.extend_from_slice(&interrupt.pch_msi_count.to_le_bytes());
    structures.extend_from_slice(&[22, 17, 1]);
    structures.extend_from_slice(&interrupt.pch_pic_base.to_le_bytes());
    structures.extend_from_slice(&interrupt.pch_pic_size.to_le_bytes());
    structures.extend_from_slice(&0u16.to_le_bytes());
    structures.extend_from_slice(&interrupt.pch_pic_gsi_base.to_le_bytes());

    let mut madt = Sdt::new(*b"APIC", 44, 1, OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    madt.write_u32(36, 0);
    madt.write_u32(40, 1);
    madt.append_slice(&structures);
    aml_bytes(&madt)
}

pub(super) fn build_srat(regions: &[AcpiMemoryRegion]) -> Vec<u8> {
    let mut srat = SRAT::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    for region in regions.iter().filter(|region| region.size != 0) {
        srat.add_memory_affinity(MemoryAffinity::new(0, region.base, region.size).enabled());
    }
    aml_bytes(&srat)
}

pub(super) fn build_spcr(serial: &LoongArchAcpiSerial) -> Vec<u8> {
    let mut spcr = Sdt::new(*b"SPCR", 94, 2, OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    spcr.write_u8(36, 0);
    spcr.write_u8(40, 0);
    spcr.write_u8(41, 8);
    spcr.write_u8(43, 1);
    spcr.write_u64(44, serial.base);
    spcr.write_u32(54, serial.gsi);
    spcr.write_u8(58, 7);
    spcr.write_u8(60, 1);
    spcr.write_u8(62, 3);
    spcr.write_u16(64, u16::MAX);
    spcr.write_u16(66, u16::MAX);
    spcr.write_u32(80, serial.clock_hz);
    spcr.write_u32(84, serial.baud);
    aml_bytes(&spcr)
}

pub(super) fn build_mcfg(pci: &LoongArchAcpiPci) -> MachinePlanResult<Vec<u8>> {
    let mut mcfg = MCFG::new(OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    mcfg.add_ecam(pci.ecam_base, 0, 0, checked_pci_end_bus(pci)?);
    Ok(aml_bytes(&mcfg))
}

fn append_serial_aml(aml: &mut Vec<u8>, serial: LoongArchAcpiSerial) -> MachinePlanResult<()> {
    let base = u32::try_from(serial.base).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "LoongArch serial base {:#x} exceeds the AML fixed-memory width",
            serial.base
        ),
    })?;
    let length = u32::try_from(serial.size).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "LoongArch serial size {:#x} exceeds the AML fixed-memory width",
            serial.size
        ),
    })?;
    let memory = Memory32Fixed::new(true, base, length);
    let interrupt = Interrupt::new(true, false, false, false, serial.gsi);
    let resources = ResourceTemplate::new(vec![&memory, &interrupt]);
    let hid = EISAName::new("PNP0501");
    let hid_name = Name::new("_HID".into(), &hid);
    let uid = 0u8;
    let uid_name = Name::new("_UID".into(), &uid);
    let crs_name = Name::new("_CRS".into(), &resources);
    Device::new("_SB_.COMA".into(), vec![&hid_name, &uid_name, &crs_name]).to_aml_bytes(aml);
    Ok(())
}

fn append_pci_aml(aml: &mut Vec<u8>, pci: &LoongArchAcpiPci) -> MachinePlanResult<()> {
    let end_bus = checked_pci_end_bus(pci)?;
    let hid = EISAName::new("PNP0A08");
    let cid = EISAName::new("PNP0A03");
    let hid_name = Name::new("_HID".into(), &hid);
    let cid_name = Name::new("_CID".into(), &cid);
    let segment = 0u8;
    let segment_name = Name::new("_SEG".into(), &segment);
    let first_bus = 0u8;
    let bus_name = Name::new("_BBN".into(), &first_bus);
    let buses = AddressSpace::new_bus_number(0u16, u16::from(end_bus));
    let memory = AddressSpace::new_memory(
        AddressSpaceCacheable::NotCacheable,
        true,
        pci.mmio_base,
        checked_inclusive_end(pci.mmio_base, pci.mmio_size, "PCI MMIO")?,
        None,
    );
    let io = AddressSpace::new_io(
        pci.io_base,
        checked_inclusive_end(pci.io_base, pci.io_size, "PCI I/O")?,
        None,
    );
    let resources = ResourceTemplate::new(vec![&buses, &memory, &io]);
    let crs_name = Name::new("_CRS".into(), &resources);
    Device::new(
        "_SB_.PCI0".into(),
        vec![&hid_name, &cid_name, &segment_name, &bus_name, &crs_name],
    )
    .to_aml_bytes(aml);
    Ok(())
}

fn checked_inclusive_end(base: u64, size: u64, resource: &str) -> MachinePlanResult<u64> {
    base.checked_add(size.saturating_sub(1))
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("LoongArch {resource} aperture overflows"),
        })
}

fn checked_pci_end_bus(pci: &LoongArchAcpiPci) -> MachinePlanResult<u8> {
    if pci.ecam_size == 0 || !pci.ecam_size.is_multiple_of(1 << 20) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!("invalid LoongArch PCI ECAM size {:#x}", pci.ecam_size),
        });
    }
    u8::try_from((pci.ecam_size >> 20) - 1).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "LoongArch PCI ECAM size {:#x} exceeds 256 buses",
            pci.ecam_size
        ),
    })
}
