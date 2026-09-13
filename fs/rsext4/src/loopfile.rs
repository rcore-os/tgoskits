//! Path walking and inode block-resolution helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::*,
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

    let root = (fs.root_inode, fs.get_root(block_dev)?);
    let mut current = root;
    let mut ancestors = Vec::from([root]);

    // Keep the inode number and contents in one path-stack element: pairing
    // either value with a different directory would make block mapping and
    // metadata checksums use different inode identities.
    for name in components {
        if !current.1.is_dir() {
            return Ok(None);
        }

        if name == "." {
            continue;
        }
        if name == ".." {
            if ancestors.len() > 1 {
                ancestors.pop();
                current = *ancestors.last().ok_or_else(Ext4Error::corrupted)?;
            }
            continue;
        }

        let target = name.as_bytes();
        let inode_num = match lookup_directory_entry(fs, block_dev, current.0, &current.1, target) {
            Ok(result) => result.inode,
            Err(HashTreeError::EntryNotFound) => return Ok(None),
            Err(error) => return Err(error.into_ext4("htree:path_lookup")),
        };

        // Refresh the current inode after each successful component resolution.
        current = (inode_num, fs.get_inode_by_num(block_dev, inode_num)?);
        ancestors.push(current);
    }

    Ok(Some(current))
}
