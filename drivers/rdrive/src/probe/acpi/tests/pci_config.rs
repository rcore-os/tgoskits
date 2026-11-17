use alloc::{vec, vec::Vec};
use core::mem::{size_of, size_of_val};

use acpi::{AcpiTables, Handler, PciAddress};

use super::super::{AcpiHandler, AcpiRoot, read_pci_ecam_regions};

fn bytes(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: The exclusive borrow covers initialized u64 storage. Every byte
    // pattern remains a valid u64, and the slice stays within that allocation.
    unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), size_of_val(words)) }
}

fn checksum(bytes: &mut [u8], at: usize) {
    bytes[at] = 0;
    bytes[at] = bytes
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
        .wrapping_neg();
}

fn sdt(signature: &[u8; 4], oem: &[u8; 6], table: &[u8; 8], body: &[u8]) -> Vec<u64> {
    let length = 36 + body.len();
    let mut words = vec![0; length.div_ceil(8)];
    let contents = &mut bytes(&mut words)[..length];
    contents[..4].copy_from_slice(signature);
    contents[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    contents[8] = 1;
    contents[10..16].copy_from_slice(oem);
    contents[16..24].copy_from_slice(table);
    contents[24..28].copy_from_slice(&1u32.to_le_bytes());
    contents[28..32].copy_from_slice(b"TEST");
    contents[36..].copy_from_slice(body);
    checksum(contents, 9);
    words
}

#[test]
fn firmware_selected_pci_access_reads_and_updates_the_real_function() {
    for (oem, table, function_offset) in [
        (*b"BOCHS ", *b"BXPC    ", 0x1_8000),
        (*b"LOONGS", *b"LOONGSON", 0x1800),
    ] {
        // Both the conventional ECAM address and the native CAM address are
        // backed, so an incorrect layout yields an absent function, never UB.
        let mut config = vec![u32::MAX; 0x4_0000 / size_of::<u32>()];
        config[function_offset / 4] = 0x4321_1234;
        config[(function_offset + 4) / 4] = 0xa5a5_0000;
        let mut mcfg_body = vec![0; 24];
        mcfg_body[8..16].copy_from_slice(&(config.as_mut_ptr() as u64).to_le_bytes());
        let mcfg = sdt(b"MCFG", &oem, &table, &mcfg_body);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"PCI_TEST",
            &(mcfg.as_ptr() as u64).to_le_bytes(),
        );
        let mut rsdp = vec![0u64; 5];
        let rsdp_bytes = &mut bytes(&mut rsdp)[..36];
        rsdp_bytes[..8].copy_from_slice(b"RSD PTR ");
        rsdp_bytes[9..15].copy_from_slice(b"TESTOS");
        rsdp_bytes[15] = 2;
        rsdp_bytes[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp_bytes[24..32].copy_from_slice(&(xsdt.as_ptr() as u64).to_le_bytes());
        checksum(&mut rsdp_bytes[..20], 8);
        checksum(rsdp_bytes, 32);

        let root = AcpiRoot::identity(rsdp.as_ptr() as usize);
        let handler = AcpiHandler::new(root, Vec::new());
        // SAFETY: The aligned, checksummed tables and configuration storage
        // remain allocated until every mapping and handler below has dropped.
        let tables = unsafe { AcpiTables::from_rsdp(handler, root.rsdp) }.unwrap();
        let regions = read_pci_ecam_regions(&tables).unwrap();
        let handler = AcpiHandler::new(root, regions);
        let address = PciAddress::new(0, 0, 3, 0);

        assert_eq!(handler.read_pci_u32(address, 0), 0x4321_1234, "OEM {oem:?}");
        handler.write_pci_u16(address, 4, 6);
        assert_eq!(config[(function_offset + 4) / 4], 0xa5a5_0006);
    }
}
