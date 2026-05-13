//! Path walking and inode block-resolution helpers.

use alloc::{collections::BTreeMap, vec::Vec};
use core::{convert::TryFrom, mem::size_of};

use log::{debug, error};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber, LogicalBN},
    checksum::verify_ext4_dirblock_checksum,
    config::runtime_block_size,
    disknode::*,
    endian::read_u32_le,
    entries::*,
    error::*,
    ext4::*,
    extents_tree::*,
    hashtree::*,
    checksum::*,
};
/// Number of direct data-block slots stored inline in `inode.i_block`.
///
/// Classic ext2/3/4 inodes use the remaining three words for the single,
/// double, and triple indirect roots respectively.
const DIRECT_BLOCK_COUNT: usize = 12;

/// Decoded lookup path for classic non-extent block addressing.
///
/// The payload stores the pointer indexes that must be followed from the inode
/// root to resolve a logical block:
/// - `Direct(i)` reads `inode.i_block[i]`
/// - `Single(i)` reads `inode.i_block[12]` then slot `i`
/// - `Double(i, j)` reads `inode.i_block[13]`, then `i`, then `j`
/// - `Triple(i, j, k)` reads `inode.i_block[14]`, then `i`, then `j`, then `k`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassicBlockPath {
    Direct(usize),
    Single(usize),
    Double(usize, usize),
    Triple(usize, usize, usize),
}

/// Returns how many `u32` block pointers fit in one filesystem block.
///
/// Classic indirect blocks are plain arrays of little-endian `u32` physical
/// block numbers, so the valid fan-out is `block_size / 4`.
fn pointers_per_block() -> Ext4Result<usize> {
    let block_size = runtime_block_size();
    if block_size < size_of::<u32>() || !block_size.is_multiple_of(size_of::<u32>()) {
        return Err(Ext4Error::corrupted());
    }
    Ok(block_size / size_of::<u32>())
}

/// Decodes a logical block number into the classic direct/indirect walk.
///
/// This mirrors the old ext2/3/4 addressing scheme:
/// 1. consume the 12 direct slots,
/// 2. then the single-indirect fan-out,
/// 3. then the square fan-out of the double-indirect tree,
/// 4. finally the cube fan-out of the triple-indirect tree.
///
/// Returning `None` means the logical block lies beyond what the classic
/// `i_block` layout can represent for the current block size.
fn decode_classic_block_path(
    logical_block: u32,
    pointers_per_block: usize,
) -> Option<ClassicBlockPath> {
    let mut remaining = logical_block as usize;

    if remaining < DIRECT_BLOCK_COUNT {
        return Some(ClassicBlockPath::Direct(remaining));
    }
    remaining -= DIRECT_BLOCK_COUNT;

    if remaining < pointers_per_block {
        return Some(ClassicBlockPath::Single(remaining));
    }
    remaining -= pointers_per_block;

    let double_span = pointers_per_block.checked_mul(pointers_per_block)?;
    if remaining < double_span {
        return Some(ClassicBlockPath::Double(
            remaining / pointers_per_block,
            remaining % pointers_per_block,
        ));
    }
    remaining -= double_span;

    let triple_span = double_span.checked_mul(pointers_per_block)?;
    if remaining < triple_span {
        let first = remaining / double_span;
        let second = (remaining / pointers_per_block) % pointers_per_block;
        let third = remaining % pointers_per_block;
        return Some(ClassicBlockPath::Triple(first, second, third));
    }

    None
}

/// Reads one physical block pointer from a classic indirect block.
///
/// A zero entry means "hole / not allocated" and is surfaced as `Ok(None)`
/// rather than a corruption error, matching ext4 sparse-file semantics.
fn read_pointer_from_block<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    block_num: AbsoluteBN,
    pointer_index: usize,
) -> Ext4Result<Option<AbsoluteBN>> {
    block_dev.read_block(block_num)?;
    let buffer = block_dev.buffer();
    let start = pointer_index
        .checked_mul(size_of::<u32>())
        .ok_or_else(Ext4Error::corrupted)?;
    let end = start + size_of::<u32>();
    if end > buffer.len() {
        return Err(Ext4Error::corrupted());
    }

    let raw = read_u32_le(&buffer[start..end]);
    Ok((raw != 0).then(|| AbsoluteBN::new(u64::from(raw))))
}

/// Resolves a previously decoded classic block-addressing path.
///
/// Each zero parent pointer short-circuits to `None`, because an absent
/// indirect root means the entire subtree is a hole.
fn resolve_classic_path<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
    path: ClassicBlockPath,
) -> Ext4Result<Option<AbsoluteBN>> {
    match path {
        ClassicBlockPath::Direct(idx) => {
            let raw = inode.i_block[idx];
            Ok((raw != 0).then(|| AbsoluteBN::new(u64::from(raw))))
        }
        ClassicBlockPath::Single(idx) => {
            let parent = inode.i_block[12];
            if parent == 0 {
                return Ok(None);
            }
            read_pointer_from_block(block_dev, AbsoluteBN::new(u64::from(parent)), idx)
        }
        ClassicBlockPath::Double(left, right) => {
            let parent = inode.i_block[13];
            if parent == 0 {
                return Ok(None);
            }
            let first =
                read_pointer_from_block(block_dev, AbsoluteBN::new(u64::from(parent)), left)?;
            match first {
                Some(block) => read_pointer_from_block(block_dev, block, right),
                None => Ok(None),
            }
        }
        ClassicBlockPath::Triple(a, b, c) => {
            let parent = inode.i_block[14];
            if parent == 0 {
                return Ok(None);
            }
            let first = read_pointer_from_block(block_dev, AbsoluteBN::new(u64::from(parent)), a)?;
            let Some(block2) = first else {
                return Ok(None);
            };
            let second = read_pointer_from_block(block_dev, block2, b)?;
            let Some(block3) = second else {
                return Ok(None);
            };
            read_pointer_from_block(block_dev, block3, c)
        }
    }
}

/// Materializes the full logical-to-physical map for a classic inode.
///
/// This is intentionally simple and used by higher-level helpers such as
/// `SEEK_DATA/SEEK_HOLE`, which want a unified "mapped blocks" view regardless
/// of whether the inode uses extents or indirect blocks.
fn resolve_classic_block_map<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
) -> Ext4Result<BTreeMap<LogicalBN, AbsoluteBN>> {
    let block_size = runtime_block_size() as u64;
    if block_size == 0 {
        return Ok(BTreeMap::new());
    }

    let total_blocks = inode.size().div_ceil(block_size);
    let total_blocks = u32::try_from(total_blocks).map_err(|_| Ext4Error::corrupted())?;
    let mut out = BTreeMap::new();

    for lbn in 0..total_blocks {
        if let Some(phys) = resolve_inode_block(block_dev, inode, lbn)? {
            out.insert(LogicalBN::new(lbn), phys);
        }
    }

    Ok(out)
}

/// Resolves a logical block number to an absolute physical block number.
///
/// `logical_block` starts at 0.
///
/// Extent inodes and classic indirect-block inodes deliberately share the same
/// outward contract here:
/// - `Some(phys)` for a mapped initialized data block
/// - `None` for a hole, an unwritten extent, or an unmapped classic slot
pub fn resolve_inode_block<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
    logical_block: u32,
) -> Ext4Result<Option<AbsoluteBN>> {
    if inode.have_extend_header_and_use_extend() {
        let mut tree = ExtentTree::new(inode);
        if let Some(ext) = tree.find_extent(block_dev, logical_block)? {
            let raw_len = ext.ee_len as u32;
            // ext4 encodes unwritten extents by setting the high bit in
            // `ee_len`. They reserve space but do not expose initialized data,
            // so callers that care about holes should treat them as absent.
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
        let pointers_per_block = pointers_per_block()?;
        let Some(path) = decode_classic_block_path(logical_block, pointers_per_block) else {
            return Ok(None);
        };
        resolve_classic_path(block_dev, inode, path)
    }
}

/// Builds a full logical-block to physical-block map for one inode.
///
/// The historical function name mentions extents, but the helper now serves
/// both extent and classic indirect-block inodes. Callers use it as a unified
/// "where is initialized data?" view.
pub fn resolve_inode_block_allextend<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
) -> Ext4Result<BTreeMap<LogicalBN, AbsoluteBN>> {
    if !inode.have_extend_header_and_use_extend() {
        return resolve_classic_block_map(block_dev, inode);
    }

    fn push_extent_blocks(out: &mut Vec<(LogicalBN, AbsoluteBN)>, ext: &Ext4Extent) {
        let raw_len = ext.ee_len as u32;
        // Skip unwritten extents so the resulting map only represents blocks
        // that currently contain initialized file data.
        if (raw_len & 0x8000) != 0 {
            return;
        }
        let len = raw_len;
        if len == 0 {
            return;
        }
        let base = ((ext.ee_start_hi as u64) << 32) | ext.ee_start_lo as u64;
        for i in 0..len {
            let lbn = LogicalBN::new(ext.ee_block.saturating_add(i));
            out.push((lbn, AbsoluteBN::new(base + i as u64)));
        }
    }

    fn walk_node<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        out: &mut Vec<(LogicalBN, AbsoluteBN)>,
    ) -> Ext4Result<()> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for ext in entries {
                    push_extent_blocks(out, ext);
                }
                Ok(())
            }
            ExtentNode::Index { entries, .. } => {
                // Depth-first traversal keeps the helper independent from the
                // actual extent-tree depth stored on disk.
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

    let mut blocks: Vec<(LogicalBN, AbsoluteBN)> = Vec::new();
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
                let block_bytes = fs.block_size;
                let blocks = resolve_inode_block_allextend(block_dev, &mut current_inode)?;
                debug!(
                    "Directory inode size: {} bytes, blocks used: {}",
                    &total_size,
                    &blocks.len()
                );

                for (idx, phys) in blocks.iter().enumerate() {
                    debug!("Scan dir block idx {} phys {}", &idx, phys.1);
                    let cached_block = fs.datablock_cache.get_or_load(block_dev, *phys.1)?;
                    let block_data = &cached_block.data[..block_bytes];

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
