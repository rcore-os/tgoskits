//! Hash tree lookup flow and fallback logic.

#![forbid(unsafe_code)]

use alloc::{vec, vec::Vec};

use super::{
    Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, HashTreeNode, HashTreeSearchResult,
};
use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    entries::{DirEntryIterator, Ext4DirEntryTail, Ext4DxEntry, Ext4DxRootInfo, classic_dir},
    ext4::Ext4FileSystem,
    loopfile::{resolve_inode_block, resolve_inode_blocks},
    superblock::Ext4Superblock,
};

#[derive(Clone, Copy)]
pub(super) struct HashSearch<'a> {
    pub(super) dir_ino: InodeNumber,
    pub(super) dir_inode: &'a Ext4Inode,
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

pub(super) fn lookup<B: BlockIo>(
    manager: &HashTreeManager,
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    dir_ino: InodeNumber,
    dir_inode: &Ext4Inode,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    if !dir_inode.is_htree_indexed() {
        return manager.fallback_to_linear_search(fs, block_dev, dir_ino, dir_inode, target_name);
    }

    let indexed_result = manager
        .prepare_search(fs, block_dev, dir_ino, dir_inode, target_name)
        .and_then(|(search, root)| manager.search_collision_chain(fs, block_dev, search, &root));

    match indexed_result {
        Ok(result) => Ok(result),
        Err(error) if error.allows_linear_fallback() => {
            manager.fallback_to_linear_search(fs, block_dev, dir_ino, dir_inode, target_name)
        }
        Err(error) => Err(error),
    }
}

impl HashTreeManager {
    pub(super) fn prepare_search<'a, B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_ino: InodeNumber,
        dir_inode: &'a Ext4Inode,
        target_name: &'a [u8],
    ) -> Result<(HashSearch<'a>, HashTreeNode), HashTreeError> {
        let root_block = self.get_root_block(fs, block_dev, dir_ino, dir_inode)?;
        let root_data = self.read_block_data(fs, block_dev, root_block)?;
        if crate::checksum::verify_ext4_dx_checksum(
            &fs.superblock,
            dir_ino.raw(),
            dir_inode.i_generation,
            &root_data,
        ) == Some(false)
        {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:root"),
            ));
        }
        let has_metadata_checksum =
            crate::crc32c::ext4_superblock_has_metadata_csum(&fs.superblock);
        let max_indirect_levels = if fs
            .superblock
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
            && fs.superblock.s_flags & Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH != 0
        {
            root_hash_version + 3
        } else {
            root_hash_version
        };
        let target_hash = super::calculate_hash(target_name, hash_version, &self.hash_seed)?.major;
        let search = HashSearch {
            dir_ino,
            dir_inode,
            target_hash,
            target_name,
            hash_version,
            indirect_levels,
        };
        Ok((search, root_info))
    }
    pub(super) fn get_root_block<B: BlockIo>(
        &self,
        fs: &Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_ino: InodeNumber,
        dir_inode: &Ext4Inode,
    ) -> Result<AbsoluteBN, HashTreeError> {
        match resolve_inode_block(fs, block_dev, dir_ino, &mut dir_inode.clone(), 0) {
            Ok(Some(block)) => Ok(block),
            Ok(None) => Err(HashTreeError::InvalidHashTree),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn read_block_data<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Result<Vec<u8>, HashTreeError> {
        fs.datablock_cache
            .get_or_load(block_dev, block_num)
            .map(|cached_block| cached_block.data.as_ref().clone())
            .map_err(HashTreeError::from)
    }

    fn search_collision_chain<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        root: &HashTreeNode,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let mut path = self.probe_path(fs, block_dev, search, root)?;
        loop {
            match self.search_current_leaf(fs, block_dev, search, &path) {
                Ok(result) => return Ok(result),
                Err(HashTreeError::EntryNotFound) => {
                    if !self.advance_collision_path(fs, block_dev, search, &mut path)? {
                        return Err(HashTreeError::EntryNotFound);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn probe_path<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
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
            let entries =
                self.read_internal_entries(fs, block_dev, search, &path, logical_block)?;
            let selected = select_entry(&entries, search.target_hash)?;
            path.frames.push(HashTreeFrame {
                source_block: logical_block,
                entries,
                selected,
            });
        }

        Ok(path)
    }

    fn search_current_leaf<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        path: &HashTreePath,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let (block_num, block_data) = self.read_current_leaf_data(fs, block_dev, search, path)?;
        self.search_in_leaf_data(&block_data, search.target_name, block_num)
    }

    pub(super) fn read_current_leaf_data<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        path: &HashTreePath,
    ) -> Result<(AbsoluteBN, Vec<u8>), HashTreeError> {
        let logical_block = path.current_entry()?.block;
        if path
            .frames
            .iter()
            .any(|frame| frame.source_block == logical_block)
        {
            return Err(HashTreeError::BlockOutOfRange);
        }
        let block_num = resolve_logical_block(fs, block_dev, search, logical_block)?;
        let block_data = self.read_block_data(fs, block_dev, block_num)?;
        if !crate::checksum::verify_ext4_dirblock_checksum(
            &fs.superblock,
            search.dir_ino.raw(),
            search.dir_inode.i_generation,
            &block_data,
        ) {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:leaf"),
            ));
        }
        Ok((block_num, block_data))
    }

    fn read_internal_entries<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
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
        let block_num = resolve_logical_block(fs, block_dev, search, logical_block)?;
        let block_data = self.read_block_data(fs, block_dev, block_num)?;
        if crate::checksum::verify_ext4_dx_checksum(
            &fs.superblock,
            search.dir_ino.raw(),
            search.dir_inode.i_generation,
            &block_data,
        ) == Some(false)
        {
            return Err(HashTreeError::Filesystem(
                crate::Ext4Error::checksum().with_operation("htree:index"),
            ));
        }
        let has_metadata_checksum =
            crate::crc32c::ext4_superblock_has_metadata_csum(&fs.superblock);
        let HashTreeNode::Internal { entries } =
            self.parse_internal_node(&block_data, has_metadata_checksum)?
        else {
            return Err(HashTreeError::InvalidHashTree);
        };
        Ok(entries)
    }

    fn advance_collision_path<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        path: &mut HashTreePath,
    ) -> Result<bool, HashTreeError> {
        let Some(continuation_hash) = self.advance_path(fs, block_dev, search, path)? else {
            return Ok(false);
        };
        Ok(continuation_hash & !1 == search.target_hash)
    }

    /// Advances an HTree path to the next leaf and returns its index boundary.
    ///
    /// This is the path-only part of Linux `ext4_htree_next_block()`. Lookup
    /// filters the returned boundary to a collision continuation, while
    /// readdir accepts every next leaf.
    pub(super) fn advance_path<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
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
            let entries = self.read_internal_entries(fs, block_dev, search, path, logical_block)?;
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

    pub(super) fn fallback_to_linear_search<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_ino: InodeNumber,
        dir_inode: &Ext4Inode,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let total_size = usize::try_from(fs.inode_size(dir_inode))
            .map_err(|_| HashTreeError::BlockOutOfRange)?;
        let block_bytes = fs.block_size();
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        let mut inode_copy = *dir_inode;
        let blocks_map = resolve_inode_blocks(fs, block_dev, dir_ino, &mut inode_copy)
            .map_err(HashTreeError::from)?;

        for lbn in 0..total_blocks {
            let phys = match blocks_map.get(&(lbn as u32)) {
                Some(block) => *block,
                None => continue,
            };

            let cached_block = fs
                .datablock_cache
                .get_or_load(block_dev, phys)
                .map_err(HashTreeError::from)?;
            let block_data = &cached_block.data;
            let checksum_ok = if dir_inode.is_htree_indexed() {
                crate::checksum::verify_ext4_dx_checksum(
                    &fs.superblock,
                    dir_ino.raw(),
                    dir_inode.i_generation,
                    block_data,
                )
                .unwrap_or_else(|| {
                    crate::checksum::verify_ext4_dirblock_checksum(
                        &fs.superblock,
                        dir_ino.raw(),
                        dir_inode.i_generation,
                        block_data,
                    )
                })
            } else {
                crate::checksum::verify_ext4_dirblock_checksum(
                    &fs.superblock,
                    dir_ino.raw(),
                    dir_inode.i_generation,
                    block_data,
                )
            };
            if !checksum_ok {
                return Err(HashTreeError::Filesystem(
                    crate::Ext4Error::checksum().with_operation("htree:linear"),
                ));
            }

            if let Some((entry, offset)) =
                classic_dir::find_entry_with_offset(block_data, target_name)
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

pub(super) fn resolve_logical_block<B: BlockIo>(
    fs: &Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    search: HashSearch<'_>,
    logical_block: u32,
) -> Result<AbsoluteBN, HashTreeError> {
    let total_blocks = dir_inode_block_count(fs, search.dir_inode)?;
    if u64::from(logical_block) >= total_blocks {
        return Err(HashTreeError::BlockOutOfRange);
    }
    resolve_inode_block(
        fs,
        block_dev,
        search.dir_ino,
        &mut search.dir_inode.clone(),
        logical_block,
    )
    .map_err(HashTreeError::from)?
    .ok_or(HashTreeError::BlockOutOfRange)
}

fn dir_inode_block_count(fs: &Ext4FileSystem, inode: &Ext4Inode) -> Result<u64, HashTreeError> {
    let block_size = u64::try_from(fs.block_size()).map_err(|_| HashTreeError::BlockOutOfRange)?;
    Ok(fs.inode_size(inode).div_ceil(block_size))
}
