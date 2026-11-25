//! Kani harness for the superblock decode: it reads only inside its 1024-byte record. Behind
//! `#[cfg(kani)]`, so only `cargo kani` compiles it.

use super::Ext4Superblock;
use crate::endian::DiskFormat;

/// **Decoding a superblock from its 1024 bytes, whatever they hold, does not panic**, so no read
/// falls past the record: every field read indexes the slice, and an index past its end panics.
#[kani::proof]
fn superblock_decode_reads_only_its_record() {
    const SIZE: usize = 1024;
    let record: [u8; SIZE] = kani::any();
    assert_eq!(Ext4Superblock::disk_size(), SIZE);
    let superblock = Ext4Superblock::from_disk_bytes(&record);
    assert_eq!(
        superblock.s_magic,
        u16::from_le_bytes([record[56], record[57]])
    );
}
