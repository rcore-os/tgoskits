//! Decodes the group descriptors that follow a superblock. The input is a 1024-byte superblock
//! followed by the descriptor table; the superblock gives the descriptor size and group count, as
//! it does at mount.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rsext4::{
    blockgroup_description::{BlockGroupDescTable, Ext4GroupDesc},
    bmalloc::BGIndex,
    endian::DiskFormat,
    superblock::Ext4Superblock,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 1024 {
        return;
    }
    let (superblock, table) = data.split_at(1024);
    let superblock = Ext4Superblock::from_disk_bytes(superblock);
    if superblock.validate_geometry().is_err() {
        return;
    }
    let desc_size = usize::from(superblock.get_desc_size());
    let Ok(groups) = superblock.checked_block_groups_count() else {
        return;
    };
    let groups = groups.min((table.len() / desc_size.max(1)) as u32);
    for record in table.chunks_exact(desc_size).take(groups as usize) {
        if let Ok(desc) = Ext4GroupDesc::decode_checked(record) {
            let _ = desc.block_bitmap_csum(&superblock);
            let _ = desc.inode_bitmap_csum(&superblock);
            let _ = (desc.is_uninit_bg(), desc.is_inode_table_zeroed());
        }
    }
    let view = BlockGroupDescTable::new(table, desc_size, groups);
    for index in 0..groups {
        let _ = view.get_desc(BGIndex::new(index));
    }
    let _ = (
        view.total_free_blocks(),
        view.total_free_inodes(),
        view.total_used_dirs(),
    );
});
