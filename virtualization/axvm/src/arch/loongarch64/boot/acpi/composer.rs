//! LoongArch ACPI composition and fw_cfg relocation plan.

use std::vec::Vec;

use axdevice::{FwCfgAcpiBlobs, FwCfgRamRegion};

use super::{
    super::GuestPlatform,
    config::{
        LoongArchFwCfgInterruptConfig, LoongArchFwCfgPciConfig, LoongArchFwCfgSerialConfig,
        interrupt_config, pci_config, serial_config,
    },
    tables::*,
};
use crate::boot::acpi::*;

const ACPI_TABLE_FILE: &str = "etc/acpi/tables";
const ACPI_RSDP_FILE: &str = "etc/acpi/rsdp";

pub(in crate::arch::loongarch64::boot) fn build(
    cpu_num: u16,
    platform: &GuestPlatform,
    srat_regions: &[FwCfgRamRegion],
) -> Result<FwCfgAcpiBlobs, AcpiBuildError> {
    let serial = serial_config(platform);
    let pci = pci_config(platform);
    let interrupt = interrupt_config(platform);
    build_acpi(cpu_num, srat_regions, &serial, &pci, &interrupt)
}

fn build_acpi(
    cpu_num: u16,
    srat_regions: &[FwCfgRamRegion],
    serial: &LoongArchFwCfgSerialConfig,
    pci: &LoongArchFwCfgPciConfig,
    interrupt: &LoongArchFwCfgInterruptConfig,
) -> Result<FwCfgAcpiBlobs, AcpiBuildError> {
    let mut tables = Vec::new();
    let mut loader = AcpiLoaderPlan::new();

    loader.allocate(ACPI_TABLE_FILE, 64, LoaderZone::High)?;

    let facs = tables.len() as u32;
    build_facs(&mut tables);

    let dsdt = tables.len() as u32;
    build_dsdt(&mut tables, serial, pci);
    add_table_checksum(&mut loader, dsdt as usize, table_len(&tables, dsdt))?;

    let fadt = tables.len() as u32;
    build_fadt(&mut tables);
    write_le_at(&mut tables, facs as u64, fadt as usize + 36, 4);
    loader.add_pointer(ACPI_TABLE_FILE, ACPI_TABLE_FILE, fadt + 36, 4)?;
    write_le_at(&mut tables, dsdt as u64, fadt as usize + 40, 4);
    loader.add_pointer(ACPI_TABLE_FILE, ACPI_TABLE_FILE, fadt + 40, 4)?;
    write_le_at(&mut tables, dsdt as u64, fadt as usize + 140, 8);
    loader.add_pointer(ACPI_TABLE_FILE, ACPI_TABLE_FILE, fadt + 140, 8)?;
    add_table_checksum(&mut loader, fadt as usize, table_len(&tables, fadt))?;

    let madt = tables.len() as u32;
    build_madt(&mut tables, cpu_num, interrupt);
    add_table_checksum(&mut loader, madt as usize, table_len(&tables, madt))?;

    let srat = tables.len() as u32;
    build_srat(&mut tables, cpu_num, srat_regions);
    add_table_checksum(&mut loader, srat as usize, table_len(&tables, srat))?;

    let spcr = tables.len() as u32;
    build_spcr(&mut tables, serial);
    add_table_checksum(&mut loader, spcr as usize, table_len(&tables, spcr))?;

    let mcfg = tables.len() as u32;
    build_mcfg(&mut tables, pci);
    add_table_checksum(&mut loader, mcfg as usize, table_len(&tables, mcfg))?;

    let table_offsets = [fadt, madt, srat, spcr, mcfg];
    let rsdt = tables.len() as u32;
    build_rsdt(&mut tables, &table_offsets);
    for (idx, table_offset) in table_offsets.iter().enumerate() {
        write_le_at(
            &mut tables,
            *table_offset as u64,
            rsdt as usize + 36 + (idx * 4),
            4,
        );
        loader.add_pointer(
            ACPI_TABLE_FILE,
            ACPI_TABLE_FILE,
            rsdt + 36 + (idx as u32 * 4),
            4,
        )?;
    }
    add_table_checksum(&mut loader, rsdt as usize, table_len(&tables, rsdt))?;

    let mut rsdp = Vec::new();
    loader.allocate(ACPI_RSDP_FILE, 16, LoaderZone::Fseg)?;
    build_rsdp(&mut rsdp);
    write_le_at(&mut rsdp, rsdt as u64, 16, 4);
    loader.add_pointer(ACPI_RSDP_FILE, ACPI_TABLE_FILE, 16, 4)?;
    loader.add_checksum(ACPI_RSDP_FILE, 8, 0, 20)?;

    let mut arena = AcpiTableArena::new(0, u64::from(u32::MAX) + 1)?;
    let allocation = arena.reserve("LoongArch ACPI tables", tables.len(), 1)?;
    if allocation.gpa() != 0 || allocation.offset() != 0 {
        return Err(AcpiBuildError::InvalidValue {
            field: "LoongArch ACPI table base",
            value: std::format!("{:#x}", allocation.gpa()),
        });
    }
    arena.write(&allocation, &tables)?;

    Ok(FwCfgAcpiBlobs {
        tables: arena.into_bytes(),
        rsdp,
        loader: loader.serialize(),
    })
}

fn add_table_checksum(
    loader: &mut AcpiLoaderPlan,
    start: usize,
    length: usize,
) -> Result<(), AcpiBuildError> {
    let checksum = start
        .checked_add(9)
        .ok_or_else(|| AcpiBuildError::AddressOverflow {
            object: "LoongArch ACPI checksum byte".into(),
        })?;
    loader.add_checksum(
        ACPI_TABLE_FILE,
        u32_value(checksum, "ACPI checksum offset")?,
        u32_value(start, "ACPI table offset")?,
        u32_value(length, "ACPI table length")?,
    )
}

fn u32_value(value: usize, field: &'static str) -> Result<u32, AcpiBuildError> {
    u32::try_from(value).map_err(|_| AcpiBuildError::InvalidValue {
        field,
        value: std::format!("{value:#x}"),
    })
}

fn table_len(tables: &[u8], offset: u32) -> usize {
    u32::from_le_bytes(
        tables[offset as usize + 4..offset as usize + 8]
            .try_into()
            .unwrap(),
    ) as usize
}
