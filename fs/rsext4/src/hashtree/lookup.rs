//! Hash tree lookup flow and fallback logic.

use alloc::{vec, vec::Vec};

use super::{
    Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, HashTreeNode, HashTreeSearchResult,
};
use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    entries::{DirEntryIterator, Ext4DirEntryInfo, Ext4DxEntry, Ext4DxRootInfo, classic_dir},
    ext4::Ext4FileSystem,
    loopfile::{resolve_inode_block, resolve_inode_blocks},
    superblock::Ext4Superblock,
};

#[derive(Clone, Copy)]
struct HashSearch<'a> {
    dir_ino: InodeNumber,
    dir_inode: &'a Ext4Inode,
    target_hash: u32,
    target_name: &'a [u8],
    indirect_levels: u8,
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

    let root_block = manager.get_root_block(fs, block_dev, dir_ino, dir_inode)?;
    let root_data = manager.read_block_data(fs, block_dev, root_block)?;
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
    let has_metadata_checksum = crate::crc32c::ext4_superblock_has_metadata_csum(&fs.superblock);
    let max_indirect_levels = if fs
        .superblock
        .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR)
    {
        2
    } else {
        1
    };
    let root_info =
        manager.parse_root_node(&root_data, has_metadata_checksum, max_indirect_levels)?;
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
    let target_hash = super::calculate_hash(target_name, hash_version, &manager.hash_seed)?.major;
    let search = HashSearch {
        dir_ino,
        dir_inode,
        target_hash,
        target_name,
        indirect_levels,
    };

    match manager.search_in_hash_tree(fs, block_dev, search, &root_info, 0, &mut vec![0]) {
        Ok(result) => Ok(result),
        Err(error @ HashTreeError::Filesystem(_)) => Err(error),
        Err(_) => manager.fallback_to_linear_search(fs, block_dev, dir_ino, dir_inode, target_name),
    }
}

impl HashTreeManager {
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

    fn search_in_hash_tree<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        node: &HashTreeNode,
        level: u32,
        visited_blocks: &mut Vec<u32>,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        match node {
            HashTreeNode::Root { entries, .. } | HashTreeNode::Internal { entries, .. } => {
                self.search_in_entries(fs, block_dev, search, entries, level, visited_blocks)
            }
        }
    }

    fn search_in_entries<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        search: HashSearch<'_>,
        entries: &[Ext4DxEntry],
        level: u32,
        visited_blocks: &mut Vec<u32>,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let mut selected_entry = None;
        for entry in entries {
            if entry.hash <= search.target_hash {
                selected_entry = Some(entry);
            } else {
                break;
            }
        }

        let entry = selected_entry.ok_or(HashTreeError::EntryNotFound)?;
        let total_blocks = dir_inode_block_count(search.dir_inode, fs.block_size())?;
        if u64::from(entry.block) >= total_blocks || visited_blocks.contains(&entry.block) {
            return Err(HashTreeError::BlockOutOfRange);
        }
        visited_blocks.push(entry.block);
        let block_num = resolve_inode_block(
            fs,
            block_dev,
            search.dir_ino,
            &mut search.dir_inode.clone(),
            entry.block,
        )
        .map_err(HashTreeError::from)?
        .ok_or(HashTreeError::BlockOutOfRange)?;
        let block_data = self.read_block_data(fs, block_dev, block_num)?;

        if level >= u32::from(search.indirect_levels) {
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
            self.search_in_leaf_data(&block_data, search.target_name, block_num)
        } else {
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
            let internal_node = self.parse_internal_node(&block_data, has_metadata_checksum)?;
            self.search_in_hash_tree(
                fs,
                block_dev,
                search,
                &internal_node,
                level + 1,
                visited_blocks,
            )
        }
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
                    entry: unsafe {
                        core::mem::transmute::<Ext4DirEntryInfo<'_>, Ext4DirEntryInfo<'_>>(entry)
                    },
                    block_num,
                    offset: offset as usize,
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
        let total_size = dir_inode.size() as usize;
        let block_bytes = fs.block_size();
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        if dir_inode.uses_extents() {
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
                if let Some((entry, offset)) =
                    classic_dir::find_entry_with_offset(block_data, target_name)
                {
                    return Ok(HashTreeSearchResult {
                        entry: unsafe {
                            core::mem::transmute::<Ext4DirEntryInfo<'_>, Ext4DirEntryInfo<'_>>(
                                entry,
                            )
                        },
                        block_num: phys,
                        offset,
                    });
                }
            }

            return Err(HashTreeError::EntryNotFound);
        }

        Err(HashTreeError::CorruptedHashTree)
    }
}

fn dir_inode_block_count(inode: &Ext4Inode, block_size: usize) -> Result<u64, HashTreeError> {
    let block_size = u64::try_from(block_size).map_err(|_| HashTreeError::BlockOutOfRange)?;
    Ok(inode.size().div_ceil(block_size))
}
