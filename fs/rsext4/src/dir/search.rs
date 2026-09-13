//! Exact-byte child lookup through a directory's read-only block capability.

use alloc::vec::Vec;

use super::DirectoryBlockRead;
use crate::{
    Ext4Error, Ext4Result,
    bmalloc::{AbsoluteBN, InodeNumber},
    entries::decode_directory_record_length,
    hashtree::{
        Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, lookup_directory_with_reader,
    },
};

/// A located record. Its physical position is usable for mutation only while
/// the caller still owns the namespace/mapping exclusion used for lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParentDirEntry {
    pub ino: InodeNumber,
    pub phys: AbsoluteBN,
    pub offset: usize,
    pub file_type: u8,
}

pub(crate) fn find_named_entry<R: DirectoryBlockRead>(
    reader: &mut R,
    name: &[u8],
) -> Ext4Result<ParentDirEntry> {
    if !reader.inode().is_dir() {
        return Err(Ext4Error::not_dir());
    }
    if reader.inode().is_htree_indexed() {
        let manager = HashTreeManager::new(reader.superblock().s_hash_seed);
        return match lookup_directory_with_reader(&manager, reader, name) {
            Ok(result) => Ok(ParentDirEntry {
                ino: result.inode,
                phys: result.block_num,
                offset: result.offset,
                file_type: result.file_type,
            }),
            Err(HashTreeError::EntryNotFound) => Err(Ext4Error::not_found()),
            Err(error) => Err(error.into_ext4("htree:parent_lookup")),
        };
    }

    for physical in parent_data_blocks(reader)? {
        let bytes = reader.read_block(physical)?;
        if !crate::checksum::verify_ext4_dirblock_checksum(
            reader.superblock(),
            reader.directory().raw(),
            reader.inode().i_generation,
            &bytes,
        ) {
            return Err(Ext4Error::checksum().with_operation("directory:lookup_block"));
        }
        if let Some((inode, file_type, offset)) = find_record(&bytes, name) {
            return Ok(ParentDirEntry {
                ino: InodeNumber::new(inode).map_err(|_| Ext4Error::corrupted())?,
                phys: physical,
                offset,
                file_type,
            });
        }
    }
    Err(Ext4Error::not_found())
}

fn parent_data_blocks<R: DirectoryBlockRead>(reader: &mut R) -> Ext4Result<Vec<AbsoluteBN>> {
    let mut blocks: Vec<_> = if reader.inode().uses_extents() {
        reader.mapped_blocks()?.into_values().collect()
    } else {
        let total_size =
            usize::try_from(reader.inode_size()).map_err(|_| Ext4Error::file_too_large())?;
        let total_blocks = total_size.div_ceil(reader.block_size());
        let mut blocks = Vec::new();
        for logical in 0..total_blocks {
            if let Some(physical) = reader.map_block(logical as u32)? {
                blocks.push(physical);
            }
        }
        blocks
    };
    // Preserve the existing linear parent scan's physical ordering and alias
    // deduplication. HTree's logical/hash traversal has its own ordering.
    blocks.sort_unstable();
    blocks.dedup();
    Ok(blocks)
}

fn find_record(bytes: &[u8], name: &[u8]) -> Option<(u32, u8, usize)> {
    let block_bytes = bytes.len();
    let mut offset: usize = 0;
    while offset + 8 <= block_bytes {
        let inode = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let record_len = decode_directory_record_length(
            u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]),
            block_bytes,
        );
        if record_len < 8 || !record_len.is_multiple_of(4) {
            break;
        }
        let name_len = bytes[offset + 6] as usize;
        let Some(entry_end) = offset.checked_add(record_len) else {
            break;
        };
        if entry_end > block_bytes {
            break;
        }
        if name_len > 0 && offset + 8 + name_len <= entry_end {
            let candidate = &bytes[offset + 8..offset + 8 + name_len];
            if inode != 0 && candidate == name {
                return Some((inode, bytes[offset + 7], offset));
            }
        }
        if entry_end >= block_bytes {
            break;
        }
        offset = entry_end;
    }
    None
}
