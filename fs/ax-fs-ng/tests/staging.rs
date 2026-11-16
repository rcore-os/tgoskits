#![cfg(feature = "host-test")]

use ax_fs_ng::MemoryFs;
use ax_runtime as _;
use axfs_ng_vfs::{
    MetadataUpdate, MutationCredentials, NameState, NodePermission, NodeType, VfsError,
};

#[test]
fn staging_preserves_copied_content_identity_and_explicit_cleanup() {
    use core::time::Duration;

    use ax_fs_ng::file::{OpenOptions, copy_staging_data};
    use axfs_ng_vfs::{
        MetadataChange, NamespaceCommandScope, NamespaceEffect, NamespaceState, PreparedWrite,
        StagingNode, TimeChange, TrustedInitialization,
    };

    struct HostPages;
    impl ax_fs_ng::os::FsPageProvider for HostPages {
        fn alloc_page(&self) -> axfs_ng_vfs::VfsResult<ax_fs_ng::os::FsPage> {
            let size = ax_fs_ng::os::memory::PAGE_SIZE;
            let layout = std::alloc::Layout::from_size_align(size, size).unwrap();
            // SAFETY: the nonzero layout is page-sized and page-aligned.
            // On success the allocation is exclusively transferred to FsPage.
            let address = unsafe { std::alloc::alloc_zeroed(layout) };
            if address.is_null() {
                return Err(VfsError::NoMemory);
            }
            // SAFETY: this unique, writable allocation has exactly one page
            // and remains owned by the token until this provider deallocates it.
            Ok(unsafe { ax_fs_ng::os::FsPage::from_raw(address as usize) })
        }
        fn dealloc_page(&self, page: ax_fs_ng::os::FsPage) {
            let size = ax_fs_ng::os::memory::PAGE_SIZE;
            let layout = std::alloc::Layout::from_size_align(size, size).unwrap();
            // SAFETY: the provider receives its own unique page token once;
            // its allocation used the identical size/alignment above.
            unsafe { std::alloc::dealloc(page.addr() as *mut u8, layout) };
        }
        fn virt_to_phys(&self, _: usize) -> Option<usize> {
            None
        }
    }
    static PAGES: HostPages = HostPages;
    ax_fs_ng::os::install_page_provider(&PAGES);

    let credentials = MutationCredentials::root();
    let filesystem = MemoryFs::new();
    let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
    let root = mount.root_location();
    let context = ax_fs_ng::FsContext::from_tree(mount.clone(), root.clone()).unwrap();
    let work = root
        .create(
            "work",
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o700),
            0,
            0,
            &credentials,
        )
        .unwrap();
    let source = root
        .create(
            "source",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o644),
            0,
            0,
            &credentials,
        )
        .unwrap();
    source
        .entry()
        .set_xattr(
            b"user.staging",
            b"native source attribute",
            axfs_ng_vfs::XattrSetMode::Upsert,
        )
        .unwrap();
    let data = b"actual native cached staging content";
    let file = OpenOptions::new()
        .write(true)
        .open(&context, "source", &credentials)
        .unwrap()
        .into_file()
        .unwrap();
    assert_eq!(file.write_at(&data[..], 0).unwrap(), data.len());
    file.sync(false).unwrap();
    drop(file);
    TrustedInitialization::kernel()
        .restore_metadata(
            &source,
            MetadataUpdate {
                mode: Some(NodePermission::from_bits_truncate(0o6755)),
                owner: Some((77, 88)),
                atime: Some(Duration::from_secs(42)),
                mtime: Some(Duration::from_secs(43)),
                ..Default::default()
            },
        )
        .unwrap();
    let write = PreparedWrite::acquire(&root).unwrap();

    let publish = |sealed: &mut axfs_ng_vfs::SealedStaging, destination: Option<&str>| {
        let command = NamespaceCommandScope::new(&write);
        let scope = command.attempt::<axfs_ng_vfs::Metadata>().unwrap();
        let observed;
        {
            let context = scope.context();
            // A real metadata root and a native staging child share the one
            // scope. The child cannot finish pressure or admissions itself.
            scope
                .change_metadata(
                    &context,
                    &root,
                    MetadataChange::Times {
                        atime: TimeChange::Now,
                        mtime: TimeChange::Now,
                    },
                    &credentials,
                )
                .unwrap();
            observed = match destination {
                Some(destination) => {
                    sealed.install(&context, (&root, destination), None, &credentials)
                }
                None => sealed.detach(&context, &credentials),
            }
            .unwrap();
        }
        let outcome = scope
            .finish()
            .unwrap_or_else(|_| panic!("staging completion owner mismatch"));
        outcome.into_result().unwrap();
        command
            .finish::<()>(Ok(axfs_ng_vfs::NamespaceOutcome::unchanged(Ok(()))))
            .unwrap()
            .into_result()
            .unwrap();
        observed
    };
    let stage = || {
        let mut stage = StagingNode::reserve(&write, &work, &source).unwrap();
        stage.create(&credentials).unwrap();
        copy_staging_data(&stage).unwrap();
        stage.copy_xattrs(&credentials).unwrap();
        stage.restore(&credentials).unwrap();
        stage
            .seal()
            .unwrap_or_else(|_| panic!("complete native stage must seal"))
    };

    let mut complete = stage();
    let identity = complete.location().entry().key();
    let observed = publish(&mut complete, Some("installed"));
    assert_eq!(
        (observed.state(), observed.result()),
        (NamespaceState::Applied, Ok(()))
    );
    assert!(matches!(
        observed.observation(),
        Some(NamespaceEffect::Rename(_))
    ));
    let installed = root.lookup_no_follow("installed").unwrap();
    assert_eq!(installed.entry().key(), identity);
    let file = OpenOptions::new()
        .read(true)
        .open(&context, "installed", &credentials)
        .unwrap()
        .into_file()
        .unwrap();
    let mut contents = [0; 64];
    let read = file.read_at(&mut contents[..], 0).unwrap();
    assert_eq!(&contents[..read], data);
    drop(file);
    let metadata = installed.metadata().unwrap();
    assert_eq!(
        (
            metadata.uid,
            metadata.gid,
            metadata.mode.bits(),
            metadata.atime.as_secs(),
            metadata.mtime.as_secs()
        ),
        (77, 88, 0o6755, 42, 43)
    );
    assert_eq!(
        installed.entry().get_xattr(b"user.staging").unwrap(),
        b"native source attribute"
    );
    complete.cleanup(&credentials).unwrap();
    assert!(root.lookup_no_follow("installed").is_ok());

    let mut anonymous = stage();
    let anonymous_name = anonymous
        .location()
        .entry()
        .name_snapshot()
        .name()
        .to_owned();
    let anonymous_identity = anonymous.location().entry().key();
    let detached = publish(&mut anonymous, None);
    assert_eq!(
        (detached.state(), detached.result()),
        (NamespaceState::Applied, Ok(()))
    );
    assert!(matches!(
        detached.observation(),
        Some(NamespaceEffect::Unlink(_))
    ));
    assert_eq!(anonymous.location().entry().key(), anonymous_identity);
    assert_eq!(
        anonymous.location().entry().name_state(),
        NameState::Unlinked
    );
    assert_eq!(anonymous.location().metadata().unwrap().nlink, 0);
    assert!(matches!(
        work.lookup_no_follow(&anonymous_name),
        Err(VfsError::NotFound)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open_loc_with_credentials(anonymous.location().clone(), &credentials)
        .unwrap()
        .into_file()
        .unwrap();
    let mut contents = [0; 64];
    let read = file.read_at(&mut contents[..], 0).unwrap();
    assert_eq!(&contents[..read], data);
    assert_eq!(file.write_at(b"orphan", 0).unwrap(), 6);
    file.sync(false).unwrap();
    drop(file);
    let replacement = work
        .create(
            &anonymous_name,
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
            &credentials,
        )
        .unwrap();
    anonymous.cleanup(&credentials).unwrap();
    assert!(
        work.lookup_no_follow(&anonymous_name)
            .unwrap()
            .entry()
            .ptr_eq(replacement.entry())
    );
    work.unlink(&anonymous_name, false, &credentials).unwrap();
    drop(replacement);

    let mut denied = StagingNode::reserve(&write, &work, &source).unwrap();
    let constrained = MutationCredentials {
        cap_chown: false,
        ..credentials
    };
    denied.create(&constrained).unwrap();
    let denied_name = denied
        .target()
        .unwrap()
        .entry()
        .name_snapshot()
        .name()
        .to_owned();
    assert_eq!(
        denied.restore(&constrained),
        Err(VfsError::OperationNotPermitted)
    );
    assert_eq!(denied.target().unwrap().metadata().unwrap().uid, 0);
    denied.cleanup(&credentials).unwrap();
    assert!(matches!(
        work.lookup_no_follow(&denied_name),
        Err(VfsError::NotFound)
    ));

    let mut incomplete = StagingNode::reserve(&write, &work, &source).unwrap();
    incomplete.create(&credentials).unwrap();
    incomplete.restore(&credentials).unwrap();
    let mut incomplete = match incomplete.seal() {
        Err((VfsError::InvalidData, stage)) => stage,
        _ => panic!("an uncopied nonempty source must not seal"),
    };
    incomplete.cleanup(&credentials).unwrap();
    drop(incomplete);

    let mut replaced = stage();
    let old_name = replaced
        .location()
        .entry()
        .name_snapshot()
        .name()
        .to_owned();
    work.rename(&old_name, &work, "moved-stage", &credentials)
        .unwrap();
    let replacement = work
        .create(
            &old_name,
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
            &credentials,
        )
        .unwrap();
    let replacement_identity = replacement.entry().key();
    let rejected = publish(&mut replaced, Some("must-stay-missing"));
    assert_eq!(
        (rejected.state(), rejected.result()),
        (NamespaceState::Unchanged, Err(VfsError::BadState))
    );
    assert!(matches!(
        root.lookup_no_follow("must-stay-missing"),
        Err(VfsError::NotFound)
    ));
    let rejected = publish(&mut replaced, None);
    assert_eq!(
        (rejected.state(), rejected.result()),
        (NamespaceState::Unchanged, Err(VfsError::BadState))
    );
    assert_eq!(replaced.cleanup(&credentials), Err(VfsError::BadState));
    assert_eq!(
        work.lookup_no_follow(&old_name).unwrap().entry().key(),
        replacement_identity
    );
    // The fixture itself moved the owned stage. Clean only those two exact
    // fixture objects; the production cleanup deliberately refused replacement.
    work.unlink("moved-stage", false, &credentials).unwrap();
    work.unlink(&old_name, false, &credentials).unwrap();
    drop(replaced);
    drop(replacement);

    let mut abandoned = stage();
    let abandoned_name = abandoned
        .location()
        .entry()
        .name_snapshot()
        .name()
        .to_owned();
    abandoned.cleanup(&credentials).unwrap();
    assert!(matches!(
        work.lookup_no_follow(&abandoned_name),
        Err(VfsError::NotFound)
    ));
    // Sealed and cleaned owners still retain the original graph until retired.
    drop(write);
    assert_eq!(mount.set_readonly(true), Err(VfsError::ResourceBusy));
    drop(abandoned);
    drop(denied);
    drop(complete);
    drop(anonymous);
    mount.set_readonly(true).unwrap();
}
