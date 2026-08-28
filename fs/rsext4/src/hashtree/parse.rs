//! Hash tree node parsing helpers.

use alloc::vec::Vec;

use super::{HashTreeError, HashTreeManager, HashTreeNode};
use crate::{
    endian::{read_u16_le, read_u32_le},
    entries::{Ext4DxEntry, Ext4DxRootInfo, decode_directory_record_length},
};

const DIRENT_HEADER_LEN: usize = 8;
const ROOT_DOT_OFFSET: usize = 0;
const ROOT_DOTDOT_OFFSET: usize = 12;
const ROOT_INFO_OFFSET: usize = 24;
const ROOT_COUNTLIMIT_OFFSET: usize = 32;
const NODE_COUNTLIMIT_OFFSET: usize = 8;
const DX_ENTRY_LEN: usize = 8;
const DX_TAIL_LEN: usize = 8;
const DX_BLOCK_MASK: u32 = 0x0fff_ffff;

impl HashTreeManager {
    pub(super) fn parse_root_node(
        &self,
        data: &[u8],
        has_metadata_checksum: bool,
        max_indirect_levels: u8,
    ) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < ROOT_COUNTLIMIT_OFFSET + DX_ENTRY_LEN {
            return Err(HashTreeError::BufferTooSmall);
        }

        validate_root_dirent(data, ROOT_DOT_OFFSET, 12, b".")?;
        validate_root_dirent(
            data,
            ROOT_DOTDOT_OFFSET,
            data.len().saturating_sub(ROOT_DOTDOT_OFFSET),
            b"..",
        )?;

        let reserved_zero = read_u32_le(&data[ROOT_INFO_OFFSET..ROOT_INFO_OFFSET + 4]);
        let hash_version = data[ROOT_INFO_OFFSET + 4];
        let info_length = data[ROOT_INFO_OFFSET + 5];
        let indirect_levels = data[ROOT_INFO_OFFSET + 6];
        let unused_flags = data[ROOT_INFO_OFFSET + 7];
        if reserved_zero != 0
            || info_length != Ext4DxRootInfo::INFO_LENGTH
            || unused_flags & 1 != 0
            || indirect_levels > max_indirect_levels
        {
            return Err(HashTreeError::CorruptedHashTree);
        }
        if !matches!(
            hash_version,
            Ext4DxRootInfo::DX_HASH_LEGACY
                | Ext4DxRootInfo::DX_HASH_HALF_MD4
                | Ext4DxRootInfo::DX_HASH_TEA
                | Ext4DxRootInfo::DX_HASH_SIPHASH
        ) {
            return Err(HashTreeError::UnsupportedHashVersion);
        }

        let expected_limit = dx_limit(data.len(), ROOT_COUNTLIMIT_OFFSET, has_metadata_checksum)?;
        let entries = self.parse_dx_entries(data, ROOT_COUNTLIMIT_OFFSET, expected_limit)?;

        Ok(HashTreeNode::Root {
            hash_version,
            indirect_levels,
            entries,
        })
    }

    pub(super) fn parse_dx_entries(
        &self,
        data: &[u8],
        countlimit_offset: usize,
        expected_limit: usize,
    ) -> Result<Vec<Ext4DxEntry>, HashTreeError> {
        let header = data
            .get(countlimit_offset..countlimit_offset + DX_ENTRY_LEN)
            .ok_or(HashTreeError::BufferTooSmall)?;
        let limit = usize::from(read_u16_le(&header[..2]));
        let count = usize::from(read_u16_le(&header[2..4]));
        if limit != expected_limit || count == 0 || count > limit {
            return Err(HashTreeError::CorruptedHashTree);
        }
        let entries_end = countlimit_offset
            .checked_add(
                count
                    .checked_mul(DX_ENTRY_LEN)
                    .ok_or(HashTreeError::CorruptedHashTree)?,
            )
            .ok_or(HashTreeError::CorruptedHashTree)?;
        if entries_end > data.len() {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let mut entries = Vec::with_capacity(count);
        let first_block = read_u32_le(&header[4..8]) & DX_BLOCK_MASK;
        if first_block == 0 {
            return Err(HashTreeError::CorruptedHashTree);
        }
        entries.push(Ext4DxEntry {
            hash: 0,
            block: first_block,
        });

        let mut previous_hash = 0;
        for index in 1..count {
            let offset = countlimit_offset + index * DX_ENTRY_LEN;
            let hash = read_u32_le(&data[offset..offset + 4]);
            let block = read_u32_le(&data[offset + 4..offset + 8]) & DX_BLOCK_MASK;
            if block == 0 || hash < previous_hash {
                return Err(HashTreeError::CorruptedHashTree);
            }
            entries.push(Ext4DxEntry { hash, block });
            previous_hash = hash;
        }

        Ok(entries)
    }

    pub(super) fn parse_internal_node(
        &self,
        data: &[u8],
        has_metadata_checksum: bool,
    ) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < NODE_COUNTLIMIT_OFFSET + DX_ENTRY_LEN {
            return Err(HashTreeError::BufferTooSmall);
        }
        let inode = read_u32_le(&data[..4]);
        let record_len = decode_directory_record_length(read_u16_le(&data[4..6]), data.len());
        if inode != 0 || record_len != data.len() || data[6] != 0 || data[7] != 0 {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let expected_limit = dx_limit(data.len(), NODE_COUNTLIMIT_OFFSET, has_metadata_checksum)?;
        let entries = self.parse_dx_entries(data, NODE_COUNTLIMIT_OFFSET, expected_limit)?;

        Ok(HashTreeNode::Internal { entries })
    }
}

fn validate_root_dirent(
    data: &[u8],
    offset: usize,
    expected_record_len: usize,
    expected_name: &[u8],
) -> Result<(), HashTreeError> {
    let header = data
        .get(offset..offset + DIRENT_HEADER_LEN)
        .ok_or(HashTreeError::BufferTooSmall)?;
    let inode = read_u32_le(&header[..4]);
    let record_len = decode_directory_record_length(read_u16_le(&header[4..6]), data.len());
    let name_len = usize::from(header[6]);
    let name = data
        .get(offset + DIRENT_HEADER_LEN..offset + DIRENT_HEADER_LEN + name_len)
        .ok_or(HashTreeError::CorruptedHashTree)?;
    if inode == 0
        || record_len != expected_record_len
        || name_len != expected_name.len()
        || name != expected_name
    {
        return Err(HashTreeError::CorruptedHashTree);
    }
    Ok(())
}

fn dx_limit(
    block_size: usize,
    countlimit_offset: usize,
    has_metadata_checksum: bool,
) -> Result<usize, HashTreeError> {
    let tail_len = if has_metadata_checksum {
        DX_TAIL_LEN
    } else {
        0
    };
    let entries_len = block_size
        .checked_sub(countlimit_offset)
        .and_then(|len| len.checked_sub(tail_len))
        .ok_or(HashTreeError::BufferTooSmall)?;
    Ok(entries_len / DX_ENTRY_LEN)
}
