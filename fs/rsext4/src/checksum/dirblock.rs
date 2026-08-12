//! Directory block checksum helpers.

use super::core::ext4_metadata_csum32;
use crate::{
    crc32c::{ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    endian::read_u16_le,
    entries::{Ext4DirEntryTail, Ext4DxEntry, Ext4DxRootInfo, decode_directory_record_length},
    superblock::Ext4Superblock,
};

/// Computes the CRC32C for a generic metadata block payload.
pub fn ext4_metadata_block_csum32(sb: &Ext4Superblock, data: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    ext4_metadata_csum32(seed, &[data])
}

/// Verifies a checksummed directory block on the read path.
pub fn verify_ext4_dirblock_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &[u8],
) -> bool {
    if !ext4_superblock_has_metadata_csum(sb) {
        return true;
    }
    let block_size = block_bytes.len();
    if block_size < Ext4DirEntryTail::TAIL_LEN as usize {
        return false;
    }

    let tail_offset = block_size - Ext4DirEntryTail::TAIL_LEN as usize;
    let tail = &block_bytes[tail_offset..];
    if tail[..4] != [0; 4]
        || read_u16_le(&tail[4..6]) != Ext4DirEntryTail::TAIL_LEN
        || tail[6] != 0
        || tail[7] != Ext4DirEntryTail::RESERVED_FT
    {
        return false;
    }

    let stored = u32::from_le_bytes([
        block_bytes[block_size - 4],
        block_bytes[block_size - 3],
        block_bytes[block_size - 2],
        block_bytes[block_size - 1],
    ]);
    let computed = ext4_dirblock_csum32(sb, ino, generation, &block_bytes[..tail_offset]);
    computed == stored
}

/// Updates the tail checksum stored in a directory data block.
pub fn update_ext4_dirblock_csum32(
    sb: &Ext4Superblock,
    parent_dir_ino: u32,
    generation: u32,
    block_bytes: &mut [u8],
) {
    if ext4_superblock_has_metadata_csum(sb) {
        let block_size = block_bytes.len();
        if block_size < Ext4DirEntryTail::TAIL_LEN as usize {
            return;
        }
        let data_len = block_size - Ext4DirEntryTail::TAIL_LEN as usize;
        let checksum =
            ext4_dirblock_csum32(sb, parent_dir_ino, generation, &block_bytes[..data_len]);
        block_bytes[block_size - 4..].copy_from_slice(&checksum.to_le_bytes());
    }
}

/// Computes the CRC32C bound to a specific directory inode and generation.
pub fn ext4_dirblock_csum32(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &[u8],
) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let ino_le = ino.to_le_bytes();
    let generation_le = generation.to_le_bytes();
    ext4_metadata_csum32(seed, &[&ino_le, &generation_le, block_bytes])
}

/// Updates the checksum field inside a directory entry tail.
pub fn ext4_update_dirblock_tail_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &mut [u8],
    tail_offset: usize,
) {
    if tail_offset + 12 > block_bytes.len() {
        return;
    }

    block_bytes[tail_offset + 8..tail_offset + 12].fill(0);
    let checksum = ext4_dirblock_csum32(sb, ino, generation, &block_bytes[..tail_offset]);
    block_bytes[tail_offset + 8..tail_offset + 12].copy_from_slice(&checksum.to_le_bytes());
}

/// Verifies the checksum stored in an HTree dx_tail block.
pub fn verify_ext4_dx_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &[u8],
) -> Option<bool> {
    if !ext4_superblock_has_metadata_csum(sb) {
        return Some(true);
    }
    let block_size = block_bytes.len();
    if block_size < 8 {
        return Some(false);
    }

    let count_offset = dx_countlimit_offset(block_bytes)?;
    if count_offset + 4 > block_size {
        return Some(false);
    }

    let limit = read_u16_le(&block_bytes[count_offset..count_offset + 2]) as usize;
    let count = read_u16_le(&block_bytes[count_offset + 2..count_offset + 4]) as usize;
    let entry_size = core::mem::size_of::<Ext4DxEntry>();
    let tail_len = core::mem::size_of::<u64>();

    if count > limit || count_offset + limit.saturating_mul(entry_size) > block_size - tail_len {
        return Some(false);
    }

    let tail_offset = count_offset + limit * entry_size;
    if tail_offset + tail_len > block_size {
        return Some(false);
    }

    let data_len = count_offset + count * entry_size;
    if data_len > tail_offset {
        return Some(false);
    }

    let stored = u32::from_le_bytes([
        block_bytes[tail_offset + 4],
        block_bytes[tail_offset + 5],
        block_bytes[tail_offset + 6],
        block_bytes[tail_offset + 7],
    ]);
    let zero_checksum = [0u8; 4];
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let ino_le = ino.to_le_bytes();
    let generation_le = generation.to_le_bytes();
    let computed = ext4_metadata_csum32(
        seed,
        &[
            &ino_le,
            &generation_le,
            &block_bytes[..data_len],
            &block_bytes[tail_offset..tail_offset + 4],
            &zero_checksum,
        ],
    );

    Some(computed == stored)
}

/// Updates the checksum stored in an HTree `dx_tail`.
pub(crate) fn update_ext4_dx_checksum(
    sb: &Ext4Superblock,
    ino: u32,
    generation: u32,
    block_bytes: &mut [u8],
) -> bool {
    if !ext4_superblock_has_metadata_csum(sb) {
        return true;
    }
    let Some(count_offset) = dx_countlimit_offset(block_bytes) else {
        return false;
    };
    if count_offset + 4 > block_bytes.len() {
        return false;
    }
    let limit = usize::from(read_u16_le(&block_bytes[count_offset..count_offset + 2]));
    let count = usize::from(read_u16_le(
        &block_bytes[count_offset + 2..count_offset + 4],
    ));
    let entry_size = core::mem::size_of::<Ext4DxEntry>();
    let Some(tail_offset) = count_offset.checked_add(limit.saturating_mul(entry_size)) else {
        return false;
    };
    let Some(data_len) = count_offset.checked_add(count.saturating_mul(entry_size)) else {
        return false;
    };
    if count == 0
        || count > limit
        || data_len > tail_offset
        || tail_offset + core::mem::size_of::<u64>() > block_bytes.len()
    {
        return false;
    }

    block_bytes[tail_offset + 4..tail_offset + 8].fill(0);
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let ino_le = ino.to_le_bytes();
    let generation_le = generation.to_le_bytes();
    let checksum = ext4_metadata_csum32(
        seed,
        &[
            &ino_le,
            &generation_le,
            &block_bytes[..data_len],
            &block_bytes[tail_offset..tail_offset + 8],
        ],
    );
    block_bytes[tail_offset + 4..tail_offset + 8].copy_from_slice(&checksum.to_le_bytes());
    true
}

fn dx_countlimit_offset(block_bytes: &[u8]) -> Option<usize> {
    if block_bytes.len() < 6 {
        return None;
    }
    let block_size = block_bytes.len();
    let rec_len = decode_directory_record_length(read_u16_le(&block_bytes[4..6]), block_size);
    if rec_len == block_size {
        return Some(8);
    }

    if rec_len != 12 || block_size < 32 {
        return None;
    }

    let root_info_offset = 24;
    let reserved_zero = u32::from_le_bytes([
        block_bytes[root_info_offset],
        block_bytes[root_info_offset + 1],
        block_bytes[root_info_offset + 2],
        block_bytes[root_info_offset + 3],
    ]);
    let info_length = block_bytes[root_info_offset + 5];
    if reserved_zero != 0 || info_length != Ext4DxRootInfo::INFO_LENGTH {
        return None;
    }

    Some(32)
}
