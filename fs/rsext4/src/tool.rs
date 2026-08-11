//! Small utility helpers shared across the filesystem implementation.

use alloc::{vec, vec::*};

use crate::{ext4::BlockGroupLayout, superblock::*};

/// Generates a deterministic UUID-like value as four `u32` words.
pub fn generate_uuid() -> UUID {
    // Use a stable crate-specific seed until entropy is wired into mkfs.
    let mut orign_uuid = [1_u32; 4];
    let target_seed = 0x5253_4558;
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
    let target_seed = 0x52;
    let mut last_idx: usize = 0;
    orign_uuid[0] ^= target_seed;
    for idx in 0..orign_uuid.len() * 2 {
        let real_idx = idx % orign_uuid.len();
        orign_uuid[real_idx] ^= orign_uuid[last_idx];
        last_idx = real_idx;
    }

    orign_uuid
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
pub fn calc_group_layout(
    gid: u32,
    sb: &Ext4Superblock,
    blocks_per_group: u32,
    inode_table_blocks: u32,
    group0_block_bitmap: u32,
    group0_inode_bitmap: u32,
    group0_inode_table: u32,
    gdt_blocks: u32,
) -> BlockGroupLayout {
    if gid == 0 {
        return BlockGroupLayout {
            group_start_block: u64::from(sb.s_first_data_block),
            group_block_bitmap_start_block: group0_block_bitmap as u64,
            group_inode_bitmap_start_block: group0_inode_bitmap as u64,
            group_inode_table_start_block: group0_inode_table as u64,
            metadata_blocks_in_group: (group0_inode_table + inode_table_blocks)
                .saturating_sub(sb.s_first_data_block),
        };
    }

    // Non-zero groups place their metadata relative to the group's first block.
    let group_start = u64::from(sb.s_first_data_block)
        .saturating_add(u64::from(gid).saturating_mul(u64::from(blocks_per_group)));

    // Sparse-super decides whether this group carries backup metadata.
    let sparse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);

    let has_backup = sparse_feature && need_redundant_backup(gid);

    let (block_bitmap, inode_bitmap, inode_table, meta_blocks) = if has_backup {
        let bb = group_start + 1 + u64::from(gdt_blocks);
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

    BlockGroupLayout {
        group_start_block: group_start,
        group_block_bitmap_start_block: block_bitmap,
        group_inode_bitmap_start_block: inode_bitmap,
        group_inode_table_start_block: inode_table,
        metadata_blocks_in_group: meta_blocks,
    }
}
