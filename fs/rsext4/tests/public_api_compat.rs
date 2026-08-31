//! Behavioral regressions for the corrected public API names.

use std::{cell::Cell, collections::BTreeMap};

use rsext4::{
    bmalloc::AbsoluteBN,
    cache::bitmap::CacheKey,
    dir::normalize_path,
    disknode::{Ext4Extent, Ext4Inode},
    error::{Ext4Error, Ext4Result},
    extents_tree::{ExtentNode, ExtentTree},
    loopfile::resolve_inode_blocks,
    tool::calc_group_layout,
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

impl BlockIo for CompatBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + required;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                self.geometry().block_count,
            ));
        }
        buffer[..required].copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + required;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                self.geometry().block_count,
            ));
        }
        self.data[start..end].copy_from_slice(&buffer[..required]);
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.flush_count += 1;
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, {
            (self.data.len() / self.block_size as usize) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }
}

impl rsext4::Clock for CompatBlockDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn setup_fs(total_blocks: u64) -> (Jbd2Dev<CompatBlockDevice>, Ext4FileSystem) {
    let device = CompatBlockDevice::new(total_blocks);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, false);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
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
    fs: &Ext4FileSystem,
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
    let tree = ExtentTree::with_filesystem(inode, fs, fs.root_inode);
    if let Ok(root) = tree.load_root_from_inode() {
        walk(dev, &root, &mut out);
    }
    out.sort_unstable();
    out
}

fn build_mapped_inode(
    fs: &mut Ext4FileSystem,
    dev: &mut Jbd2Dev<CompatBlockDevice>,
) -> (Ext4Inode, AbsoluteBN, AbsoluteBN) {
    let mut inode = new_extent_inode();
    let first = alloc_contiguous(fs, dev, 2);
    let second = alloc_contiguous(fs, dev, 1);
    let mut tree = ExtentTree::with_filesystem(&mut inode, fs, fs.root_inode);
    tree.insert_extent(fs, Ext4Extent::new(0, first.raw(), 2), dev)
        .expect("insert first extent");
    tree.insert_extent(fs, Ext4Extent::new(4, second.raw(), 1), dev)
        .expect("insert second extent");
    (inode, first, second)
}

#[derive(Debug, PartialEq, Eq)]
struct RemovedExtentObservation {
    extents: Vec<(u32, u32, u64)>,
    allocated_blocks: BTreeMap<u64, bool>,
}

#[test]
fn flush_persists_cached_block_device_writes() {
    let payload = vec![0x5a; BLOCK_SIZE];
    let mut dev = Jbd2Dev::initial_jbd2dev(0, CompatBlockDevice::new(16), false);

    dev.write_blocks(&payload, AbsoluteBN::new(3), 1, false)
        .expect("write through cached device");
    dev.flush().expect("flush cached device");

    let inner = dev.into_inner();
    assert_eq!(inner.flush_count, 1);
    assert_eq!(&inner.data[3 * BLOCK_SIZE..4 * BLOCK_SIZE], &payload);
}

#[test]
fn path_exists_resolves_existing_and_missing_paths() {
    let (mut dev, mut fs) = setup_fs(16 * 1024);
    mkfile(&mut dev, &mut fs, "/compat/file", Some(b"hello"), None).expect("create file");

    assert!(fs.path_exists(&mut dev, "/").expect("resolve root"));
    assert!(
        fs.path_exists(&mut dev, "/compat/file")
            .expect("resolve file")
    );
    assert!(
        !fs.path_exists(&mut dev, "/compat/missing")
            .expect("resolve missing path")
    );
}

#[test]
fn normalize_path_collapses_separators_and_trims_trailing_slashes() {
    assert_eq!(normalize_path("/"), "/");
    assert_eq!(normalize_path("//a///b/"), "/a/b");
    assert_eq!(normalize_path("a//b//"), "a/b");
    assert_eq!(normalize_path("///"), "/");
}

#[test]
fn calc_group_layout_handles_primary_sparse_super_and_regular_groups() {
    let (_, fs) = setup_fs(16 * 1024);
    let sb = &fs.superblock;
    let inode_table_blocks =
        (sb.s_inodes_per_group * sb.s_inode_size as u32).div_ceil(BLOCK_SIZE as u32);
    let calculate = |gid| {
        calc_group_layout(
            gid,
            sb,
            sb.s_blocks_per_group,
            inode_table_blocks,
            fs.group_descs[0].block_bitmap() as u32,
            fs.group_descs[0].inode_bitmap() as u32,
            fs.group_descs[0].inode_table() as u32,
            1,
        )
    };

    let primary = calculate(0);
    assert_eq!(primary.group_start_block, u64::from(sb.s_first_data_block));
    assert_eq!(
        primary.group_block_bitmap_start_block,
        fs.group_descs[0].block_bitmap()
    );
    assert_eq!(
        primary.group_inode_bitmap_start_block,
        fs.group_descs[0].inode_bitmap()
    );
    assert_eq!(
        primary.group_inode_table_start_block,
        fs.group_descs[0].inode_table()
    );

    let backup = calculate(1);
    let backup_start = u64::from(sb.s_first_data_block) + u64::from(sb.s_blocks_per_group);
    assert_eq!(backup.group_start_block, backup_start);
    assert_eq!(backup.group_block_bitmap_start_block, backup_start + 2);
    assert_eq!(backup.group_inode_bitmap_start_block, backup_start + 3);
    assert_eq!(backup.group_inode_table_start_block, backup_start + 4);
    assert_eq!(backup.metadata_blocks_in_group, inode_table_blocks + 4);

    let regular = calculate(2);
    let regular_start = u64::from(sb.s_first_data_block) + 2 * u64::from(sb.s_blocks_per_group);
    assert_eq!(regular.group_start_block, regular_start);
    assert_eq!(regular.group_block_bitmap_start_block, regular_start);
    assert_eq!(regular.group_inode_bitmap_start_block, regular_start + 1);
    assert_eq!(regular.group_inode_table_start_block, regular_start + 2);
    assert_eq!(regular.metadata_blocks_in_group, inode_table_blocks + 2);
}

#[test]
fn resolve_inode_blocks_returns_all_initialized_extent_mappings() {
    let (mut dev, mut fs) = setup_fs(16 * 1024);
    let (mut inode, first, second) = build_mapped_inode(&mut fs, &mut dev);

    let inode_num = fs.root_inode;
    let resolved = resolve_inode_blocks(&mut fs, &mut dev, inode_num, &mut inode)
        .expect("resolve initialized mappings");

    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved.get(&0), Some(&first));
    assert_eq!(
        resolved.get(&1),
        Some(&first.checked_add(1).expect("first + 1"))
    );
    assert_eq!(resolved.get(&4), Some(&second));
}

#[test]
fn remove_extent_updates_tree_and_bitmap() {
    fn run() -> (RemovedExtentObservation, AbsoluteBN) {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();
        let base = alloc_contiguous(&mut fs, &mut dev, 4);
        let inserted = Ext4Extent::new(0, base.raw(), 4);
        ExtentTree::with_filesystem(&mut inode, &fs, fs.root_inode)
            .insert_extent(&mut fs, inserted, &mut dev)
            .expect("insert extent");
        inode
            .set_blocks_count(4 * (BLOCK_SIZE as u64 / 512), BLOCK_SIZE as u32, true)
            .expect("account inserted data blocks");

        let deleted = Ext4Extent::new(1, 0, 2);
        ExtentTree::with_filesystem(&mut inode, &fs, fs.root_inode)
            .remove_extent(&mut fs, deleted, &mut dev)
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

        (
            RemovedExtentObservation {
                extents: collect_extents(&fs, &mut inode, &mut dev),
                allocated_blocks: allocated,
            },
            base,
        )
    }

    let (observation, base) = run();
    assert_eq!(
        observation.extents,
        vec![
            (0, 1, base.raw()),
            (3, 1, base.checked_add(3).expect("base + 3").raw()),
        ]
    );
    assert_eq!(observation.allocated_blocks.get(&base.raw()), Some(&true));
    assert_eq!(
        observation
            .allocated_blocks
            .get(&base.checked_add(1).expect("base + 1").raw()),
        Some(&false)
    );
    assert_eq!(
        observation
            .allocated_blocks
            .get(&base.checked_add(2).expect("base + 2").raw()),
        Some(&false)
    );
    assert_eq!(
        observation
            .allocated_blocks
            .get(&base.checked_add(3).expect("base + 3").raw()),
        Some(&true)
    );
}
