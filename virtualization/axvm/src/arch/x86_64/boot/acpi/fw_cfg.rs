//! QEMU table-loader serialization of the same logical x86 ACPI tables.

use acpi_tables::{facs::FACS, rsdp::Rsdp, xsdt::XSDT};
use axdevice::FwCfgAcpiBlobs;

use super::{
    aml::{build_dsdt, build_spcr, oem_id, oem_revision, oem_table_id},
    config::X86FirmwarePlan,
    tables::{build_fadt, build_madt, serialize},
};
use crate::boot::acpi::*;

const TABLE_FILE: &str = "etc/acpi/tables";
const RSDP_FILE: &str = "etc/acpi/rsdp";
const FADT_X_FIRMWARE_CTRL_OFFSET: usize = 132;
const FADT_X_DSDT_OFFSET: usize = 140;
const XSDT_HEADER_SIZE: usize = 36;

pub(crate) fn build_fw_cfg_blobs(plan: &X86FirmwarePlan) -> Result<FwCfgAcpiBlobs, AcpiBuildError> {
    let mut dsdt = build_dsdt(plan)?;
    let mut madt = build_madt(plan);
    let mut spcr = build_spcr(plan);
    let mut arena = AcpiTableArena::new(0, u64::from(u32::MAX) + 1)?;
    let xsdt_slot = arena.reserve("XSDT", XSDT_HEADER_SIZE + 3 * 8, 8)?;
    let fadt_slot = arena.reserve("FADT", acpi_tables::fadt::FADT::len(), 8)?;
    let facs_slot = arena.reserve("FACS", FACS::len(), 64)?;
    let dsdt_slot = arena.reserve("DSDT", dsdt.len(), 8)?;
    let madt_slot = arena.reserve("MADT", madt.len(), 8)?;
    let spcr_slot = arena.reserve("SPCR", spcr.len(), 8)?;

    let mut fadt = build_fadt(plan, dsdt_slot.gpa(), facs_slot.gpa());
    let mut xsdt = XSDT::new(oem_id(), oem_table_id(), oem_revision());
    xsdt.add_entry(fadt_slot.gpa());
    xsdt.add_entry(madt_slot.gpa());
    xsdt.add_entry(spcr_slot.gpa());
    let mut xsdt = serialize(&xsdt);
    let facs = serialize(&FACS::new());

    for (name, table) in [
        ("XSDT", xsdt.as_mut_slice()),
        ("FADT", fadt.as_mut_slice()),
        ("DSDT", dsdt.as_mut_slice()),
        ("MADT", madt.as_mut_slice()),
        ("SPCR", spcr.as_mut_slice()),
    ] {
        clear_checksum(table, 9, name)?;
    }

    arena.write(&xsdt_slot, &xsdt)?;
    arena.write(&fadt_slot, &fadt)?;
    arena.write(&facs_slot, &facs)?;
    arena.write(&dsdt_slot, &dsdt)?;
    arena.write(&madt_slot, &madt)?;
    arena.write(&spcr_slot, &spcr)?;

    let mut rsdp = serialize(&Rsdp::new(oem_id(), xsdt_slot.gpa()));
    clear_checksum(&mut rsdp, 8, "RSDP")?;
    clear_checksum(&mut rsdp, 32, "RSDP")?;
    let mut loader = AcpiLoaderPlan::new();
    loader.allocate(TABLE_FILE, 64, LoaderZone::High)?;
    loader.allocate(RSDP_FILE, 16, LoaderZone::Fseg)?;
    add_table_pointer(
        &mut loader,
        &fadt_slot,
        FADT_X_FIRMWARE_CTRL_OFFSET,
        TABLE_FILE,
        8,
    )?;
    add_table_pointer(&mut loader, &fadt_slot, FADT_X_DSDT_OFFSET, TABLE_FILE, 8)?;
    for index in 0..3 {
        add_table_pointer(
            &mut loader,
            &xsdt_slot,
            XSDT_HEADER_SIZE + index * 8,
            TABLE_FILE,
            8,
        )?;
    }
    loader.add_pointer(RSDP_FILE, TABLE_FILE, 24, 8)?;
    for (slot, length) in [
        (&xsdt_slot, xsdt.len()),
        (&fadt_slot, fadt.len()),
        (&dsdt_slot, dsdt.len()),
        (&madt_slot, madt.len()),
        (&spcr_slot, spcr.len()),
    ] {
        add_table_checksum(&mut loader, slot, length)?;
    }
    loader.add_checksum(RSDP_FILE, 8, 0, 20)?;
    loader.add_checksum(RSDP_FILE, 32, 0, u32_len(rsdp.len(), "RSDP length")?)?;

    Ok(FwCfgAcpiBlobs {
        tables: arena.into_bytes(),
        rsdp,
        loader: loader.serialize(),
    })
}

fn add_table_pointer(
    loader: &mut AcpiLoaderPlan,
    slot: &AcpiAllocation,
    field_offset: usize,
    pointee_file: &str,
    size: u8,
) -> Result<(), AcpiBuildError> {
    let offset =
        slot.offset()
            .checked_add(field_offset)
            .ok_or_else(|| AcpiBuildError::AddressOverflow {
                object: "ACPI table-loader pointer".into(),
            })?;
    loader.add_pointer(
        TABLE_FILE,
        pointee_file,
        u32_len(offset, "pointer offset")?,
        size,
    )
}

fn add_table_checksum(
    loader: &mut AcpiLoaderPlan,
    slot: &AcpiAllocation,
    length: usize,
) -> Result<(), AcpiBuildError> {
    let start = u32_len(slot.offset(), "table offset")?;
    let checksum = slot
        .offset()
        .checked_add(9)
        .ok_or_else(|| AcpiBuildError::AddressOverflow {
            object: "ACPI checksum byte".into(),
        })?;
    loader.add_checksum(
        TABLE_FILE,
        u32_len(checksum, "checksum offset")?,
        start,
        u32_len(length, "table length")?,
    )
}

fn u32_len(value: usize, field: &'static str) -> Result<u32, AcpiBuildError> {
    u32::try_from(value).map_err(|_| AcpiBuildError::InvalidValue {
        field,
        value: std::format!("{value:#x}"),
    })
}

fn clear_checksum(
    bytes: &mut [u8],
    offset: usize,
    object: &'static str,
) -> Result<(), AcpiBuildError> {
    let expected = offset + 1;
    let actual = bytes.len();
    let checksum = bytes
        .get_mut(offset)
        .ok_or_else(|| AcpiBuildError::LengthMismatch {
            object: object.into(),
            expected,
            actual,
        })?;
    *checksum = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_checksum_targets_start_zeroed() {
        let blobs = build_fw_cfg_blobs(&super::super::config::test_plan(2)).unwrap();

        for entry in blobs.loader.chunks_exact(128) {
            if u32::from_le_bytes(entry[..4].try_into().unwrap()) != 3 {
                continue;
            }
            let file_end = entry[4..60].iter().position(|byte| *byte == 0).unwrap();
            let file = &entry[4..4 + file_end];
            let checksum_offset = u32::from_le_bytes(entry[60..64].try_into().unwrap()) as usize;
            let data = match file {
                b"etc/acpi/tables" => &blobs.tables,
                b"etc/acpi/rsdp" => &blobs.rsdp,
                _ => panic!("unexpected checksum file"),
            };
            assert_eq!(data[checksum_offset], 0, "checksum target in {file:?}");
        }
    }
}
