//! QEMU fw_cfg ACPI table-loader command encoding.

use alloc::vec::Vec;

use crate::machine::{MachinePlanError, MachinePlanResult};

const TABLE_FILE: &str = "etc/acpi/tables";
const RSDP_FILE: &str = "etc/acpi/rsdp";
const ENTRY_SIZE: usize = 128;
const ALLOCATE: u32 = 1;
const ADD_POINTER: u32 = 2;
const ADD_CHECKSUM: u32 = 3;
const ALLOC_HIGH: u8 = 1;
const ALLOC_FSEG: u8 = 2;

pub(super) fn append_table(tables: &mut Vec<u8>, bytes: &[u8]) -> MachinePlanResult<u32> {
    let offset = u32::try_from(tables.len()).map_err(|_| MachinePlanError::FirmwareEncoding {
        detail: "fw_cfg ACPI table file exceeds u32 offsets".into(),
    })?;
    tables.extend_from_slice(bytes);
    Ok(offset)
}

pub(super) fn append_sdt(
    tables: &mut Vec<u8>,
    loader: &mut Vec<u8>,
    bytes: &[u8],
) -> MachinePlanResult<u32> {
    let offset = append_table(tables, bytes)?;
    push_sdt_checksum(loader, offset, bytes.len())?;
    Ok(offset)
}

pub(super) fn push_allocate_tables(out: &mut Vec<u8>, alignment: u32) {
    push_allocate(out, TABLE_FILE, alignment, ALLOC_HIGH);
}

pub(super) fn push_table_pointer(out: &mut Vec<u8>, pointer_offset: u32, pointer_size: u8) {
    push_pointer(out, TABLE_FILE, pointer_offset, pointer_size, TABLE_FILE);
}

pub(super) fn push_sdt_checksum(
    out: &mut Vec<u8>,
    table_offset: u32,
    table_length: usize,
) -> MachinePlanResult<()> {
    let checksum_offset =
        table_offset
            .checked_add(9)
            .ok_or_else(|| MachinePlanError::FirmwareEncoding {
                detail: "ACPI checksum offset overflows u32".into(),
            })?;
    push_checksum(out, TABLE_FILE, checksum_offset, table_offset, table_length)
}

pub(super) fn push_rsdp_loader_entries(
    out: &mut Vec<u8>,
    rsdp_length: usize,
) -> MachinePlanResult<()> {
    push_allocate(out, RSDP_FILE, 16, ALLOC_FSEG);
    push_pointer(out, RSDP_FILE, 24, 8, TABLE_FILE);
    push_checksum(out, RSDP_FILE, 8, 0, 20)?;
    push_checksum(out, RSDP_FILE, 32, 0, rsdp_length)
}

fn push_allocate(out: &mut Vec<u8>, file: &str, alignment: u32, zone: u8) {
    let mut entry = [0; ENTRY_SIZE];
    entry[..4].copy_from_slice(&ALLOCATE.to_le_bytes());
    write_file(&mut entry[4..60], file);
    entry[60..64].copy_from_slice(&alignment.to_le_bytes());
    entry[64] = zone;
    out.extend_from_slice(&entry);
}

fn push_pointer(
    out: &mut Vec<u8>,
    pointer_file: &str,
    pointer_offset: u32,
    pointer_size: u8,
    pointee_file: &str,
) {
    let mut entry = [0; ENTRY_SIZE];
    entry[..4].copy_from_slice(&ADD_POINTER.to_le_bytes());
    write_file(&mut entry[4..60], pointer_file);
    write_file(&mut entry[60..116], pointee_file);
    entry[116..120].copy_from_slice(&pointer_offset.to_le_bytes());
    entry[120] = pointer_size;
    out.extend_from_slice(&entry);
}

fn push_checksum(
    out: &mut Vec<u8>,
    file: &str,
    checksum_offset: u32,
    range_offset: u32,
    range_length: usize,
) -> MachinePlanResult<()> {
    let range_length =
        u32::try_from(range_length).map_err(|_| MachinePlanError::FirmwareEncoding {
            detail: "fw_cfg checksum range exceeds u32".into(),
        })?;
    let mut entry = [0; ENTRY_SIZE];
    entry[..4].copy_from_slice(&ADD_CHECKSUM.to_le_bytes());
    write_file(&mut entry[4..60], file);
    entry[60..64].copy_from_slice(&checksum_offset.to_le_bytes());
    entry[64..68].copy_from_slice(&range_offset.to_le_bytes());
    entry[68..72].copy_from_slice(&range_length.to_le_bytes());
    out.extend_from_slice(&entry);
    Ok(())
}

fn write_file(target: &mut [u8], file: &str) {
    let length = file.len().min(target.len().saturating_sub(1));
    target[..length].copy_from_slice(&file.as_bytes()[..length]);
}
