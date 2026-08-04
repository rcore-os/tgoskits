//! Hash tree node parsing helpers.

use alloc::vec::Vec;

use super::{HashTreeError, HashTreeManager, HashTreeNode};
use crate::{
    endian::{read_u16_le, read_u32_le},
    entries::{
        Ext4DirEntry2, Ext4DirEntryInfo, Ext4DxCountlimit, Ext4DxEntry, Ext4DxNode, Ext4DxRoot,
        Ext4DxRootInfo,
    },
};

impl HashTreeManager {
    pub(super) fn parse_root_node(&self, data: &[u8]) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < core::mem::size_of::<Ext4DxRoot>() {
            return Err(HashTreeError::BufferTooSmall);
        }

        let dot = Ext4DirEntryInfo::parse_from_bytes(&data[0..8])
            .ok_or(HashTreeError::CorruptedHashTree)?;
        let dotdot = Ext4DirEntryInfo::parse_from_bytes(&data[dot.inode as usize..])
            .ok_or(HashTreeError::CorruptedHashTree)?;

        let info_offset = dot.inode as usize + dotdot.inode as usize;
        if info_offset + core::mem::size_of::<Ext4DxRootInfo>() > data.len() {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let info_bytes = &data[info_offset..info_offset + core::mem::size_of::<Ext4DxRootInfo>()];
        let hash_version = info_bytes[5];
        let indirect_levels = info_bytes[6];

        let entries_offset = info_offset + core::mem::size_of::<Ext4DxRootInfo>();
        let entries = self.parse_dx_entries(&data[entries_offset..])?;

        Ok(HashTreeNode::Root {
            hash_version,
            indirect_levels,
            entries,
        })
    }

    pub(super) fn parse_dx_entries(&self, data: &[u8]) -> Result<Vec<Ext4DxEntry>, HashTreeError> {
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset + core::mem::size_of::<Ext4DxEntry>() <= data.len() {
            let hash = read_u32_le(&data[offset..offset + 4]);
            let block = read_u32_le(&data[offset + 4..offset + 8]);

            if block == 0 {
                break;
            }

            entries.push(Ext4DxEntry { hash, block });
            offset += core::mem::size_of::<Ext4DxEntry>();
        }

        Ok(entries)
    }

    pub(super) fn parse_internal_node(&self, data: &[u8]) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < core::mem::size_of::<Ext4DxNode>() {
            return Err(HashTreeError::BufferTooSmall);
        }

        let fake_entry_size = core::mem::size_of::<Ext4DirEntry2>();
        let countlimit_offset = fake_entry_size;
        if countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>() > data.len() {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let countlimit_bytes =
            &data[countlimit_offset..countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>()];
        let _count = read_u16_le(&countlimit_bytes[2..4]) as usize;

        let entries_offset = countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>();
        let entries = self.parse_dx_entries(&data[entries_offset..])?;

        Ok(HashTreeNode::Internal { entries, level: 0 })
    }
}
