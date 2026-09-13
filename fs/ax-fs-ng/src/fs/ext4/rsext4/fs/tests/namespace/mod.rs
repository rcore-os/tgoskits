//! Atomic namespace retries observed at the real detached flush boundary.

use core::cell::RefCell;

use axfs_ng_vfs::{DirNodeOps, NodeOps, NodePermission, NodeType, RenameOptions, WritebackPolicy};

use super::*;

mod revalidation;

type FlushAction = Box<dyn FnOnce() -> BlockResult>;

std::thread_local! {
    static FLUSH_ACTION: RefCell<Option<FlushAction>> = const { RefCell::new(None) };
}

struct FlushProbe;

#[test]
fn namespace_staging_releases_lookup_before_real_flush() {
    for kind in [
        NodeType::RegularFile,
        NodeType::Directory,
        NodeType::Fifo,
        NodeType::Socket,
        NodeType::CharacterDevice,
        NodeType::BlockDevice,
    ] {
        let (filesystem, root) = fixture();
        let probe = lookup_during_staging(&filesystem, &root);

        let created = root
            .create("created", kind, NodePermission::default(), 0, 0)
            .unwrap();

        probe.finish();
        assert_eq!(created.node_type(), kind);
        assert_eq!(root.lookup("created").unwrap().inode(), created.inode());
        assert_admission_released(&filesystem);
    }
}

#[test]
fn real_checkpoint_pressure_retries_without_holding_namespace_rights() {
    let (filesystem, root) = fixture();
    let batch = filesystem
        .lock()
        .ext4
        .prepare_sync_for_checkpoint()
        .unwrap();
    let mut receipt = batch.execute();
    filesystem.lock().ext4.finish_sync(&mut receipt).unwrap();
    assert!(filesystem.lock().ext4.writeback_checkpoint_pending());
    let pressure = filesystem
        .lock()
        .ext4
        .create_regular_file(
            rsext4::MutationContext::new(0, 0, 0, 0),
            inode_number(&root),
            rsext4::FileName::new(b"pressure").unwrap(),
            rsext4::FilePermissions::new(0o644).unwrap(),
        )
        .unwrap_err();
    assert!(pressure.requires_journal_progress());
    assert_eq!(root.lookup("pressure").unwrap_err(), VfsError::NotFound);
    assert!(!filesystem.lock().staging);
    let lookup = root.clone();
    let probe = watch_flush(move || {
        lookup
            .lookup("witness")
            .expect("checkpoint pressure retained namespace exclusion");
        Ok(())
    });

    let created = create_file(&root, "pressure");

    probe.finish();
    assert_eq!(root.lookup("pressure").unwrap().inode(), created.inode());
    assert!(!filesystem.lock().ext4.writeback_checkpoint_pending());
    assert_admission_released(&filesystem);
}

#[test]
fn symlink_and_hardlink_staging_release_namespace_rights() {
    let (filesystem, root) = fixture();
    let witness = root.lookup("witness").unwrap();
    let probe = lookup_during_staging(&filesystem, &root);
    let symlink = root
        .create_symlink("symbolic", "witness", NodePermission::default(), 0, 0)
        .unwrap();
    probe.finish();
    assert_eq!(symlink.node_type(), NodeType::Symlink);

    let probe = lookup_during_staging(&filesystem, &root);
    let alias = root.link("alias", &witness).unwrap();
    probe.finish();
    assert_eq!(alias.inode(), witness.inode());
    assert_eq!(alias.metadata().unwrap().nlink, 2);
    assert_admission_released(&filesystem);
}

#[test]
fn unlink_and_rmdir_staging_release_both_namespace_scopes() {
    for kind in [NodeType::RegularFile, NodeType::Directory] {
        let (filesystem, root) = fixture();
        let victim = root
            .create("victim", kind, NodePermission::default(), 0, 0)
            .unwrap();
        let probe = lookup_during_staging(&filesystem, &root);

        root.unlink("victim", kind == NodeType::Directory).unwrap();

        probe.finish();
        assert_eq!(root.lookup("victim").unwrap_err(), VfsError::NotFound);
        assert_eq!(victim.metadata().unwrap().nlink, 0);
        let number = InodeNumber::new(victim.inode().try_into().unwrap()).unwrap();
        assert!(filesystem.lock().lifetimes.zero_link.contains(&number));
        drop(victim);
        assert!(!filesystem.lock().lifetimes.zero_link.contains(&number));
        assert_admission_released(&filesystem);
    }
}

#[test]
fn rename_replace_and_exchange_retain_current_successful_inodes() {
    for options in [RenameOptions::REPLACE, RenameOptions::EXCHANGE] {
        let (filesystem, root) = fixture();
        let source = create_file(&root, "source");
        let target = create_file(&root, "target");
        let directory = filesystem.root_dir();
        let probe = lookup_during_staging(&filesystem, &root);

        root.rename("source", directory.as_dir().unwrap(), "target", options)
            .unwrap();

        probe.finish();
        assert_eq!(root.lookup("target").unwrap().inode(), source.inode());
        if options.exchange() {
            assert_eq!(root.lookup("source").unwrap().inode(), target.inode());
            assert_eq!(target.metadata().unwrap().nlink, 1);
        } else {
            assert_eq!(root.lookup("source").unwrap_err(), VfsError::NotFound);
            assert_eq!(target.metadata().unwrap().nlink, 0);
        }
        assert_admission_released(&filesystem);
    }
}

#[test]
fn failed_pressure_flush_returns_io_without_publishing_a_child() {
    let (filesystem, root) = fixture();
    filesystem.lock().staging = true;
    let lookup = root.clone();
    let probe = watch_flush(move || {
        lookup.lookup("witness").unwrap();
        Err(BlockError::Io)
    });

    assert_eq!(create_result(&root, "failed").unwrap_err(), VfsError::Io);

    probe.finish();
    assert!(!filesystem.lock().staging);
    assert!(filesystem.lock().ext4.writeback_failure().is_some());
    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    assert_admission_released(&filesystem);
}

#[test]
fn non_retryable_namespace_error_does_not_wait_for_journal_progress() {
    let (filesystem, root) = fixture();
    let probe = watch_flush(|| panic!("AlreadyExists must not force journal progress"));

    assert_eq!(
        create_result(&root, "witness").unwrap_err(),
        VfsError::AlreadyExists
    );

    assert!(FLUSH_ACTION.with_borrow(Option::is_some));
    drop(probe);
    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    assert_admission_released(&filesystem);
}

#[test]
fn closed_mount_admission_rejects_namespace_mutation_without_a_guard_leak() {
    let (filesystem, root) = fixture();
    filesystem.admission.close_and_drain().unwrap();

    assert_eq!(
        create_result(&root, "closed").unwrap_err(),
        VfsError::ResourceBusy
    );

    assert!(filesystem.namespace.lookup(inode_number(&root)).is_ok());
    filesystem.admission.reopen();
    create_file(&root, "reopened");
}

fn fixture() -> (Arc<Ext4Filesystem>, Arc<Inode>) {
    let (filesystem, root, _) = super::sync_policy::background_mount(WritebackPolicy::empty());
    let root: Arc<Inode> = root.entry().downcast().unwrap();
    create_file(&root, "witness");
    (filesystem, root)
}

fn create_file(root: &Inode, name: &str) -> DirEntry {
    create_result(root, name).unwrap()
}

fn create_result(root: &Inode, name: &str) -> VfsResult<DirEntry> {
    root.create(name, NodeType::RegularFile, NodePermission::default(), 0, 0)
}

fn inode_number(inode: &Inode) -> InodeNumber {
    InodeNumber::new(inode.inode().try_into().unwrap()).unwrap()
}

fn lookup_during_staging(filesystem: &Arc<Ext4Filesystem>, root: &Arc<Inode>) -> FlushProbe {
    filesystem.lock().staging = true;
    let filesystem = filesystem.clone();
    let root = root.clone();
    watch_flush(move || {
        assert!(
            filesystem.inner.try_lock().is_some(),
            "flush retained mount state"
        );
        root.lookup("witness")
            .expect("namespace lookup blocked while journal I/O progresses");
        Ok(())
    })
}

fn assert_admission_released(filesystem: &Ext4Filesystem) {
    // The fixture has no waiting runtime: a leaked admission fails this drain.
    filesystem.admission.close_and_drain().unwrap();
    filesystem.admission.reopen();
}

fn watch_flush(action: impl FnOnce() -> BlockResult + 'static) -> FlushProbe {
    FLUSH_ACTION.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Box::new(action));
    });
    FlushProbe
}

impl FlushProbe {
    fn finish(self) {
        assert!(
            FLUSH_ACTION.with_borrow(Option::is_none),
            "no real device flush observed"
        );
    }
}

impl Drop for FlushProbe {
    fn drop(&mut self) {
        let action = FLUSH_ACTION.with_borrow_mut(Option::take);
        drop(action);
    }
}

pub(super) fn observe_device_flush() -> BlockResult {
    let action = FLUSH_ACTION.with_borrow_mut(Option::take);
    action.map_or(Ok(()), |action| action())
}
