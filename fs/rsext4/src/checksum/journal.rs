//! JBD2 journal checksum helpers.

use crate::{
    crc32c::{crc32c_append, crc32c_init},
    endian::DiskFormat,
    jbd2::jbdstruct::{
        JBD2_COMMIT_HEADER_SIZE, JBD2_CRC32C_CHKSUM, JBD2_UUID_SIZE, JournalSuperBlock,
    },
};

const JBD2_BLOCK_TAIL_SIZE: usize = 4;
const JBD2_COMMIT_CHECKSUM_OFFSET: usize = 16;

/// Appends bytes to the raw big-endian CRC32 accumulator used by JBD2's
/// `FEATURE_COMPAT_CHECKSUM` transaction checksum.
pub(crate) fn jbd2_compat_checksum_append(mut checksum: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        checksum ^= u32::from(byte) << 24;
        for _ in 0..8 {
            checksum = if checksum & 0x8000_0000 != 0 {
                (checksum << 1) ^ 0x04c1_1db7
            } else {
                checksum << 1
            };
        }
    }
    checksum
}

/// Computes the checksum stored in the JBD2 journal superblock.
pub fn jbd2_superblock_csum32(jsb: &JournalSuperBlock) -> u32 {
    let mut bytes = [0u8; 1024];
    let mut jsb_for_csum = *jsb;
    jsb_for_csum.s_checksum = 0;
    jsb_for_csum.to_disk_bytes(&mut bytes);
    crc32c_append(crc32c_init(), &bytes)
}

/// Updates the stored JBD2 journal superblock checksum.
pub fn jbd2_update_superblock_checksum(jsb: &mut JournalSuperBlock) {
    if jsb.is_v1() {
        return;
    }
    if jsb.s_checksum_type == JBD2_CRC32C_CHKSUM {
        jsb.s_checksum = jbd2_superblock_csum32(jsb);
    } else if jsb.s_checksum_type == 0 {
        jsb.s_checksum = 0;
    }
}

/// Derives the raw CRC32C accumulator used by JBD2 metadata checksums.
pub(crate) fn jbd2_checksum_seed(uuid: &[u8; JBD2_UUID_SIZE]) -> u32 {
    crc32c_append(crc32c_init(), uuid)
}

/// Computes the checksum stored in a CSUM_V3 descriptor tag.
pub(crate) fn jbd2_tag_csum32(
    uuid: &[u8; JBD2_UUID_SIZE],
    sequence: u32,
    journal_data: &[u8],
) -> u32 {
    let seed = jbd2_checksum_seed(uuid);
    let checksum = crc32c_append(seed, &sequence.to_be_bytes());
    crc32c_append(checksum, journal_data)
}

/// Computes the checksum stored in the final four bytes of a descriptor or revoke block.
pub(crate) fn jbd2_descriptor_block_csum32(
    uuid: &[u8; JBD2_UUID_SIZE],
    block: &[u8],
) -> Option<u32> {
    let checksum_offset = block.len().checked_sub(JBD2_BLOCK_TAIL_SIZE)?;
    checksum_with_zeroed_u32(jbd2_checksum_seed(uuid), block, checksum_offset)
}

/// Computes the checksum stored in `commit_header.h_chksum[0]`.
pub(crate) fn jbd2_commit_block_csum32(uuid: &[u8; JBD2_UUID_SIZE], block: &[u8]) -> Option<u32> {
    checksum_with_zeroed_u32(jbd2_checksum_seed(uuid), block, JBD2_COMMIT_CHECKSUM_OFFSET)
}

/// Computes Linux's fallback checksum for a commit block whose tail was only
/// partially persisted.
pub(crate) fn jbd2_partial_commit_block_csum32(
    uuid: &[u8; JBD2_UUID_SIZE],
    block: &[u8],
) -> Option<u32> {
    let commit_header = block.get(..JBD2_COMMIT_HEADER_SIZE)?;
    let checksum_end = JBD2_COMMIT_CHECKSUM_OFFSET.checked_add(4)?;
    let mut checksum = crc32c_append(
        jbd2_checksum_seed(uuid),
        &commit_header[..JBD2_COMMIT_CHECKSUM_OFFSET],
    );
    checksum = crc32c_append(checksum, &[0; 4]);
    checksum = crc32c_append(checksum, &commit_header[checksum_end..]);

    const ZERO_CHUNK: [u8; 64] = [0; 64];
    let mut zero_tail_len = block.len() - JBD2_COMMIT_HEADER_SIZE;
    while zero_tail_len >= ZERO_CHUNK.len() {
        checksum = crc32c_append(checksum, &ZERO_CHUNK);
        zero_tail_len -= ZERO_CHUNK.len();
    }
    Some(crc32c_append(checksum, &ZERO_CHUNK[..zero_tail_len]))
}

fn checksum_with_zeroed_u32(seed: u32, block: &[u8], offset: usize) -> Option<u32> {
    let checksum_end = offset.checked_add(4)?;
    if checksum_end > block.len() {
        return None;
    }

    let checksum = crc32c_append(seed, &block[..offset]);
    let checksum = crc32c_append(checksum, &[0; 4]);
    Some(crc32c_append(checksum, &block[checksum_end..]))
}
