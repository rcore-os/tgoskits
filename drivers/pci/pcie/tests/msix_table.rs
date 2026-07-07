use core::ptr::NonNull;

use pcie::{MsiMessage, MsixError, MsixTableEntry, MsixTableInfo, MsixTableRegion};

#[test]
fn msix_table_entry_masks_before_programming_message() {
    let mut raw = [0u32; 4];
    let entry = unsafe { MsixTableEntry::from_raw(raw.as_mut_ptr()) };

    entry.program_masked(MsiMessage::new(0xfee0_0000, 0x45));

    assert_eq!(raw[0], 0xfee0_0000);
    assert_eq!(raw[1], 0);
    assert_eq!(raw[2], 0x45);
    assert_eq!(raw[3] & 1, 1);
}

#[test]
fn msix_table_region_rejects_out_of_range_vectors() {
    let mut raw = [0u32; 4];
    let region = unsafe { MsixTableRegion::new(NonNull::new(raw.as_mut_ptr().cast()).unwrap(), 1) };

    assert_eq!(region.mask(1), Err(MsixError::InvalidVector));
}

#[test]
fn msix_table_info_rejects_tables_outside_bar() {
    let info = MsixTableInfo {
        bar: 2,
        offset: 0x80,
        entries: 4,
        pba_bar: 2,
        pba_offset: 0x100,
    };

    assert_eq!(
        info.table_range(0x1000..0x10bf),
        Err(MsixError::TableOutsideBar)
    );
    assert_eq!(info.table_range(0x1000..0x10c0).unwrap(), 0x1080..0x10c0);
}
