//! Physical cold inode reads exercise the production admission/lock boundary.

use core::cell::RefCell;

use axfs_ng_vfs::NodeOps;
use rsext4::{FileName, FilePermissions, MutationContext};

use super::*;

std::thread_local! {
    static PROBE: RefCell<Option<ReadProbe>> = const { RefCell::new(None) };
}

struct ReadProbe {
    filesystem: Arc<Ext4Filesystem>,
    number: InodeNumber,
    armed: bool,
    reads: usize,
    interference: ReadInterference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadInterference {
    None,
    Unlink,
    UnlinkAfterChildRetention,
    CreateElsewhere(InodeNumber),
    IoFailure,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        PROBE.with_borrow_mut(|slot| *slot = None);
    }
}

#[test]
fn cold_metadata_io_does_not_hold_the_mount_lock() {
    let (filesystem, input) = cold_inode();
    let _probe = watch(&filesystem, &input, ReadInterference::None);

    assert_eq!(input.metadata().unwrap().size, 5);

    assert_read_observed();
}

#[test]
fn unlink_during_cold_metadata_io_cannot_reap_the_live_inode() {
    let (filesystem, input) = cold_inode();
    let probe = watch(&filesystem, &input, ReadInterference::Unlink);

    assert_eq!(input.metadata().unwrap().nlink, 0);

    assert_read_observed();
    assert!(filesystem.lock().claim_pending_reap().is_none());
    // The independent read is complete. Final reference release below runs
    // the existing serialized reap writer, not the cold read being observed.
    drop(probe);
    drop(input);
    assert!(!filesystem.lock().has_pending_reaps());
}

#[test]
fn failed_cold_inode_read_releases_admission_and_retains_the_reference() {
    let (filesystem, input) = cold_inode();
    let probe = watch(&filesystem, &input, ReadInterference::IoFailure);

    assert_eq!(input.metadata().unwrap_err(), VfsError::Io);

    assert_read_observed();
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
    drop(probe);
    assert_eq!(input.metadata().unwrap().size, 5);
}

#[test]
fn child_lookup_keeps_its_reference_across_cold_metadata_io_and_unlink() {
    let (filesystem, input) = cold_inode();
    let root = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        state.retain_inode(&filesystem, root)
    };
    let probe = watch(
        &filesystem,
        &input,
        ReadInterference::UnlinkAfterChildRetention,
    );

    let located = root
        .lookup(FileName::new(b"input").unwrap())
        .unwrap()
        .unwrap();

    assert_read_observed();
    assert_eq!(located.lifetime.number().as_u64(), input.inode());
    assert_eq!(located.lifetime.metadata().unwrap().links, 0);
    drop(input);
    assert!(filesystem.lock().claim_pending_reap().is_none());
    // Stop observing reads before the final owner triggers the reap writer.
    drop(probe);
    drop(located);
    assert!(!filesystem.lock().has_pending_reaps());
}

#[test]
fn directory_io_allows_a_different_directory_to_create_an_entry() {
    let (filesystem, input, other) = remounted_input();
    // Keep parent and child inode metadata warm so the observed I/O can only
    // come from directory/mapping blocks, not an inode-table cache miss.
    input.metadata().unwrap();
    let root = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        state.retain_inode(&filesystem, root)
    };
    assert!(
        filesystem
            .inode_metadata
            .try_get(root.number())
            .unwrap()
            .is_some()
    );
    assert!(
        filesystem
            .inode_metadata
            .try_get(InodeNumber::new(input.inode() as u32).unwrap())
            .unwrap()
            .is_some()
    );
    let _probe = watch(
        &filesystem,
        &input,
        ReadInterference::CreateElsewhere(other),
    );

    let located = root
        .lookup(FileName::new(b"input").unwrap())
        .unwrap()
        .unwrap();

    assert_eq!(located.lifetime.number().as_u64(), input.inode());
    assert_read_observed();
    PROBE.with_borrow(|slot| {
        assert_eq!(slot.as_ref().unwrap().interference, ReadInterference::None)
    });
    assert!(
        filesystem
            .lock()
            .ext4
            .lookup_child_number(other, FileName::new(b"during-read").unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn failed_directory_lookup_releases_namespace_and_mount_admission() {
    let (filesystem, input, _) = remounted_input();
    let root = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        state.retain_inode(&filesystem, root)
    };
    let probe = watch(&filesystem, &input, ReadInterference::IoFailure);

    assert!(matches!(
        root.lookup(FileName::new(b"input").unwrap()),
        Err(VfsError::Io)
    ));

    assert_read_observed();
    drop(probe);
    drop(
        filesystem
            .namespace
            .change(root.number(), NamespaceChange::Topology)
            .unwrap(),
    );
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
    assert!(
        root.lookup(FileName::new(b"input").unwrap())
            .unwrap()
            .is_some()
    );
}

fn cold_inode() -> (Arc<Ext4Filesystem>, Arc<Inode>) {
    let (filesystem, input, _) = remounted_input();
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    assert!(filesystem.inode_metadata.try_get(number).unwrap().is_none());
    (filesystem, input)
}

fn remounted_input() -> (Arc<Ext4Filesystem>, Arc<Inode>, InodeNumber) {
    let (storage, flushes) = formatted_test_storage();
    let (filesystem, _) = mount_test_storage(storage.clone(), flushes.clone(), false);
    let (input, other) = {
        let mut state = filesystem.lock();
        let root = state.ext4.root_inode();
        let input = state
            .ext4
            .create_regular_file(
                MutationContext::new(0, 0, 0, 0),
                root,
                FileName::new(b"input").unwrap(),
                FilePermissions::new(0o600).unwrap(),
            )
            .unwrap()
            .number;
        state.ext4.write_inode(input, 0, b"hello").unwrap();
        let other = state
            .ext4
            .create_directory(
                MutationContext::new(0, 0, 0, 0),
                root,
                FileName::new(b"other").unwrap(),
                FilePermissions::new(0o700).unwrap(),
            )
            .unwrap()
            .number;
        state.unmount().unwrap();
        (input, other)
    };
    drop(filesystem);
    let (filesystem, _) = mount_test_storage(storage, flushes, false);
    let filesystem = Arc::new(filesystem);
    let lifetime = {
        let mut state = filesystem.lock();
        // The fixture just cleanly remounted its own unchanged image. The
        // authoritative created allocation is still live, but its inode cache
        // is cold. Construct no stale wrapper from the retired mount.
        state.retain_inode(&filesystem, input)
    };
    (filesystem, Inode::new(lifetime, None), other)
}

fn watch(
    filesystem: &Arc<Ext4Filesystem>,
    input: &Inode,
    interference: ReadInterference,
) -> ProbeGuard {
    PROBE.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(ReadProbe {
            filesystem: filesystem.clone(),
            number: InodeNumber::new(input.inode() as u32).unwrap(),
            armed: false,
            reads: 0,
            interference,
        });
    });
    ProbeGuard
}

fn assert_read_observed() {
    PROBE.with_borrow(|slot| {
        let probe = slot.as_ref().unwrap();
        assert!(probe.armed, "no independent inode endpoint was prepared");
        assert!(probe.reads > 0, "no physical inode-table read was observed");
        assert!(
            !matches!(
                probe.interference,
                ReadInterference::Unlink | ReadInterference::UnlinkAfterChildRetention
            ),
            "unlink never overlapped a physical read"
        );
    });
}

pub(super) fn observe_fork() {
    PROBE.with_borrow_mut(|slot| {
        if let Some(probe) = slot {
            probe.armed = true;
        }
    });
}

pub(super) fn observe_read() -> BlockResult {
    // Temporarily remove the probe: the interfering unlink runs real core I/O,
    // which must not recursively execute the same interference callback.
    let Some(mut probe) = PROBE.with_borrow_mut(Option::take) else {
        return Ok(());
    };
    let result = if probe.armed {
        assert!(
            probe.filesystem.inner.try_lock().is_some(),
            "cold inode I/O retained the mount lock"
        );
        probe.reads += 1;
        if let ReadInterference::CreateElsewhere(parent) = probe.interference {
            probe.interference = ReadInterference::None;
            let _namespace = probe
                .filesystem
                .namespace
                .change(parent, NamespaceChange::Directory)
                .unwrap();
            probe
                .filesystem
                .lock()
                .ext4
                .create_regular_file(
                    MutationContext::new(0, 0, 0, 0),
                    parent,
                    FileName::new(b"during-read").unwrap(),
                    FilePermissions::new(0o600).unwrap(),
                )
                .unwrap();
        }
        // Lookup now reads directory blocks independently too. Interleave
        // unlink only after its child pin was published and the namespace read
        // guard released; an inline writer before that point would deadlock the
        // test itself instead of representing a schedulable concurrent task.
        if probe.interference == ReadInterference::UnlinkAfterChildRetention
            && probe
                .filesystem
                .lock()
                .lifetimes
                .live_refs
                .get(&probe.number)
                .copied()
                .unwrap_or(0)
                >= 2
        {
            probe.interference = ReadInterference::Unlink;
        }
        if probe.interference == ReadInterference::Unlink {
            probe.interference = ReadInterference::None;
            let root = probe.filesystem.lock().ext4.root_inode();
            let _namespace = probe
                .filesystem
                .namespace
                .change(root, NamespaceChange::Directory)
                .unwrap();
            let mut state = probe.filesystem.lock();
            let outcome = state
                .ext4
                .unlink(root, FileName::new(b"input").unwrap())
                .unwrap();
            assert_eq!(outcome.inode, probe.number);
            assert!(outcome.requires_reap());
            assert!(state.publish_zero_link(probe.number).is_none());
        }
        if probe.interference == ReadInterference::IoFailure {
            Err(BlockError::Io)
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    PROBE.with_borrow_mut(|slot| *slot = Some(probe));
    result
}
