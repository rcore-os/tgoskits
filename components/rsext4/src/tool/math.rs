//! Math-like helpers and Linux-aligned size limit calculations.
//!
//! This module currently mirrors Linux ext4's `s_maxbytes` and `s_bitmap_maxbytes`
//! computations (see Linux 6.6.98 `fs/ext4/super.c`).

use crate::{config::runtime_block_size, disknode::Ext4Inode, superblock::Ext4Superblock};

// Maximum logical block number representable by ext4 extents.
//
// Linux uses a 32-bit `ee_block` field (logical start block) and reserves the
// last value, hence `0xFFFF_FFFE` in `fs/ext4/ext4.h`.
const EXT4_MAX_LOGICAL_BLOCK: u64 = 0xFFFF_FFFE;

/// Number of direct block pointers in the classic ext2/ext3 `i_block[]` layout.
///
/// This is used by Linux's `ext4_max_bitmap_size()` calculation for block-mapped
/// inodes.
const EXT4_NDIR_BLOCKS: u128 = 12;

/// Upper bound imposed by the VFS / `loff_t` range (mirrors `MAX_LFS_FILESIZE`).
///
/// Linux clamps ext4 maxbytes against `MAX_LFS_FILESIZE` (which is `LLONG_MAX`
/// on 64-bit).
const MAX_LFS_FILESIZE: u128 = i64::MAX as u128;

/// Integer ceil division for positive integers.
const fn div_ceil_u128(n: u128, d: u128) -> u128 {
    if d == 0 { 0 } else { n.div_ceil(d) }
}

/// Linux `ext4_max_size()` equivalent for extent-mapped inodes, returned in bytes.
///
/// Source of truth: `other/linux-6.6.98/fs/ext4/super.c:ext4_max_size()`.
fn ext4_max_size_bytes(blkbits: u32, has_huge_files: bool) -> u64 {
    if blkbits < 9 {
        return 0;
    }

    // Without huge_file, the i_blocks accounting limits the addressable size.
    let mut upper_limit = MAX_LFS_FILESIZE;
    if !has_huge_files {
        upper_limit = (u128::from(u32::MAX) >> (blkbits - 9)) << blkbits;
    }

    // Extents have a 32-bit logical block start; Linux uses `((2^32 - 1) << blkbits)`.
    let mut res = u128::from(u32::MAX) << blkbits;
    if res > upper_limit {
        res = upper_limit;
    }

    res.min(u128::from(u64::MAX)) as u64
}

/// Linux `ext4_max_bitmap_size()` equivalent for block-mapped inodes, in bytes.
///
/// Source of truth: `other/linux-6.6.98/fs/ext4/super.c:ext4_max_bitmap_size()`.
fn ext4_max_bitmap_size_bytes(bits: u32, has_huge_files: bool) -> u64 {
    if bits < 9 {
        return 0;
    }

    // Maximum block count allowed by the on-disk i_blocks encoding rules.
    let mut upper_limit = if !has_huge_files {
        u128::from(u32::MAX) >> (bits - 9)
    } else {
        (1u128 << 48) - 1
    };

    let ppb = 1u128 << (bits - 2);
    let mut res = EXT4_NDIR_BLOCKS + ppb + ppb * ppb + ppb * ppb * ppb;

    // Metadata blocks needed for single/double/triple indirect addressing.
    let mut meta_blocks = 1u128;
    meta_blocks += 1 + ppb;
    meta_blocks += 1 + ppb + ppb * ppb;

    if res + meta_blocks > upper_limit {
        res = upper_limit;
        upper_limit = upper_limit.saturating_sub(EXT4_NDIR_BLOCKS);

        // Recompute how many metadata blocks are required for addressing `upper_limit`.
        meta_blocks = 1;
        upper_limit = upper_limit.saturating_sub(ppb);
        if upper_limit < ppb * ppb {
            meta_blocks += 1 + div_ceil_u128(upper_limit, ppb);
            res = res.saturating_sub(meta_blocks);
        } else {
            meta_blocks += 1 + ppb;
            upper_limit = upper_limit.saturating_sub(ppb * ppb);
            meta_blocks +=
                1 + div_ceil_u128(upper_limit, ppb) + div_ceil_u128(upper_limit, ppb * ppb);
            res = res.saturating_sub(meta_blocks);
        }
    }

    let mut bytes = res << bits;
    if bytes > MAX_LFS_FILESIZE {
        bytes = MAX_LFS_FILESIZE;
    }

    bytes.min(u128::from(u64::MAX)) as u64
}

/// Linux-like ext4 max file offset (in bytes) for this inode type.
///
/// For extent inodes this mirrors `ext4_get_maxbytes()` via `s_maxbytes`.
/// For bitmap-mapped inodes this mirrors `s_bitmap_maxbytes`.
pub fn ext4_get_maxbytes(sb: &Ext4Superblock, inode: &Ext4Inode) -> u64 {
    let block_size = runtime_block_size();
    if block_size == 0 || !block_size.is_power_of_two() {
        return 0;
    }

    let blkbits = block_size.trailing_zeros();
    let has_huge_files = sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);

    if inode.i_flags & Ext4Inode::EXT4_EXTENTS_FL != 0 {
        // `ee_block` is 32-bit. Keep this explicit guard even though
        // `ext4_max_size_bytes` already mirrors Linux's limit.
        let extent_guard = (EXT4_MAX_LOGICAL_BLOCK.saturating_add(1)).saturating_mul(block_size);
        ext4_max_size_bytes(blkbits, has_huge_files).min(extent_guard)
    } else {
        // no have extent
        ext4_max_bitmap_size_bytes(blkbits, has_huge_files)
    }
}
