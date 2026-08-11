//! Linux-compatible unwritten extent semantics.

use std::{cell::Cell, rc::Rc};

use rsext4::{
    BLOCK_SIZE, BlockIo, Clock, DeviceCapabilities, DeviceGeometry, Ext4Error, Ext4Result,
    Ext4Timestamp, Jbd2Dev, PreallocationOptions, SectorId, ZeroRangeOptions, dir,
    disknode::{Ext4Extent, Ext4ExtentHeader},
    extents_tree::{ExtentBlockMapping, ExtentNode, ExtentTree},
    mkfile, mkfs, mount, operate_inode_range, preallocate_inode, punch_hole_inode, read_file,
    read_inode_data_into,
    superblock::Ext4Superblock,
    truncate_inode, write_file, zero_range_inode,
};

struct MemoryDevice {
    bytes: Vec<u8>,
    now: Cell<i64>,
    fail_write: Rc<Cell<Option<(u64, u32)>>>,
}

struct StaticClock(Cell<i64>);

impl Clock for StaticClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.0.get();
        self.0.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            bytes: vec![0; blocks * BLOCK_SIZE],
            now: Cell::new(1_700_000_000),
            fail_write: Rc::new(Cell::new(None)),
        }
    }

    fn with_write_failure(blocks: usize) -> (Self, Rc<Cell<Option<(u64, u32)>>>) {
        let device = Self::new(blocks);
        let failure = Rc::clone(&device.fail_write);
        (device, failure)
    }
}

impl BlockIo for MemoryDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        let sector_u32 = sector.to_u32()?;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let source = self.bytes.get(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(sector_u32, self.geometry().block_count)
        })?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        if let Some((target, remaining)) = self.fail_write.get()
            && target == sector.raw()
        {
            if remaining <= 1 {
                self.fail_write.set(None);
                return Err(Ext4Error::io());
            }
            self.fail_write.set(Some((target, remaining - 1)));
        }
        let start = sector.as_usize()? * BLOCK_SIZE;
        let sector_u32 = sector.to_u32()?;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let total_sectors = self.geometry().block_count;
        let destination = self
            .bytes
            .get_mut(start..end)
            .ok_or_else(|| Ext4Error::block_out_of_range(sector_u32, total_sectors))?;
        destination.copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(BLOCK_SIZE as u32, (self.bytes.len() / BLOCK_SIZE) as u64)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

impl Clock for MemoryDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.now.get();
        self.now.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

#[test]
fn partial_write_converts_only_the_written_unwritten_block() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/preallocated", None, None)
        .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/preallocated")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;

    let first = filesystem
        .alloc_block(&mut journal)
        .expect("allocation failed");
    let second = filesystem
        .alloc_block(&mut journal)
        .expect("allocation failed");
    let third = filesystem
        .alloc_block(&mut journal)
        .expect("allocation failed");
    assert_eq!(second.raw(), first.raw() + 1);
    assert_eq!(third.raw(), first.raw() + 2);
    for block in [first, second, third] {
        filesystem
            .datablock_cache
            .modify_new(&mut journal, block, |contents| contents.fill(0xa5))
            .expect("stale-data injection failed");
    }

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    let mut unwritten = Ext4Extent::new(0, first.raw(), 3);
    unwritten.ee_len = Ext4Extent::encode_len(3, true).expect("valid unwritten length");
    ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .insert_extent(&mut filesystem, unwritten, &mut journal)
        .expect("unwritten extent insertion failed");
    inode.i_size_lo = (3 * BLOCK_SIZE) as u32;
    inode.i_size_high = 0;
    inode.i_blocks_lo = (3 * BLOCK_SIZE / 512) as u32;
    inode.l_i_blocks_high = 0;
    filesystem
        .modify_inode(&mut journal, inode_number, |on_disk| *on_disk = inode)
        .expect("inode publication failed");

    let before =
        read_file(&mut journal, &mut filesystem, "/preallocated").expect("unwritten read failed");
    assert!(before.iter().all(|byte| *byte == 0));
    let free_blocks_before_write = filesystem.superblock.free_blocks_count();
    let blocks_before_write = inode.i_blocks_lo;

    let payload = b"initialized middle";
    let write_offset = BLOCK_SIZE as u64 + 123;
    write_file(
        &mut journal,
        &mut filesystem,
        "/preallocated",
        write_offset,
        payload,
    )
    .expect("writing an unwritten extent failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated inode read failed");
    assert_eq!(inode.i_blocks_lo, blocks_before_write);
    assert_eq!(
        filesystem.superblock.free_blocks_count(),
        free_blocks_before_write
    );
    let mut tree = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number);
    let left = tree
        .find_extent(&mut journal, 0)
        .expect("left lookup failed")
        .expect("left extent missing");
    let middle = tree
        .find_extent(&mut journal, 1)
        .expect("middle lookup failed")
        .expect("middle extent missing");
    let right = tree
        .find_extent(&mut journal, 2)
        .expect("right lookup failed")
        .expect("right extent missing");
    assert!(left.is_unwritten());
    assert_eq!(left.ee_block, 0);
    assert_eq!(left.len(), 1);
    assert_eq!(left.start_block(), first.raw());
    assert!(middle.is_initialized());
    assert_eq!(middle.ee_block, 1);
    assert_eq!(middle.len(), 1);
    assert_eq!(middle.start_block(), second.raw());
    assert!(right.is_unwritten());
    assert_eq!(right.ee_block, 2);
    assert_eq!(right.len(), 1);
    assert_eq!(right.start_block(), third.raw());

    let after = read_file(&mut journal, &mut filesystem, "/preallocated")
        .expect("post-conversion read failed");
    assert!(after[..BLOCK_SIZE + 123].iter().all(|byte| *byte == 0));
    assert_eq!(
        &after[BLOCK_SIZE + 123..BLOCK_SIZE + 123 + payload.len()],
        payload
    );
    assert!(
        after[BLOCK_SIZE + 123 + payload.len()..]
            .iter()
            .all(|byte| *byte == 0)
    );
}

#[test]
fn preallocation_reserves_unwritten_blocks_and_honors_keep_size() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/reserve", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/reserve")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let free_before = filesystem.superblock.free_blocks_count();

    preallocate_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        BLOCK_SIZE as u64 + 123,
        BLOCK_SIZE as u64,
        PreallocationOptions::KEEP_SIZE,
    )
    .expect("KEEP_SIZE preallocation failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    assert_eq!(inode.size(), 0);
    assert_eq!(inode.i_blocks_lo, (2 * BLOCK_SIZE / 512) as u32);
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before - 2);
    let mut tree = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number);
    let reserved = tree
        .find_extent(&mut journal, 1)
        .expect("extent lookup failed")
        .expect("reserved extent missing");
    assert!(reserved.is_unwritten());
    assert_eq!(reserved.ee_block, 1);
    assert_eq!(reserved.len(), 2);

    truncate_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        3 * BLOCK_SIZE as u64,
    )
    .expect("sparse size publication failed");
    let zeros =
        read_file(&mut journal, &mut filesystem, "/reserve").expect("reserved extent read failed");
    assert_eq!(zeros.len(), 3 * BLOCK_SIZE);
    assert!(zeros.iter().all(|byte| *byte == 0));

    preallocate_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        4 * BLOCK_SIZE as u64,
        2 * BLOCK_SIZE as u64,
        PreallocationOptions::EXTEND_SIZE,
    )
    .expect("size-extending preallocation failed");
    let inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated inode read failed");
    assert_eq!(inode.size(), 6 * BLOCK_SIZE as u64);
    assert_eq!(inode.i_blocks_lo, (4 * BLOCK_SIZE / 512) as u32);
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before - 4);
    let zeros = read_file(&mut journal, &mut filesystem, "/reserve")
        .expect("extended preallocation read failed");
    assert_eq!(zeros.len(), 6 * BLOCK_SIZE);
    assert!(zeros.iter().all(|byte| *byte == 0));
}

#[test]
fn unwritten_middle_split_grows_a_full_inline_root_before_data_io() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/root-split", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/root-split")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let mut blocks = Vec::new();
    for _ in 0..6 {
        blocks.push(
            filesystem
                .alloc_block(&mut journal)
                .expect("allocation failed"),
        );
    }
    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    let root = ExtentNode::Leaf {
        header: Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 4,
            eh_max: 4,
            eh_depth: 0,
            eh_generation: 0,
        },
        entries: vec![
            Ext4Extent::new(0, blocks[0].raw(), 1),
            Ext4Extent::new(2, blocks[1].raw(), 1),
            Ext4Extent::new_unwritten(10, blocks[2].raw(), 3).expect("valid unwritten extent"),
            Ext4Extent::new(20, blocks[5].raw(), 1),
        ],
    };
    ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .store_root_to_inode(&root)
        .expect("inline root construction failed");
    inode.i_size_lo = (21 * BLOCK_SIZE) as u32;
    inode.i_blocks_lo = (6 * BLOCK_SIZE / 512) as u32;
    filesystem
        .modify_inode(&mut journal, inode_number, |on_disk| *on_disk = inode)
        .expect("inode publication failed");
    let free_before_write = filesystem.superblock.free_blocks_count();

    write_file(
        &mut journal,
        &mut filesystem,
        "/root-split",
        11 * BLOCK_SIZE as u64 + 7,
        b"split",
    )
    .expect("unwritten root split write failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated inode read failed");
    assert_eq!(
        inode.i_blocks_lo,
        (8 * BLOCK_SIZE / 512) as u32,
        "two extent-tree blocks must be counted"
    );
    assert_eq!(
        filesystem.superblock.free_blocks_count(),
        free_before_write - 2,
        "conversion may allocate metadata but must reuse all data blocks"
    );
    let tree = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number);
    let root = tree
        .load_root_from_inode()
        .expect("updated root parse failed");
    match root {
        ExtentNode::Index { header, entries } => {
            assert_eq!(header.eh_depth, 1);
            assert_eq!(entries.len(), 2);
        }
        ExtentNode::Leaf { .. } => panic!("full inline root must have split"),
    }
    let mut output = [0x55; 32];
    let copied = read_inode_data_into(
        &mut journal,
        &mut filesystem,
        inode_number,
        11 * BLOCK_SIZE as u64,
        &mut output,
    )
    .expect("converted block read failed");
    assert_eq!(copied, output.len());
    assert_eq!(&output[..7], &[0; 7]);
    assert_eq!(&output[7..12], b"split");
    assert_eq!(&output[12..], &[0; 20]);
}

#[test]
fn failed_finish_restores_external_leaf_to_unwritten() {
    let (device, fail_write) = MemoryDevice::with_write_failure(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/finish-failure", None, None)
        .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/finish-failure")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let mut blocks = Vec::new();
    for _ in 0..6 {
        blocks.push(
            filesystem
                .alloc_block(&mut journal)
                .expect("allocation failed"),
        );
    }
    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .store_root_to_inode(&ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 4,
                eh_max: 4,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: vec![
                Ext4Extent::new(0, blocks[0].raw(), 1),
                Ext4Extent::new(2, blocks[1].raw(), 1),
                Ext4Extent::new_unwritten(10, blocks[2].raw(), 3).expect("valid unwritten extent"),
                Ext4Extent::new(20, blocks[5].raw(), 1),
            ],
        })
        .expect("inline root construction failed");
    inode.i_size_lo = (21 * BLOCK_SIZE) as u32;
    inode.i_blocks_lo = (6 * BLOCK_SIZE / 512) as u32;
    filesystem
        .modify_inode(&mut journal, inode_number, |on_disk| *on_disk = inode)
        .expect("inode publication failed");
    write_file(
        &mut journal,
        &mut filesystem,
        "/finish-failure",
        11 * BLOCK_SIZE as u64 + 7,
        b"first",
    )
    .expect("root split setup write failed");

    journal.umount_commit().expect("commit split tree failed");
    journal
        .set_journal_use(false)
        .expect("disable journal for direct failure injection");
    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("split inode read failed");
    let root = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .load_root_from_inode()
        .expect("split root read failed");
    let leaf_block = match root {
        ExtentNode::Index { entries, .. } => {
            let index = entries
                .iter()
                .rev()
                .find(|index| index.ei_block <= 12)
                .expect("leaf index for logical block 12");
            (u64::from(index.ei_leaf_hi) << 32) | u64::from(index.ei_leaf_lo)
        }
        ExtentNode::Leaf { .. } => panic!("setup must create an external leaf"),
    };
    let free_before = filesystem.superblock.free_blocks_count();
    // The first leaf write is prepare (still unwritten); fail the second leaf
    // write, which attempts to publish initialized state after data I/O.
    fail_write.set(Some((leaf_block, 2)));
    let error = write_file(
        &mut journal,
        &mut filesystem,
        "/finish-failure",
        12 * BLOCK_SIZE as u64 + 9,
        b"must-remain-hidden",
    )
    .expect_err("finish metadata failure must propagate");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::Io);
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before);

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("restored inode read failed");
    let extent = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .find_extent(&mut journal, 12)
        .expect("restored extent lookup failed")
        .expect("restored extent missing");
    assert!(extent.is_unwritten());
    let mut output = [0x55; 64];
    let copied = read_inode_data_into(
        &mut journal,
        &mut filesystem,
        inode_number,
        12 * BLOCK_SIZE as u64,
        &mut output,
    )
    .expect("read after failed finish");
    assert_eq!(copied, output.len());
    assert_eq!(output, [0; 64]);
}

#[test]
fn owned_core_exposes_preallocation_without_os_types() {
    let device = rsext4::format(
        MemoryDevice::new(32 * 1024),
        StaticClock(Cell::new(1_700_000_000)),
        rsext4::MkfsOptions::default(),
    )
    .expect("owned format failed");
    let services = rsext4::MountServices::new(
        StaticClock(Cell::new(1_800_000_000)),
        (),
        (),
        (),
        rsext4::NoopObserver,
    );
    let mut filesystem = rsext4::Ext4::mount(device, services, rsext4::MountOptions::read_write())
        .expect("owned mount failed");
    let context = rsext4::MutationContext::new(1000, 1000, 0, 0o022);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            rsext4::FileName::new(b"owned-reserve").expect("valid file name"),
            rsext4::FilePermissions::new(0o666).expect("valid permissions"),
        )
        .expect("owned file creation failed");

    filesystem
        .preallocate_inode(
            context,
            file.number,
            0,
            2 * BLOCK_SIZE as u64,
            PreallocationOptions::KEEP_SIZE,
        )
        .expect("owned KEEP_SIZE preallocation failed");
    let info = filesystem
        .inode(file.number)
        .expect("owned inode inspection failed");
    assert_eq!(info.size, 0);
    assert_eq!(info.blocks, (2 * BLOCK_SIZE / 512) as u64);

    filesystem
        .preallocate_inode(
            context,
            file.number,
            2 * BLOCK_SIZE as u64,
            BLOCK_SIZE as u64,
            PreallocationOptions::EXTEND_SIZE,
        )
        .expect("owned size-extending preallocation failed");
    let info = filesystem
        .inode(file.number)
        .expect("updated owned inode inspection failed");
    assert_eq!(info.size, 3 * BLOCK_SIZE as u64);
    assert_eq!(info.blocks, (3 * BLOCK_SIZE / 512) as u64);
    let mut output = vec![0x55; 3 * BLOCK_SIZE];
    let copied = filesystem
        .read_inode(file.number, 0, &mut output)
        .expect("owned preallocated read failed");
    assert_eq!(copied, output.len());
    assert!(output.iter().all(|byte| *byte == 0));
    filesystem.unmount().expect("owned unmount failed");
}

#[test]
fn owned_core_reports_sparse_and_unwritten_file_extents() {
    let device = rsext4::format(
        MemoryDevice::new(32 * 1024),
        StaticClock(Cell::new(1_700_000_000)),
        rsext4::MkfsOptions::default(),
    )
    .expect("owned format failed");
    let services = rsext4::MountServices::new(
        StaticClock(Cell::new(1_800_000_000)),
        (),
        (),
        (),
        rsext4::NoopObserver,
    );
    let mut filesystem = rsext4::Ext4::mount(device, services, rsext4::MountOptions::read_write())
        .expect("owned mount failed");
    let context = rsext4::MutationContext::new(1000, 1000, 0, 0o022);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            rsext4::FileName::new(b"owned-extents").expect("valid file name"),
            rsext4::FilePermissions::new(0o666).expect("valid permissions"),
        )
        .expect("owned file creation failed");
    filesystem
        .write_inode(context, file.number, 0, &vec![0x11; BLOCK_SIZE])
        .expect("first extent write failed");
    filesystem
        .write_inode(
            context,
            file.number,
            2 * BLOCK_SIZE as u64,
            &vec![0x22; BLOCK_SIZE],
        )
        .expect("sparse extent write failed");
    filesystem
        .preallocate_inode(
            context,
            file.number,
            4 * BLOCK_SIZE as u64,
            BLOCK_SIZE as u64,
            PreallocationOptions::EXTEND_SIZE,
        )
        .expect("unwritten extent allocation failed");

    let mappings = filesystem
        .inode_extents(
            file.number,
            BLOCK_SIZE as u64 / 2,
            5 * BLOCK_SIZE as u64,
            rsext4::FileExtentTarget::Data,
            8,
        )
        .expect("file extent inspection failed");
    assert_eq!(mappings.mapped_extents, 3);
    assert!(mappings.complete);
    assert_eq!(mappings.extents.len(), 3);
    assert_eq!(mappings.extents[0].logical_start, 0);
    assert_eq!(mappings.extents[0].length, BLOCK_SIZE as u64);
    assert_eq!(
        mappings.extents[0].state,
        rsext4::FileExtentState::Initialized
    );
    assert_eq!(mappings.extents[1].logical_start, 2 * BLOCK_SIZE as u64);
    assert_eq!(
        mappings.extents[1].state,
        rsext4::FileExtentState::Initialized
    );
    assert_eq!(mappings.extents[2].logical_start, 4 * BLOCK_SIZE as u64);
    assert_eq!(
        mappings.extents[2].state,
        rsext4::FileExtentState::Unwritten
    );

    let count_only = filesystem
        .inode_extents(file.number, 0, u64::MAX, rsext4::FileExtentTarget::Data, 0)
        .expect("count-only extent inspection failed");
    assert_eq!(count_only.mapped_extents, 3);
    assert!(count_only.extents.is_empty());
    assert!(count_only.complete);

    let bounded = filesystem
        .inode_extents(file.number, 0, u64::MAX, rsext4::FileExtentTarget::Data, 2)
        .expect("bounded extent inspection failed");
    assert_eq!(bounded.mapped_extents, 2);
    assert_eq!(bounded.extents.len(), 2);
    assert!(!bounded.complete);

    let zero_length = filesystem
        .inode_extents(file.number, 0, 0, rsext4::FileExtentTarget::Data, 1)
        .expect_err("zero-length extent query must fail");
    assert_eq!(zero_length.kind(), rsext4::Ext4ErrorKind::InvalidInput);

    let zero_length_xattr = filesystem
        .inode_extents(
            file.number,
            0,
            0,
            rsext4::FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect_err("range validation must precede xattr capability rejection");
    assert_eq!(
        zero_length_xattr.kind(),
        rsext4::Ext4ErrorKind::InvalidInput
    );

    let unsupported_xattr = filesystem
        .inode_extents(
            file.number,
            0,
            u64::MAX,
            rsext4::FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect_err("xattr extent inspection is not implemented yet");
    assert_eq!(unsupported_xattr.kind(), rsext4::Ext4ErrorKind::Unsupported);

    let directory_mappings = filesystem
        .inode_extents(
            filesystem.root_inode(),
            0,
            u64::MAX,
            rsext4::FileExtentTarget::Data,
            8,
        )
        .expect("directory inode extent inspection failed");
    assert!(!directory_mappings.extents.is_empty());
    assert!(directory_mappings.complete);
    assert!(
        directory_mappings
            .extents
            .iter()
            .all(|extent| extent.state == rsext4::FileExtentState::Initialized)
    );

    let maximum_extent_bytes = u64::from(u32::MAX) * BLOCK_SIZE as u64;
    let error = filesystem
        .inode_extents(
            file.number,
            maximum_extent_bytes,
            1,
            rsext4::FileExtentTarget::Data,
            1,
        )
        .expect_err("extent FIEMAP at the ee_block limit must fail");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::FileTooLarge);
}

#[test]
fn file_extent_inspection_merges_legacy_indirect_block_runs() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/legacy-map", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/legacy-map")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    filesystem
        .modify_inode(&mut journal, inode_number, |inode| {
            inode.i_flags &= !rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
        })
        .expect("legacy inode conversion failed");
    write_file(
        &mut journal,
        &mut filesystem,
        "/legacy-map",
        0,
        &vec![0x5a; 15 * BLOCK_SIZE],
    )
    .expect("legacy write failed");

    let mappings = rsext4::inspect_inode_extents(
        &mut journal,
        &mut filesystem,
        inode_number,
        0,
        u64::MAX,
        rsext4::FileExtentTarget::Data,
        usize::MAX,
    )
    .expect("legacy extent inspection failed");
    assert!(mappings.complete);
    assert_eq!(mappings.mapped_extents, mappings.extents.len());
    assert!(!mappings.extents.is_empty());
    assert!(mappings.extents.iter().all(|extent| extent.merged));
    assert!(
        mappings
            .extents
            .iter()
            .all(|extent| { extent.state == rsext4::FileExtentState::Initialized })
    );
    assert_eq!(mappings.extents[0].logical_start, 0);
    assert_eq!(
        mappings
            .extents
            .iter()
            .map(|extent| extent.length)
            .sum::<u64>(),
        15 * BLOCK_SIZE as u64
    );
    for pair in mappings.extents.windows(2) {
        assert_eq!(
            pair[0].logical_start + pair[0].length,
            pair[1].logical_start
        );
    }

    let pointers = (BLOCK_SIZE / core::mem::size_of::<u32>()) as u64;
    let maximum_legacy_blocks = 12 + pointers + pointers.pow(2) + pointers.pow(3);
    let error = rsext4::inspect_inode_extents(
        &mut journal,
        &mut filesystem,
        inode_number,
        maximum_legacy_blocks * BLOCK_SIZE as u64,
        1,
        rsext4::FileExtentTarget::Data,
        1,
    )
    .expect_err("legacy FIEMAP at the indirect-tree limit must fail");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::FileTooLarge);
}

#[test]
fn punch_hole_zeros_partial_edges_and_releases_complete_blocks() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/punch", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/punch")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let original = vec![0xa5; 5 * BLOCK_SIZE];
    write_file(&mut journal, &mut filesystem, "/punch", 0, &original)
        .expect("initial write failed");
    let before = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    let free_before = filesystem.superblock.free_blocks_count();
    let offset = BLOCK_SIZE as u64 + 123;
    let len = 2 * BLOCK_SIZE as u64;

    punch_hole_inode(&mut journal, &mut filesystem, inode_number, offset, len)
        .expect("punch hole failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated inode read failed");
    assert_eq!(inode.size(), before.size());
    assert_eq!(
        inode.i_blocks_lo,
        before.i_blocks_lo - (BLOCK_SIZE / 512) as u32
    );
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before + 1);
    assert_eq!(
        ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
            .map_block(&mut journal, 2)
            .expect("punched mapping lookup failed"),
        ExtentBlockMapping::Hole
    );
    let after =
        read_file(&mut journal, &mut filesystem, "/punch").expect("punched file read failed");
    let end = usize::try_from(offset + len).expect("test range fits usize");
    let start = usize::try_from(offset).expect("test range fits usize");
    assert_eq!(&after[..start], &original[..start]);
    assert!(after[start..end].iter().all(|byte| *byte == 0));
    assert_eq!(&after[end..], &original[end..]);
}

#[test]
fn zero_range_zeros_partial_edges_but_keeps_unwritten_allocation() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/zero", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/zero")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let original = vec![0x5a; 5 * BLOCK_SIZE];
    write_file(&mut journal, &mut filesystem, "/zero", 0, &original).expect("initial write failed");
    let before = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    let free_before = filesystem.superblock.free_blocks_count();
    let offset = BLOCK_SIZE as u64 + 123;
    let len = 2 * BLOCK_SIZE as u64;

    zero_range_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        offset,
        len,
        ZeroRangeOptions::KEEP_SIZE,
    )
    .expect("zero range failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated inode read failed");
    assert_eq!(inode.size(), before.size());
    assert_eq!(inode.i_blocks_lo, before.i_blocks_lo);
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before);
    assert!(matches!(
        ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
            .map_block(&mut journal, 2)
            .expect("zeroed mapping lookup failed"),
        ExtentBlockMapping::Unwritten(_)
    ));
    let after = read_file(&mut journal, &mut filesystem, "/zero").expect("zeroed file read failed");
    let end = usize::try_from(offset + len).expect("test range fits usize");
    let start = usize::try_from(offset).expect("test range fits usize");
    assert_eq!(&after[..start], &original[..start]);
    assert!(after[start..end].iter().all(|byte| *byte == 0));
    assert_eq!(&after[end..], &original[end..]);
}

#[test]
fn punch_hole_prunes_a_finite_range_across_legacy_direct_and_indirect_blocks() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/legacy-punch", None, None)
        .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/legacy-punch")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    filesystem
        .modify_inode(&mut journal, inode_number, |inode| {
            inode.i_flags &= !rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
        })
        .expect("legacy inode conversion failed");
    let original = vec![0x3c; 15 * BLOCK_SIZE];
    write_file(&mut journal, &mut filesystem, "/legacy-punch", 0, &original)
        .expect("legacy write failed");
    let before = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("legacy inode read failed");
    assert_ne!(
        before.i_block[12], 0,
        "fixture needs a single-indirect root"
    );
    let free_before = filesystem.superblock.free_blocks_count();

    punch_hole_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        10 * BLOCK_SIZE as u64,
        4 * BLOCK_SIZE as u64,
    )
    .expect("legacy punch failed");

    let inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("updated legacy inode read failed");
    assert_eq!(inode.size(), before.size());
    assert_eq!(
        inode.i_blocks_lo,
        before.i_blocks_lo - (4 * BLOCK_SIZE / 512) as u32
    );
    assert_eq!(inode.i_block[10], 0);
    assert_eq!(inode.i_block[11], 0);
    assert_ne!(inode.i_block[12], 0, "remaining block 14 needs the root");
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before + 4);
    let after = read_file(&mut journal, &mut filesystem, "/legacy-punch")
        .expect("legacy punched read failed");
    assert_eq!(&after[..10 * BLOCK_SIZE], &original[..10 * BLOCK_SIZE]);
    assert!(
        after[10 * BLOCK_SIZE..14 * BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(&after[14 * BLOCK_SIZE..], &original[14 * BLOCK_SIZE..]);
}

#[test]
fn truncating_preallocated_unwritten_extents_releases_their_blocks() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(
        &mut journal,
        &mut filesystem,
        "/truncate-unwritten",
        None,
        None,
    )
    .expect("file creation failed");
    let inode_number =
        dir::get_inode_with_num(&mut filesystem, &mut journal, "/truncate-unwritten")
            .expect("lookup failed")
            .expect("created inode missing")
            .0;
    let free_before = filesystem.superblock.free_blocks_count();
    preallocate_inode(
        &mut journal,
        &mut filesystem,
        inode_number,
        0,
        4 * BLOCK_SIZE as u64,
        PreallocationOptions::EXTEND_SIZE,
    )
    .expect("unwritten preallocation failed");
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before - 4);

    truncate_inode(&mut journal, &mut filesystem, inode_number, 0)
        .expect("truncate unwritten extents failed");

    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("truncated inode read failed");
    assert_eq!(inode.size(), 0);
    assert_eq!(inode.i_blocks_lo, 0);
    assert_eq!(filesystem.superblock.free_blocks_count(), free_before);
    assert_eq!(
        ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
            .map_block(&mut journal, 0)
            .expect("truncated mapping lookup failed"),
        ExtentBlockMapping::Hole
    );
}

#[test]
fn collapse_range_removes_blocks_and_shifts_later_extents_left() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/collapse", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/collapse")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let mut original = vec![0; 4 * BLOCK_SIZE];
    for (index, block) in original.chunks_exact_mut(BLOCK_SIZE).enumerate() {
        block.fill(index as u8 + 1);
    }
    write_file(&mut journal, &mut filesystem, "/collapse", 0, &original)
        .expect("initial write failed");

    operate_inode_range(
        &mut journal,
        &mut filesystem,
        inode_number,
        BLOCK_SIZE as u64,
        BLOCK_SIZE as u64,
        rsext4::RangeOperation::Collapse,
    )
    .expect("collapse range failed");

    let after =
        read_file(&mut journal, &mut filesystem, "/collapse").expect("collapsed file read failed");
    assert_eq!(after.len(), 3 * BLOCK_SIZE);
    assert_eq!(&after[..BLOCK_SIZE], &original[..BLOCK_SIZE]);
    assert_eq!(
        &after[BLOCK_SIZE..],
        &original[2 * BLOCK_SIZE..4 * BLOCK_SIZE]
    );
}

#[test]
fn insert_range_creates_a_hole_and_shifts_later_extents_right() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/insert", None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/insert")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let mut original = vec![0; 3 * BLOCK_SIZE];
    for (index, block) in original.chunks_exact_mut(BLOCK_SIZE).enumerate() {
        block.fill(index as u8 + 1);
    }
    write_file(&mut journal, &mut filesystem, "/insert", 0, &original)
        .expect("initial write failed");

    operate_inode_range(
        &mut journal,
        &mut filesystem,
        inode_number,
        BLOCK_SIZE as u64,
        BLOCK_SIZE as u64,
        rsext4::RangeOperation::Insert,
    )
    .expect("insert range failed");

    let after =
        read_file(&mut journal, &mut filesystem, "/insert").expect("inserted file read failed");
    assert_eq!(after.len(), 4 * BLOCK_SIZE);
    assert_eq!(&after[..BLOCK_SIZE], &original[..BLOCK_SIZE]);
    assert!(
        after[BLOCK_SIZE..2 * BLOCK_SIZE]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(&after[2 * BLOCK_SIZE..], &original[BLOCK_SIZE..]);
}

#[test]
fn collapse_range_requires_bigalloc_cluster_alignment() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(
        &mut journal,
        &mut filesystem,
        "/collapse-cluster",
        None,
        None,
    )
    .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/collapse-cluster")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    write_file(
        &mut journal,
        &mut filesystem,
        "/collapse-cluster",
        0,
        &vec![0x5a; 4 * BLOCK_SIZE],
    )
    .expect("initial write failed");
    filesystem.superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_BIGALLOC;
    filesystem.superblock.s_log_cluster_size = filesystem.superblock.s_log_block_size + 1;
    filesystem.superblock.s_clusters_per_group /= 2;

    let error = operate_inode_range(
        &mut journal,
        &mut filesystem,
        inode_number,
        BLOCK_SIZE as u64,
        BLOCK_SIZE as u64,
        rsext4::RangeOperation::Collapse,
    )
    .expect_err("block-aligned but cluster-unaligned collapse must fail");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::InvalidInput);
    assert_eq!(
        error.context(),
        Some(rsext4::ErrorContext::Operation {
            op: "fallocate:collapse_alignment"
        })
    );
}

#[test]
fn insert_range_requires_bigalloc_cluster_alignment() {
    let device = MemoryDevice::new(32 * 1024);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, "/insert-cluster", None, None)
        .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/insert-cluster")
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    write_file(
        &mut journal,
        &mut filesystem,
        "/insert-cluster",
        0,
        &vec![0xa5; 4 * BLOCK_SIZE],
    )
    .expect("initial write failed");
    filesystem.superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_BIGALLOC;
    filesystem.superblock.s_log_cluster_size = filesystem.superblock.s_log_block_size + 1;
    filesystem.superblock.s_clusters_per_group /= 2;

    let error = operate_inode_range(
        &mut journal,
        &mut filesystem,
        inode_number,
        BLOCK_SIZE as u64,
        BLOCK_SIZE as u64,
        rsext4::RangeOperation::Insert,
    )
    .expect_err("block-aligned but cluster-unaligned insert must fail");
    assert_eq!(error.kind(), rsext4::Ext4ErrorKind::InvalidInput);
    assert_eq!(
        error.context(),
        Some(rsext4::ErrorContext::Operation {
            op: "fallocate:insert_alignment"
        })
    );
}
