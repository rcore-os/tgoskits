//! Path walking and inode block-resolution helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    disknode::*,
    entries::*,
    error::*,
    ext4::*,
    extents_tree::*,
    hashtree::*,
    indirect::{resolve_legacy_inode_block, resolve_legacy_inode_blocks},
};

/// Resolves a logical block number to an absolute physical block number.
pub fn resolve_inode_block<B: BlockIo>(
    fs: &Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    logical_block: u32,
) -> Ext4Result<Option<AbsoluteBN>> {
    if inode.uses_extents() {
        let mut tree = ExtentTree::with_filesystem(inode, fs, inode_num);
        match tree.map_block(block_dev, logical_block)? {
            ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => Ok(None),
            ExtentBlockMapping::Initialized(physical) => Ok(Some(physical)),
        }
    } else {
        resolve_legacy_inode_block(fs, block_dev, inode_num, inode, logical_block)
    }
}

/// Builds a logical-block to physical-block map for an extent-based inode.
///
/// The helper walks the entire extent tree, materializes every mapped block,
/// and returns the final map sorted by logical block number.
pub fn resolve_inode_blocks<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
    if !inode.uses_extents() {
        return resolve_legacy_inode_blocks(fs, block_dev, inode_num, inode);
    }

    let mut tree = ExtentTree::with_filesystem(inode, fs, inode_num);
    let runs = tree.initialized_runs_in_range(block_dev, 0, u32::MAX)?;
    let mut out = BTreeMap::new();
    for run in runs {
        for offset in 0..run.len {
            let lbn = run
                .logical_start
                .checked_add(offset)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
            let phys = run.physical_start.checked_add(offset)?;
            if out.insert(lbn, phys).is_some() {
                return Err(Ext4Error::corrupted().with_operation("extent:duplicate_mapping"));
            }
        }
    }
    Ok(out)
}

/// Resolves a path to its inode number and inode contents.
///
/// The path walk tries hash-tree lookup first for each component and falls back
/// to a linear directory scan when the indexed lookup cannot answer the query.
pub fn get_file_inode<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<Option<(InodeNumber, Ext4Inode)>> {
    if path.is_empty() || path == "/" {
        let inode = fs.get_root(block_dev)?;
        return Ok(Some((fs.root_inode, inode)));
    }

    let components = path.split('/').filter(|s| !s.is_empty());

    let mut current_inode = fs.get_root(block_dev)?;
    let mut current_ino_num = fs.root_inode;
    let mut path_vec: Vec<Ext4Inode> = Vec::new();
    path_vec.push(current_inode);

    // Walk the namespace one component at a time, carrying a small ancestor stack for `..`.
    for name in components {
        if !current_inode.is_dir() {
            return Ok(None);
        }

        if name == "." {
            continue;
        }
        if name == ".." {
            if path_vec.len() > 1 {
                path_vec.pop();
                if let Some(parent_inode) = path_vec.last() {
                    current_inode = *parent_inode;
                }
            }
            continue;
        }

        let target = name.as_bytes();
        let mut found_inode_num: Option<InodeNumber> = None;

        // Prefer the hashed directory path and fall back to a full scan only when needed.
        match lookup_directory_entry(fs, block_dev, current_ino_num, &current_inode, target) {
            Ok(result) => {
                found_inode_num =
                    Some(InodeNumber::new(result.entry.inode).map_err(|_| Ext4Error::corrupted())?);
            }
            Err(HashTreeError::Filesystem(error)) => return Err(error),
            Err(_) => {
                let blocks =
                    resolve_inode_blocks(fs, block_dev, current_ino_num, &mut current_inode)?;

                for phys in &blocks {
                    let cached_block = fs.datablock_cache.get_or_load(block_dev, *phys.1)?;
                    let block_data = &cached_block.data;

                    let checksum_ok = if current_inode.is_htree_indexed() {
                        verify_ext4_dx_checksum(
                            &fs.superblock,
                            current_ino_num.raw(),
                            current_inode.i_generation,
                            block_data,
                        )
                        .unwrap_or_else(|| {
                            verify_ext4_dirblock_checksum(
                                &fs.superblock,
                                current_ino_num.raw(),
                                current_inode.i_generation,
                                block_data,
                            )
                        })
                    } else {
                        verify_ext4_dirblock_checksum(
                            &fs.superblock,
                            current_ino_num.raw(),
                            current_inode.i_generation,
                            block_data,
                        )
                    };

                    if !checksum_ok {
                        return Err(Ext4Error::checksum());
                    }

                    if let Some(entry) = classic_dir::find_entry(block_data, target)
                        && entry.file_type != Ext4DirEntryTail::RESERVED_FT
                    {
                        found_inode_num = Some(
                            InodeNumber::new(entry.inode).map_err(|_| Ext4Error::corrupted())?,
                        );
                        break;
                    }
                }
            }
        }

        let inode_num = match found_inode_num {
            Some(n) => n,
            None => return Ok(None),
        };

        // Refresh the current inode after each successful component resolution.
        current_inode = fs.get_inode_by_num(block_dev, inode_num)?;
        current_ino_num = inode_num;
        path_vec.push(current_inode);
    }

    Ok(Some((current_ino_num, current_inode)))
}
