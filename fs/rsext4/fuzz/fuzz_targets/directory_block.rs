//! Parses one directory block the way a linear directory read does, and verifies its checksum
//! tail against a fixed superblock. The input is the block; its length is the block size.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::{
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    entries::{DirEntryIterator, classic_dir::list_entries},
    superblock::Ext4Superblock,
};

fuzz_target!(|data: &[u8]| {
    for (entry, _offset) in DirEntryIterator::new(data) {
        let _ = (entry.name_str(), entry.is_dot(), entry.is_dotdot());
    }
    let _ = list_entries(data);
    let mut superblock = Ext4Superblock::default();
    superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
    let _ = verify_ext4_dirblock_checksum(&superblock, 2, 0, data);
    let _ = verify_ext4_dx_checksum(&superblock, 2, 0, data);
});
