//! Hash-ordered enumeration of indexed directory leaves.

use alloc::{collections::BTreeSet, vec::Vec};

use super::{HashTreeError, HashTreeManager, HashTreeNode, calculate_hash, lookup::HashSearch};
use crate::{
    Ext4Error, Ext4Result,
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::InodeNumber,
    dir::FileName,
    disknode::Ext4Inode,
    entries::decode_directory_record_length,
    ext4::Ext4FileSystem,
};

const DIRENT_HEADER_LEN: usize = 8;

/// One active record extracted from an indexed directory leaf.
#[derive(Debug)]
pub(crate) struct IndexedDirectoryRecord {
    pub(crate) inode: u32,
    pub(crate) file_type: u8,
    pub(crate) name: Vec<u8>,
    pub(crate) major: u32,
    pub(crate) minor: u32,
    pub(crate) collision: u32,
}

/// One Linux-style HTree hash range cached by an open directory reader.
#[derive(Debug)]
pub(crate) struct IndexedDirectoryRange {
    pub(crate) start: (u32, u32, u32),
    pub(crate) records: Vec<IndexedDirectoryRecord>,
    pub(crate) next_start: Option<(u32, u32, u32)>,
}

/// Reads one HTree hash range from the leaf selected by `start`.
///
/// Like Linux `ext4_htree_fill_tree()`, this collects and sorts only the
/// current index range plus all low-bit collision-continuation leaves. It does
/// not scan or sort the complete directory.
pub(crate) fn read_indexed_directory_range<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    directory: InodeNumber,
    inode: &Ext4Inode,
    start: (u32, u32, u32),
) -> Ext4Result<IndexedDirectoryRange> {
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let (mut search, root) = manager
        .prepare_search(fs, block_dev, directory, inode, b"")
        .map_err(hash_tree_error)?;
    if !matches!(root, HashTreeNode::Root { .. }) {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_root"));
    }
    search.target_hash = start.0;
    let mut path = manager
        .probe_path(fs, block_dev, search, &root)
        .map_err(hash_tree_error)?;

    let root_block = manager
        .get_root_block(fs, block_dev, directory, inode)
        .map_err(hash_tree_error)?;
    let root_data = manager
        .read_block_data(fs, block_dev, root_block)
        .map_err(hash_tree_error)?;
    let specials = [
        parse_root_record(&root_data, 0, 0)?,
        parse_root_record(&root_data, 12, 2)?,
    ];
    let mut seen_leaves = BTreeSet::new();
    let mut range = Vec::new();
    range.extend(specials);
    read_current_leaf_records(
        fs,
        block_dev,
        &manager,
        search,
        &path,
        &mut seen_leaves,
        &mut range,
    )?;

    let next_start = loop {
        match manager
            .advance_path(fs, block_dev, search, &mut path)
            .map_err(hash_tree_error)?
        {
            Some(boundary_hash) if boundary_hash & 1 != 0 => {
                read_current_leaf_records(
                    fs,
                    block_dev,
                    &manager,
                    search,
                    &path,
                    &mut seen_leaves,
                    &mut range,
                )?;
            }
            Some(boundary_hash) => break Some((boundary_hash, 0, 0)),
            None => break None,
        }
    };

    Ok(IndexedDirectoryRange {
        start,
        records: prepare_range(range, start)?,
        next_start,
    })
}

fn read_current_leaf_records<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    manager: &HashTreeManager,
    search: HashSearch<'_>,
    path: &super::lookup::HashTreePath,
    seen_leaves: &mut BTreeSet<u32>,
    output: &mut Vec<IndexedDirectoryRecord>,
) -> Ext4Result<()> {
    let logical_block = path.current_entry().map_err(hash_tree_error)?.block;
    if !seen_leaves.insert(logical_block) {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_cycle"));
    }
    let (_, data) = manager
        .read_current_leaf_data(fs, block_dev, search, path)
        .map_err(hash_tree_error)?;
    parse_leaf_records(&data, search, manager, output)
}

fn prepare_range(
    mut range: Vec<IndexedDirectoryRecord>,
    start: (u32, u32, u32),
) -> Ext4Result<Vec<IndexedDirectoryRecord>> {
    range.sort_by_key(|record| (record.major, record.minor));
    let mut previous_hash = None;
    let mut collision = 0_u32;
    let range_len = range.len();
    let mut first_visible = range_len;
    for (index, record) in range.iter_mut().enumerate() {
        let hash = (record.major, record.minor);
        if previous_hash == Some(hash) {
            collision = collision.checked_add(1).ok_or_else(Ext4Error::overflow)?;
        } else {
            previous_hash = Some(hash);
            collision = 0;
        }
        record.collision = collision;
        if first_visible == range_len && (record.major, record.minor, collision) >= start {
            first_visible = index;
        }
    }
    range.drain(..first_visible);
    Ok(range)
}

fn parse_root_record(data: &[u8], offset: usize, major: u32) -> Ext4Result<IndexedDirectoryRecord> {
    let (inode, file_type, name, _) = parse_record(data, offset)?;
    if inode == 0 {
        return Err(Ext4Error::corrupted().with_operation("htree:readdir_dot"));
    }
    Ok(IndexedDirectoryRecord {
        inode,
        file_type,
        name,
        major,
        minor: 0,
        collision: 0,
    })
}

fn parse_leaf_records(
    data: &[u8],
    search: HashSearch<'_>,
    manager: &HashTreeManager,
    output: &mut Vec<IndexedDirectoryRecord>,
) -> Ext4Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let (inode, file_type, name, next_offset) = parse_record(data, offset)?;
        if inode != 0 {
            FileName::new(&name)
                .map_err(|_| Ext4Error::corrupted().with_operation("htree:readdir_name"))?;
            let hash = calculate_hash(&name, search.hash_version, &manager.hash_seed)
                .map_err(hash_tree_error)?;
            output.push(IndexedDirectoryRecord {
                inode,
                file_type,
                name,
                major: hash.major,
                minor: hash.minor,
                collision: 0,
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
        // A checked HTree walk must always contain a current entry. Unlike a
        // pathname lookup miss, reaching this state is malformed index data.
        HashTreeError::EntryNotFound => {
            Ext4Error::corrupted().with_operation("htree:readdir_probe")
        }
        error => error.into_ext4("htree:readdir_probe"),
    }
}
