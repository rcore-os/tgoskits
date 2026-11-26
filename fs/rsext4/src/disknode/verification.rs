//! Kani harness for the three twelve-byte extent records: each decodes from its first twelve
//! bytes and no others. Behind `#[cfg(kani)]`, so only `cargo kani` compiles it.

use super::*;

/// **The extent header, extent and extent index decode from any slice of at least twelve bytes
/// without panicking, and read only those twelve bytes**: a slice with four more arbitrary bytes
/// after the same twelve decodes to the same fields.
#[kani::proof]
fn extent_records_decode_reads_only_twelve_bytes() {
    let record: [u8; 12] = kani::any();
    let mut longer = [0u8; 16];
    longer[..12].copy_from_slice(&record);
    longer[12..].copy_from_slice(&kani::any::<[u8; 4]>());

    let (a, b) = (
        Ext4ExtentHeader::from_disk_bytes(&record),
        Ext4ExtentHeader::from_disk_bytes(&longer),
    );
    assert_eq!(
        (
            a.eh_magic,
            a.eh_entries,
            a.eh_max,
            a.eh_depth,
            a.eh_generation
        ),
        (
            b.eh_magic,
            b.eh_entries,
            b.eh_max,
            b.eh_depth,
            b.eh_generation
        )
    );
    let (a, b) = (
        Ext4Extent::from_disk_bytes(&record),
        Ext4Extent::from_disk_bytes(&longer),
    );
    assert_eq!(
        (a.ee_block, a.ee_len, a.ee_start_hi, a.ee_start_lo),
        (b.ee_block, b.ee_len, b.ee_start_hi, b.ee_start_lo)
    );
    let (a, b) = (
        Ext4ExtentIdx::from_disk_bytes(&record),
        Ext4ExtentIdx::from_disk_bytes(&longer),
    );
    assert_eq!(
        (a.ei_block, a.ei_leaf_lo, a.ei_leaf_hi, a.ei_unused),
        (b.ei_block, b.ei_leaf_lo, b.ei_leaf_hi, b.ei_unused)
    );
}
