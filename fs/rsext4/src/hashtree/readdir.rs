//! Hash-ordered enumeration of indexed directory leaves.

use alloc::{collections::BTreeSet, vec::Vec};

use super::{
    HashTreeError, HashTreeManager, HashTreeNode, calculate_hash,
    lookup::{HashSearch, resolve_logical_block},
};
use crate::{
    Ext4Error, Ext4Result,
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::InodeNumber,
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    dir::FileName,
    disknode::Ext4Inode,
    entries::{Ext4DxEntry, decode_directory_record_length},
    ext4::Ext4FileSystem,
};

const DIRENT_HEADER_LEN: usize = 8;

/// One active record extracted from an indexed directory leaf.
pub(crate) struct IndexedDirectoryRecord {
    pub(crate) inode: u32,
    pub(crate) file_type: u8,
    pub(crate) name: Vec<u8>,
    pub(crate) major: u32,
    pub(crate) minor: u32,
    pub(crate) collision: u32,
}

struct UnorderedDirectoryRecord {
    inode: u32,
    file_type: u8,
    name: Vec<u8>,
    major: u32,
    minor: u32,
}

/// Enumerates only HTree leaves and orders their records by the complete ext4
/// directory hash. Index blocks are validated but never interpreted as
/// ordinary directory records.
pub(crate) fn read_indexed_directory<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    directory: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<Vec<IndexedDirectoryRecord>> {
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let (search, root) = manager
        .prepare_search(fs, block_dev, directory, inode, b"")
        .map_err(hash_tree_error)?;
    let HashTreeNode::Root {
        indirect_levels,
        entries,
        ..
    } = root
    else {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_root"));
    };

    let root_block = manager
        .get_root_block(fs, block_dev, directory, inode)
        .map_err(hash_tree_error)?;
    let root_data = manager
        .read_block_data(fs, block_dev, root_block)
        .map_err(hash_tree_error)?;
    let mut records = Vec::new();
    records.push(parse_root_record(&root_data, 0, 0)?);
    records.push(parse_root_record(&root_data, 12, 2)?);

    let mut seen_logical_blocks = BTreeSet::new();
    seen_logical_blocks.insert(0);
    let mut pending = Vec::new();
    push_children(&mut pending, &entries, indirect_levels);
    while let Some((logical_block, remaining_levels)) = pending.pop() {
        if !seen_logical_blocks.insert(logical_block) {
            return Err(Ext4Error::corrupted().with_operation("htree:readdir_cycle"));
        }
        let physical_block =
            resolve_logical_block(fs, block_dev, search, logical_block).map_err(hash_tree_error)?;
        let data = manager
            .read_block_data(fs, block_dev, physical_block)
            .map_err(hash_tree_error)?;

        if remaining_levels == 0 {
            if !verify_ext4_dirblock_checksum(
                &fs.superblock,
                directory.raw(),
                inode.i_generation,
                &data,
            ) {
                return Err(Ext4Error::checksum().with_operation("htree:readdir_leaf"));
            }
            parse_leaf_records(&data, search, &manager, &mut records)?;
            continue;
        }

        if verify_ext4_dx_checksum(&fs.superblock, directory.raw(), inode.i_generation, &data)
            == Some(false)
        {
            return Err(Ext4Error::checksum().with_operation("htree:readdir_index"));
        }
        let has_metadata_checksum =
            crate::crc32c::ext4_superblock_has_metadata_csum(&fs.superblock);
        let HashTreeNode::Internal { entries } = manager
            .parse_internal_node(&data, has_metadata_checksum)
            .map_err(hash_tree_error)?
        else {
            return Err(Ext4Error::corrupted().with_operation("htree:readdir_index"));
        };
        push_children(&mut pending, &entries, remaining_levels - 1);
    }

    records.sort_by_key(|record| (record.major, record.minor));
    let mut previous_hash = None;
    let mut collision = 0_u32;
    records
        .into_iter()
        .map(|record| {
            let hash = (record.major, record.minor);
            if previous_hash == Some(hash) {
                collision = collision.checked_add(1).ok_or_else(Ext4Error::overflow)?;
            } else {
                previous_hash = Some(hash);
                collision = 0;
            }
            Ok(IndexedDirectoryRecord {
                inode: record.inode,
                file_type: record.file_type,
                name: record.name,
                major: record.major,
                minor: record.minor,
                collision,
            })
        })
        .collect()
}

fn push_children(pending: &mut Vec<(u32, u8)>, entries: &[Ext4DxEntry], levels: u8) {
    pending.extend(entries.iter().rev().map(|entry| (entry.block, levels)));
}

fn parse_root_record(
    data: &[u8],
    offset: usize,
    major: u32,
) -> Ext4Result<UnorderedDirectoryRecord> {
    let (inode, file_type, name, _) = parse_record(data, offset)?;
    if inode == 0 {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_dot"));
    }
    Ok(UnorderedDirectoryRecord {
        inode,
        file_type,
        name,
        major,
        minor: 0,
    })
}

fn parse_leaf_records(
    data: &[u8],
    search: HashSearch<'_>,
    manager: &HashTreeManager,
    output: &mut Vec<UnorderedDirectoryRecord>,
) -> Ext4Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let (inode, file_type, name, next_offset) = parse_record(data, offset)?;
        if inode != 0 {
            FileName::new(&name)
                .map_err(|_| Ext4Error::corrupted().with_operation("htree:readdir_name"))?;
            let hash = calculate_hash(&name, search.hash_version, &manager.hash_seed)
                .map_err(hash_tree_error)?;
            output.push(UnorderedDirectoryRecord {
                inode,
                file_type,
                name,
                major: hash.major,
                minor: hash.minor,
            });
        }
        offset = next_offset;
    }
    Ok(())
}

fn parse_record(data: &[u8], offset: usize) -> Ext4Result<(u32, u8, Vec<u8>, usize)> {
    let header = data
        .get(offset..offset + DIRENT_HEADER_LEN)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:readdir_record_header"))?;
    let inode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let record_len =
        decode_directory_record_length(u16::from_le_bytes([header[4], header[5]]), data.len());
    let name_len = usize::from(header[6]);
    let file_type = header[7];
    let next_offset = offset
        .checked_add(record_len)
        .ok_or_else(Ext4Error::overflow)?;
    if record_len < DIRENT_HEADER_LEN
        || !record_len.is_multiple_of(4)
        || next_offset > data.len()
        || name_len > record_len - DIRENT_HEADER_LEN
    {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_record"));
    }
    Ok((
        inode,
        file_type,
        data[offset + DIRENT_HEADER_LEN..offset + DIRENT_HEADER_LEN + name_len].to_vec(),
        next_offset,
    ))
}

fn hash_tree_error(error: HashTreeError) -> Ext4Error {
    match error {
        HashTreeError::Filesystem(error) => error,
        HashTreeError::UnsupportedHashVersion => {
            Ext4Error::unsupported().with_operation("htree:readdir_hash_version")
        }
        HashTreeError::EntryNotFound => {
            Ext4Error::corrupted().with_operation("htree:readdir_probe")
        }
        HashTreeError::InvalidHashTree
        | HashTreeError::CorruptedHashTree
        | HashTreeError::BlockOutOfRange
        | HashTreeError::BufferTooSmall => {
            Ext4Error::corrupted().with_operation("htree:readdir_probe")
        }
    }
}
