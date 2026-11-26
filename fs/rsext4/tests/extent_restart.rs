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
    failed_commit_trigger: Cell<Option<u32>>,
    cut_before_commit_trigger: Cell<Option<u32>>,
    failed_block_write: Cell<Option<AbsoluteBN>>,
}

impl PowerCutProbe {
    fn reset_observation(&self) {
        self.commit_trigger.set(None);
        self.commit_writes.set(0);
        self.commits_before_cut.set(None);
        self.failed_commit_trigger.set(None);
        self.cut_before_commit_trigger.set(None);
        self.failed_block_write.set(None);
    }

    fn arm_committed_transaction_power_cut(&self, occurrence: u32) {
        assert!(occurrence > 0, "power-cut occurrence must be positive");
        self.reset_observation();
        self.commit_trigger.set(Some(occurrence));
    }

    /// Fails the `occurrence`th JBD2 commit-block write without writing it,
    /// so that transaction never becomes durable.
    fn arm_failed_commit(&self, occurrence: u32) {
        assert!(occurrence > 0, "failed-commit occurrence must be positive");
        self.reset_observation();
        self.failed_commit_trigger.set(Some(occurrence));
    }

    /// Cuts power just before the `occurrence`th JBD2 commit-block write, so
    /// that transaction never becomes durable.
    fn arm_power_cut_before_commit(&self, occurrence: u32) {
        assert!(occurrence > 0, "power-cut occurrence must be positive");
        self.reset_observation();
        self.cut_before_commit_trigger.set(Some(occurrence));
    }

    /// Fails the next write that covers `block` without writing it.
    #[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
    fn arm_failed_block_write(&self, block: AbsoluteBN) {
        self.reset_observation();
        self.failed_block_write.set(Some(block));
    }

    fn disable(&self) {
        self.commit_trigger.set(None);
        self.failed_commit_trigger.set(None);
        self.cut_before_commit_trigger.set(None);
        self.failed_block_write.set(None);
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
        if let Some(block) = self.power_cut.failed_block_write.get() {
            let first = sector.as_usize()?;
            let target = block.to_u32().expect("block number") as usize;
            if (first..first + buffer.len() / BLOCK_SIZE).contains(&target) {
                self.power_cut.failed_block_write.set(None);
                return Err(Ext4Error::io());
            }
        }
        let is_commit = is_jbd2_commit(buffer);
        if is_commit {
            self.power_cut
                .commit_writes
                .set(self.power_cut.commit_writes.get() + 1);
            if let Some(occurrence) = self.power_cut.failed_commit_trigger.get() {
                if occurrence == 1 {
                    self.power_cut.failed_commit_trigger.set(None);
                    return Err(Ext4Error::io());
                }
                self.power_cut
                    .failed_commit_trigger
                    .set(Some(occurrence - 1));
            }
            if let Some(occurrence) = self.power_cut.cut_before_commit_trigger.get() {
                if occurrence == 1 {
                    self.power_cut
                        .commits_before_cut
                        .set(Some(self.power_cut.commit_writes.get() - 1));
                    panic!("simulated power loss before a JBD2 commit block");
                }
                self.power_cut
                    .cut_before_commit_trigger
                    .set(Some(occurrence - 1));
            }
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
fn paused_large_truncate_resumes_cleanup_after_publishing_the_new_size() {
    let mut fixture = build_large_extent_fixture("/background-truncate");
    install_small_journal(&mut fixture);
    fixture.journal.enable_background_commits().unwrap();
    let mut paused_after_resize = false;
    let mut completed = false;
    for _ in 0..1024 {
        match truncate_inode(
            &mut fixture.journal,
            &mut fixture.filesystem,
            fixture.inode_number,
            0,
        ) {
            Ok(()) => {
                completed = true;
                break;
            }
            Err(error) => {
                assert!(
                    error.requires_journal_progress(),
                    "unexpected error: {error:?}"
                );
                let inode = fixture
                    .filesystem
                    .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
                    .unwrap();
                paused_after_resize |= inode.size() == 0;
                // This fixture drives the portable progress boundary itself;
                // adapter concurrency is covered by detached-commit tests.
                fixture.journal.disable_background_commits().unwrap();
                fixture.journal.flush().unwrap();
                fixture.journal.enable_background_commits().unwrap();
            }
        }
    }
    assert!(completed, "bounded removal must finish");
    assert!(
        paused_after_resize,
        "the fixture must exercise the already-resized continuation"
    );
    assert_eq!(fixture.filesystem.superblock.s_last_orphan, 0);
    assert_removed_extent_state(
        &mut fixture.journal,
        &mut fixture.filesystem,
        RemovedExtentExpectation {
            inode_number: fixture.inode_number,
            logical_blocks: fixture.logical_blocks,
            data_blocks: &fixture.data_blocks,
            gap_blocks: &fixture.gap_blocks,
            external_blocks: &fixture.external_blocks,
            free_before: fixture.free_before,
            expected_size: 0,
        },
    );
}

#[test]
fn truncating_an_open_unlinked_inode_preserves_final_reap_ownership() {
    let mut fixture = build_large_extent_fixture("/unlinked-truncate");
    let outcome = unlink(
        &mut fixture.filesystem,
        &mut fixture.journal,
        "/unlinked-truncate",
    )
    .unwrap();
    assert_eq!(outcome.inode, fixture.inode_number);
    assert!(outcome.requires_reap());
    let inode = fixture
        .filesystem
        .get_inode_by_num(&mut fixture.journal, fixture.inode_number)
        .unwrap();
    assert_eq!(inode.i_links_count, 0);
    // Publish the orphan before constraining the journal: unlink reserves more
    // credits than the small transactions whose truncate/reap ownership we test.
    fixture
        .filesystem
        .sync_filesystem(&mut fixture.journal)
        .expect("orphan fixture sync failed");
    fixture
        .journal
        .flush()
        .expect("orphan fixture checkpoint failed");
    install_small_journal(&mut fixture);
    fixture.power_cut.reset_observation();
    truncate_inode(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        0,
    )
    .unwrap();
    assert!(
        fixture.power_cut.commit_writes.get() >= 1,
        "truncate must cross a commit boundary while preserving the orphan"
    );
    assert_eq!(
        fixture.filesystem.superblock.s_last_orphan,
        fixture.inode_number.raw()
    );
    assert!(
        fixture
            .filesystem
            .inode_num_already_allocated(&mut fixture.journal, fixture.inode_number)
            .unwrap()
    );
    reap_unlinked_inode(
        &mut fixture.filesystem,
        &mut fixture.journal,
        fixture.inode_number,
    )
    .unwrap();
    assert_eq!(fixture.filesystem.superblock.s_last_orphan, 0);
    assert!(
        !fixture
            .filesystem
            .inode_num_already_allocated(&mut fixture.journal, fixture.inode_number)
            .unwrap()
    );
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
    // The first commit carries the new size and the orphan record, and the
    // second the first removal chunk.
    fixture.power_cut.arm_committed_transaction_power_cut(2);

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
        commits_before_cut, 2,
        "the new size and the first removal chunk must be durable"
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
    // The first commit carries the new size and the orphan record, and the
    // second the first truncate chunk.
    fixture.power_cut.arm_committed_transaction_power_cut(2);

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
        commits_before_cut, 2,
        "the new size and the first truncate chunk must be durable before power loss"
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

#[test]
fn failed_first_restart_transaction_restores_the_extent_tail() {
    let mut fixture = build_large_extent_fixture("/restart-tail-extent");
    install_small_journal(&mut fixture);
    let retained_block = fixture.data_blocks[0];
    assert_failed_first_restart_transaction_restores_tail(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        retained_block,
    );
}

#[test]
fn failed_first_restart_transaction_restores_the_legacy_tail() {
    let mut fixture = build_restarting_legacy_tail_fixture("/restart-tail-legacy");
    let retained_block = fixture.removed_blocks[0];
    assert_failed_first_restart_transaction_restores_tail(
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        retained_block,
    );
}

/// Builds an indirect-block file whose shrink to a partial first block
/// restarts.
fn build_restarting_legacy_tail_fixture(path: &str) -> LargeLegacyFixture {
    let mut fixture = build_large_legacy_fixture(path);
    // Map a second group-zero block behind the retained one, so the shrink
    // still detaches blocks from both allocation groups and must restart.
    let second_direct = fixture.gap_blocks[0];
    fixture
        .filesystem
        .modify_inode(&mut fixture.journal, fixture.inode_number, |inode| {
            inode.i_block[1] = second_direct.to_u32().expect("direct block number");
            inode.i_blocks_lo += (BLOCK_SIZE / 512) as u32;
        })
        .expect("second direct mapping failed");
    fixture
        .filesystem
        .sync_filesystem(&mut fixture.journal)
        .expect("fixture sync failed");
    fixture.journal.flush().expect("fixture checkpoint failed");
    install_journal_with_maxlen_raw(&mut fixture.journal, &fixture.filesystem, 24);
    fixture
}

/// Fails the size-and-orphan transaction that starts a restarted shrink and
/// checks that the file keeps its old size and every byte of its EOF block,
/// and that a later truncate of the same file succeeds.
fn assert_failed_first_restart_transaction_restores_tail(
    journal: &mut Jbd2Dev<RestartDevice>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    retained_block: AbsoluteBN,
) {
    filesystem
        .datablock_cache
        .modify(journal, retained_block, |block| {
            for (index, byte) in block.iter_mut().enumerate() {
                *byte = (index % 251) as u8 + 1;
            }
        })
        .expect("retained block fill failed");
    // A stale orphan link makes `add_orphan` fail inside the first
    // transaction. No earlier check reads it.
    filesystem
        .modify_inode(journal, inode_number, |inode| inode.i_dtime = 1)
        .expect("stale orphan link injection failed");
    let original_size = filesystem
        .get_inode_by_num(journal, inode_number)
        .expect("inode read failed")
        .size();
    let mut original = vec![0u8; BLOCK_SIZE];
    read_inode_data_into(journal, filesystem, inode_number, 0, &mut original)
        .expect("retained block read failed");

    let error = truncate_inode(journal, filesystem, inode_number, 100)
        .expect_err("a failed first restart transaction must fail the truncate");
    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
    assert_eq!(
        error.context(),
        Some(rsext4::error::ErrorContext::Operation {
            op: "orphan:add_stale_dtime"
        }),
        "the shrink must take the restart path and fail in its first transaction"
    );

    let inode = filesystem
        .get_inode_by_num(journal, inode_number)
        .expect("failed truncate must leave the inode readable");
    assert_eq!(inode.size(), original_size);
    assert_eq!(filesystem.superblock.s_last_orphan, 0);
    let mut data = vec![0u8; BLOCK_SIZE];
    read_inode_data_into(journal, filesystem, inode_number, 0, &mut data)
        .expect("retained block read failed");
    let first_changed = data.iter().zip(&original).position(|(now, was)| now != was);
    assert_eq!(
        first_changed, None,
        "a failed truncate must leave every byte of the file as it was"
    );

    // The failed transaction must not leave the shrink pending: the next
    // truncate would then try to finish it and remove an orphan record that
    // was never added. A truncate to the current size changes nothing else.
    filesystem
        .modify_inode(journal, inode_number, |inode| inode.i_dtime = 0)
        .expect("stale orphan link repair failed");
    truncate_inode(journal, filesystem, inode_number, original_size)
        .expect("a later truncate must succeed after the failed one");
    let inode = filesystem
        .get_inode_by_num(journal, inode_number)
        .expect("inode read after the later truncate failed");
    assert_eq!(inode.size(), original_size);
    assert_eq!(filesystem.superblock.s_last_orphan, 0);
}

#[test]
fn failed_first_restart_commit_keeps_the_extent_tail() {
    assert_extent_first_restart_commit(FirstRestartCommit::Fails);
}

#[test]
fn failed_first_restart_commit_keeps_the_legacy_tail() {
    assert_legacy_first_restart_commit(FirstRestartCommit::Fails);
}

#[test]
fn power_cut_before_first_restart_commit_keeps_the_extent_tail() {
    assert_extent_first_restart_commit(FirstRestartCommit::PowerCutBefore);
}

#[test]
fn power_cut_before_first_restart_commit_keeps_the_legacy_tail() {
    assert_legacy_first_restart_commit(FirstRestartCommit::PowerCutBefore);
}

#[test]
fn power_cut_after_first_restart_commit_zeroes_the_extent_tail() {
    assert_extent_first_restart_commit(FirstRestartCommit::PowerCutAfter);
}

#[test]
fn power_cut_after_first_restart_commit_zeroes_the_legacy_tail() {
    assert_legacy_first_restart_commit(FirstRestartCommit::PowerCutAfter);
}

/// What happens to the first commit of a restarted shrink, the one that
/// carries its new size and orphan record.
#[derive(Clone, Copy, Debug)]
enum FirstRestartCommit {
    /// Writing the commit block fails and returns an error.
    Fails,
    /// Power is cut just before the commit block is written.
    PowerCutBefore,
    /// Power is cut just after the commit block is written.
    PowerCutAfter,
}

fn assert_extent_first_restart_commit(fault: FirstRestartCommit) {
    let mut fixture = build_large_extent_fixture("/restart-commit-extent");
    install_small_journal(&mut fixture);
    let retained_block = fixture.data_blocks[0];
    assert_first_restart_commit(
        fault,
        &fixture.power_cut,
        fixture.journal,
        fixture.filesystem,
        fixture.inode_number,
        retained_block,
    );
}

fn assert_legacy_first_restart_commit(fault: FirstRestartCommit) {
    let fixture = build_restarting_legacy_tail_fixture("/restart-commit-legacy");
    let retained_block = fixture.removed_blocks[0];
    assert_first_restart_commit(
        fault,
        &fixture.power_cut,
        fixture.journal,
        fixture.filesystem,
        fixture.inode_number,
        retained_block,
    );
}

/// Shrinks a file to 100 bytes on the restart path, applies `fault` to the
/// shrink's first commit, remounts, and checks the EOF block.
///
/// When that commit never became durable, the file must keep its old size
/// and every byte of its EOF block. When it did, recovery must finish the
/// shrink: the file has 100 bytes, those bytes are unchanged, and the rest of
/// the EOF block is zero.
fn assert_first_restart_commit(
    fault: FirstRestartCommit,
    power_cut: &PowerCutProbe,
    mut journal: Jbd2Dev<RestartDevice>,
    mut filesystem: Ext4FileSystem,
    inode_number: InodeNumber,
    retained_block: AbsoluteBN,
) {
    const NEW_SIZE: usize = 100;
    filesystem
        .datablock_cache
        .modify(&mut journal, retained_block, |block| {
            for (index, byte) in block.iter_mut().enumerate() {
                *byte = (index % 251) as u8 + 1;
            }
        })
        .expect("retained block fill failed");
    filesystem
        .sync_filesystem(&mut journal)
        .expect("fixture sync failed");
    journal.flush().expect("fixture checkpoint failed");
    let original_size = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed")
        .size();
    let mut original = vec![0u8; BLOCK_SIZE];
    read_inode_data_into(
        &mut journal,
        &mut filesystem,
        inode_number,
        0,
        &mut original,
    )
    .expect("retained block read failed");

    match fault {
        FirstRestartCommit::Fails => {
            power_cut.arm_failed_commit(1);
            let error =
                truncate_inode(&mut journal, &mut filesystem, inode_number, NEW_SIZE as u64)
                    .expect_err("a failed first commit must fail the truncate");
            assert_eq!(
                error.kind(),
                Ext4ErrorKind::Io,
                "unexpected error {error:?}"
            );
            assert_eq!(
                power_cut.commit_writes.get(),
                1,
                "the failed commit must be the first commit of the restarted shrink"
            );
        }
        FirstRestartCommit::PowerCutBefore | FirstRestartCommit::PowerCutAfter => {
            if matches!(fault, FirstRestartCommit::PowerCutBefore) {
                power_cut.arm_power_cut_before_commit(1);
            } else {
                power_cut.arm_committed_transaction_power_cut(1);
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                truncate_inode(&mut journal, &mut filesystem, inode_number, NEW_SIZE as u64)
            }));
            assert!(outcome.is_err(), "the fixture must cut power");
            let durable = if matches!(fault, FirstRestartCommit::PowerCutBefore) {
                0
            } else {
                1
            };
            assert_eq!(power_cut.commits_before_cut.get(), Some(durable));
        }
    }

    drop(filesystem);
    let device = journal.into_inner();
    device.power_cut.disable();
    if matches!(fault, FirstRestartCommit::PowerCutAfter) {
        // The cut comes before the tail is zeroed, so recovery has to do it.
        let start = retained_block.to_u32().expect("block number") as usize * BLOCK_SIZE;
        assert_eq!(
            device.bytes[start..start + BLOCK_SIZE],
            original[..],
            "the tail must still be intact on the device at the cut"
        );
    }
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("mount after the fault failed");
    let inode = remounted
        .get_inode_by_num(&mut remount_device, inode_number)
        .expect("inode read after remount failed");
    let mut data = vec![0u8; BLOCK_SIZE];
    remount_device
        .read_blocks(&mut data, retained_block, 1)
        .expect("retained block read after remount failed");
    if matches!(fault, FirstRestartCommit::PowerCutAfter) {
        assert_eq!(
            inode.size(),
            NEW_SIZE as u64,
            "recovery must keep the new size"
        );
        assert_eq!(
            remounted.superblock.s_last_orphan, 0,
            "recovery must finish the shrink"
        );
        assert_eq!(
            data[..NEW_SIZE],
            original[..NEW_SIZE],
            "the retained bytes must survive"
        );
        assert_eq!(
            data[NEW_SIZE..].iter().position(|byte| *byte != 0),
            None,
            "recovery must zero the EOF block past the new size"
        );
    } else {
        assert_eq!(inode.size(), original_size, "the shrink never committed");
        let first_changed = data.iter().zip(&original).position(|(now, was)| now != was);
        assert_eq!(
            first_changed, None,
            "a shrink whose first commit is not durable must leave every byte of the file as it \
             was"
        );
    }
}

#[test]
fn failed_commit_after_small_extent_truncate_keeps_the_tail() {
    assert_small_truncate_commit(SmallFileMapping::Extent, CommitAfterTruncate::Fails);
}

#[test]
fn failed_commit_after_small_legacy_truncate_keeps_the_tail() {
    assert_small_truncate_commit(SmallFileMapping::Legacy, CommitAfterTruncate::Fails);
}

#[test]
fn power_cut_before_commit_after_small_extent_truncate_keeps_the_tail() {
    assert_small_truncate_commit(
        SmallFileMapping::Extent,
        CommitAfterTruncate::PowerCutBefore,
    );
}

#[test]
fn power_cut_before_commit_after_small_legacy_truncate_keeps_the_tail() {
    assert_small_truncate_commit(
        SmallFileMapping::Legacy,
        CommitAfterTruncate::PowerCutBefore,
    );
}

/// How the two blocks of a small test file are mapped.
#[derive(Clone, Copy, Debug)]
enum SmallFileMapping {
    Extent,
    Legacy,
}

/// What happens to the first commit after a shrink of a small file returned.
#[derive(Clone, Copy, Debug)]
enum CommitAfterTruncate {
    /// Writing the commit block fails and returns an error.
    Fails,
    /// Power is cut just before the commit block is written.
    PowerCutBefore,
}

/// Builds a two-block file whose shrink fits in one transaction, and returns
/// its journal, filesystem, power-cut probe, inode and first data block.
fn build_small_file_fixture(
    mapping: SmallFileMapping,
) -> (
    Jbd2Dev<RestartDevice>,
    Ext4FileSystem,
    Rc<PowerCutProbe>,
    InodeNumber,
    AbsoluteBN,
) {
    const PATH: &str = "/small-truncate";
    let device = RestartDevice::new(16 * 1024);
    let power_cut = Rc::clone(&device.power_cut);
    let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut journal).expect("mkfs failed");
    let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount failed");
    let initial_data = match mapping {
        SmallFileMapping::Extent => Some(vec![0u8; 2 * BLOCK_SIZE]),
        SmallFileMapping::Legacy => None,
    };
    mkfile(
        &mut journal,
        &mut filesystem,
        PATH,
        initial_data.as_deref(),
        None,
    )
    .expect("file creation failed");
    let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, PATH)
        .expect("lookup failed")
        .expect("created inode missing")
        .0;
    if let SmallFileMapping::Legacy = mapping {
        let blocks = [
            filesystem
                .alloc_block(&mut journal)
                .expect("first data allocation failed"),
            filesystem
                .alloc_block(&mut journal)
                .expect("second data allocation failed"),
        ];
        filesystem
            .modify_inode(&mut journal, inode_number, |inode| {
                inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
                for (slot, block) in blocks.iter().enumerate() {
                    inode.i_block[slot] = block.to_u32().expect("direct block number");
                }
                inode.i_size_lo = (2 * BLOCK_SIZE) as u32;
                inode.i_size_high = 0;
                inode.i_blocks_lo = 2 * (BLOCK_SIZE / 512) as u32;
            })
            .expect("legacy inode publication failed");
    }
    let mut inode = filesystem
        .get_inode_by_num(&mut journal, inode_number)
        .expect("inode read failed");
    assert_eq!(
        inode.uses_extents(),
        matches!(mapping, SmallFileMapping::Extent)
    );
    assert_eq!(inode.size(), 2 * BLOCK_SIZE as u64);
    let first_block = rsext4::loopfile::resolve_inode_block(
        &filesystem,
        &mut journal,
        inode_number,
        &mut inode,
        0,
    )
    .expect("first block lookup failed")
    .expect("first block must be mapped");
    filesystem
        .datablock_cache
        .modify(&mut journal, first_block, |block| {
            for (index, byte) in block.iter_mut().enumerate() {
                *byte = (index % 251) as u8 + 1;
            }
        })
        .expect("first block fill failed");
    filesystem
        .sync_filesystem(&mut journal)
        .expect("fixture sync failed");
    journal.flush().expect("fixture checkpoint failed");
    (journal, filesystem, power_cut, inode_number, first_block)
}

/// Shrinks a small file to 100 bytes, applies `fault` to the first commit
/// after the shrink returned, remounts, and checks the EOF block.
///
/// Whichever size recovery keeps, the bytes inside it must be the original
/// ones and the rest of the EOF block must be zero: the old size with its
/// old tail, or 100 bytes followed by zeros.
fn assert_small_truncate_commit(mapping: SmallFileMapping, fault: CommitAfterTruncate) {
    const NEW_SIZE: usize = 100;
    let (mut journal, mut filesystem, power_cut, inode_number, first_block) =
        build_small_file_fixture(mapping);
    let original_size = 2 * BLOCK_SIZE as u64;
    let mut original = vec![0u8; BLOCK_SIZE];
    read_inode_data_into(
        &mut journal,
        &mut filesystem,
        inode_number,
        0,
        &mut original,
    )
    .expect("first block read failed");

    truncate_inode(&mut journal, &mut filesystem, inode_number, NEW_SIZE as u64)
        .expect("the shrink itself must succeed");
    match fault {
        CommitAfterTruncate::Fails => {
            power_cut.arm_failed_commit(1);
            let error = filesystem
                .sync_filesystem(&mut journal)
                .expect_err("the failed commit must fail the sync");
            assert_eq!(
                error.kind(),
                Ext4ErrorKind::Io,
                "unexpected error {error:?}"
            );
        }
        CommitAfterTruncate::PowerCutBefore => {
            power_cut.arm_power_cut_before_commit(1);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                filesystem.sync_filesystem(&mut journal)
            }));
            assert!(outcome.is_err(), "the fixture must cut power");
        }
    }
    assert_eq!(
        power_cut.commit_writes.get(),
        1,
        "the fault must hit the first commit after the shrink returned"
    );

    drop(filesystem);
    let device = journal.into_inner();
    device.power_cut.disable();
    let mut remount_device = Jbd2Dev::initial_jbd2dev(0, device, false);
    let mut remounted =
        Ext4FileSystem::mount(&mut remount_device).expect("mount after the fault failed");
    let size = remounted
        .get_inode_by_num(&mut remount_device, inode_number)
        .expect("inode read after remount failed")
        .size();
    assert!(
        size == original_size || size == NEW_SIZE as u64,
        "recovery must keep the old or the new size, not {size}"
    );
    assert_eq!(
        remounted.superblock.s_last_orphan, 0,
        "recovery must leave no orphan"
    );
    let mut data = vec![0u8; BLOCK_SIZE];
    remount_device
        .read_blocks(&mut data, first_block, 1)
        .expect("first block read after remount failed");
    let visible = core::cmp::min(size, BLOCK_SIZE as u64) as usize;
    let first_changed = data[..visible]
        .iter()
        .zip(&original[..visible])
        .position(|(now, was)| now != was);
    assert_eq!(
        first_changed, None,
        "after a {fault:?} fault, a {size}-byte file must keep every byte inside its size"
    );
    assert_eq!(
        data[visible..].iter().position(|byte| *byte != 0),
        None,
        "the EOF block past the size must be zero"
    );
}

#[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
#[test]
fn failed_tail_zeroing_after_first_restart_commit_keeps_old_extent_bytes_hidden() {
    let mut fixture = build_large_extent_fixture("/restart-zeroing-extent");
    install_small_journal(&mut fixture);
    let retained_block = fixture.data_blocks[0];
    let next_block = fixture.data_blocks[1];
    assert_failed_tail_zeroing_keeps_old_bytes_hidden(
        &fixture.power_cut,
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        retained_block,
        next_block,
        // The 30-block journal that forces the restart cannot hold the
        // worst-case reservation of an extent write that allocates.
        Some(LATER_WRITE_JOURNAL_BLOCKS),
    );
}

#[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
#[test]
fn failed_tail_zeroing_after_first_restart_commit_keeps_old_legacy_bytes_hidden() {
    let mut fixture = build_restarting_legacy_tail_fixture("/restart-zeroing-legacy");
    let retained_block = fixture.removed_blocks[0];
    // build_restarting_legacy_tail_fixture maps this block at logical block 1.
    let next_block = fixture.gap_blocks[0];
    assert_failed_tail_zeroing_keeps_old_bytes_hidden(
        &fixture.power_cut,
        &mut fixture.journal,
        &mut fixture.filesystem,
        fixture.inode_number,
        retained_block,
        next_block,
        None,
    );
}

#[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
#[test]
fn failed_tail_zeroing_after_small_truncate_commit_keeps_old_bytes_hidden() {
    for mapping in [SmallFileMapping::Extent, SmallFileMapping::Legacy] {
        let (mut journal, mut filesystem, power_cut, inode_number, first_block) =
            build_small_file_fixture(mapping);
        let mut inode = filesystem
            .get_inode_by_num(&mut journal, inode_number)
            .expect("inode read failed");
        let second_block = rsext4::loopfile::resolve_inode_block(
            &filesystem,
            &mut journal,
            inode_number,
            &mut inode,
            1,
        )
        .expect("second block lookup failed")
        .expect("second block must be mapped");
        assert_failed_tail_zeroing_keeps_old_bytes_hidden(
            &power_cut,
            &mut journal,
            &mut filesystem,
            inode_number,
            first_block,
            second_block,
            None,
        );
    }
}

#[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
const LATER_WRITE_JOURNAL_BLOCKS: u32 = 128;

/// Shrinks a file to 100 bytes and fails the write that zeroes its EOF tail,
/// after the first commit has published the new size.
///
/// The old bytes past 100 must not come back. This standalone data block
/// cache keeps the failed block dirty, so a sync must leave the tail zero on
/// the device. A later write on the same mount that grows the file into its
/// second logical block must read back zero between 100 and itself, in the
/// EOF block and in the block the shrink had not yet removed, because the
/// write finishes the shrink first.
/// `later_write_journal` replaces the checkpointed journal with one of that
/// many blocks before the later write.
#[cfg(not(feature = "USE_MULTILEVEL_CACHE"))]
fn assert_failed_tail_zeroing_keeps_old_bytes_hidden(
    power_cut: &PowerCutProbe,
    journal: &mut Jbd2Dev<RestartDevice>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    retained_block: AbsoluteBN,
    next_block: AbsoluteBN,
    later_write_journal: Option<u32>,
) {
    const NEW_SIZE: usize = 100;
    const LATER_OFFSET: usize = BLOCK_SIZE + 500;
    const LATER_BYTES: &[u8] = b"later";
    for block in [retained_block, next_block] {
        filesystem
            .datablock_cache
            .modify(journal, block, |data| {
                for (index, byte) in data.iter_mut().enumerate() {
                    *byte = (index % 251) as u8 + 1;
                }
            })
            .expect("data block fill failed");
    }
    filesystem
        .sync_filesystem(journal)
        .expect("fixture sync failed");
    journal.flush().expect("fixture checkpoint failed");
    let mut original = vec![0u8; BLOCK_SIZE];
    read_inode_data_into(journal, filesystem, inode_number, 0, &mut original)
        .expect("retained block read failed");

    power_cut.arm_failed_block_write(retained_block);
    let error = truncate_inode(journal, filesystem, inode_number, NEW_SIZE as u64)
        .expect_err("a failed tail write must fail the truncate");
    assert_eq!(
        error.kind(),
        Ext4ErrorKind::Io,
        "unexpected error {error:?}"
    );
    assert_eq!(
        power_cut.failed_block_write.get(),
        None,
        "the truncate must have written the EOF block"
    );
    let inode = filesystem
        .get_inode_by_num(journal, inode_number)
        .expect("inode read failed");
    assert_eq!(
        inode.size(),
        NEW_SIZE as u64,
        "the first commit must have published the new size"
    );
    assert_eq!(
        filesystem.superblock.s_last_orphan,
        inode_number.raw(),
        "the shrink must be left unfinished on its orphan record"
    );

    filesystem
        .sync_filesystem(journal)
        .expect("sync after the failed truncate failed");
    let mut on_device = vec![0u8; BLOCK_SIZE];
    journal
        .read_blocks(&mut on_device, retained_block, 1)
        .expect("retained block read failed");
    assert_eq!(
        on_device[NEW_SIZE..].iter().position(|byte| *byte != 0),
        None,
        "the old bytes past the committed size must not be written back"
    );

    if let Some(maxlen) = later_write_journal {
        journal
            .flush()
            .expect("checkpoint before the journal swap failed");
        install_journal_with_maxlen_raw(journal, filesystem, maxlen);
    }
    rsext4::write_inode_data(
        journal,
        filesystem,
        inode_number,
        LATER_OFFSET as u64,
        LATER_BYTES,
    )
    .expect("a later write must finish the shrink and succeed");
    let end = LATER_OFFSET + LATER_BYTES.len();
    let mut data = vec![0xffu8; end];
    let copied = read_inode_data_into(journal, filesystem, inode_number, 0, &mut data)
        .expect("read after the later write failed");
    assert_eq!(copied, end);
    assert_eq!(data[..NEW_SIZE], original[..NEW_SIZE]);
    assert_eq!(
        data[NEW_SIZE..LATER_OFFSET]
            .iter()
            .position(|byte| *byte != 0),
        None,
        "the bytes between the committed size and the later write must read as zero"
    );
    assert_eq!(&data[LATER_OFFSET..], LATER_BYTES);
    assert_eq!(
        filesystem.superblock.s_last_orphan, 0,
        "the later write must finish the shrink"
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
