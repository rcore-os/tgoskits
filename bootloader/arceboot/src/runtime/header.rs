//! Shared construction of standard UEFI table headers.
//!
//! Every UEFI table (system, boot services, runtime services) starts with a
//! [`Header`] carrying the table signature, spec revision, table size and a
//! CRC-32 over the entire table. Payloads and standard EFI libraries validate
//! these fields before using a table, so they must be filled in for real.

use uefi_raw::table::{Header, Revision};

/// Spec revision claimed by the tables ArceBoot publishes.
pub(crate) const TABLE_REVISION: Revision = Revision::EFI_2_70;

/// `'BOOTSERV'` — `EFI_BOOT_SERVICES_SIGNATURE`.
pub(crate) const BOOT_SERVICES_SIGNATURE: u64 = 0x5652_4553_544F_4F42;
/// `'RUNTSERV'` — `EFI_RUNTIME_SERVICES_SIGNATURE`.
pub(crate) const RUNTIME_SERVICES_SIGNATURE: u64 = 0x5652_4553_544E_5552;

/// CRC-32 (reflected, polynomial `0xEDB8_8320`, init/final XOR), the algorithm
/// behind the UEFI `CalculateCrc32` boot service and the table header CRC.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    const CRC32_TABLE: [u32; 256] = {
        const P: u32 = 0xEDB8_8320;
        let mut tbl = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            let mut c = i as u32;
            let mut j = 0;
            while j < 8 {
                // reflected step
                c = if (c & 1) != 0 { (c >> 1) ^ P } else { c >> 1 };
                j += 1;
            }
            tbl[i] = c;
            i += 1;
        }
        tbl
    };

    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
    }
    crc ^ 0xFFFF_FFFF
}

/// Build the header for a `repr(C)` UEFI table whose first field is the
/// [`Header`]. The CRC is left zero; [`stamp_table_crc`] fills it in once the
/// table contents are final.
pub(crate) fn table_header<T>(signature: u64) -> Header {
    Header {
        signature,
        revision: TABLE_REVISION,
        size: u32::try_from(core::mem::size_of::<T>()).expect("table size fits in u32"),
        crc: 0,
        reserved: 0,
    }
}

/// Stamp the CRC-32 over an entire finalized UEFI table into its header.
///
/// # Safety
///
/// `table` must point to a `repr(C)` UEFI table whose first field is a
/// [`Header`] constructed by [`table_header`], and whose
/// `size_of::<T>()` bytes of contents are final: the CRC covers the whole
/// table, not just the header.
pub(crate) unsafe fn stamp_table_crc<T>(table: *mut T) {
    let bytes =
        unsafe { core::slice::from_raw_parts(table.cast::<u8>(), core::mem::size_of::<T>()) };
    let header = unsafe { &mut *table.cast::<Header>() };
    debug_assert_eq!(header.crc, 0);
    header.crc = crc32(bytes);
}
