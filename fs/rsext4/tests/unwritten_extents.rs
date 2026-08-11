//! Linux-compatible unwritten extent semantics.

use std::{cell::Cell, rc::Rc};

use rsext4::{
    BLOCK_SIZE, BlockIo, Clock, DeviceCapabilities, DeviceGeometry, Ext4Error, Ext4Result,
    Ext4Timestamp, Jbd2Dev, PreallocationOptions, SectorId, dir,
    disknode::{Ext4Extent, Ext4ExtentHeader},
    extents_tree::{ExtentNode, ExtentTree},
    mkfile, mkfs, mount, preallocate_inode, read_file, read_inode_data_into, truncate_inode,
    write_file,
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
