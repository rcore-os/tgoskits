//! LoongArch-specific ACPI table encoders.

use std::vec::Vec;

use axdevice::FwCfgRamRegion;

use super::{aml::*, config::*};

const ACPI_OEM_ID: &[u8; 6] = b"BOCHS ";
const ACPI_OEM_TABLE_ID: &[u8; 8] = b"BXPC    ";

pub(super) fn build_facs(tables: &mut Vec<u8>) {
    tables.extend_from_slice(b"FACS");
    push_le(tables, 64, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    tables.resize(tables.len() + 40, 0);
}

pub(super) fn build_dsdt(
    tables: &mut Vec<u8>,
    serial: &LoongArchFwCfgSerialConfig,
    pci: &LoongArchFwCfgPciConfig,
) {
    let start = begin_acpi_table(tables, b"DSDT", 1);
    tables.extend_from_slice(&build_loongarch_dsdt_aml(serial, pci));
    end_acpi_table(tables, start);
}

pub(super) fn build_fadt(tables: &mut Vec<u8>) {
    let start = begin_acpi_table(tables, b"FACP", 5);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 2);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    for _ in 0..8 {
        push_le(tables, 0, 4);
    }
    for _ in 0..8 {
        push_le(tables, 0, 1);
    }
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    for _ in 0..5 {
        push_le(tables, 0, 1);
    }
    push_le(tables, 0, 2);
    push_le(tables, 0, 1);
    push_le(tables, (1u64 << 10) | (1u64 << 20), 4);
    push_gas(tables, 0, 8, 0, 1, 0x100e_001e);
    push_le(tables, 0x42, 1);
    push_le(tables, 0, 3);
    push_le(tables, 0, 8);
    push_le(tables, 0, 8);
    for _ in 0..8 {
        push_gas(tables, 0, 0, 0, 0, 0);
    }
    push_gas(tables, 0, 8, 0, 1, 0x100e_001c);
    push_gas(tables, 0, 8, 0, 1, 0x100e_001d);
    end_acpi_table(tables, start);
}

pub(super) fn build_madt(
    tables: &mut Vec<u8>,
    cpu_num: u16,
    interrupt: &LoongArchFwCfgInterruptConfig,
) {
    let start = begin_acpi_table(tables, b"APIC", 1);
    push_le(tables, 0, 4);
    push_le(tables, 1, 4);

    for cpu_id in 0..cpu_num {
        push_le(tables, 17, 1);
        push_le(tables, 15, 1);
        push_le(tables, 1, 1);
        push_le(tables, cpu_id as u64, 4);
        push_le(tables, cpu_id as u64, 4);
        push_le(tables, 1, 4);
    }

    push_le(tables, 20, 1);
    push_le(tables, 13, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.eiointc_irq as u64, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0xffff, 8);

    push_le(tables, 21, 1);
    push_le(tables, 19, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.pch_msi_base, 8);
    push_le(tables, interrupt.pch_msi_start as u64, 4);
    push_le(tables, interrupt.pch_msi_count as u64, 4);

    push_le(tables, 22, 1);
    push_le(tables, 17, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.pch_pic_base, 8);
    push_le(tables, interrupt.pch_pic_size as u64, 2);
    push_le(tables, 0, 2);
    push_le(tables, interrupt.pch_pic_gsi_base as u64, 2);

    end_acpi_table(tables, start);
}

pub(super) fn build_srat(tables: &mut Vec<u8>, cpu_num: u16, ram_regions: &[FwCfgRamRegion]) {
    let start = begin_acpi_table(tables, b"SRAT", 1);
    push_le(tables, 1, 4);
    push_le(tables, 0, 8);

    for cpu_id in 0..cpu_num {
        push_le(tables, 0, 1);
        push_le(tables, 16, 1);
        push_le(tables, 0, 1);
        push_le(tables, cpu_id as u64, 1);
        push_le(tables, 1, 4);
        push_le(tables, 0, 1);
        push_le(tables, 0, 3);
        push_le(tables, 0, 4);
    }

    for region in ram_regions {
        if region.size != 0 {
            push_srat_memory(tables, region.base, region.size);
        }
    }

    end_acpi_table(tables, start);
}

fn push_srat_memory(tables: &mut Vec<u8>, base: u64, length: u64) {
    push_le(tables, 1, 1);
    push_le(tables, 40, 1);
    push_le(tables, 0, 4);
    push_le(tables, 0, 2);
    push_le(tables, base, 4);
    push_le(tables, base >> 32, 4);
    push_le(tables, length, 4);
    push_le(tables, length >> 32, 4);
    push_le(tables, 0, 4);
    push_le(tables, 1, 4);
    push_le(tables, 0, 8);
}

pub(super) fn build_spcr(tables: &mut Vec<u8>, serial: &LoongArchFwCfgSerialConfig) {
    let start = begin_acpi_table(tables, b"SPCR", 2);
    push_le(tables, 0, 1);
    push_le(tables, 0, 3);
    push_gas(tables, 0, 8, 0, 1, serial.base);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, serial.irq as u64, 4);
    push_le(tables, 7, 1);
    push_le(tables, 0, 1);
    push_le(tables, 1, 1);
    push_le(tables, 0, 1);
    push_le(tables, 3, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0xffff, 2);
    push_le(tables, 0xffff, 2);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 4);
    push_le(tables, serial.clock_hz as u64, 4);
    push_le(tables, serial.baud as u64, 4);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    end_acpi_table(tables, start);
}

pub(super) fn build_mcfg(tables: &mut Vec<u8>, pci: &LoongArchFwCfgPciConfig) {
    let start = begin_acpi_table(tables, b"MCFG", 1);
    push_le(tables, 0, 8);
    push_le(tables, pci.ecam_base, 8);
    push_le(tables, 0, 2);
    push_le(tables, 0, 1);
    push_le(tables, (pci.ecam_size - 1) >> 20, 1);
    push_le(tables, 0, 4);
    end_acpi_table(tables, start);
}

pub(super) fn build_rsdt(tables: &mut Vec<u8>, table_offsets: &[u32]) {
    let start = begin_acpi_table(tables, b"RSDT", 1);
    for _ in table_offsets {
        push_le(tables, 0, 4);
    }
    end_acpi_table(tables, start);
}

pub(super) fn build_rsdp(rsdp: &mut Vec<u8>) {
    rsdp.extend_from_slice(b"RSD PTR ");
    push_le(rsdp, 0, 1);
    rsdp.extend_from_slice(ACPI_OEM_ID);
    push_le(rsdp, 0, 1);
    push_le(rsdp, 0, 4);
}

fn begin_acpi_table(tables: &mut Vec<u8>, signature: &[u8; 4], revision: u8) -> usize {
    let start = tables.len();
    tables.extend_from_slice(signature);
    push_le(tables, 0, 4);
    push_le(tables, revision as u64, 1);
    push_le(tables, 0, 1);
    tables.extend_from_slice(ACPI_OEM_ID);
    tables.extend_from_slice(ACPI_OEM_TABLE_ID);
    push_le(tables, 1, 4);
    tables.extend_from_slice(b"BXPC");
    push_le(tables, 1, 4);
    start
}

fn end_acpi_table(tables: &mut [u8], start: usize) {
    let length = (tables.len() - start) as u32;
    tables[start + 4..start + 8].copy_from_slice(&length.to_le_bytes());
}

fn push_gas(out: &mut Vec<u8>, space: u8, bit_width: u8, bit_offset: u8, access: u8, addr: u64) {
    out.push(space);
    out.push(bit_width);
    out.push(bit_offset);
    out.push(access);
    push_le(out, addr, 8);
}

pub(super) fn write_le_at(out: &mut [u8], value: u64, offset: usize, size: u8) {
    let bytes = value.to_le_bytes();
    out[offset..offset + size as usize].copy_from_slice(&bytes[..size as usize]);
}

fn push_le(out: &mut Vec<u8>, value: u64, size: usize) {
    out.extend_from_slice(&value.to_le_bytes()[..size]);
}
