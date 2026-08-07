//! Public API compatibility tests for corrected misspellings.
//!
//! These tests intentionally call the old misspelled public entry points from
//! outside the crate. They keep the compatibility wrappers compiled and verify
//! that each old name behaves like its corrected counterpart.

use std::{cell::Cell, collections::BTreeMap};

use rsext4::{
    bmalloc::AbsoluteBN,
    cache::bitmap::CacheKey,
    dir::{normalize_path, split_paren_child_and_translatevalid},
    disknode::{Ext4Extent, Ext4Inode},
    error::{Ext4Error, Ext4Result},
    ext4::{BlcokGroupLayout, BlockGroupLayout, file_entry_exisr, file_entry_exist},
    extents_tree::{ExtentNode, ExtentTree},
    loopfile::{resolve_inode_block_allextend, resolve_inode_blocks},
    tool::{calc_group_layout, cloc_group_layout},
    *,
};

#[derive(Clone)]
struct CompatBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    flush_count: u32,
    now: Cell<i64>,
}

impl CompatBlockDevice {
    fn new(total_blocks: u64) -> Self {
        Self {
            data: vec![0; total_blocks as usize * BLOCK_SIZE],
            block_size: BLOCK_SIZE as u32,
            flush_count: 0,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for CompatBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + required;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                self.total_blocks(),
            ));
        }
        buffer[..required].copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + required;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                self.total_blocks(),
            ));
        }
        self.data[start..end].copy_from_slice(&buffer[..required]);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.flush_count += 1;
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.len() / self.block_size as usize) as u64
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn setup_fs(total_blocks: u64) -> (Jbd2Dev<CompatBlockDevice>, Ext4FileSystem) {
    let device = CompatBlockDevice::new(total_blocks);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn new_extent_inode() -> Ext4Inode {
    let mut inode = Ext4Inode::default();
    inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    inode.write_extend_header();
    inode
}

fn alloc_contiguous(
    fs: &mut Ext4FileSystem,
    dev: &mut Jbd2Dev<CompatBlockDevice>,
    count: u32,
) -> AbsoluteBN {
    assert!(count > 0);
    let first = fs.alloc_block(dev).expect("allocate first block");
    let mut previous = first;
    for _ in 1..count {
        let block = fs.alloc_block(dev).expect("allocate next block");
        assert_eq!(block, previous.checked_add(1).expect("next block number"));
        previous = block;
    }
    first
}

fn bitmap_block_is_allocated(
    fs: &mut Ext4FileSystem,
    dev: &mut Jbd2Dev<CompatBlockDevice>,
    global_block: AbsoluteBN,
) -> bool {
    let (group_idx, block_in_group) = fs
        .block_allocator
        .global_to_group(global_block)
        .expect("block should map to a group");
    let desc = fs
        .group_descs
        .get(group_idx.as_usize().expect("group index"))
        .expect("group descriptor");
    let bitmap_block = AbsoluteBN::new(desc.block_bitmap());
    let bitmap = fs
        .bitmap_cache
        .get_or_load(dev, CacheKey::new_block(group_idx), bitmap_block)
        .expect("load block bitmap");
    let idx = block_in_group.as_usize().expect("block index in group");
    ((bitmap.data[idx / 8] >> (idx % 8)) & 1) == 1
}

fn collect_extents(
    inode: &mut Ext4Inode,
    dev: &mut Jbd2Dev<CompatBlockDevice>,
) -> Vec<(u32, u32, u64)> {
    fn walk(
        dev: &mut Jbd2Dev<CompatBlockDevice>,
        node: &ExtentNode,
        out: &mut Vec<(u32, u32, u64)>,
    ) {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                out.extend(
                    entries
                        .iter()
                        .map(|extent| (extent.ee_block, extent.len(), extent.start_block())),
                );
            }
            ExtentNode::Index { entries, .. } => {
                for idx in entries {
                    let child_block = ((idx.ei_leaf_hi as u64) << 32) | idx.ei_leaf_lo as u64;
                    dev.read_block(AbsoluteBN::new(child_block))
                        .expect("read child extent node");
                    let child = ExtentTree::parse_node(dev.buffer()).expect("parse child node");
                    walk(dev, &child, out);
                }
            }
        }
    }

    let mut out = Vec::new();
    let tree = ExtentTree::new(inode);
    if let Some(root) = tree.load_root_from_inode() {
        walk(dev, &root, &mut out);
    }
    out.sort_unstable();
    out
}

fn build_mapped_inode(fs: &mut Ext4FileSystem, dev: &mut Jbd2Dev<CompatBlockDevice>) -> Ext4Inode {
    let mut inode = new_extent_inode();
    let first = alloc_contiguous(fs, dev, 2);
    let second = alloc_contiguous(fs, dev, 1);
    let mut tree = ExtentTree::new(&mut inode);
    tree.insert_extent(fs, Ext4Extent::new(0, first.raw(), 2), dev)
        .expect("insert first extent");
    tree.insert_extent(fs, Ext4Extent::new(4, second.raw(), 1), dev)
        .expect("insert second extent");
    inode
}

fn assert_layout_eq(left: BlockGroupLayout, right: BlcokGroupLayout) {
    assert_eq!(left.group_start_block, right.group_start_block);
    assert_eq!(
        left.group_blcok_bitmap_startblocks,
        right.group_blcok_bitmap_startblocks
    );
    assert_eq!(
        left.group_inode_bitmap_startblocks,
        right.group_inode_bitmap_startblocks
    );
    assert_eq!(
        left.group_inode_table_startblocks,
        right.group_inode_table_startblocks
    );
    assert_eq!(
        left.metadata_blocks_in_group,
        right.metadata_blocks_in_group
    );
}

#[derive(Debug, PartialEq, Eq)]
struct RemovedExtentObservation {
    extents: Vec<(u32, u32, u64)>,
    allocated_blocks: BTreeMap<u64, bool>,
}

#[test]
fn cantflush_matches_flush_on_cached_block_device() {
    let payload = vec![0x5a; BLOCK_SIZE];
    let mut new_dev = Jbd2Dev::initial_jbd2dev(0, CompatBlockDevice::new(16), false);
    let mut old_dev = Jbd2Dev::initial_jbd2dev(0, CompatBlockDevice::new(16), false);

    new_dev
        .write_blocks(&payload, AbsoluteBN::new(3), 1, false)
        .expect("write through new device");
    old_dev
        .write_blocks(&payload, AbsoluteBN::new(3), 1, false)
        .expect("write through old device");

    new_dev.flush().expect("flush through corrected API");
    old_dev
        .cantflush()
        .expect("flush through compatibility API");

    let new_inner = new_dev.into_inner();
    let old_inner = old_dev.into_inner();
    assert_eq!(new_inner.flush_count, old_inner.flush_count);
    assert_eq!(new_inner.data, old_inner.data);
}

#[test]
fn file_entry_exisr_matches_file_entry_exist() {
    let (mut dev, mut fs) = setup_fs(16 * 1024);
    mkfile(&mut dev, &mut fs, "/compat/file", Some(b"hello"), None).expect("create file");

    for path in ["/", "/compat/file", "/compat/missing"] {
        let corrected = file_entry_exist(&mut fs, &mut dev, path);
        let compatible = file_entry_exisr(&mut fs, &mut dev, path);
        assert_eq!(
            corrected.map_err(|err| err.code),
            compatible.map_err(|err| err.code),
            "path {path}"
        );
    }
}

#[test]
fn split_paren_child_and_translatevalid_matches_normalize_path() {
    for path in ["/", "//a///b/", "a//b//", "///"] {
        assert_eq!(
            normalize_path(path),
            split_paren_child_and_translatevalid(path),
            "path {path}"
        );
    }
}

#[test]
fn cloc_group_layout_matches_calc_group_layout() {
    let (_, fs) = setup_fs(16 * 1024);
    let sb = &fs.superblock;
    let inode_table_blocks =
        (sb.s_inodes_per_group * sb.s_inode_size as u32).div_ceil(BLOCK_SIZE as u32);

    for gid in [0, 1, 2, 3, 5, 7, 11] {
        let corrected = calc_group_layout(
            gid,
            sb,
            sb.s_blocks_per_group,
            inode_table_blocks,
            fs.group_descs[0].block_bitmap() as u32,
            fs.group_descs[0].inode_bitmap() as u32,
            fs.group_descs[0].inode_table() as u32,
            1,
        );
        let compatible = cloc_group_layout(
            gid,
            sb,
            sb.s_blocks_per_group,
            inode_table_blocks,
            fs.group_descs[0].block_bitmap() as u32,
            fs.group_descs[0].inode_bitmap() as u32,
            fs.group_descs[0].inode_table() as u32,
            1,
        );
        assert_layout_eq(corrected, compatible);
    }
}

#[test]
fn resolve_inode_block_allextend_matches_resolve_inode_blocks() {
    let (mut dev, mut fs) = setup_fs(16 * 1024);
    let inode = build_mapped_inode(&mut fs, &mut dev);
    let mut corrected_inode = inode;
    let mut compatible_inode = inode;

    let corrected = resolve_inode_blocks(&mut fs, &mut dev, &mut corrected_inode)
        .expect("resolve through corrected API");
    let compatible = resolve_inode_block_allextend(&mut fs, &mut dev, &mut compatible_inode)
        .expect("resolve through compatibility API");

    assert_eq!(corrected, compatible);
    assert_eq!(corrected.len(), 3);
}

#[test]
fn remove_extend_matches_remove_extent() {
    fn run_with(
        remove: impl FnOnce(
            &mut ExtentTree<'_>,
            &mut Ext4FileSystem,
            Ext4Extent,
            &mut Jbd2Dev<CompatBlockDevice>,
        ) -> Ext4Result<()>,
    ) -> RemovedExtentObservation {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();
        let base = alloc_contiguous(&mut fs, &mut dev, 4);
        let inserted = Ext4Extent::new(0, base.raw(), 4);
        ExtentTree::new(&mut inode)
            .insert_extent(&mut fs, inserted, &mut dev)
            .expect("insert extent");

        let deleted = Ext4Extent::new(1, 0, 2);
        remove(&mut ExtentTree::new(&mut inode), &mut fs, deleted, &mut dev)
            .expect("remove extent");

        let allocated = [
            base,
            base.checked_add(1).expect("base + 1"),
            base.checked_add(2).expect("base + 2"),
            base.checked_add(3).expect("base + 3"),
        ]
        .into_iter()
        .map(|block| {
            (
                block.raw(),
                bitmap_block_is_allocated(&mut fs, &mut dev, block),
            )
        })
        .collect();

        RemovedExtentObservation {
            extents: collect_extents(&mut inode, &mut dev),
            allocated_blocks: allocated,
        }
    }

    let corrected = run_with(|tree, fs, extent, dev| tree.remove_extent(fs, extent, dev));
    let compatible = run_with(|tree, fs, extent, dev| tree.remove_extend(fs, extent, dev));

    assert_eq!(corrected, compatible);
}
