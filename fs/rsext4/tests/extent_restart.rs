//! Transaction restart and crash recovery for large extent removals.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

use rsext4::{
    BLOCK_SIZE, BlockIo, Clock, DeviceCapabilities, DeviceGeometry, Ext4Error, Ext4ErrorKind,
    Ext4Result, Ext4Timestamp, Jbd2Dev, SectorId,
    bmalloc::{AbsoluteBN, InodeNumber},
    cache::bitmap::CacheKey,
    dir,
    disknode::{Ext4Extent, Ext4Inode},
    ext4::Ext4FileSystem,
    extents_tree::{ExtentNode, ExtentTree},
    jbd2::jbdstruct::{
        JBD2_BLOCKTYPE_COMMIT, JBD2_BLOCKTYPE_SUPERBLOCK_V1, JBD2_MAGIC, JournalSuperBlock,
    },
    mkfile, mkfs, mount, punch_hole_inode,
    superblock::Ext4Superblock,
    truncate_inode,
};

struct RestartDevice {
    bytes: Vec<u8>,
    now: Cell<i64>,
    power_cut: Rc<PowerCutProbe>,
}

#[derive(Default)]
struct PowerCutProbe {
    trigger: Cell<Option<(u64, u32)>>,
    commit_writes: Cell<u32>,
    target_writes: Cell<u32>,
    commits_at_last_target_write: Cell<u32>,
    commits_before_cut: Cell<Option<u32>>,
}

impl PowerCutProbe {
    fn reset_observation(&self) {
        self.trigger.set(None);
        self.commit_writes.set(0);
        self.target_writes.set(0);
        self.commits_at_last_target_write.set(0);
        self.commits_before_cut.set(None);
    }

    fn arm_checkpoint_power_cut(&self, target: u64, occurrence: u32) {
        assert!(occurrence > 0, "power-cut occurrence must be positive");
        self.reset_observation();
        self.trigger.set(Some((target, occurrence)));
    }

    fn disable(&self) {
        self.trigger.set(None);
    }
}

impl RestartDevice {
    fn new(blocks: usize) -> Self {
        Self {
            bytes: vec![0; blocks * BLOCK_SIZE],
            now: Cell::new(1_700_000_000),
            power_cut: Rc::new(PowerCutProbe::default()),
        }
    }
}

impl BlockIo for RestartDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let source = self.bytes.get(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(
                sector.to_u32().unwrap_or(u32::MAX),
                self.geometry().block_count,
            )
        })?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        if is_jbd2_commit(buffer) {
            self.power_cut
                .commit_writes
                .set(self.power_cut.commit_writes.get() + 1);
        }
        let watched_target = self
            .power_cut
            .trigger
            .get()
            .filter(|(target, _)| *target == sector.raw());
        if let Some((target, occurrence)) = watched_target {
            if occurrence == 1 {
                self.power_cut
                    .commits_before_cut
                    .set(Some(self.power_cut.commit_writes.get()));
                panic!("simulated power loss before filesystem block {target} write");
            }
            self.power_cut.trigger.set(Some((target, occurrence - 1)));
        }
        let start = sector.as_usize()? * BLOCK_SIZE;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let total_blocks = self.geometry().block_count;
        let destination = self.bytes.get_mut(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(sector.to_u32().unwrap_or(u32::MAX), total_blocks)
        })?;
        destination.copy_from_slice(buffer);
        if watched_target.is_some() {
            self.power_cut
                .target_writes
                .set(self.power_cut.target_writes.get() + 1);
            self.power_cut
                .commits_at_last_target_write
                .set(self.power_cut.commit_writes.get());
        }
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

fn is_jbd2_commit(buffer: &[u8]) -> bool {
    buffer.len() >= 8
        && u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) == JBD2_MAGIC
        && u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) == JBD2_BLOCKTYPE_COMMIT
}

impl Clock for RestartDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.now.get();
        self.now.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

struct LargeExtentFixture {
    journal: Jbd2Dev<RestartDevice>,
    filesystem: Ext4FileSystem,
    power_cut: Rc<PowerCutProbe>,
    inode_number: InodeNumber,
    inode: Ext4Inode,
    logical_blocks: u32,
    data_blocks: Vec<AbsoluteBN>,
    gap_blocks: Vec<AbsoluteBN>,
    external_blocks: Vec<AbsoluteBN>,
    checkpoint_leaf: u64,
    free_before: u64,
}

#[derive(Clone, Copy)]
enum ExtentRemovalOperation {
    Punch,
    Truncate,
}

struct RemovedExtentExpectation<'a> {
    inode_number: InodeNumber,
    logical_blocks: u32,
    data_blocks: &'a [AbsoluteBN],
    gap_blocks: &'a [AbsoluteBN],
    external_blocks: &'a [AbsoluteBN],
    free_before: u64,
    expected_size: u64,
}

#[test]
fn large_extent_range_removal_restarts_across_small_journal_transactions() {
    assert_large_extent_removal_restarts("/large-punch", ExtentRemovalOperation::Punch);
    assert_large_extent_removal_restarts("/large-truncate", ExtentRemovalOperation::Truncate);
}

#[test]
fn large_extent_truncate_recovery_resumes_after_transaction_boundary_power_cut() {
    let mut fixture = build_large_extent_fixture("/restart-crash");
    install_small_journal(&mut fixture);
    fixture
        .power_cut
        .arm_checkpoint_power_cut(fixture.checkpoint_leaf, 2);

    let power_cut = catch_unwind(AssertUnwindSafe(|| {
        truncate_inode(
            &mut fixture.journal,
            &mut fixture.filesystem,
            fixture.inode_number,
            0,
        )
    }));
    assert!(
        power_cut.is_err(),
        "fixture must cut power after one committed removal chunk"
    );
    assert_eq!(
        fixture.power_cut.target_writes.get(),
        1,
        "one earlier checkpoint write must reach the target leaf"
    );
    let commits_before_cut = fixture
        .power_cut
        .commits_before_cut
        .get()
        .expect("power cut must record the durable commit count");
    assert!(
        commits_before_cut > fixture.power_cut.commits_at_last_target_write.get(),
        "the interrupted checkpoint must follow a newly written commit block"
    );

    let LargeExtentFixture {
        journal,
        filesystem,
        inode_number,
        logical_blocks,
        data_blocks,
        gap_blocks,
        external_blocks,
        free_before,
        ..
    } = fixture;
    drop(filesystem);
    let device = journal.into_inner();
    device.power_cut.disable();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted = mount(&mut remount_device).expect("mount must replay and resume truncate");
    assert_removed_extent_state(
        &mut remount_device,
        &mut remounted,
        RemovedExtentExpectation {
            inode_number,
            logical_blocks,
            data_blocks: &data_blocks,
            gap_blocks: &gap_blocks,
            external_blocks: &external_blocks,
            free_before,
            expected_size: 0,
        },
    );
}

#[test]
fn undersized_journal_rejects_restart_before_publishing_truncate_intent() {
    let mut fixture = build_large_extent_fixture("/restart-too-small");
    install_journal_with_maxlen(&mut fixture, 6);
    fixture.power_cut.reset_observation();
    let original_size = fixture.inode.size();

    let error = truncate_inode(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        0,
    )
    .expect_err("one removal chunk cannot fit this journal");
    assert_eq!(error.kind(), Ext4ErrorKind::NoSpace);

    let inode = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
        .expect("failed truncate must leave the original inode readable");
    assert_eq!(inode.size(), original_size);
    assert_eq!(fixture.filesystem.superblock.s_last_orphan, 0);
    assert_eq!(fixture.power_cut.commit_writes.get(), 0);
}

fn assert_large_extent_removal_restarts(path: &str, operation: ExtentRemovalOperation) {
    let mut fixture = build_large_extent_fixture(path);
    install_small_journal(&mut fixture);
    fixture.power_cut.reset_observation();
    let size_before = fixture.inode.size();
    match operation {
        ExtentRemovalOperation::Truncate => truncate_inode(
            &mut fixture.journal,
            &mut fixture.filesystem,
            fixture.inode_number,
            0,
        )
        .expect("large truncate must restart its journal transaction"),
        ExtentRemovalOperation::Punch => punch_hole_inode(
            &mut fixture.journal,
            &mut fixture.filesystem,
            fixture.inode_number,
            0,
            size_before,
        )
        .expect("large punch must restart its journal transaction"),
    }
    assert!(
        fixture.power_cut.commit_writes.get() >= 2,
        "small journal must force more than one transaction before unmount"
    );

    fixture
        .filesystem
        .umount(&mut fixture.journal)
        .expect("large range removal unmount failed");
    let device = fixture.journal.into_inner();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted = mount(&mut remount_device).expect("large range removal remount failed");
    assert_removed_extent_state(
        &mut remount_device,
        &mut remounted,
        RemovedExtentExpectation {
            inode_number: fixture.inode_number,
            logical_blocks: fixture.logical_blocks,
            data_blocks: &fixture.data_blocks,
            gap_blocks: &fixture.gap_blocks,
            external_blocks: &fixture.external_blocks,
            free_before: fixture.free_before,
            expected_size: match operation {
                ExtentRemovalOperation::Punch => size_before,
                ExtentRemovalOperation::Truncate => 0,
            },
        },
    );
}

fn build_large_extent_fixture(path: &str) -> LargeExtentFixture {
    let device = RestartDevice::new(16 * 1024);
    let power_cut = Rc::clone(&device.power_cut);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, path, None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, path)
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    let huge_file = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let sectors_per_block = (BLOCK_SIZE / 512) as u64;
    let mut logical_blocks = 0u32;
    let mut data_blocks = Vec::new();
    let mut gap_blocks = Vec::new();
    loop {
        let physical = filesystem
            .alloc_block(&mut journal)
            .expect("data allocation failed");
        let physical_gap = filesystem
            .alloc_block(&mut journal)
            .expect("gap allocation failed");
        data_blocks.push(physical);
        gap_blocks.push(physical_gap);
        ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
            .insert_extent(
                &mut filesystem,
                Ext4Extent::new(logical_blocks, physical.raw(), 1),
                &mut journal,
            )
            .expect("extent insertion failed");
        let blocks_count = inode.blocks_count(BLOCK_SIZE as u32, huge_file);
        inode
            .set_blocks_count(
                blocks_count + sectors_per_block,
                BLOCK_SIZE as u32,
                huge_file,
            )
            .expect("data block accounting failed");
        logical_blocks += 1;

        let root = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
            .load_root_from_inode()
            .expect("extent root parse failed");
        if matches!(
            root,
            ExtentNode::Index {
                header,
                ref entries,
            } if header.eh_depth == 1 && entries.len() == 4
        ) {
            break;
        }
        assert!(
            logical_blocks < 2_000,
            "fixture did not build four external leaves"
        );
    }
    inode.i_size_lo = logical_blocks * BLOCK_SIZE as u32;
    filesystem
        .modify_inode(&mut journal, inode_number, |on_disk| *on_disk = inode)
        .expect("inode publication failed");
    filesystem
        .sync_filesystem(&mut journal)
        .expect("fixture sync failed");

    let external_blocks = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .external_node_blocks(&mut journal)
        .expect("external node enumeration failed");
    assert_eq!(external_blocks.len(), 4);
    let root = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .load_root_from_inode()
        .expect("external root parse failed");
    let checkpoint_leaf = match root {
        ExtentNode::Index { entries, .. } => entries
            .get(1)
            .map(|index| (u64::from(index.ei_leaf_hi) << 32) | u64::from(index.ei_leaf_lo))
            .expect("external root must reference a reusable crash leaf"),
        ExtentNode::Leaf { .. } => panic!("fixture must create external leaves"),
    };
    let free_before = filesystem.superblock.free_blocks_count();
    LargeExtentFixture {
        journal,
        filesystem,
        power_cut,
        inode_number,
        inode,
        logical_blocks,
        data_blocks,
        gap_blocks,
        external_blocks,
        checkpoint_leaf,
        free_before,
    }
}

fn install_small_journal(fixture: &mut LargeExtentFixture) {
    install_journal_with_maxlen(fixture, 10);
}

fn install_journal_with_maxlen(fixture: &mut LargeExtentFixture, maxlen: u32) {
    let journal_start = fixture
        .filesystem
        .journal_sb_block_start
        .expect("internal journal block missing");
    let mut small_journal = JournalSuperBlock::default();
    small_journal.s_header.h_blocktype = JBD2_BLOCKTYPE_SUPERBLOCK_V1;
    small_journal.s_blocksize = BLOCK_SIZE as u32;
    small_journal.s_maxlen = maxlen;
    small_journal.s_first = 1;
    small_journal.s_sequence = 1;
    small_journal.s_start = 0;
    small_journal.s_errno = 0;
    fixture
        .journal
        .set_journal_superblock(small_journal, journal_start)
        .expect("small journal installation failed");
}

fn assert_removed_extent_state(
    device: &mut Jbd2Dev<RestartDevice>,
    filesystem: &mut Ext4FileSystem,
    expectation: RemovedExtentExpectation<'_>,
) {
    let inode = filesystem
        .get_inode_by_num(device, expectation.inode_number)
        .expect("remounted inode read failed");
    let huge_file = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    assert_eq!(inode.size(), expectation.expected_size);
    assert_eq!(inode.blocks_count(BLOCK_SIZE as u32, huge_file), 0);
    assert_eq!(
        filesystem.superblock.free_blocks_count(),
        expectation.free_before
            + u64::from(expectation.logical_blocks)
            + expectation.external_blocks.len() as u64
    );
    for block in expectation
        .data_blocks
        .iter()
        .chain(expectation.external_blocks)
    {
        assert!(
            !bitmap_block_is_allocated(filesystem, device, *block),
            "removed data or extent-tree block {block:?} must be free"
        );
    }
    for block in expectation.gap_blocks {
        assert!(
            bitmap_block_is_allocated(filesystem, device, *block),
            "unmapped gap block {block:?} must remain allocated"
        );
    }
    let extents = rsext4::inspect_inode_extents(
        device,
        filesystem,
        expectation.inode_number,
        0,
        u64::MAX,
        rsext4::FileExtentTarget::Data,
        usize::MAX,
    )
    .expect("remounted extent inspection failed");
    assert!(extents.extents.is_empty());
    let mut root_inode = inode;
    let root = ExtentTree::with_filesystem(&mut root_inode, filesystem, expectation.inode_number)
        .load_root_from_inode()
        .expect("empty extent root must remain valid");
    assert!(matches!(
        root,
        ExtentNode::Leaf { header, entries }
            if header.eh_depth == 0 && header.eh_entries == 0 && entries.is_empty()
    ));
}

fn bitmap_block_is_allocated(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<RestartDevice>,
    block: AbsoluteBN,
) -> bool {
    let (group, block_in_group) = filesystem
        .block_allocator
        .global_to_group(block)
        .expect("block must belong to one allocation group");
    let descriptor = filesystem
        .group_descs
        .get(group.as_usize().expect("group index must fit usize"))
        .expect("allocation group descriptor must exist");
    let bitmap = filesystem
        .bitmap_cache
        .get_or_load(
            device,
            CacheKey::new_block(group),
            AbsoluteBN::new(descriptor.block_bitmap()),
        )
        .expect("block bitmap must load");
    bitmap
        .data
        .get(
            block_in_group
                .as_usize()
                .expect("block index must fit usize")
                / 8,
        )
        .map(|byte| {
            let bit = block_in_group.raw() % 8;
            byte & (1 << bit) != 0
        })
        .expect("block index must fit bitmap")
}
