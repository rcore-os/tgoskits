//! Directory block checksum helpers.

use super::core::ext4_metadata_csum32;
use crate::{
    BLOCK_SIZE,
    crc32c::{ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    entries::Ext4DirEntryTail,
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
    if block_bytes.len() < BLOCK_SIZE || BLOCK_SIZE < 12 {
        return false;
    }

    let tail_ft = block_bytes[BLOCK_SIZE - 5];
    if tail_ft != 0xDE {
        return true;
    }

    let stored = u32::from_le_bytes([
        block_bytes[BLOCK_SIZE - 4],
        block_bytes[BLOCK_SIZE - 3],
        block_bytes[BLOCK_SIZE - 2],
        block_bytes[BLOCK_SIZE - 1],
    ]);
    let data_len = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
    let computed = ext4_dirblock_csum32(sb, ino, generation, &block_bytes[..data_len]);
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
        let data_len = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
        let checksum =
            ext4_dirblock_csum32(sb, parent_dir_ino, generation, &block_bytes[..data_len]);
        block_bytes[BLOCK_SIZE - 4..].copy_from_slice(&checksum.to_le_bytes());
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
