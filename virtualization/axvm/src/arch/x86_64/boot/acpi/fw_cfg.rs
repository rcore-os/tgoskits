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
    use std::ops::Range;

    use super::*;

    const LOADER_ENTRY_SIZE: usize = 128;

    #[derive(Debug, Eq, PartialEq)]
    enum DecodedLoaderCommand {
        Allocate {
            file: String,
            alignment: u32,
            zone: u8,
        },
        Pointer {
            pointer_file: String,
            pointee_file: String,
            pointer_offset: u32,
            pointer_size: u8,
        },
        Checksum {
            file: String,
            checksum_offset: u32,
            start: u32,
            length: u32,
        },
    }

    #[test]
    fn fw_cfg_blobs_describe_the_complete_x86_acpi_loader_plan() {
        let blobs = build_fw_cfg_blobs(&super::super::config::test_plan(2)).unwrap();
        let commands = decode_loader(&blobs.loader);

        assert_eq!(blobs.loader.len(), 15 * LOADER_ENTRY_SIZE);
        assert_eq!(commands.len(), 15);
        assert_eq!(
            commands[0],
            DecodedLoaderCommand::Allocate {
                file: TABLE_FILE.into(),
                alignment: 64,
                zone: LoaderZone::High as u8,
            }
        );
        assert_eq!(
            commands[1],
            DecodedLoaderCommand::Allocate {
                file: RSDP_FILE.into(),
                alignment: 16,
                zone: LoaderZone::Fseg as u8,
            }
        );

        let xsdt = acpi_table_range(&blobs.tables, b"XSDT");
        let fadt = acpi_table_range(&blobs.tables, b"FACP");
        let facs = acpi_table_range(&blobs.tables, b"FACS");
        let dsdt = acpi_table_range(&blobs.tables, b"DSDT");
        let madt = acpi_table_range(&blobs.tables, b"APIC");
        let spcr = acpi_table_range(&blobs.tables, b"SPCR");

        for (command, pointer_file, offset, target) in [
            (&commands[2], TABLE_FILE, fadt.start + 132, &facs),
            (&commands[3], TABLE_FILE, fadt.start + 140, &dsdt),
            (&commands[4], TABLE_FILE, xsdt.start + 36, &fadt),
            (&commands[5], TABLE_FILE, xsdt.start + 44, &madt),
            (&commands[6], TABLE_FILE, xsdt.start + 52, &spcr),
            (&commands[7], RSDP_FILE, 24, &xsdt),
        ] {
            assert_pointer_command(
                command,
                pointer_file,
                offset,
                target,
                &blobs.tables,
                &blobs.rsdp,
            );
        }

        for (command, table) in commands[8..13]
            .iter()
            .zip([&xsdt, &fadt, &dsdt, &madt, &spcr])
        {
            assert_checksum_command(command, TABLE_FILE, table, &blobs.tables);
        }
        assert_eq!(
            commands[13],
            DecodedLoaderCommand::Checksum {
                file: RSDP_FILE.into(),
                checksum_offset: 8,
                start: 0,
                length: 20,
            }
        );
        assert_eq!(blobs.rsdp[8], 0);
        assert_eq!(
            commands[14],
            DecodedLoaderCommand::Checksum {
                file: RSDP_FILE.into(),
                checksum_offset: 32,
                start: 0,
                length: blobs.rsdp.len() as u32,
            }
        );
        assert_eq!(blobs.rsdp[32], 0);
    }

    fn decode_loader(bytes: &[u8]) -> Vec<DecodedLoaderCommand> {
        assert_eq!(bytes.len() % LOADER_ENTRY_SIZE, 0);
        bytes
            .chunks_exact(LOADER_ENTRY_SIZE)
            .map(|entry| match read_u32(entry, 0) {
                1 => DecodedLoaderCommand::Allocate {
                    file: read_file(&entry[4..60]),
                    alignment: read_u32(entry, 60),
                    zone: entry[64],
                },
                2 => DecodedLoaderCommand::Pointer {
                    pointer_file: read_file(&entry[4..60]),
                    pointee_file: read_file(&entry[60..116]),
                    pointer_offset: read_u32(entry, 116),
                    pointer_size: entry[120],
                },
                3 => DecodedLoaderCommand::Checksum {
                    file: read_file(&entry[4..60]),
                    checksum_offset: read_u32(entry, 60),
                    start: read_u32(entry, 64),
                    length: read_u32(entry, 68),
                },
                command => panic!("unexpected table-loader command {command}"),
            })
            .collect()
    }

    fn read_file(field: &[u8]) -> String {
        let length = field
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(field.len());
        String::from_utf8(field[..length].to_vec()).unwrap()
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    fn acpi_table_range(tables: &[u8], signature: &[u8; 4]) -> Range<usize> {
        let start = tables
            .windows(signature.len())
            .position(|window| window == signature)
            .unwrap_or_else(|| panic!("missing ACPI table signature {signature:?}"));
        let length = read_u32(tables, start + 4) as usize;
        let end = start.checked_add(length).unwrap();
        assert!(end <= tables.len(), "ACPI table extends past blob end");
        start..end
    }

    fn assert_pointer_command(
        command: &DecodedLoaderCommand,
        pointer_file: &str,
        expected_offset: usize,
        target: &Range<usize>,
        tables: &[u8],
        rsdp: &[u8],
    ) {
        let DecodedLoaderCommand::Pointer {
            pointer_file: actual_pointer_file,
            pointee_file,
            pointer_offset,
            pointer_size,
        } = command
        else {
            panic!("expected pointer command, got {command:?}");
        };
        assert_eq!(actual_pointer_file, pointer_file);
        assert_eq!(pointee_file, TABLE_FILE);
        assert_eq!(*pointer_offset as usize, expected_offset);
        assert_eq!(*pointer_size, 8);

        let source = if pointer_file == TABLE_FILE {
            tables
        } else {
            assert_eq!(pointer_file, RSDP_FILE);
            rsdp
        };
        let pointer_end = expected_offset.checked_add(8).unwrap();
        assert!(pointer_end <= source.len());
        assert_eq!(read_u64(source, expected_offset) as usize, target.start);
        assert!(target.end <= tables.len());
    }

    fn assert_checksum_command(
        command: &DecodedLoaderCommand,
        expected_file: &str,
        table: &Range<usize>,
        data: &[u8],
    ) {
        let DecodedLoaderCommand::Checksum {
            file,
            checksum_offset,
            start,
            length,
        } = command
        else {
            panic!("expected checksum command, got {command:?}");
        };
        assert_eq!(file, expected_file);
        assert_eq!(*checksum_offset as usize, table.start + 9);
        assert_eq!(*start as usize, table.start);
        assert_eq!(*length as usize, table.len());
        assert!(table.end <= data.len());
        assert_eq!(data[*checksum_offset as usize], 0);
    }
}
