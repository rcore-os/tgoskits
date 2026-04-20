//! Hash tree lookup flow and fallback logic.

use alloc::vec::Vec;

use log::{debug, error, warn};

use super::{
    Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, HashTreeNode, HashTreeSearchResult,
};
use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    bmalloc::AbsoluteBN,
    config::BLOCK_SIZE,
    disknode::Ext4Inode,
    entries::{DirEntryIterator, Ext4DirEntryInfo, Ext4DxEntry, classic_dir, htree_dir},
    ext4::Ext4FileSystem,
    loopfile::{resolve_inode_block, resolve_inode_block_allextend},
};

pub(super) fn lookup<B: BlockDevice>(
    manager: &HashTreeManager,
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    dir_inode: &Ext4Inode,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    debug!(
        "Starting hash tree lookup: {:?}",
        core::str::from_utf8(target_name)
    );

    if !dir_inode.is_htree_indexed() {
        return manager.fallback_to_linear_search(fs, block_dev, dir_inode, target_name);
    }

    let target_hash =
        htree_dir::calculate_hash(target_name, manager.hash_version, &manager.hash_seed);
    debug!("Target hash value: 0x{target_hash:08x}");

    let root_block = manager.get_root_block(block_dev, dir_inode)?;
    let root_data = manager.read_block_data(fs, block_dev, root_block)?;
    let root_info = manager.parse_root_node(&root_data)?;

    match manager.search_in_hash_tree(
        fs,
        block_dev,
        dir_inode,
        &root_info,
        target_hash,
        target_name,
    ) {
        Ok(result) => Ok(result),
        Err(err) => {
            warn!("Hash tree lookup failed: {err}, falling back to linear search");
            manager.fallback_to_linear_search(fs, block_dev, dir_inode, target_name)
        }
    }
}

impl HashTreeManager {
    pub(super) fn get_root_block<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
    ) -> Result<AbsoluteBN, HashTreeError> {
        match resolve_inode_block(block_dev, &mut dir_inode.clone(), 0) {
            Ok(Some(block)) => Ok(block),
            Ok(None) => Err(HashTreeError::InvalidHashTree),
            Err(_) => Err(HashTreeError::BlockOutOfRange),
        }
    }

    pub(super) fn read_block_data<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
    ) -> Result<Vec<u8>, HashTreeError> {
        match fs.datablock_cache.get_or_load(block_dev, block_num) {
            Ok(cached_block) => Ok(cached_block.data.clone()),
            Err(_) => Err(HashTreeError::BlockOutOfRange),
        }
    }

    pub(super) fn search_in_hash_tree<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
        node: &HashTreeNode,
        target_hash: u32,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        match node {
            HashTreeNode::Root { entries, .. } | HashTreeNode::Internal { entries, .. } => self
                .search_in_entries(
                    fs,
                    block_dev,
                    dir_inode,
                    entries,
                    target_hash,
                    target_name,
                    0,
                ),
            HashTreeNode::Leaf { block_num, .. } => {
                self.search_in_leaf_block(fs, block_dev, *block_num, target_name)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn search_in_entries<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
        entries: &[Ext4DxEntry],
        target_hash: u32,
        target_name: &[u8],
        level: u32,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let mut selected_entry = None;
        for entry in entries {
            if entry.hash <= target_hash {
                selected_entry = Some(entry);
            } else {
                break;
            }
        }

        let entry = selected_entry.ok_or(HashTreeError::EntryNotFound)?;
        let block_num = resolve_inode_block(block_dev, &mut dir_inode.clone(), entry.block)
            .map_err(|_| HashTreeError::BlockOutOfRange)?
            .ok_or(HashTreeError::BlockOutOfRange)?;
        let block_data = self.read_block_data(fs, block_dev, block_num)?;

        if level >= self.indirect_levels as u32 {
            self.search_in_leaf_data(&block_data, target_name, block_num)
        } else {
            let internal_node = self.parse_internal_node(&block_data)?;
            self.search_in_hash_tree(
                fs,
                block_dev,
                dir_inode,
                &internal_node,
                target_hash,
                target_name,
            )
        }
    }

    pub(super) fn search_in_leaf_block<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let block_data = self.read_block_data(fs, block_dev, block_num)?;
        self.search_in_leaf_data(&block_data, target_name, block_num)
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

    pub(super) fn fallback_to_linear_search<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        debug!(
            "Using linear search: {:?}",
            core::str::from_utf8(target_name)
        );

        let total_size = dir_inode.size() as usize;
        let block_bytes = BLOCK_SIZE;
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        if dir_inode.have_extend_header_and_use_extend() {
            let mut inode_copy = *dir_inode;
            let blocks_map = match resolve_inode_block_allextend(fs, block_dev, &mut inode_copy) {
                Ok(map) => map,
                Err(_) => return Err(HashTreeError::BlockOutOfRange),
            };

            for lbn in 0..total_blocks {
                let phys = match blocks_map.get(&(lbn as u32)) {
                    Some(block) => *block,
                    None => continue,
                };

                let cached_block = match fs.datablock_cache.get_or_load(block_dev, phys) {
                    Ok(block) => block,
                    Err(_) => return Err(HashTreeError::BlockOutOfRange),
                };

                let block_data = &cached_block.data[..block_bytes];
                if let Some(entry) = classic_dir::find_entry(block_data, target_name) {
                    return Ok(HashTreeSearchResult {
                        entry: unsafe {
                            core::mem::transmute::<Ext4DirEntryInfo<'_>, Ext4DirEntryInfo<'_>>(
                                entry,
                            )
                        },
                        block_num: phys,
                        offset: 0,
                    });
                }
            }

            return Err(HashTreeError::EntryNotFound);
        }

        error!("FS NOT SUPPORT NORMAL MULTIPUL POINTER ,PLEASE TURN ON EXTEND FEATURE!");
        Err(HashTreeError::CorruptedHashTree)
    }
}
