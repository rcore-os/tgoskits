//! JBD2 journal checksum helpers.

use crate::{crc32c::crc32c, endian::DiskFormat, jbd2::jbdstruct::JournalSuperBllockS};

/// Computes the checksum stored in the JBD2 journal superblock.
pub fn jbd2_superblock_csum32(jsb: &JournalSuperBllockS) -> u32 {
    let mut bytes = [0u8; 1024];
    let mut jsb_for_csum = *jsb;
    jsb_for_csum.s_checksum = 0;
    jsb_for_csum.to_disk_bytes(&mut bytes);
    crc32c(&bytes)
}

/// Updates the stored JBD2 journal superblock checksum.
pub fn jbd2_update_superblock_checksum(jsb: &mut JournalSuperBllockS) {
    jsb.s_checksum = jbd2_superblock_csum32(jsb);
}
