//! Independent lookup exercises the mounted core, real block images and parsers.

use core::cell::Cell;

use super::*;

std::thread_local! {
    static READS: Cell<usize> = const { Cell::new(0) };
    static FAIL_READ: Cell<bool> = const { Cell::new(false) };
    static FORK_ERROR: Cell<Option<Ext4Error>> = const { Cell::new(None) };
}

#[test]
fn cold_parent_and_directory_reads_are_confined_to_execution() {
    let (mut mount, child) = cold_directory();
    let parent = mount.root_inode();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, parent)
        .unwrap();
    let before = READS.get();
    let DirectoryLookupPreparation::Parent(prepared) =
        mount.prepare_directory_lookup(parent).unwrap()
    else {
        panic!("expected an independently loaded cold parent");
    };
    assert_eq!(READS.get(), before);
    let completed = prepared.execute();
    assert_eq!(READS.get(), before + 1);
    let DirectoryLookupPreparation::Lookup(prepared) = mount
        .finish_directory_parent_read(completed)
        .unwrap()
        .unwrap()
    else {
        panic!("expected an independent directory owner");
    };
    assert_eq!(READS.get(), before + 1, "parent publication performed I/O");
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    assert!(READS.get() > before + 1, "no directory block was read");
    let after_execution = READS.get();
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Found(child)
    );
    assert_eq!(
        READS.get(),
        after_execution,
        "result publication performed I/O"
    );
}

#[test]
fn found_and_missing_match_serialized_lookup_without_loading_children() {
    let (mut mount, child) = cold_directory();
    mount
        .filesystem
        .inodetable_cache
        .evict(&mut mount.device, child)
        .unwrap();
    assert_eq!(
        lookup(&mut mount, b"input"),
        DirectoryLookupOutcome::Found(child)
    );
    assert!(
        mount
            .inode_metadata_reader()
            .try_get(child)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        lookup(&mut mount, b"absent"),
        DirectoryLookupOutcome::Missing
    );
    let parent = mount.root_inode();
    assert_eq!(
        mount.lookup_child_number(parent, name(b"input")).unwrap(),
        Some(child)
    );
    assert_eq!(
        mount.lookup_child_number(parent, name(b"absent")).unwrap(),
        None
    );
}

#[test]
fn dirty_directory_images_take_precedence_over_home_blocks() {
    let mut mount = mounted_filesystem();
    let child = create_file(&mut mount);
    let prepared = prepare(&mut mount);
    let before = READS.get();
    let failure = FailRead::new();
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    drop(failure);
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Found(child)
    );
    assert_eq!(
        READS.get(),
        before,
        "dirty directory lookup touched its old home block"
    );
}

#[cfg(feature = "USE_MULTILEVEL_CACHE")]
#[test]
fn journal_only_directory_images_are_visible_without_home_reads() {
    let mut mount = mounted_filesystem();
    mount.enable_background_writeback().unwrap();
    let child = create_file(&mut mount);
    mount
        .filesystem
        .datablock_cache
        .flush_all(&mut mount.device)
        .unwrap();
    mount.filesystem.datablock_cache.clear();
    let prepared = prepare(&mut mount);
    let before = READS.get();
    let failure = FailRead::new();
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    drop(failure);
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Found(child)
    );
    assert_eq!(READS.get(), before);
}

#[test]
fn io_failure_is_exposed_only_after_current_version_validation() {
    let (mut mount, _) = cold_directory();
    let prepared = prepare(&mut mount);
    let failure = FailRead::new();
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    drop(failure);
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap_err().kind(),
        Ext4ErrorKind::Io
    );
    assert!(matches!(
        lookup(&mut mount, b"input"),
        DirectoryLookupOutcome::Found(_)
    ));
}

#[test]
fn superseded_io_failure_requests_retry_without_leaking_an_old_error() {
    let (mut mount, child) = cold_directory();
    let prepared = prepare(&mut mount);
    let failure = FailRead::new();
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    drop(failure);
    let parent = mount.root_inode();
    mount
        .filesystem
        .modify_inode(&mut mount.device, parent, |inode| inode.set_uid(73))
        .unwrap();
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Retry
    );
    assert_eq!(
        lookup(&mut mount, b"input"),
        DirectoryLookupOutcome::Found(child)
    );
}

#[test]
fn unrelated_metadata_update_does_not_restart_a_directory_lookup() {
    let (mut mount, child) = cold_directory();
    let prepared = prepare(&mut mount);
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    mount
        .filesystem
        .modify_inode(&mut mount.device, child, |inode| inode.set_uid(73))
        .unwrap();
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Found(child)
    );
}

#[test]
fn rollback_invalidates_a_completed_lookup() {
    let (mut mount, child) = cold_directory();
    let prepared = prepare(&mut mount);
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    let parent = mount.root_inode();
    let result: Ext4Result<()> =
        mount
            .filesystem
            .with_metadata_transaction(&mut mount.device, 4, |filesystem, device| {
                filesystem.modify_inode(device, parent, |inode| inode.set_uid(73))?;
                Err(Ext4Error::no_space())
            });
    assert_eq!(result, Err(Ext4Error::no_space()));
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Retry
    );
    assert_eq!(
        lookup(&mut mount, b"input"),
        DirectoryLookupOutcome::Found(child)
    );
}

#[test]
fn foreign_mount_rejects_both_visibility_queries_and_result_publication() {
    let (mut mount, _) = cold_directory();
    let prepared = prepare(&mut mount);
    let mut foreign = mounted_filesystem();
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut foreign));
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap_err().kind(),
        Ext4ErrorKind::InvalidInput
    );
    let prepared = prepare(&mut mount);
    let completed = prepared.execute(name(b"input"), &mut Visible(&mut mount));
    assert_eq!(
        foreign
            .finish_directory_lookup(completed)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::InvalidInput
    );
}

#[test]
fn independent_lookup_preserves_legacy_direct_directory_mapping() {
    let (mut mount, child) = cold_directory();
    let parent = mount.root_inode();
    let mut inode = mount
        .filesystem
        .get_inode_by_num(&mut mount.device, parent)
        .unwrap();
    let mappings =
        resolve_inode_blocks(&mut mount.filesystem, &mut mount.device, parent, &mut inode).unwrap();
    assert!(mappings.len() <= 12);
    assert!(mappings.keys().all(|logical| *logical < 12));
    let mut pointers = [0; 15];
    for (logical, physical) in mappings {
        pointers[logical as usize] = u32::try_from(physical.raw()).unwrap();
    }
    mount
        .filesystem
        .modify_inode(&mut mount.device, parent, |inode| {
            inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = pointers;
        })
        .unwrap();
    mount.sync().unwrap();
    mount.filesystem.datablock_cache.clear();
    assert_eq!(
        lookup(&mut mount, b"input"),
        DirectoryLookupOutcome::Found(child)
    );
    assert_eq!(
        lookup(&mut mount, b"absent"),
        DirectoryLookupOutcome::Missing
    );
}

#[test]
fn a_regular_file_is_not_a_lookup_parent() {
    let (mut mount, child) = cold_directory();
    assert_eq!(
        mount.prepare_directory_lookup(child).unwrap_err().kind(),
        Ext4ErrorKind::NotDirectory
    );
}

#[test]
fn indexed_lookup_reads_only_the_selected_path_through_external_extents() {
    use crate::extents_tree::{ExtentNode, ExtentTree};

    let mut mount = mounted_filesystem();
    let parent = mount.root_inode();
    let block_size = mount.filesystem.block_size();
    let payload = alloc::vec![0x5a; block_size];
    let mut last_name = alloc::string::String::new();
    for index in 0..128 {
        last_name = alloc::format!("{index:03}{}", "a".repeat(252));
        let path = alloc::format!("/{last_name}");
        // Payload allocations separate growing directory extents, following
        // the existing directory accounting fixture's real allocation path.
        crate::mkfile(
            &mut mount.device,
            &mut mount.filesystem,
            &path,
            Some(&payload),
            None,
        )
        .unwrap();
    }
    let expected = mount
        .lookup_child_number(parent, name(last_name.as_bytes()))
        .unwrap()
        .unwrap();
    let mut inode = mount
        .filesystem
        .get_inode_by_num(&mut mount.device, parent)
        .unwrap();
    assert!(inode.is_htree_indexed());
    assert!(matches!(
        ExtentTree::with_filesystem(&mut inode, &mount.filesystem, parent)
            .load_root_from_inode()
            .unwrap(),
        ExtentNode::Index { .. }
    ));
    let directory_blocks = inode.size().div_ceil(block_size as u64);
    mount.sync().unwrap();
    mount.device.flush().unwrap();
    mount.filesystem.datablock_cache.clear();
    let prepared = prepare(&mut mount);
    let before = READS.get();

    let completed = prepared.execute(name(last_name.as_bytes()), &mut Visible(&mut mount));

    let reads = READS.get() - before;
    assert!(reads > 0);
    assert!(
        (reads as u64) < directory_blocks,
        "indexed lookup scanned the whole directory: {reads} reads for {directory_blocks} blocks"
    );
    assert_eq!(
        mount.finish_directory_lookup(completed).unwrap(),
        DirectoryLookupOutcome::Found(expected)
    );
}

#[test]
fn only_unsupported_fork_selects_serialized_lookup() {
    let (mut mount, _) = cold_directory();
    let parent = mount.root_inode();
    let failure = ForkFailure::new(Ext4Error::new(Ext4ErrorKind::UnsupportedCapability));
    assert!(matches!(
        mount.prepare_directory_lookup(parent).unwrap(),
        DirectoryLookupPreparation::Serialized
    ));
    drop(failure);
    for error in [Ext4Error::io(), Ext4Error::no_memory()] {
        let failure = ForkFailure::new(error);
        assert_eq!(mount.prepare_directory_lookup(parent).unwrap_err(), error);
        drop(failure);
    }
    assert!(matches!(
        mount.prepare_directory_lookup(parent).unwrap(),
        DirectoryLookupPreparation::Lookup(_)
    ));
}

fn cold_directory() -> (TestMount, InodeNumber) {
    let mut mount = mounted_filesystem();
    let child = create_file(&mut mount);
    mount.sync().unwrap();
    // fsync makes the journal durable, but its checkpoint images remain
    // authoritative and intentionally avoid home reads. Drain them as well.
    mount.device.flush().unwrap();
    mount.filesystem.datablock_cache.clear();
    (mount, child)
}

fn prepare(mount: &mut TestMount) -> PreparedDirectoryLookup<MemoryDevice> {
    let parent = mount.root_inode();
    match mount.prepare_directory_lookup(parent).unwrap() {
        DirectoryLookupPreparation::Lookup(prepared) => prepared,
        phase => panic!("expected a cached-parent lookup, got {phase:?}"),
    }
}

fn lookup(mount: &mut TestMount, bytes: &[u8]) -> DirectoryLookupOutcome {
    let prepared = prepare(mount);
    let completed = prepared.execute(name(bytes), &mut Visible(mount));
    mount.finish_directory_lookup(completed).unwrap()
}

fn name(bytes: &[u8]) -> FileName<'_> {
    FileName::new(bytes).unwrap()
}

struct Visible<'a>(&'a mut TestMount);

impl DirectoryReadCache for Visible<'_> {
    fn visible(&mut self, request: &DirectoryBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        self.0.directory_block_image(request)
    }
}

struct FailRead;

impl FailRead {
    fn new() -> Self {
        assert!(!FAIL_READ.replace(true));
        Self
    }
}

impl Drop for FailRead {
    fn drop(&mut self) {
        FAIL_READ.set(false);
    }
}

pub(super) fn observe_read() -> Ext4Result<()> {
    READS.set(READS.get() + 1);
    if FAIL_READ.get() {
        Err(Ext4Error::io())
    } else {
        Ok(())
    }
}

struct ForkFailure;

impl ForkFailure {
    fn new(error: Ext4Error) -> Self {
        assert!(FORK_ERROR.replace(Some(error)).is_none());
        Self
    }
}

impl Drop for ForkFailure {
    fn drop(&mut self) {
        FORK_ERROR.set(None);
    }
}

pub(super) fn check_fork() -> Ext4Result<()> {
    FORK_ERROR.get().map_or(Ok(()), Err)
}
