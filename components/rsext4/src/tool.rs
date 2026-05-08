//! Small utility helpers shared across the filesystem implementation.

use alloc::{vec, vec::*};

use log::debug;

use crate::{ext4::*, superblock::*};

/// Generates a deterministic UUID-like value as four `u32` words.
pub fn generate_uuid() -> UUID {
    // Mix a stable function pointer value into the seed and diffuse it across
    // the whole array. This is lightweight and deterministic for tests.
    let mut orign_uuid = [1_u32; 4];
    let target_seed = debug_super_and_desc as *const () as u32;
    let mut last_idx: usize = 0;
    orign_uuid[0] ^= target_seed;
    for idx in 0..orign_uuid.len() * 2 {
        let real_idx = idx % orign_uuid.len();
        orign_uuid[real_idx] ^= orign_uuid[last_idx];
        last_idx = real_idx;
    }

    UUID(orign_uuid)
}

/// Generates a deterministic UUID-like value as raw bytes.
pub fn generate_uuid_8() -> [u8; 16] {
    // Reuse the same diffusion strategy as `generate_uuid`, but keep the result
    // in byte form for on-disk fields that expect `[u8; 16]`.
    let mut orign_uuid = [1_u8; 16];
    let target_seed = debug_super_and_desc as *const () as u8;
    let mut last_idx: usize = 0;
    orign_uuid[0] ^= target_seed;
    for idx in 0..orign_uuid.len() * 2 {
        let real_idx = idx % orign_uuid.len();
        orign_uuid[real_idx] ^= orign_uuid[last_idx];
        last_idx = real_idx;
    }

    orign_uuid
}

pub fn debug_super_and_desc(superblock: &Ext4Superblock, fs: &Ext4FileSystem) {
    debug!("Superblock info: {:?}", &superblock);
    debug!("Block group descriptors:");
    let desc = &fs.group_descs;
    for gid in desc {
        debug!("Group descriptor: {gid:?}");
    }
}

/// Returns whether this group should carry a sparse-super backup copy.
pub fn need_redundant_backup(gid: u32) -> bool {
    if gid == 0 || gid == 1 {
        return true;
    }
    let tmp_number = gid as usize;
    let count: Vec<usize> = vec![3, 5, 7];
    for gid in count {
        if is_numbers_power(tmp_number, gid) {
            return true;
        }
    }
    false
}
/// Returns whether `number` is an exact power of `base`.
pub fn is_numbers_power(number: usize, base: usize) -> bool {
    let mut tmp_number = number;
    if tmp_number == 1 {
        return true;
    }
    while tmp_number.is_multiple_of(base) {
        tmp_number /= base;
    }
    tmp_number == 1
}

/// Computes the physical layout of one block group during mkfs.
///
/// Group 0 uses the explicitly precomputed primary layout. Other groups follow
/// the sparse-super rules and either reserve space for backup superblock/GDT
/// copies or start directly with their bitmaps.
#[allow(clippy::too_many_arguments)]
pub fn cloc_group_layout(
    gid: u32,
    sb: &Ext4Superblock,
    blocks_per_group: u32,
    inode_table_blocks: u32,
    group0_block_bitmap: u32,
    group0_inode_bitmap: u32,
    group0_inode_table: u32,
    gdt_blocks: u32,
) -> BlcokGroupLayout {
    if gid == 0 {
        return BlcokGroupLayout {
            group_start_block: 0,
            group_blcok_bitmap_startblocks: group0_block_bitmap as u64,
            group_inode_bitmap_startblocks: group0_inode_bitmap as u64,
            group_inode_table_startblocks: group0_inode_table as u64,
            metadata_blocks_in_group: (group0_inode_table + inode_table_blocks),
        };
    }

    // Non-zero groups place their metadata relative to the group's first block.
    let group_start = gid * blocks_per_group;

    // Sparse-super decides whether this group carries backup metadata.
    let sparse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);

    let has_backup = sparse_feature && need_redundant_backup(gid);

    let (block_bitmap, inode_bitmap, inode_table, meta_blocks) = if has_backup {
        let bb = group_start + 1 + gdt_blocks;
        let ib = bb + 1;
        let it = ib + 1;
        let meta = 1 + gdt_blocks + 1 + 1 + inode_table_blocks;
        (bb, ib, it, meta)
    } else {
        let bb = group_start;
        let ib = group_start + 1;
        let it = group_start + 2;
        let meta = 1 + 1 + inode_table_blocks;
        (bb, ib, it, meta)
    };

    BlcokGroupLayout {
        group_start_block: group_start as u64,
        group_blcok_bitmap_startblocks: block_bitmap as u64,
        group_inode_bitmap_startblocks: inode_bitmap as u64,
        group_inode_table_startblocks: inode_table as u64,
        metadata_blocks_in_group: meta_blocks,
    }
}
