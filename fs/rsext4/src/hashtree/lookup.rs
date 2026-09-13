//! Hash tree lookup flow and fallback logic.

#![forbid(unsafe_code)]

use alloc::{sync::Arc, vec, vec::Vec};

use super::{
    Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, HashTreeNode, HashTreeSearchResult,
};
use crate::{
    bmalloc::{AbsoluteBN, InodeNumber},
    dir::DirectoryBlockRead,
    entries::{DirEntryIterator, Ext4DirEntryTail, Ext4DxEntry, Ext4DxRootInfo, classic_dir},
    superblock::Ext4Superblock,
};

#[derive(Clone, Copy)]
pub(super) struct HashSearch<'a> {
    pub(super) target_hash: u32,
    pub(super) target_name: &'a [u8],
    pub(super) hash_version: u8,
    pub(super) indirect_levels: u8,
}

pub(super) struct HashTreeFrame {
    pub(super) source_block: u32,
    pub(super) entries: Vec<Ext4DxEntry>,
    pub(super) selected: usize,
}

pub(super) struct HashTreePath {
    pub(super) frames: Vec<HashTreeFrame>,
}

pub(crate) fn lookup<R: DirectoryBlockRead>(
    manager: &HashTreeManager,
    reader: &mut R,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    if !reader.inode().is_htree_indexed() {
        return manager.fallback_to_linear_search(reader, target_name);
    }

    let indexed_result = manager
        .prepare_search(reader, target_name)
        .and_then(|(search, root)| manager.search_collision_chain(reader, search, &root));

    match indexed_result {
        Ok(result) => Ok(result),
        Err(error) if error.allows_linear_fallback() => {
            manager.fallback_to_linear_search(reader, target_name)
        }
        Err(error) => Err(error),
    }
}

impl HashTreeManager {
    pub(super) fn prepare_search<'a, R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        target_name: &'a [u8],
    ) -> Result<(HashSearch<'a>, HashTreeNode), HashTreeError> {
        let root_block = self.get_root_block(reader)?;
        let root_data = reader.read_block(root_block)?;
        if crate::checksum::verify_ext4_dx_checksum(
            reader.superblock(),
            reader.directory().raw(),
            reader.inode().i_generation,
            &root_data,
        ) == Some(false)
        {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:root"),
            ));
        }
        let has_metadata_checksum =
            crate::crc32c::ext4_superblock_has_metadata_csum(reader.superblock());
        let max_indirect_levels = if reader
            .superblock()
            .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR)
        {
            2
        } else {
            1
        };
        let root_info =
            self.parse_root_node(&root_data, has_metadata_checksum, max_indirect_levels)?;
        let (root_hash_version, indirect_levels) = match &root_info {
            HashTreeNode::Root {
                hash_version,
                indirect_levels,
                ..
            } => (*hash_version, *indirect_levels),
            _ => return Err(HashTreeError::InvalidHashTree),
        };
        let hash_version = if root_hash_version <= Ext4DxRootInfo::DX_HASH_TEA
            && reader.superblock().s_flags & Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH != 0
        {
            root_hash_version + 3
        } else {
            root_hash_version
        };
        let target_hash = super::calculate_hash(target_name, hash_version, &self.hash_seed)?.major;
        let search = HashSearch {
            target_hash,
            target_name,
            hash_version,
            indirect_levels,
        };
        Ok((search, root_info))
    }
    pub(super) fn get_root_block<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
    ) -> Result<AbsoluteBN, HashTreeError> {
        match reader.map_block(0) {
            Ok(Some(block)) => Ok(block),
            Ok(None) => Err(HashTreeError::InvalidHashTree),
            Err(error) => Err(error.into()),
        }
    }

    fn search_collision_chain<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        search: HashSearch<'_>,
        root: &HashTreeNode,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let mut path = self.probe_path(reader, search, root)?;
        loop {
            match self.search_current_leaf(reader, search, &path) {
                Ok(result) => return Ok(result),
                Err(HashTreeError::EntryNotFound) => {
                    if !self.advance_collision_path(reader, search, &mut path)? {
                        return Err(HashTreeError::EntryNotFound);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn probe_path<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        search: HashSearch<'_>,
        root: &HashTreeNode,
    ) -> Result<HashTreePath, HashTreeError> {
        let HashTreeNode::Root { entries, .. } = root else {
            return Err(HashTreeError::InvalidHashTree);
        };
        let selected = select_entry(entries, search.target_hash)?;
        let mut path = HashTreePath {
            frames: vec![HashTreeFrame {
                source_block: 0,
                entries: entries.clone(),
                selected,
            }],
        };

        for _ in 0..search.indirect_levels {
            let logical_block = path.current_entry()?.block;
            let entries = self.read_internal_entries(reader, &path, logical_block)?;
            let selected = select_entry(&entries, search.target_hash)?;
            path.frames.push(HashTreeFrame {
                source_block: logical_block,
                entries,
                selected,
            });
        }

        Ok(path)
    }

    fn search_current_leaf<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        search: HashSearch<'_>,
        path: &HashTreePath,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let (block_num, block_data) = self.read_current_leaf_data(reader, path)?;
        self.search_in_leaf_data(&block_data, search.target_name, block_num)
    }

    pub(super) fn read_current_leaf_data<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        path: &HashTreePath,
    ) -> Result<(AbsoluteBN, Arc<Vec<u8>>), HashTreeError> {
        let logical_block = path.current_entry()?.block;
        if path
            .frames
            .iter()
            .any(|frame| frame.source_block == logical_block)
        {
            return Err(HashTreeError::BlockOutOfRange);
        }
        let block_num = resolve_logical_block(reader, logical_block)?;
        let block_data = reader.read_block(block_num)?;
        if !crate::checksum::verify_ext4_dirblock_checksum(
            reader.superblock(),
            reader.directory().raw(),
            reader.inode().i_generation,
            &block_data,
        ) {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:leaf"),
            ));
        }
        Ok((block_num, block_data))
    }

    fn read_internal_entries<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        path: &HashTreePath,
        logical_block: u32,
    ) -> Result<Vec<Ext4DxEntry>, HashTreeError> {
        if path
            .frames
            .iter()
            .any(|frame| frame.source_block == logical_block)
        {
            return Err(HashTreeError::BlockOutOfRange);
        }
        let block_num = resolve_logical_block(reader, logical_block)?;
        let block_data = reader.read_block(block_num)?;
        if crate::checksum::verify_ext4_dx_checksum(
            reader.superblock(),
            reader.directory().raw(),
            reader.inode().i_generation,
            &block_data,
        ) == Some(false)
        {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:index"),
            ));
        }
        let has_metadata_checksum =
            crate::crc32c::ext4_superblock_has_metadata_csum(reader.superblock());
        let HashTreeNode::Internal { entries } =
            self.parse_internal_node(&block_data, has_metadata_checksum)?
        else {
            return Err(HashTreeError::InvalidHashTree);
        };
        Ok(entries)
    }

    fn advance_collision_path<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        search: HashSearch<'_>,
        path: &mut HashTreePath,
    ) -> Result<bool, HashTreeError> {
        let Some(continuation_hash) = self.advance_path(reader, search, path)? else {
            return Ok(false);
        };
        Ok(continuation_hash & !1 == search.target_hash)
    }

    /// Advances an HTree path to the next leaf and returns its index boundary.
    ///
    /// This is the path-only part of Linux `ext4_htree_next_block()`. Lookup
    /// filters the returned boundary to a collision continuation, while
    /// readdir accepts every next leaf.
    pub(super) fn advance_path<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        search: HashSearch<'_>,
        path: &mut HashTreePath,
    ) -> Result<Option<u32>, HashTreeError> {
        let mut level = path
            .frames
            .len()
            .checked_sub(1)
            .ok_or(HashTreeError::InvalidHashTree)?;
        loop {
            let frame = &mut path.frames[level];
            if frame.selected + 1 < frame.entries.len() {
                frame.selected += 1;
                break;
            }
            if level == 0 {
                return Ok(None);
            }
            level -= 1;
        }

        let continuation_hash = path.frames[level].entries[path.frames[level].selected].hash;
        path.frames.truncate(level + 1);
        while path.frames.len() < usize::from(search.indirect_levels) + 1 {
            let logical_block = path.current_entry()?.block;
            let entries = self.read_internal_entries(reader, path, logical_block)?;
            path.frames.push(HashTreeFrame {
                source_block: logical_block,
                entries,
                selected: 0,
            });
        }
        Ok(Some(continuation_hash))
    }

    pub(super) fn search_in_leaf_data(
        &self,
        data: &[u8],
        target_name: &[u8],
        block_num: AbsoluteBN,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let iter = DirEntryIterator::new(data);

        for (entry, offset) in iter {
            if entry.name == target_name {
                return Ok(HashTreeSearchResult {
                    inode: InodeNumber::new(entry.inode)
                        .map_err(|_| HashTreeError::CorruptedHashTree)?,
                    file_type: entry.file_type,
                    block_num,
                    offset,
                });
            }
        }

        Err(HashTreeError::EntryNotFound)
    }

    pub(super) fn fallback_to_linear_search<R: DirectoryBlockRead>(
        &self,
        reader: &mut R,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let total_size =
            usize::try_from(reader.inode_size()).map_err(|_| HashTreeError::BlockOutOfRange)?;
        let block_bytes = reader.block_size();
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        let blocks_map = reader.mapped_blocks()?;

        for lbn in 0..total_blocks {
            let phys = match blocks_map.get(&(lbn as u32)) {
                Some(block) => *block,
                None => continue,
            };

            let block_data = reader.read_block(phys)?;
            let checksum_ok = if reader.inode().is_htree_indexed() {
                crate::checksum::verify_ext4_dx_checksum(
                    reader.superblock(),
                    reader.directory().raw(),
                    reader.inode().i_generation,
                    &block_data,
                )
                .unwrap_or_else(|| {
                    crate::checksum::verify_ext4_dirblock_checksum(
                        reader.superblock(),
                        reader.directory().raw(),
                        reader.inode().i_generation,
                        &block_data,
                    )
                })
            } else {
                crate::checksum::verify_ext4_dirblock_checksum(
                    reader.superblock(),
                    reader.directory().raw(),
                    reader.inode().i_generation,
                    &block_data,
                )
            };
            if !checksum_ok {
                return Err(HashTreeError::Filesystem(
                    crate::Ext4Error::checksum().with_operation("htree:linear"),
                ));
            }

            if let Some((entry, offset)) =
                classic_dir::find_entry_with_offset(&block_data, target_name)
                && entry.file_type != Ext4DirEntryTail::RESERVED_FT
            {
                return Ok(HashTreeSearchResult {
                    inode: InodeNumber::new(entry.inode)
                        .map_err(|_| HashTreeError::CorruptedHashTree)?,
                    file_type: entry.file_type,
                    block_num: phys,
                    offset,
                });
            }
        }

        Err(HashTreeError::EntryNotFound)
    }
}

impl HashTreePath {
    pub(super) fn current_entry(&self) -> Result<&Ext4DxEntry, HashTreeError> {
        let frame = self.frames.last().ok_or(HashTreeError::InvalidHashTree)?;
        frame
            .entries
            .get(frame.selected)
            .ok_or(HashTreeError::CorruptedHashTree)
    }
}

fn select_entry(entries: &[Ext4DxEntry], target_hash: u32) -> Result<usize, HashTreeError> {
    entries
        .iter()
        .rposition(|entry| entry.hash <= target_hash)
        .ok_or(HashTreeError::EntryNotFound)
}

fn resolve_logical_block<R: DirectoryBlockRead>(
    reader: &mut R,
    logical_block: u32,
) -> Result<AbsoluteBN, HashTreeError> {
    let block_size =
        u64::try_from(reader.block_size()).map_err(|_| HashTreeError::BlockOutOfRange)?;
    let total_blocks = reader.inode_size().div_ceil(block_size);
    if u64::from(logical_block) >= total_blocks {
        return Err(HashTreeError::BlockOutOfRange);
    }
    reader
        .map_block(logical_block)
        .map_err(HashTreeError::from)?
        .ok_or(HashTreeError::BlockOutOfRange)
}
