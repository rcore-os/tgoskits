use alloc::{vec, vec::Vec};
use core::mem::size_of;

use acpi::{AcpiTables, Handler, PciAddress};

use super::{
    super::{AcpiHandler, AcpiRoot, PciRegisterAccess, read_pci_ecam_regions},
    rsdp_for, sdt,
};

#[test]
fn firmware_selected_pci_access_reads_and_updates_the_real_function() {
    for (oem, table, function_offset, secondary_extended_offset) in [
        (*b"BOCHS ", *b"BXPC    ", 0x1_8000, 0x11_8104),
        (*b"LOONGS", *b"LOONGSON", 0x1800, 0x1101_1804),
    ] {
        // Both the conventional ECAM address and the native CAM address are
        // backed, so an incorrect layout yields an absent function, never UB.
        let mut config = vec![u32::MAX; 0x10_0000 / size_of::<u32>()];
        config[function_offset / 4] = 0x4321_1234;
        config[(function_offset + 4) / 4] = 0xa5a5_0000;
        config[0x4900 / 4] = 0x1111_2222;
        let mut mcfg_body = vec![0; 24];
        mcfg_body[8..16].copy_from_slice(&(config.as_mut_ptr() as u64).to_le_bytes());
        mcfg_body[19] = 1;
        let mcfg = sdt(b"MCFG", &oem, &table, &mcfg_body);
        let xsdt = sdt(
            b"XSDT",
            b"TESTOS",
            b"PCI_TEST",
            &(mcfg.as_ptr() as u64).to_le_bytes(),
        );
        let rsdp = rsdp_for(&xsdt);

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

        // Validate the secondary-bus extended register address without
        // dereferencing its intentionally unbacked, sparse aperture location.
        let PciRegisterAccess::Mapped(pointer) =
            handler.pci_register(PciAddress::new(0, 1, 3, 0), 0x104)
        else {
            panic!("secondary-bus register is not addressable");
        };
        assert_eq!(
            pointer.as_ptr() as usize,
            config.as_ptr() as usize + secondary_extended_offset
        );
        if oem == *b"LOONGS" {
            let hidden = PciAddress::new(0, 0, 9, 1);
            assert_eq!(handler.read_pci_u32(hidden, 0), u32::MAX);
            handler.write_pci_u32(hidden, 0, 0);
            assert_eq!(config[0x4900 / 4], 0x1111_2222);
        }
    }
}
