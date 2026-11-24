//! Decodes a 1024-byte ext4 superblock and runs every check mount runs on it before it reads
//! anything else.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::{endian::DiskFormat, superblock::Ext4Superblock};

fuzz_target!(|data: &[u8]| {
    if data.len() < 1024 {
        return;
    }
    let superblock = Ext4Superblock::from_disk_bytes(&data[..1024]);
    let _ = superblock.verify_superblock();
    let _ = superblock.checked_block_size();
    let _ = superblock.checked_cluster_size();
    let _ = superblock.checked_block_groups_count();
    let _ = superblock.primary_gdt_byte_offset();
    let _ = superblock.validate_geometry();
    let _ = superblock.descs_per_block();
    let _ = superblock.inode_table_blocks();
    let _ = superblock.blocks_count();
    let _ = superblock.free_blocks_count();
    let _ = superblock.reserved_blocks_count();
});
