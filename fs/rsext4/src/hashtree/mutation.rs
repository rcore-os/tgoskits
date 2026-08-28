//! HTree directory mutation planning and publication.

use alloc::{vec, vec::Vec};

use super::{HashTreeError, HashTreeManager};
use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::{
        update_ext4_dirblock_csum32, update_ext4_dx_checksum, verify_ext4_dirblock_checksum,
    },
    crc32c::ext4_superblock_has_metadata_csum,
    dir::FileName,
    disknode::Ext4Inode,
    endian::{DiskFormat, read_u16_le, read_u32_le, write_u16_le, write_u32_le},
    entries::{
        Ext4DirEntry2, Ext4DirEntryTail, Ext4DxEntry, Ext4DxRootInfo,
        decode_directory_record_length, encode_directory_record_length,
    },
    error::{Ext4Error, Ext4Result},
    ext4::Ext4FileSystem,
    extents_tree::ExtentTree,
    loopfile::resolve_inode_block,
    metadata::Ext4InodeMetadataUpdate,
    superblock::Ext4Superblock,
};

const DX_ENTRY_LEN: usize = 8;
const DX_ROOT_COUNTLIMIT_OFFSET: usize = 32;
const DX_ROOT_INDIRECT_LEVELS_OFFSET: usize = 30;
const DX_NODE_COUNTLIMIT_OFFSET: usize = 8;
const DX_TAIL_LEN: usize = 8;
const DX_BLOCK_MASK: u32 = 0x0fff_ffff;

#[derive(Clone)]
struct LeafEntry {
    hash: u32,
    inode: InodeNumber,
    file_type: u8,
    name: Vec<u8>,
    source_record_len: usize,
}

struct LeafSplitRequest<'a> {
    manager: &'a HashTreeManager,
    parent_ino: InodeNumber,
    parent_inode: &'a mut Ext4Inode,
    hash_version: u8,
    target_hash: u32,
    child_ino: InodeNumber,
    child_name: FileName<'a>,
    file_type: u8,
    path: super::lookup::HashTreePath,
    old_leaf_block: AbsoluteBN,
    old_leaf_data: Vec<u8>,
}

struct IndexSplitPlan {
    left: Vec<Ext4DxEntry>,
    right: Vec<Ext4DxEntry>,
    parent: Vec<Ext4DxEntry>,
}

impl LeafEntry {
    fn record_len(&self) -> usize {
        usize::from(Ext4DirEntry2::entry_len(self.name.len() as u8))
    }
}

/// Inserts into the leaf selected by the on-disk HTree, splitting it when full.
pub(crate) fn insert_indexed_directory_entry<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
    child_ino: InodeNumber,
    child_name: FileName<'_>,
    file_type: u8,
) -> Ext4Result<()> {
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let indexed_inode = *parent_inode;
    let (search, root) = manager
        .prepare_search(
            fs,
            device,
            parent_ino,
            &indexed_inode,
            child_name.as_bytes(),
        )
        .map_err(hash_tree_error)?;
    let path = manager
        .probe_path(fs, device, search, &root)
        .map_err(hash_tree_error)?;
    let logical_block = path.current_entry().map_err(hash_tree_error)?.block;
    let physical_block = resolve_inode_block(
        fs,
        device,
        parent_ino,
        &mut parent_inode.clone(),
        logical_block,
    )?
    .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_mapping"))?;

    let mut updated = fs
        .datablock_cache
        .get_or_load(device, physical_block)?
        .data
        .as_ref()
        .clone();
    if !verify_ext4_dirblock_checksum(
        &fs.superblock,
        parent_ino.raw(),
        parent_inode.i_generation,
        &updated,
    ) {
        return Err(Ext4Error::checksum().with_operation("htree:insert_leaf"));
    }
    if insert_into_leaf(
        &mut updated,
        ext4_superblock_has_metadata_csum(&fs.superblock),
        child_ino,
        child_name,
        file_type,
    )? {
        update_ext4_dirblock_csum32(
            &fs.superblock,
            parent_ino.raw(),
            parent_inode.i_generation,
            &mut updated,
        );

        fs.datablock_cache
            .modify_metadata(device, physical_block, |data| {
                data.copy_from_slice(&updated);
            })?;
        fs.datablock_cache.flush_metadata(device, physical_block)?;
        return fs.touch_parent_dir_for_entry_change(device, parent_ino);
    }

    split_leaf_and_insert(
        fs,
        device,
        LeafSplitRequest {
            manager: &manager,
            parent_ino,
            parent_inode,
            hash_version: search.hash_version,
            target_hash: search.target_hash,
            child_ino,
            child_name,
            file_type,
            path,
            old_leaf_block: physical_block,
            old_leaf_data: updated,
        },
    )
}

/// Converts a full one-block linear directory into a Linux HTree and inserts
/// the entry that triggered conversion.
pub(crate) fn make_indexed_directory<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
    child_ino: InodeNumber,
    child_name: FileName<'_>,
    file_type: u8,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    if fs.inode_size(parent_inode) != block_size as u64 || !parent_inode.is_dir() {
        return Err(Ext4Error::corrupted().with_operation("htree:make_indexed_geometry"));
    }
    let base_hash_version = fs.superblock.s_def_hash_version;
    if base_hash_version > Ext4DxRootInfo::DX_HASH_TEA {
        return Err(Ext4Error::unsupported().with_operation("htree:make_indexed_hash_version"));
    }
    let effective_hash_version =
        if fs.superblock.s_flags & Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH != 0 {
            base_hash_version + 3
        } else {
            base_hash_version
        };
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let root_physical = resolve_inode_block(fs, device, parent_ino, &mut parent_inode.clone(), 0)?
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:make_indexed_root_mapping"))?;
    let original = fs
        .datablock_cache
        .get_or_load(device, root_physical)?
        .data
        .as_ref()
        .clone();
    if !verify_ext4_dirblock_checksum(
        &fs.superblock,
        parent_ino.raw(),
        parent_inode.i_generation,
        &original,
    ) {
        return Err(Ext4Error::checksum().with_operation("htree:make_indexed_source"));
    }

    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let mut entries =
        parse_leaf_entries(&original, has_checksum, effective_hash_version, &manager)?;
    let dot = entries
        .first()
        .filter(|entry| entry.inode == parent_ino && entry.name == b".")
        .cloned()
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:make_indexed_dot"))?;
    let dotdot = entries
        .get(1)
        .filter(|entry| entry.name == b"..")
        .cloned()
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:make_indexed_dotdot"))?;
    entries.drain(..2);
    if entries.len() < 2 {
        return Err(Ext4Error::corrupted().with_operation("htree:make_indexed_entries"));
    }
    entries.sort_by_key(|entry| entry.hash);
    let split = linux_leaf_split_point(&entries, block_size)?;
    let hash2 = entries[split].hash;
    let continued = entries[split - 1].hash == hash2;
    let separator_hash = hash2
        .checked_add(u32::from(continued))
        .ok_or_else(Ext4Error::overflow)?;
    let (left_entries, right_entries) = entries.split_at(split);
    let mut left = pack_leaf(left_entries, block_size, has_checksum)?;
    let mut right = pack_leaf(right_entries, block_size, has_checksum)?;
    let child_hash = super::calculate_hash(
        child_name.as_bytes(),
        effective_hash_version,
        &manager.hash_seed,
    )
    .map_err(hash_tree_error)?
    .major;
    let target = if child_hash >= hash2 {
        &mut right
    } else {
        &mut left
    };
    if !insert_into_leaf(target, has_checksum, child_ino, child_name, file_type)? {
        return Err(Ext4Error::corrupted().with_operation("htree:make_indexed_balance"));
    }

    let mut updated_parent = *parent_inode;
    let (left_logical, left_physical) =
        append_directory_block(fs, device, parent_ino, &mut updated_parent)?;
    let (right_logical, right_physical) =
        append_directory_block(fs, device, parent_ino, &mut updated_parent)?;
    if left_logical != 1 || right_logical != 2 {
        return Err(Ext4Error::corrupted().with_operation("htree:make_indexed_mapping_order"));
    }
    let mut root = make_root_block(
        block_size,
        has_checksum,
        base_hash_version,
        &dot,
        &dotdot,
        &[
            Ext4DxEntry {
                hash: 0,
                block: left_logical,
            },
            Ext4DxEntry {
                hash: separator_hash,
                block: right_logical,
            },
        ],
    )?;
    updated_parent.i_flags |= Ext4Inode::EXT4_INDEX_FL;
    update_ext4_dirblock_csum32(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut left,
    );
    update_ext4_dirblock_csum32(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut right,
    );
    if !update_ext4_dx_checksum(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut root,
    ) {
        return Err(Ext4Error::corrupted().with_operation("htree:make_indexed_checksum"));
    }

    fs.datablock_cache
        .modify_new_metadata(device, left_physical, |data| {
            data.copy_from_slice(&left);
        })?;
    fs.datablock_cache
        .modify_new_metadata(device, right_physical, |data| {
            data.copy_from_slice(&right);
        })?;
    fs.datablock_cache
        .modify_metadata(device, root_physical, |data| {
            data.copy_from_slice(&root);
        })?;
    fs.datablock_cache.flush_metadata(device, left_physical)?;
    fs.datablock_cache.flush_metadata(device, right_physical)?;
    fs.datablock_cache.flush_metadata(device, root_physical)?;
    fs.finalize_inode_update(
        device,
        parent_ino,
        &mut updated_parent,
        Ext4InodeMetadataUpdate::parent_dir_change(),
    )?;
    *parent_inode = updated_parent;
    Ok(())
}

fn make_root_block(
    block_size: usize,
    has_checksum: bool,
    hash_version: u8,
    dot: &LeafEntry,
    dotdot: &LeafEntry,
    entries: &[Ext4DxEntry],
) -> Ext4Result<Vec<u8>> {
    let mut root = vec![0; block_size];
    write_entry(
        &mut root,
        0,
        12,
        block_size,
        dot.inode,
        FileName::new(b".").map_err(|_| Ext4Error::corrupted())?,
        dot.file_type,
    )?;
    write_entry(
        &mut root,
        12,
        block_size - 12,
        block_size,
        dotdot.inode,
        FileName::new(b"..").map_err(|_| Ext4Error::corrupted())?,
        dotdot.file_type,
    )?;
    root[28] = hash_version;
    root[29] = Ext4DxRootInfo::INFO_LENGTH;
    root[DX_ROOT_INDIRECT_LEVELS_OFFSET] = 0;
    root[31] = 0;
    encode_dx_entries(&mut root, DX_ROOT_COUNTLIMIT_OFFSET, has_checksum, entries)?;
    Ok(root)
}

fn split_leaf_and_insert<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: LeafSplitRequest<'_>,
) -> Ext4Result<()> {
    let LeafSplitRequest {
        manager,
        parent_ino,
        parent_inode,
        hash_version,
        target_hash,
        child_ino,
        child_name,
        file_type,
        path,
        old_leaf_block,
        old_leaf_data,
    } = request;
    let block_size = fs.block_size();
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let mut entries = parse_leaf_entries(&old_leaf_data, has_checksum, hash_version, manager)?;
    entries.sort_by_key(|entry| entry.hash);
    let split = linux_leaf_split_point(&entries, block_size)?;
    let hash2 = entries
        .get(split)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:split_hash"))?
        .hash;
    let continued = entries
        .get(split.wrapping_sub(1))
        .is_some_and(|entry| entry.hash == hash2);
    let separator_hash = hash2
        .checked_add(u32::from(continued))
        .ok_or_else(Ext4Error::overflow)?;
    let (left_entries, right_entries) = entries.split_at(split);
    let mut left = pack_leaf(left_entries, block_size, has_checksum)?;
    let mut right = pack_leaf(right_entries, block_size, has_checksum)?;
    let target = if target_hash >= hash2 {
        &mut right
    } else {
        &mut left
    };
    if !insert_into_leaf(target, has_checksum, child_ino, child_name, file_type)? {
        return Err(Ext4Error::corrupted().with_operation("htree:split_balance"));
    }

    let parent_frame = path
        .frames
        .last()
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:split_path"))?;
    let countlimit_offset = if parent_frame.source_block == 0 {
        DX_ROOT_COUNTLIMIT_OFFSET
    } else {
        DX_NODE_COUNTLIMIT_OFFSET
    };
    let limit = dx_limit(block_size, countlimit_offset, has_checksum)?;
    if parent_frame.entries.len() >= limit {
        grow_full_index_for_leaf_split(fs, device, parent_ino, parent_inode, &path, has_checksum)?;
        return insert_indexed_directory_entry(
            fs,
            device,
            parent_ino,
            parent_inode,
            child_ino,
            child_name,
            file_type,
        );
    }
    let insert_at = parent_frame
        .selected
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    if insert_at > parent_frame.entries.len() {
        return Err(Ext4Error::corrupted().with_operation("htree:split_parent_position"));
    }

    let parent_logical = parent_frame.source_block;
    let parent_physical = resolve_inode_block(
        fs,
        device,
        parent_ino,
        &mut parent_inode.clone(),
        parent_logical,
    )?
    .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:index_mapping"))?;
    let mut parent_data = fs
        .datablock_cache
        .get_or_load(device, parent_physical)?
        .data
        .as_ref()
        .clone();

    let mut updated_parent = *parent_inode;
    let (new_logical, new_leaf_block) =
        append_directory_block(fs, device, parent_ino, &mut updated_parent)?;
    if new_logical > DX_BLOCK_MASK {
        return Err(Ext4Error::overflow().with_operation("htree:logical_block"));
    }
    let mut parent_entries = parent_frame.entries.clone();
    parent_entries.insert(
        insert_at,
        Ext4DxEntry {
            hash: separator_hash,
            block: new_logical,
        },
    );
    encode_dx_entries(
        &mut parent_data,
        countlimit_offset,
        has_checksum,
        &parent_entries,
    )?;

    update_ext4_dirblock_csum32(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut left,
    );
    update_ext4_dirblock_csum32(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut right,
    );
    if !update_ext4_dx_checksum(
        &fs.superblock,
        parent_ino.raw(),
        updated_parent.i_generation,
        &mut parent_data,
    ) {
        return Err(Ext4Error::corrupted().with_operation("htree:index_checksum_layout"));
    }

    fs.datablock_cache
        .modify_metadata(device, old_leaf_block, |data| data.copy_from_slice(&left))?;
    fs.datablock_cache
        .modify_new_metadata(device, new_leaf_block, |data| {
            data.copy_from_slice(&right);
        })?;
    fs.datablock_cache
        .modify_metadata(device, parent_physical, |data| {
            data.copy_from_slice(&parent_data);
        })?;
    fs.datablock_cache.flush_metadata(device, old_leaf_block)?;
    fs.datablock_cache.flush_metadata(device, new_leaf_block)?;
    fs.datablock_cache.flush_metadata(device, parent_physical)?;
    fs.finalize_inode_update(
        device,
        parent_ino,
        &mut updated_parent,
        Ext4InodeMetadataUpdate::parent_dir_change(),
    )?;
    *parent_inode = updated_parent;
    Ok(())
}

fn grow_full_index_for_leaf_split<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
    path: &super::lookup::HashTreePath,
    has_checksum: bool,
) -> Ext4Result<()> {
    match index_growth_split_level(path, fs.block_size(), has_checksum)? {
        None => promote_full_root(
            fs,
            device,
            parent_ino,
            parent_inode,
            &path.frames[0],
            has_checksum,
        ),
        Some(level) => split_index_below_parent(
            fs,
            device,
            parent_ino,
            parent_inode,
            &path.frames[level - 1],
            &path.frames[level],
            has_checksum,
        ),
    }
}

fn index_growth_split_level(
    path: &super::lookup::HashTreePath,
    block_size: usize,
    has_checksum: bool,
) -> Ext4Result<Option<usize>> {
    if path.frames.is_empty() {
        return Err(Ext4Error::corrupted().with_operation("htree:index_growth_path"));
    }
    for child_level in (1..path.frames.len()).rev() {
        let parent = &path.frames[child_level - 1];
        let countlimit_offset = if parent.source_block == 0 {
            DX_ROOT_COUNTLIMIT_OFFSET
        } else {
            DX_NODE_COUNTLIMIT_OFFSET
        };
        let limit = dx_limit(block_size, countlimit_offset, has_checksum)?;
        if parent.entries.len() < limit {
            return Ok(Some(child_level));
        }
    }
    Ok(None)
}

fn promote_full_root<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
    root_frame: &super::lookup::HashTreeFrame,
    has_checksum: bool,
) -> Ext4Result<()> {
    if root_frame.source_block != 0 {
        return Err(Ext4Error::corrupted().with_operation("htree:root_source"));
    }
    let root_physical = resolve_inode_block(fs, device, parent_ino, &mut parent_inode.clone(), 0)?
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:root_mapping"))?;
    let mut root_data = fs
        .datablock_cache
        .get_or_load(device, root_physical)?
        .data
        .as_ref()
        .clone();
    let current_levels = *root_data
        .get(DX_ROOT_INDIRECT_LEVELS_OFFSET)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:root_info"))?;
    let max_levels = if fs
        .superblock
        .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR)
    {
        2
    } else {
        1
    };
    if current_levels >= max_levels {
        return Err(Ext4Error::no_space().with_operation("htree:max_depth"));
    }

    let mut updated_parent = *parent_inode;
    let (new_logical, new_index_block) =
        append_directory_block(fs, device, parent_ino, &mut updated_parent)?;
    if new_logical > DX_BLOCK_MASK {
        return Err(Ext4Error::overflow().with_operation("htree:index_logical_block"));
    }
    let mut index_data = new_internal_node(fs.block_size())?;
    encode_dx_entries(
        &mut index_data,
        DX_NODE_COUNTLIMIT_OFFSET,
        has_checksum,
        &root_frame.entries,
    )?;
    encode_dx_entries(
        &mut root_data,
        DX_ROOT_COUNTLIMIT_OFFSET,
        has_checksum,
        &[Ext4DxEntry {
            hash: 0,
            block: new_logical,
        }],
    )?;
    root_data[DX_ROOT_INDIRECT_LEVELS_OFFSET] = current_levels + 1;
    update_index_checksums(
        fs,
        parent_ino,
        updated_parent.i_generation,
        &mut root_data,
        &mut index_data,
    )?;

    fs.datablock_cache
        .modify_new_metadata(device, new_index_block, |data| {
            data.copy_from_slice(&index_data);
        })?;
    fs.datablock_cache
        .modify_metadata(device, root_physical, |data| {
            data.copy_from_slice(&root_data);
        })?;
    fs.datablock_cache.flush_metadata(device, new_index_block)?;
    fs.datablock_cache.flush_metadata(device, root_physical)?;
    *parent_inode = updated_parent;
    Ok(())
}

fn split_index_below_parent<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
    parent_frame: &super::lookup::HashTreeFrame,
    index_frame: &super::lookup::HashTreeFrame,
    has_checksum: bool,
) -> Ext4Result<()> {
    let parent_countlimit_offset = if parent_frame.source_block == 0 {
        DX_ROOT_COUNTLIMIT_OFFSET
    } else {
        DX_NODE_COUNTLIMIT_OFFSET
    };
    let parent_limit = dx_limit(fs.block_size(), parent_countlimit_offset, has_checksum)?;
    if parent_frame.entries.len() >= parent_limit {
        return Err(Ext4Error::no_space().with_operation("htree:parent_split_required"));
    }
    if parent_frame
        .entries
        .get(parent_frame.selected)
        .is_none_or(|entry| entry.block != index_frame.source_block)
        || index_frame.source_block == 0
        || index_frame.entries.len() < 2
    {
        return Err(Ext4Error::corrupted().with_operation("htree:index_split_count"));
    }

    let current_physical = resolve_inode_block(
        fs,
        device,
        parent_ino,
        &mut parent_inode.clone(),
        index_frame.source_block,
    )?
    .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:index_mapping"))?;
    let parent_physical = resolve_inode_block(
        fs,
        device,
        parent_ino,
        &mut parent_inode.clone(),
        parent_frame.source_block,
    )?
    .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:parent_mapping"))?;
    let mut current_data = fs
        .datablock_cache
        .get_or_load(device, current_physical)?
        .data
        .as_ref()
        .clone();
    let mut parent_data = fs
        .datablock_cache
        .get_or_load(device, parent_physical)?
        .data
        .as_ref()
        .clone();

    let mut updated_parent = *parent_inode;
    let (new_logical, new_index_block) =
        append_directory_block(fs, device, parent_ino, &mut updated_parent)?;
    if new_logical > DX_BLOCK_MASK {
        return Err(Ext4Error::overflow().with_operation("htree:index_logical_block"));
    }
    let plan = plan_index_split(parent_frame, index_frame, new_logical)?;
    let mut new_data = new_internal_node(fs.block_size())?;
    encode_dx_entries(
        &mut current_data,
        DX_NODE_COUNTLIMIT_OFFSET,
        has_checksum,
        &plan.left,
    )?;
    encode_dx_entries(
        &mut new_data,
        DX_NODE_COUNTLIMIT_OFFSET,
        has_checksum,
        &plan.right,
    )?;
    encode_dx_entries(
        &mut parent_data,
        parent_countlimit_offset,
        has_checksum,
        &plan.parent,
    )?;
    for block in [&mut current_data, &mut new_data, &mut parent_data] {
        if !update_ext4_dx_checksum(
            &fs.superblock,
            parent_ino.raw(),
            updated_parent.i_generation,
            block,
        ) {
            return Err(Ext4Error::corrupted().with_operation("htree:index_checksum_layout"));
        }
    }

    fs.datablock_cache
        .modify_metadata(device, current_physical, |data| {
            data.copy_from_slice(&current_data);
        })?;
    fs.datablock_cache
        .modify_new_metadata(device, new_index_block, |data| {
            data.copy_from_slice(&new_data);
        })?;
    fs.datablock_cache
        .modify_metadata(device, parent_physical, |data| {
            data.copy_from_slice(&parent_data);
        })?;
    fs.datablock_cache
        .flush_metadata(device, current_physical)?;
    fs.datablock_cache.flush_metadata(device, new_index_block)?;
    fs.datablock_cache.flush_metadata(device, parent_physical)?;
    *parent_inode = updated_parent;
    Ok(())
}

fn plan_index_split(
    parent_frame: &super::lookup::HashTreeFrame,
    index_frame: &super::lookup::HashTreeFrame,
    new_logical: u32,
) -> Ext4Result<IndexSplitPlan> {
    if index_frame.entries.len() < 2 {
        return Err(Ext4Error::corrupted().with_operation("htree:index_split_count"));
    }
    let split = index_frame.entries.len() / 2;
    let separator_hash = index_frame.entries[split].hash;
    let left = index_frame.entries[..split].to_vec();
    let mut right = index_frame.entries[split..].to_vec();
    // dx_set_count()/dx_set_limit() overwrite the first entry's hash slot on
    // disk; the parent separator retains the original boundary hash.
    right[0].hash = 0;
    let mut parent = parent_frame.entries.clone();
    parent.insert(
        parent_frame
            .selected
            .checked_add(1)
            .ok_or_else(Ext4Error::overflow)?,
        Ext4DxEntry {
            hash: separator_hash,
            block: new_logical,
        },
    );
    Ok(IndexSplitPlan {
        left,
        right,
        parent,
    })
}

fn new_internal_node(block_size: usize) -> Ext4Result<Vec<u8>> {
    let mut data = vec![0; block_size];
    let encoded = encode_directory_record_length(block_size, block_size)
        .ok_or_else(|| Ext4Error::overflow().with_operation("htree:index_record_len"))?;
    write_u16_le(encoded, &mut data[4..6]);
    Ok(data)
}

fn update_index_checksums(
    fs: &Ext4FileSystem,
    parent_ino: InodeNumber,
    generation: u32,
    first: &mut [u8],
    second: &mut [u8],
) -> Ext4Result<()> {
    if update_ext4_dx_checksum(&fs.superblock, parent_ino.raw(), generation, first)
        && update_ext4_dx_checksum(&fs.superblock, parent_ino.raw(), generation, second)
    {
        Ok(())
    } else {
        Err(Ext4Error::corrupted().with_operation("htree:index_checksum_layout"))
    }
}

fn insert_into_leaf(
    data: &mut [u8],
    has_checksum: bool,
    child_ino: InodeNumber,
    child_name: FileName<'_>,
    file_type: u8,
) -> Ext4Result<bool> {
    let block_size = data.len();
    let entries_end = if has_checksum {
        block_size
            .checked_sub(usize::from(Ext4DirEntryTail::TAIL_LEN))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_size"))?
    } else {
        block_size
    };
    let new_len = usize::from(Ext4DirEntry2::entry_len(
        u8::try_from(child_name.as_bytes().len()).map_err(|_| Ext4Error::overflow())?,
    ));
    let mut offset = 0usize;
    while offset < entries_end {
        let header = data
            .get(offset..offset + 8)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_header"))?;
        let inode = read_u32_le(&header[..4]);
        let record_len = decode_directory_record_length(read_u16_le(&header[4..6]), block_size);
        let name_len = usize::from(header[6]);
        let record_end = offset
            .checked_add(record_len)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_record_overflow"))?;
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || record_end > entries_end
            || name_len > record_len - 8
        {
            return Err(Ext4Error::corrupted().with_operation("htree:leaf_record"));
        }

        if inode == 0 && record_len >= new_len {
            write_entry(
                data, offset, record_len, block_size, child_ino, child_name, file_type,
            )?;
            return Ok(true);
        }

        if inode != 0 {
            let ideal_len = usize::from(Ext4DirEntry2::entry_len(
                u8::try_from(name_len).map_err(|_| Ext4Error::corrupted())?,
            ));
            if ideal_len > record_len {
                return Err(Ext4Error::corrupted().with_operation("htree:leaf_name_length"));
            }
            let free_len = record_len - ideal_len;
            if free_len >= new_len {
                let encoded = encode_directory_record_length(ideal_len, block_size)
                    .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_rec_len"))?;
                write_u16_le(encoded, &mut data[offset + 4..offset + 6]);
                write_entry(
                    data,
                    offset + ideal_len,
                    free_len,
                    block_size,
                    child_ino,
                    child_name,
                    file_type,
                )?;
                return Ok(true);
            }
        }
        offset = record_end;
    }

    if offset != entries_end {
        return Err(Ext4Error::corrupted().with_operation("htree:leaf_coverage"));
    }
    Ok(false)
}

fn parse_leaf_entries(
    data: &[u8],
    has_checksum: bool,
    hash_version: u8,
    manager: &HashTreeManager,
) -> Ext4Result<Vec<LeafEntry>> {
    let block_size = data.len();
    let entries_end = leaf_entries_end(block_size, has_checksum)?;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < entries_end {
        let header = data
            .get(offset..offset + 8)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:split_leaf_header"))?;
        let inode_raw = read_u32_le(&header[..4]);
        let record_len = decode_directory_record_length(read_u16_le(&header[4..6]), block_size);
        let name_len = usize::from(header[6]);
        let record_end = offset.checked_add(record_len).ok_or_else(|| {
            Ext4Error::corrupted().with_operation("htree:split_leaf_record_overflow")
        })?;
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || record_end > entries_end
            || name_len > record_len - 8
        {
            return Err(Ext4Error::corrupted().with_operation("htree:split_leaf_record"));
        }
        if inode_raw != 0 {
            if name_len == 0 {
                return Err(Ext4Error::corrupted().with_operation("htree:split_leaf_empty_name"));
            }
            let name = data[offset + 8..offset + 8 + name_len].to_vec();
            let inode = InodeNumber::new(inode_raw)
                .map_err(|_| Ext4Error::corrupted().with_operation("htree:split_leaf_inode"))?;
            let hash = super::calculate_hash(&name, hash_version, &manager.hash_seed)
                .map_err(hash_tree_error)?
                .major;
            entries.push(LeafEntry {
                hash,
                inode,
                file_type: header[7],
                name,
                source_record_len: record_len,
            });
        }
        offset = record_end;
    }
    if offset != entries_end || entries.len() < 2 {
        return Err(Ext4Error::corrupted().with_operation("htree:split_leaf_coverage"));
    }
    Ok(entries)
}

fn linux_leaf_split_point(entries: &[LeafEntry], block_size: usize) -> Ext4Result<usize> {
    if entries.len() < 2 {
        return Err(Ext4Error::corrupted().with_operation("htree:split_entry_count"));
    }
    let half = block_size / 2;
    let mut moved_size = 0usize;
    let mut move_count = 0usize;
    for entry in entries.iter().rev() {
        let entry_len = entry.source_record_len;
        if moved_size
            .checked_add(entry_len / 2)
            .ok_or_else(Ext4Error::overflow)?
            > half
        {
            break;
        }
        moved_size = moved_size
            .checked_add(entry_len)
            .ok_or_else(Ext4Error::overflow)?;
        move_count += 1;
    }
    let split = if move_count == entries.len() {
        entries.len() / 2
    } else {
        entries.len() - move_count
    };
    if split == 0 || split >= entries.len() {
        return Err(Ext4Error::corrupted().with_operation("htree:split_point"));
    }
    Ok(split)
}

fn pack_leaf(entries: &[LeafEntry], block_size: usize, has_checksum: bool) -> Ext4Result<Vec<u8>> {
    if entries.is_empty() {
        return Err(Ext4Error::corrupted().with_operation("htree:pack_empty_leaf"));
    }
    let entries_end = leaf_entries_end(block_size, has_checksum)?;
    let required = entries.iter().try_fold(0usize, |total, entry| {
        total
            .checked_add(entry.record_len())
            .ok_or_else(Ext4Error::overflow)
    })?;
    if required > entries_end {
        return Err(Ext4Error::no_space().with_operation("htree:pack_leaf"));
    }

    let mut data = vec![0; block_size];
    let mut offset = 0usize;
    for (index, entry) in entries.iter().enumerate() {
        let record_len = if index + 1 == entries.len() {
            entries_end - offset
        } else {
            entry.record_len()
        };
        let name = FileName::new(&entry.name)
            .map_err(|_| Ext4Error::corrupted().with_operation("htree:pack_name"))?;
        write_entry(
            &mut data,
            offset,
            record_len,
            block_size,
            entry.inode,
            name,
            entry.file_type,
        )?;
        offset = offset
            .checked_add(record_len)
            .ok_or_else(Ext4Error::overflow)?;
    }
    if has_checksum {
        Ext4DirEntryTail::new().to_disk_bytes(&mut data[entries_end..]);
    }
    Ok(data)
}

fn leaf_entries_end(block_size: usize, has_checksum: bool) -> Ext4Result<usize> {
    block_size
        .checked_sub(if has_checksum {
            usize::from(Ext4DirEntryTail::TAIL_LEN)
        } else {
            0
        })
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:leaf_size"))
}

fn dx_limit(block_size: usize, countlimit_offset: usize, has_checksum: bool) -> Ext4Result<usize> {
    block_size
        .checked_sub(countlimit_offset)
        .and_then(|bytes| bytes.checked_sub(if has_checksum { DX_TAIL_LEN } else { 0 }))
        .map(|bytes| bytes / DX_ENTRY_LEN)
        .filter(|limit| *limit > 0)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:index_limit"))
}

fn encode_dx_entries(
    data: &mut [u8],
    countlimit_offset: usize,
    has_checksum: bool,
    entries: &[Ext4DxEntry],
) -> Ext4Result<()> {
    let limit = dx_limit(data.len(), countlimit_offset, has_checksum)?;
    if entries.is_empty()
        || entries.len() > limit
        || entries[0].block == 0
        || entries[0].block > DX_BLOCK_MASK
    {
        return Err(Ext4Error::corrupted().with_operation("htree:index_entries"));
    }
    let count = u16::try_from(entries.len()).map_err(|_| Ext4Error::overflow())?;
    let limit = u16::try_from(limit).map_err(|_| Ext4Error::overflow())?;
    write_u16_le(limit, &mut data[countlimit_offset..countlimit_offset + 2]);
    write_u16_le(
        count,
        &mut data[countlimit_offset + 2..countlimit_offset + 4],
    );
    write_u32_le(
        entries[0].block & DX_BLOCK_MASK,
        &mut data[countlimit_offset + 4..countlimit_offset + 8],
    );
    let mut previous_hash = 0u32;
    for (index, entry) in entries.iter().enumerate().skip(1) {
        if entry.block == 0 || entry.block > DX_BLOCK_MASK || entry.hash < previous_hash {
            return Err(Ext4Error::corrupted().with_operation("htree:index_order"));
        }
        let offset = countlimit_offset + index * DX_ENTRY_LEN;
        write_u32_le(entry.hash, &mut data[offset..offset + 4]);
        write_u32_le(entry.block, &mut data[offset + 4..offset + 8]);
        previous_hash = entry.hash;
    }
    Ok(())
}

fn append_directory_block<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
) -> Ext4Result<(u32, AbsoluteBN)> {
    let block_size = fs.block_size();
    let total_size =
        usize::try_from(fs.inode_size(parent_inode)).map_err(|_| Ext4Error::file_too_large())?;
    let old_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_size)
    };
    if (!fs.superblock.has_extents() || !parent_inode.uses_extents()) && old_blocks >= 12 {
        return Err(Ext4Error::unsupported().with_operation("htree:legacy_directory_growth"));
    }
    let new_logical = u32::try_from(old_blocks).map_err(|_| Ext4Error::overflow())?;
    let new_size = total_size
        .checked_add(block_size)
        .ok_or_else(Ext4Error::overflow)?;
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let blocks_count = parent_inode.blocks_count(block_size as u32, huge_file_feature);
    let updated_blocks = blocks_count
        .checked_add(block_size as u64 / 512)
        .ok_or_else(Ext4Error::overflow)?;
    let mut accounting_check = *parent_inode;
    accounting_check.set_blocks_count(updated_blocks, block_size as u32, huge_file_feature)?;

    let new_block = fs.alloc_block(device)?;
    if fs.superblock.has_extents() && parent_inode.uses_extents() {
        let extent = crate::disknode::Ext4Extent::new(new_logical, new_block.raw(), 1);
        ExtentTree::with_filesystem(parent_inode, fs, parent_ino)
            .insert_extent(fs, extent, device)?;
    } else {
        parent_inode.i_block[old_blocks] = new_block.to_u32()?;
    }
    parent_inode.set_size(new_size as u64);
    parent_inode.set_blocks_count(updated_blocks, block_size as u32, huge_file_feature)?;
    Ok((new_logical, new_block))
}

fn write_entry(
    data: &mut [u8],
    offset: usize,
    record_len: usize,
    block_size: usize,
    child_ino: InodeNumber,
    child_name: FileName<'_>,
    file_type: u8,
) -> Ext4Result<()> {
    let name = child_name.as_bytes();
    let encoded = encode_directory_record_length(record_len, block_size)
        .ok_or_else(|| Ext4Error::overflow().with_operation("htree:encode_rec_len"))?;
    let record = data
        .get_mut(offset..offset + record_len)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("htree:write_record"))?;
    write_u32_le(child_ino.raw(), &mut record[..4]);
    write_u16_le(encoded, &mut record[4..6]);
    record[6] = u8::try_from(name.len()).map_err(|_| Ext4Error::overflow())?;
    record[7] = file_type;
    record[8..8 + name.len()].copy_from_slice(name);
    record[8 + name.len()..].fill(0);
    Ok(())
}

fn hash_tree_error(error: HashTreeError) -> Ext4Error {
    error.into_ext4("htree:probe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_entry(hash: u32, source_record_len: usize) -> LeafEntry {
        LeafEntry {
            hash,
            inode: InodeNumber::new(hash + 1).expect("non-zero test inode"),
            file_type: Ext4DirEntry2::EXT4_FT_REG_FILE,
            name: vec![b'x'],
            source_record_len,
        }
    }

    #[test]
    fn leaf_split_uses_source_record_lengths_before_packing() {
        let entries = [
            split_entry(2, 12),
            split_entry(4, 12),
            split_entry(6, 12),
            split_entry(8, 3000),
        ];

        assert_eq!(linux_leaf_split_point(&entries, 4096).unwrap(), 3);
    }

    fn index_frame(source_block: u32, entry_count: usize) -> super::super::lookup::HashTreeFrame {
        super::super::lookup::HashTreeFrame {
            source_block,
            entries: (0..entry_count)
                .map(|index| Ext4DxEntry {
                    hash: index as u32 * 2,
                    block: index as u32 + 1,
                })
                .collect(),
            selected: 0,
        }
    }

    #[test]
    fn three_level_growth_splits_below_the_nearest_parent_with_room() {
        let path = super::super::lookup::HashTreePath {
            frames: vec![
                index_frame(0, 1),
                index_frame(1, 2),
                index_frame(2, dx_limit(4096, DX_NODE_COUNTLIMIT_OFFSET, true).unwrap()),
            ],
        };

        assert_eq!(
            index_growth_split_level(&path, 4096, true).unwrap(),
            Some(2)
        );
    }

    #[test]
    fn growth_walks_up_to_the_first_parent_with_room() {
        let internal_limit = dx_limit(4096, DX_NODE_COUNTLIMIT_OFFSET, true).unwrap();
        let path = super::super::lookup::HashTreePath {
            frames: vec![
                index_frame(0, 2),
                index_frame(1, internal_limit),
                index_frame(2, internal_limit),
            ],
        };

        assert_eq!(
            index_growth_split_level(&path, 4096, true).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn growth_promotes_the_root_when_every_parent_is_full() {
        let root_limit = dx_limit(4096, DX_ROOT_COUNTLIMIT_OFFSET, true).unwrap();
        let internal_limit = dx_limit(4096, DX_NODE_COUNTLIMIT_OFFSET, true).unwrap();
        let path = super::super::lookup::HashTreePath {
            frames: vec![index_frame(0, root_limit), index_frame(1, internal_limit)],
        };

        assert_eq!(index_growth_split_level(&path, 4096, true).unwrap(), None);
    }

    #[test]
    fn internal_split_keeps_boundary_hash_only_in_the_parent() {
        let parent = super::super::lookup::HashTreeFrame {
            source_block: 4,
            entries: vec![
                Ext4DxEntry { hash: 0, block: 9 },
                Ext4DxEntry {
                    hash: 0x8000_0000,
                    block: 10,
                },
            ],
            selected: 1,
        };
        let index = super::super::lookup::HashTreeFrame {
            source_block: 10,
            entries: vec![
                Ext4DxEntry { hash: 0, block: 20 },
                Ext4DxEntry {
                    hash: 0x8100_0000,
                    block: 21,
                },
                Ext4DxEntry {
                    hash: 0x8200_0000,
                    block: 22,
                },
                Ext4DxEntry {
                    hash: 0x8300_0000,
                    block: 23,
                },
            ],
            selected: 3,
        };

        let plan = plan_index_split(&parent, &index, 30).unwrap();
        assert_eq!(plan.left.len(), 2);
        assert_eq!(plan.right[0].hash, 0);
        assert_eq!(plan.right[0].block, 22);
        assert_eq!(plan.parent[2].hash, 0x8200_0000);
        assert_eq!(plan.parent[2].block, 30);
    }
}
