//! Path walking and inode block-resolution helpers.

use alloc::{collections::BTreeMap, vec::Vec};

use log::{debug, error};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::verify_ext4_dirblock_checksum,
    config::*,
    disknode::*,
    entries::*,
    error::*,
    ext4::*,
    extents_tree::*,
    hashtree::*,
};

/// Resolves a logical block number to an absolute physical block number.
pub fn resolve_inode_block<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
    logical_block: u32,
) -> Ext4Result<Option<AbsoluteBN>> {
    if inode.have_extend_header_and_use_extend() {
        let mut tree = ExtentTree::new(inode);
        if let Some(ext) = tree.find_extent(block_dev, logical_block)? {
            let raw_len = ext.ee_len as u32;
            let is_unwritten = raw_len > 0x8000;
            let mut len = raw_len;
            if (len & 0x8000) != 0 {
                len &= 0x7FFF;
            }
            if len == 0 {
                return Ok(None);
            }

            let start_lbn = ext.ee_block;
            if logical_block < start_lbn || logical_block >= start_lbn.saturating_add(len) {
                return Ok(None);
            }

            if is_unwritten {
                return Ok(None);
            }

            let base = ((ext.ee_start_hi as u64) << 32) | ext.ee_start_lo as u64;
            let phys = base + (logical_block - start_lbn) as u64;
            return Ok(Some(AbsoluteBN::new(phys)));
        }
        Ok(None)
    } else {
        error!("Only Support Extend mode!");
        Err(Ext4Error::unsupported())
    }
}

/// Builds a logical-block to physical-block map for an extent-based inode.
///
/// The helper walks the entire extent tree, materializes every mapped block,
/// and returns the final map sorted by logical block number.
pub fn resolve_inode_block_allextend<B: BlockDevice>(
    _fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
    if !inode.have_extend_header_and_use_extend() {
        return Ok(BTreeMap::new());
    }

    fn push_extent_blocks(out: &mut Vec<(u32, AbsoluteBN)>, ext: &Ext4Extent) {
        let raw_len = ext.ee_len as u32;
        if (raw_len & 0x8000) != 0 {
            return;
        }
        let len = raw_len;
        if len == 0 {
            return;
        }
        let base = ((ext.ee_start_hi as u64) << 32) | ext.ee_start_lo as u64;
        for i in 0..len {
            let lbn = ext.ee_block.saturating_add(i);
            out.push((lbn, AbsoluteBN::new(base + i as u64)));
        }
    }

    fn walk_node<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        out: &mut Vec<(u32, AbsoluteBN)>,
    ) -> Ext4Result<()> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for ext in entries {
                    push_extent_blocks(out, ext);
                }
                Ok(())
            }
            ExtentNode::Index { entries, .. } => {
                // Depth-first traversal keeps the helper independent from the tree depth.
                for idx in entries {
                    let child_block = ((idx.ei_leaf_hi as u64) << 32) | (idx.ei_leaf_lo as u64);
                    dev.read_block(AbsoluteBN::new(child_block))?;
                    let buf = dev.buffer();
                    let child = ExtentTree::parse_node(buf).ok_or(Ext4Error::corrupted())?;
                    walk_node(dev, &child, out)?;
                }
                Ok(())
            }
        }
    }

    let tree = ExtentTree::new(inode);
    let root = match tree.load_root_from_inode() {
        Some(n) => n,
        None => return Ok(BTreeMap::new()),
    };

    let mut blocks: Vec<(u32, AbsoluteBN)> = Vec::new();
    walk_node(block_dev, &root, &mut blocks)?;
    blocks.sort_unstable_by_key(|(lbn, _)| *lbn);
    blocks.dedup_by_key(|(lbn, _)| *lbn);

    let mut out = BTreeMap::new();
    for (lbn, phys) in blocks {
        out.insert(lbn, phys);
    }
    Ok(out)
}

/// Resolves a path to its inode number and inode contents.
///
/// The path walk tries hash-tree lookup first for each component and falls back
/// to a linear directory scan when the indexed lookup cannot answer the query.
pub fn get_file_inode<B: BlockDevice>(
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
        match lookup_directory_entry(fs, block_dev, &current_inode, target) {
            Ok(result) => {
                found_inode_num =
                    Some(InodeNumber::new(result.entry.inode).map_err(|_| Ext4Error::corrupted())?);
            }
            Err(_) => {
                debug!("Hash tree lookup failed, falling back to linear search");

                let total_size = current_inode.size() as usize;
                let block_bytes = BLOCK_SIZE;
                let blocks = resolve_inode_block_allextend(fs, block_dev, &mut current_inode)?;
                debug!(
                    "Directory inode size: {} bytes, blocks used: {}",
                    &total_size,
                    &blocks.len()
                );

                for (idx, phys) in blocks.iter().enumerate() {
                    debug!("Scan dir block idx {} phys {}", &idx, phys.1);
                    let cached_block = fs.datablock_cache.get_or_load(block_dev, *phys.1)?;
                    let block_data = &cached_block.data[..block_bytes];

                    if !verify_ext4_dirblock_checksum(
                        &fs.superblock,
                        current_ino_num.raw(),
                        current_inode.i_generation,
                        block_data,
                    ) {
                        error!(
                            "dir block checksum mismatch: ino={} blk_idx={} phys={}",
                            current_ino_num, idx, phys.1
                        );
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
