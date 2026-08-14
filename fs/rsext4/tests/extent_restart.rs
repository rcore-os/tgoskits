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
    mkfile, mkfs, punch_hole_inode, read_inode_data_into, reap_unlinked_inode,
    superblock::Ext4Superblock,
    truncate_inode, unlink,
};

struct RestartDevice {
    bytes: Vec<u8>,
    now: Cell<i64>,
    power_cut: Rc<PowerCutProbe>,
}

#[derive(Default)]
struct PowerCutProbe {
    commit_trigger: Cell<Option<u32>>,
    commit_writes: Cell<u32>,
    commits_before_cut: Cell<Option<u32>>,
}

impl PowerCutProbe {
    fn reset_observation(&self) {
        self.commit_trigger.set(None);
        self.commit_writes.set(0);
        self.commits_before_cut.set(None);
    }

    fn arm_committed_transaction_power_cut(&self, occurrence: u32) {
        assert!(occurrence > 0, "power-cut occurrence must be positive");
        self.reset_observation();
        self.commit_trigger.set(Some(occurrence));
    }

    fn disable(&self) {
        self.commit_trigger.set(None);
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
        let is_commit = is_jbd2_commit(buffer);
        if is_commit {
            self.power_cut
                .commit_writes
                .set(self.power_cut.commit_writes.get() + 1);
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
        if is_commit && let Some(occurrence) = self.power_cut.commit_trigger.get() {
            if occurrence == 1 {
                self.power_cut
                    .commits_before_cut
                    .set(Some(self.power_cut.commit_writes.get()));
                panic!("simulated power loss after durable JBD2 commit");
            }
            self.power_cut.commit_trigger.set(Some(occurrence - 1));
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
    free_before: u64,
}

struct LargeLegacyFixture {
    journal: Jbd2Dev<RestartDevice>,
    filesystem: Ext4FileSystem,
    power_cut: Rc<PowerCutProbe>,
    inode_number: InodeNumber,
    removed_blocks: Vec<AbsoluteBN>,
    gap_blocks: Vec<AbsoluteBN>,
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

struct RemovedLegacyExpectation<'a> {
    inode_number: InodeNumber,
    removed_blocks: &'a [AbsoluteBN],
    gap_blocks: &'a [AbsoluteBN],
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
    fixture.power_cut.arm_committed_transaction_power_cut(1);

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
    let commits_before_cut = fixture
        .power_cut
        .commits_before_cut
        .get()
        .expect("power cut must record the durable commit count");
    assert_eq!(
        commits_before_cut, 1,
        "the first removal chunk must be durable"
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
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("mount must replay and resume truncate");
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
    install_journal_with_maxlen(&mut fixture, 15);
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

#[test]
fn large_legacy_truncate_restarts_across_allocation_groups() {
    let mut fixture = build_large_legacy_fixture("/legacy-restart");
    // One child-first removal chunk needs six buffer credits after revoke
    // records are charged by descriptor block. Keep the ring below the full
    // two-group footprint so this fixture still crosses a commit boundary.
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 24);
    fixture.power_cut.reset_observation();
    let sequence_before = fixture.journal.journal_sequence();

    truncate_inode(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        0,
    )
    .expect("legacy truncate must restart its journal transaction");
    assert!(
        fixture.power_cut.commit_writes.get() >= 1,
        "the second allocation group must cross a commit boundary, observed {}, sequence {:?} -> \
         {:?}",
        fixture.power_cut.commit_writes.get(),
        sequence_before,
        fixture.journal.journal_sequence()
    );
    assert_ne!(fixture.journal.journal_sequence(), sequence_before);
    let current_inode = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
        .expect("current inode read failed");
    assert_eq!(current_inode.i_block, [0; 15]);
    assert_eq!(current_inode.i_blocks_lo, 0);
    fixture
        .filesystem
        .umount(&mut fixture.journal)
        .expect("legacy restart unmount failed");
    assert!(
        fixture.power_cut.commit_writes.get() >= 2,
        "unmount must commit the final restarted transaction"
    );

    let device = fixture.journal.into_inner();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("legacy restart remount failed");
    assert_removed_legacy_state(
        &mut remounted,
        &mut remount_device,
        RemovedLegacyExpectation {
            inode_number: fixture.inode_number,
            removed_blocks: &fixture.removed_blocks,
            gap_blocks: &fixture.gap_blocks,
            free_before: fixture.free_before,
            expected_size: 0,
        },
    );
}

#[test]
fn large_legacy_punch_restarts_across_allocation_groups() {
    let mut fixture = build_large_legacy_fixture("/legacy-punch-restart");
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 24);
    fixture.power_cut.reset_observation();
    let size_before = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
        .expect("legacy inode read failed")
        .size();
    let sequence_before = fixture.journal.journal_sequence();

    punch_hole_inode(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        0,
        size_before,
    )
    .expect("legacy punch must restart its journal transaction");
    assert!(
        fixture.power_cut.commit_writes.get() >= 1,
        "the second allocation group must cross a commit boundary"
    );
    assert_ne!(fixture.journal.journal_sequence(), sequence_before);
    assert_eq!(fixture.filesystem.superblock.s_last_orphan, 0);

    fixture
        .filesystem
        .umount(&mut fixture.journal)
        .expect("legacy punch restart unmount failed");
    let device = fixture.journal.into_inner();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("legacy punch remount failed");
    assert_removed_legacy_state(
        &mut remounted,
        &mut remount_device,
        RemovedLegacyExpectation {
            inode_number: fixture.inode_number,
            removed_blocks: &fixture.removed_blocks,
            gap_blocks: &fixture.gap_blocks,
            free_before: fixture.free_before,
            expected_size: size_before,
        },
    );
}

#[test]
fn large_legacy_punch_remains_consistent_after_committed_transaction_power_cut() {
    let mut fixture = build_large_legacy_fixture("/legacy-punch-crash");
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 24);
    let size_before = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
        .expect("legacy inode read failed")
        .size();
    fixture.power_cut.arm_committed_transaction_power_cut(1);

    let power_cut = catch_unwind(AssertUnwindSafe(|| {
        punch_hole_inode(
            &mut fixture.journal,
            &mut fixture.filesystem,
            fixture.inode_number,
            0,
            size_before,
        )
    }));
    assert!(
        power_cut.is_err(),
        "fixture must cut power while checkpointing a committed punch chunk"
    );
    let commits_before_cut = fixture
        .power_cut
        .commits_before_cut
        .get()
        .expect("power cut must record the durable commit count");
    assert_eq!(
        commits_before_cut, 1,
        "the first punch chunk must be durable"
    );

    let device = fixture.journal.into_inner();
    device.power_cut.disable();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("mount must replay committed punch work");
    let partial = remounted
        .get_inode_by_num(&mut remount_device, fixture.inode_number)
        .expect("partially punched inode must remain readable");
    assert_eq!(partial.size(), size_before);
    assert_eq!(remounted.superblock.s_last_orphan, 0);

    // Linux does not persist a punch-range intent. A caller may retry the
    // operation after recovery; every committed intermediate tree must be a
    // valid starting point for that retry.
    punch_hole_inode(
        &mut remount_device,
        &mut remounted,
        fixture.inode_number,
        0,
        size_before,
    )
    .expect("retry must finish the remaining punch range");
    remounted
        .umount(&mut remount_device)
        .expect("retried legacy punch unmount failed");

    let device = remount_device.into_inner();
    let mut verify_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut verified =
        Ext4FileSystem::mount(&mut verify_device).expect("legacy punch verification mount failed");
    assert_removed_legacy_state(
        &mut verified,
        &mut verify_device,
        RemovedLegacyExpectation {
            inode_number: fixture.inode_number,
            removed_blocks: &fixture.removed_blocks,
            gap_blocks: &fixture.gap_blocks,
            free_before: fixture.free_before,
            expected_size: size_before,
        },
    );
}

#[test]
fn large_legacy_truncate_recovery_resumes_after_committed_transaction_power_cut() {
    let mut fixture = build_large_legacy_fixture("/legacy-restart-crash");
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 24);
    fixture.power_cut.arm_committed_transaction_power_cut(1);

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
        "fixture must cut power while checkpointing the committed indirect-group removal"
    );
    let commits_before_cut = fixture
        .power_cut
        .commits_before_cut
        .get()
        .expect("power cut must record the durable commit count");
    assert_eq!(
        commits_before_cut, 1,
        "the first truncate chunk must be durable before power loss"
    );

    let device = fixture.journal.into_inner();
    device.power_cut.disable();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted = Ext4FileSystem::mount(&mut remount_device)
        .expect("mount must replay and resume legacy truncate");
    assert_removed_legacy_state(
        &mut remounted,
        &mut remount_device,
        RemovedLegacyExpectation {
            inode_number: fixture.inode_number,
            removed_blocks: &fixture.removed_blocks,
            gap_blocks: &fixture.gap_blocks,
            free_before: fixture.free_before,
            expected_size: 0,
        },
    );
}

#[test]
fn zero_link_legacy_reap_restarts_before_final_inode_transaction() {
    let mut fixture = build_large_legacy_fixture("/legacy-reap-restart");
    let outcome = unlink(
        &mut fixture.filesystem,
        &mut fixture.journal,
        "/legacy-reap-restart",
    )
    .expect("final unlink must publish the orphan");
    assert_eq!(outcome.inode, fixture.inode_number);
    assert!(outcome.requires_reap());
    mkfile(
        &mut fixture.journal,
        &mut fixture.filesystem,
        "/legacy-reap-head",
        None,
        None,
    )
    .expect("second orphan creation failed");
    let head_inode = dir::get_inode_with_num(
        &mut fixture.filesystem,
        &mut fixture.journal,
        "/legacy-reap-head",
    )
    .expect("second orphan lookup failed")
    .expect("second orphan missing")
    .0;
    let head_outcome = unlink(
        &mut fixture.filesystem,
        &mut fixture.journal,
        "/legacy-reap-head",
    )
    .expect("second final unlink must publish a new orphan head");
    assert_eq!(head_outcome.inode, head_inode);
    assert!(head_outcome.requires_reap());
    fixture
        .filesystem
        .sync_filesystem(&mut fixture.journal)
        .expect("orphan fixture sync failed");
    fixture
        .journal
        .flush()
        .expect("orphan fixture checkpoint failed");
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 30);
    fixture.power_cut.reset_observation();

    reap_unlinked_inode(
        &mut fixture.filesystem,
        &mut fixture.journal,
        fixture.inode_number,
    )
    .expect("zero-link legacy reap must restart before its final inode transaction");
    assert!(
        fixture.power_cut.commit_writes.get() >= 1,
        "mapping removal and final inode free must cross a commit boundary"
    );
    assert_eq!(
        fixture.filesystem.superblock.s_last_orphan,
        head_inode.raw(),
        "reaping a non-head orphan must preserve and rewrite its predecessor"
    );
    let head = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, head_inode)
        .expect("remaining orphan head read failed");
    assert_eq!(head.i_dtime, 0);
    reap_unlinked_inode(&mut fixture.filesystem, &mut fixture.journal, head_inode)
        .expect("empty orphan head must fit the exact five-credit final transaction");
    fixture
        .filesystem
        .umount(&mut fixture.journal)
        .expect("legacy reap unmount failed");

    let device = fixture.journal.into_inner();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("legacy reap remount failed");
    assert_reaped_legacy_state(
        &mut remounted,
        &mut remount_device,
        RemovedLegacyExpectation {
            inode_number: fixture.inode_number,
            removed_blocks: &fixture.removed_blocks,
            gap_blocks: &fixture.gap_blocks,
            free_before: fixture.free_before,
            expected_size: 0,
        },
    );
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
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("large range removal remount failed");
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
    let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount failed");
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
    journal.flush().expect("fixture checkpoint failed");

    let external_blocks = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .external_node_blocks(&mut journal)
        .expect("external node enumeration failed");
    assert_eq!(external_blocks.len(), 4);
    let root = ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number)
        .load_root_from_inode()
        .expect("external root parse failed");
    assert!(matches!(root, ExtentNode::Index { .. }));
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
        free_before,
    }
}

fn build_large_legacy_fixture(path: &str) -> LargeLegacyFixture {
    let device = RestartDevice::new(34_000);
    let power_cut = Rc::clone(&device.power_cut);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount failed");
    mkfile(&mut journal, &mut filesystem, path, None, None).expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, path)
        .expect("lookup failed")
        .expect("created inode missing")
        .0;

    let direct_data = filesystem
        .alloc_block(&mut journal)
        .expect("group-zero data allocation failed");
    let group_zero_free = filesystem.group_descs[0].free_blocks_count();
    let gap_blocks = filesystem
        .alloc_blocks(&mut journal, group_zero_free)
        .expect("group-zero gap allocation failed");
    let single_root = filesystem
        .alloc_block(&mut journal)
        .expect("single-indirect root allocation failed");
    let single_data = filesystem
        .alloc_block(&mut journal)
        .expect("single-indirect data allocation failed");
    let double_root = filesystem
        .alloc_block(&mut journal)
        .expect("double-indirect root allocation failed");
    let double_leaf = filesystem
        .alloc_block(&mut journal)
        .expect("double-indirect leaf allocation failed");
    let empty_double_leaf = filesystem
        .alloc_block(&mut journal)
        .expect("empty double-indirect leaf allocation failed");
    let double_data = filesystem
        .alloc_block(&mut journal)
        .expect("double-indirect data allocation failed");
    let triple_root = filesystem
        .alloc_block(&mut journal)
        .expect("triple-indirect root allocation failed");
    let triple_middle = filesystem
        .alloc_block(&mut journal)
        .expect("triple-indirect middle allocation failed");
    let triple_leaf = filesystem
        .alloc_block(&mut journal)
        .expect("triple-indirect leaf allocation failed");
    let triple_data = filesystem
        .alloc_block(&mut journal)
        .expect("triple-indirect data allocation failed");
    let removed_blocks = vec![
        direct_data,
        single_root,
        single_data,
        double_root,
        double_leaf,
        empty_double_leaf,
        double_data,
        triple_root,
        triple_middle,
        triple_leaf,
        triple_data,
    ];
    let (direct_group, _) = filesystem
        .block_allocator
        .global_to_group(direct_data)
        .expect("direct group lookup failed");
    let (indirect_group, _) = filesystem
        .block_allocator
        .global_to_group(single_data)
        .expect("indirect group lookup failed");
    assert_ne!(direct_group, indirect_group);
    for block in &removed_blocks[1..] {
        let (group, _) = filesystem
            .block_allocator
            .global_to_group(*block)
            .expect("indirect block group lookup failed");
        assert_eq!(group, indirect_group);
    }
    write_pointer_block(&mut journal, single_root, single_data);
    write_pointer_block(&mut journal, double_root, double_leaf);
    write_pointer_entry(&mut journal, double_root, 1, empty_double_leaf);
    write_pointer_block(&mut journal, double_leaf, double_data);
    write_pointer_block(&mut journal, triple_root, triple_middle);
    write_pointer_block(&mut journal, triple_middle, triple_leaf);
    write_pointer_block(&mut journal, triple_leaf, triple_data);
    filesystem
        .datablock_cache
        .modify_new(&mut journal, direct_data, |block| block[0] = 0x31)
        .expect("direct marker write failed");
    filesystem
        .datablock_cache
        .modify_new(&mut journal, single_data, |block| block[0] = 0x32)
        .expect("single-indirect marker write failed");
    filesystem
        .datablock_cache
        .modify_new(&mut journal, double_data, |block| block[0] = 0x33)
        .expect("double-indirect marker write failed");
    filesystem
        .datablock_cache
        .modify_new(&mut journal, triple_data, |block| block[0] = 0x34)
        .expect("triple-indirect marker write failed");
    let triple_logical = 12u64 + 1024 + 1024 * 1024;
    let inode_size = (triple_logical + 1) * BLOCK_SIZE as u64;
    filesystem
        .modify_inode(&mut journal, inode_number, |inode| {
            inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_block[0] = direct_data.to_u32().expect("direct block number");
            inode.i_block[12] = single_root.to_u32().expect("single root number");
            inode.i_block[13] = double_root.to_u32().expect("double root number");
            inode.i_block[14] = triple_root.to_u32().expect("triple root number");
            inode.i_size_lo = inode_size as u32;
            inode.i_size_high = (inode_size >> 32) as u32;
            inode.i_blocks_lo = removed_blocks.len() as u32 * (BLOCK_SIZE / 512) as u32;
        })
        .expect("legacy inode publication failed");
    assert_large_legacy_fixture_mappings(&mut filesystem, &mut journal, inode_number);
    filesystem
        .sync_filesystem(&mut journal)
        .expect("fixture sync failed");
    journal.flush().expect("fixture checkpoint failed");
    let free_before = filesystem.superblock.free_blocks_count();
    LargeLegacyFixture {
        journal,
        filesystem,
        power_cut,
        inode_number,
        removed_blocks,
        gap_blocks,
        free_before,
    }
}

fn assert_large_legacy_fixture_mappings(
    filesystem: &mut Ext4FileSystem,
    journal: &mut Jbd2Dev<RestartDevice>,
    inode_number: InodeNumber,
) {
    let marker_cases = [
        (0, 0x31, "direct"),
        (12, 0x32, "single-indirect"),
        (12 + 1024, 0x33, "double-indirect"),
        (12 + 1024 + 1024 * 1024, 0x34, "triple-indirect"),
    ];
    let mut marker = [0u8; 1];
    for (logical, expected, level) in marker_cases {
        let bytes_read = read_inode_data_into(
            journal,
            filesystem,
            inode_number,
            logical * BLOCK_SIZE as u64,
            &mut marker,
        )
        .expect("legacy marker read failed");
        assert_eq!(bytes_read, 1, "{level} marker must be addressable");
        assert_eq!(marker, [expected], "{level} mapping resolved incorrectly");
    }

    assert_eq!(
        read_inode_data_into(
            journal,
            filesystem,
            inode_number,
            13 * BLOCK_SIZE as u64,
            &mut marker,
        )
        .expect("logical-hole read failed"),
        1
    );
    assert_eq!(marker, [0], "the sparse logical hole must remain unmapped");
}

fn write_pointer_block(
    journal: &mut Jbd2Dev<RestartDevice>,
    pointer_block: AbsoluteBN,
    child: AbsoluteBN,
) {
    let mut image = vec![0; BLOCK_SIZE];
    image[..4].copy_from_slice(
        &child
            .to_u32()
            .expect("legacy pointer block number")
            .to_le_bytes(),
    );
    journal
        .write_blocks(&image, pointer_block, 1, true)
        .expect("pointer block write failed");
}

fn write_pointer_entry(
    journal: &mut Jbd2Dev<RestartDevice>,
    pointer_block: AbsoluteBN,
    index: usize,
    child: AbsoluteBN,
) {
    journal
        .read_block(pointer_block)
        .expect("pointer block read failed");
    let mut image = journal.buffer().to_vec();
    let offset = index * core::mem::size_of::<u32>();
    image[offset..offset + 4].copy_from_slice(
        &child
            .to_u32()
            .expect("legacy pointer block number")
            .to_le_bytes(),
    );
    journal
        .write_blocks(&image, pointer_block, 1, true)
        .expect("pointer block write failed");
}

fn install_small_journal(fixture: &mut LargeExtentFixture) {
    install_journal_with_maxlen(fixture, 30);
}

fn install_journal_with_maxlen(fixture: &mut LargeExtentFixture, maxlen: u32) {
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, maxlen);
}

fn install_journal_with_maxlen_raw(
    journal: &mut Jbd2Dev<RestartDevice>,
    filesystem: &Ext4FileSystem,
    maxlen: u32,
) {
    let journal_start = filesystem
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
    journal
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

fn assert_removed_legacy_state(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<RestartDevice>,
    expectation: RemovedLegacyExpectation<'_>,
) {
    let inode = filesystem
        .get_inode_by_num(device, expectation.inode_number)
        .expect("remounted inode read failed");
    assert_eq!(inode.size(), expectation.expected_size);
    assert_eq!(inode.i_block, [0; 15]);
    assert_eq!(inode.i_blocks_lo, 0);
    assert_eq!(filesystem.superblock.s_last_orphan, 0);
    assert_eq!(
        filesystem.superblock.free_blocks_count(),
        expectation.free_before + expectation.removed_blocks.len() as u64
    );
    for &block in expectation.removed_blocks {
        assert!(!bitmap_block_is_allocated(filesystem, device, block));
    }
    assert!(bitmap_block_is_allocated(
        filesystem,
        device,
        *expectation.gap_blocks.last().expect("group-zero gap block")
    ));
}

fn assert_reaped_legacy_state(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<RestartDevice>,
    expectation: RemovedLegacyExpectation<'_>,
) {
    assert_eq!(filesystem.superblock.s_last_orphan, 0);
    assert!(
        !filesystem
            .inode_num_already_allocated(device, expectation.inode_number)
            .expect("reaped inode allocation lookup failed")
    );
    assert_eq!(
        filesystem.superblock.free_blocks_count(),
        expectation.free_before + expectation.removed_blocks.len() as u64
    );
    for &block in expectation.removed_blocks {
        assert!(!bitmap_block_is_allocated(filesystem, device, block));
    }
    assert!(bitmap_block_is_allocated(
        filesystem,
        device,
        *expectation.gap_blocks.last().expect("group-zero gap block")
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
